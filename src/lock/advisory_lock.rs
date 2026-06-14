// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! RAII exclusive advisory lock guarding an arbitrary file by a sidecar.
//!
//! Writers serialize through an exclusive lock on a `<file>.lock`
//! **sidecar** next to the guarded file (`grimoire.toml` →
//! `grimoire.toml.lock`, `catalog/<hash>.json` →
//! `catalog/<hash>.json.lock`) before mutating the state the path
//! identifies. The data file itself is never byte-range locked: Windows
//! `LockFileEx` locks are *mandatory*, so locking the data file directly
//! made every other handle's read fail with `ERROR_LOCK_VIOLATION`
//! (os error 33) — including the lock holder's own re-read (Windows CI
//! regression). With the sidecar the lock is genuinely advisory on every
//! platform: readers never lock — concurrent reads are always allowed and
//! always observe a complete file via the atomic-rename guarantee — and the
//! lock holder may freely re-read and atomically replace the data file while
//! holding the lock.
//!
//! A symlink planted at the guarded path is rejected outright (a planted
//! link is an attack signal, not data), and the sidecar is opened with
//! `O_NOFOLLOW` on Unix so a symlink cannot redirect the lock to an
//! attacker-chosen file. `O_NOFOLLOW` is applied via
//! [`std::os::unix::fs::OpenOptionsExt::custom_flags`] — a **safe** method —
//! so the crate-wide `forbid(unsafe_code)` is honoured with no `unsafe`
//! block anywhere on this path. On non-Unix a `symlink_metadata` pre-check
//! is the best-effort equivalent (narrow TOCTOU window, acceptable on
//! platforms without `O_NOFOLLOW`).
//!
//! This is the reusable mechanism; [`super::file_lock::ConfigFileLock`] is a
//! thin newtype over it for the config-flock call sites, and the catalog
//! refresh coordinator locks the per-registry cache file with it directly.

use std::ffi::OsString;
use std::fs::File;
use std::path::{Path, PathBuf};

use fs4::fs_std::FileExt;

use crate::lock::lock_error::{LockError, LockErrorKind};

/// A held exclusive advisory lock keyed by a guarded-file path.
///
/// Dropping the guard removes the sidecar (best-effort, while the lock is
/// still held) and then releases the lock by closing the handle — see
/// [`Drop`] below for the platform-specific ordering rationale.
#[derive(Debug)]
pub struct AdvisoryFileLock {
    // Held so the fd stays open for the lock's lifetime; closing it (on
    // drop) releases the lock. Never read directly.
    file: File,
    // The sidecar path, retained so Drop can unlink it.
    sidecar: PathBuf,
}

impl AdvisoryFileLock {
    /// Bounded retries for the unlink-vs-acquire window: an acquired lock
    /// whose sidecar was concurrently unlinked (a holder dropping) or a
    /// Windows delete-pending open failure triggers a fresh open+lock
    /// attempt. Contention itself never retries — it returns `Locked`.
    const MAX_ATTEMPTS: u32 = 8;

    /// Try to acquire the exclusive advisory lock for `target_path` (held on
    /// the `<file>.lock` sidecar, created when missing and removed again when
    /// the guard drops).
    ///
    /// Non-blocking: if another process holds the lock this returns
    /// [`LockErrorKind::Locked`] immediately rather than waiting.
    ///
    /// Because a dropping holder unlinks the sidecar while still holding the
    /// lock, an acquire that won the lock must prove the inode it locked is
    /// still what the sidecar path names; otherwise it locked a ghost (an
    /// unlinked or replaced file) and another process could re-create the
    /// path and lock it concurrently. On a mismatch the attempt is discarded
    /// and the open+lock retried (bounded).
    ///
    /// The guarded file itself does not have to exist; its parent directory
    /// does (the sidecar is created beside it).
    ///
    /// # Errors
    ///
    /// - [`LockErrorKind::Locked`] — another writer holds the lock.
    /// - [`LockErrorKind::Io`] — the guarded path is a symlink, or the
    ///   sidecar could not be opened (missing parent directory, permission
    ///   denied, or a symlink on Unix via `O_NOFOLLOW`).
    pub fn try_acquire(target_path: &Path) -> Result<Self, LockError> {
        // Reject a symlinked guarded path outright (defense in depth).
        if let Ok(meta) = std::fs::symlink_metadata(target_path)
            && meta.file_type().is_symlink()
        {
            return Err(LockError::new(
                target_path,
                LockErrorKind::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "guarded path is a symlink",
                )),
            ));
        }

        let sidecar = sidecar_path(target_path);
        let mut last_io: Option<std::io::Error> = None;
        for _ in 0..Self::MAX_ATTEMPTS {
            let file = match open_sidecar(target_path) {
                Ok(f) => f,
                // Windows: a sidecar marked delete-pending by a dropping
                // holder refuses new opens until the holder's handle closes
                // (moments later). Treat it as the transient window it is and
                // retry.
                Err(e)
                    if cfg!(windows)
                        && matches!(&e.kind, LockErrorKind::Io(io) if io.kind() == std::io::ErrorKind::PermissionDenied) =>
                {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                    continue;
                }
                Err(e) => return Err(e),
            };

            match FileExt::try_lock_exclusive(&file) {
                Ok(()) => {
                    if sidecar_still_current(&file, &sidecar) {
                        return Ok(Self { file, sidecar });
                    }
                    // Locked a ghost inode (holder unlinked it between our
                    // open and lock) — discard and retry on the live path.
                    continue;
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    return Err(LockError::new(target_path, LockErrorKind::Locked));
                }
                Err(e) => last_io = Some(e),
            }
        }
        Err(LockError::new(
            target_path,
            LockErrorKind::Io(
                last_io.unwrap_or_else(|| {
                    std::io::Error::other("lock sidecar kept changing underneath the acquire retries")
                }),
            ),
        ))
    }
}

impl Drop for AdvisoryFileLock {
    fn drop(&mut self) {
        // Unlink the sidecar BEFORE the handle closes, i.e. while the lock is
        // still held — a waiter can then never lock the doomed inode without
        // `try_acquire`'s identity re-check catching it.
        //
        // - Unix: unlinking an open, locked file is plain; the fd (and the
        //   lock) lives until the close below.
        // - Windows: Rust's std opens with FILE_SHARE_DELETE, so the remove
        //   marks the name delete-pending; the name disappears when our
        //   handle (the last one) closes right after. Concurrent opens during
        //   the pending window fail with a permission error, which
        //   `try_acquire` maps to a bounded retry.
        //
        // Best-effort: a failure leaves the sidecar behind, which is the
        // pre-cleanup status quo and never breaks correctness.
        let _ = std::fs::remove_file(&self.sidecar);
        // `self.file` closes after this body returns, releasing the lock.
    }
}

/// Whether the locked handle still corresponds to the file the sidecar path
/// names. On Unix this compares `(dev, ino)`; a missing path or a different
/// inode means a dropping holder unlinked/replaced the sidecar after we
/// opened it. On non-Unix platforms `std` exposes no stable file identity,
/// so the check degrades to "the path still exists" — the delete-pending
/// semantics of the Drop impl cover the rest of the window.
fn sidecar_still_current(file: &File, sidecar: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let Ok(held) = file.metadata() else { return false };
        let Ok(on_disk) = std::fs::metadata(sidecar) else {
            return false;
        };
        held.dev() == on_disk.dev() && held.ino() == on_disk.ino()
    }
    #[cfg(not(unix))]
    {
        let _ = file;
        sidecar.exists()
    }
}

/// The sidecar lock path for `target_path`: the full file name with `.lock`
/// **appended** (`grimoire.toml` → `grimoire.toml.lock`). Appended, not
/// substituted — `with_extension` would map `grimoire.toml` onto
/// `grimoire.lock`, the package lockfile.
fn sidecar_path(target_path: &Path) -> PathBuf {
    let mut name = target_path.file_name().map(OsString::from).unwrap_or_default();
    name.push(".lock");
    target_path.with_file_name(name)
}

/// Open (creating when missing) the sidecar lock file for `target_path`
/// without following a terminal symlink. No `unsafe`: `custom_flags` is a
/// safe `OpenOptionsExt` method. Errors are keyed to `target_path` — the
/// path the user knows about.
fn open_sidecar(target_path: &Path) -> Result<File, LockError> {
    let sidecar = sidecar_path(target_path);
    let mut opts = std::fs::OpenOptions::new();
    opts.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.custom_flags(libc::O_NOFOLLOW);
    }
    #[cfg(not(unix))]
    if let Ok(meta) = std::fs::symlink_metadata(&sidecar)
        && meta.file_type().is_symlink()
    {
        return Err(LockError::new(
            target_path,
            LockErrorKind::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "lock sidecar path is a symlink",
            )),
        ));
    }
    opts.open(&sidecar)
        .map_err(|e| LockError::new(target_path, LockErrorKind::Io(e)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_acquire_on_held_target_is_locked() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("grimoire.toml");
        std::fs::write(&target, "[skills]\n").unwrap();

        let first = AdvisoryFileLock::try_acquire(&target).expect("first acquire succeeds");
        let err = AdvisoryFileLock::try_acquire(&target).expect_err("second acquire must fail");
        assert!(matches!(err.kind, LockErrorKind::Locked));

        drop(first);
        AdvisoryFileLock::try_acquire(&target).expect("acquire after release succeeds");
    }

    #[test]
    fn reader_is_unaffected_by_held_lock() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("grimoire.toml");
        std::fs::write(&target, "[skills]\nx = \"ghcr.io/acme/x:1\"\n").unwrap();

        let _guard = AdvisoryFileLock::try_acquire(&target).expect("acquire");
        // A reader does not lock — plain read must complete immediately.
        let content = std::fs::read_to_string(&target).expect("reader unaffected");
        assert!(content.contains("ghcr.io/acme/x:1"));
    }

    #[test]
    fn holder_can_read_and_replace_target_under_lock() {
        // Regression: locking the data file itself made the holder's own
        // re-read fail on Windows with ERROR_LOCK_VIOLATION (os error 33) —
        // `LockFileEx` locks are mandatory, not advisory.
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("config.json");
        std::fs::write(&target, b"{}").unwrap();

        let _guard = AdvisoryFileLock::try_acquire(&target).expect("acquire");
        let read = std::fs::read(&target).expect("holder re-read must succeed under the held lock");
        assert_eq!(read, b"{}");
        crate::store::atomic_write::atomic_write(&target, b"{\"auths\":{}}")
            .expect("atomic replace must succeed under the held lock");
        assert_eq!(std::fs::read(&target).unwrap(), b"{\"auths\":{}}");
    }

    #[test]
    fn sidecar_appends_full_lock_suffix() {
        // `.lock` is appended to the whole file name, never substituted for
        // the extension: `grimoire.toml` must map to `grimoire.toml.lock`,
        // not `grimoire.lock` (the package lockfile).
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("grimoire.toml");
        std::fs::write(&target, "[skills]\n").unwrap();

        let _guard = AdvisoryFileLock::try_acquire(&target).expect("acquire");
        assert!(dir.path().join("grimoire.toml.lock").exists());
        assert!(!dir.path().join("grimoire.lock").exists());
    }

    #[test]
    fn sidecar_removed_on_drop_and_reacquirable() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("grimoire.toml");
        std::fs::write(&target, "[skills]\n").unwrap();
        let sidecar = dir.path().join("grimoire.toml.lock");

        let guard = AdvisoryFileLock::try_acquire(&target).expect("acquire");
        assert!(sidecar.exists(), "sidecar exists while the lock is held");
        drop(guard);
        assert!(!sidecar.exists(), "drop must remove the sidecar");

        // The cleaned-up path stays fully lockable.
        let again = AdvisoryFileLock::try_acquire(&target).expect("re-acquire after cleanup");
        assert!(sidecar.exists());
        drop(again);
        assert!(!sidecar.exists());
    }

    #[test]
    fn acquire_retries_past_a_ghost_inode() {
        // Simulate the drop race: a holder unlinks the sidecar while a waiter
        // has already opened it. The waiter's lock on the unlinked inode must
        // be discarded and re-acquired on the live path.
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("grimoire.toml");
        std::fs::write(&target, "[skills]\n").unwrap();
        let sidecar = dir.path().join("grimoire.toml.lock");

        // Plant a sidecar and unlink it right away — any pre-opened handle
        // (the race victim) would now point at a ghost inode. try_acquire
        // itself must end up holding a lock on the CURRENT on-disk file.
        std::fs::write(&sidecar, b"").unwrap();
        std::fs::remove_file(&sidecar).unwrap();

        let guard = AdvisoryFileLock::try_acquire(&target).expect("acquire on the live sidecar");
        assert!(
            sidecar_still_current(&guard.file, &sidecar),
            "the held lock must be on the file the path names"
        );
    }

    #[test]
    fn acquire_succeeds_when_target_missing() {
        // The sidecar carries the lock, so the guarded file itself need not
        // exist yet (first write against a fresh path).
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("config.json");
        AdvisoryFileLock::try_acquire(&target).expect("missing target is lockable");
        assert!(!target.exists(), "lock must not create the guarded file");
    }

    #[cfg(unix)]
    #[test]
    fn symlink_target_path_rejected() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("sensitive");
        let link = dir.path().join("grimoire.toml");
        symlink(&target, &link).unwrap();

        let err = AdvisoryFileLock::try_acquire(&link).expect_err("symlink must reject");
        // O_NOFOLLOW → ELOOP, surfaced as Io (not Locked).
        assert!(matches!(err.kind, LockErrorKind::Io(_)));
        assert!(!target.exists(), "symlink target must not be created");
    }
}
