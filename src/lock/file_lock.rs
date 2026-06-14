// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! RAII exclusive advisory lock guarding a config file.
//!
//! A thin newtype over the reusable [`AdvisoryFileLock`] mechanism (sidecar
//! flock, ghost-inode re-check, `O_NOFOLLOW`, Windows delete-pending
//! handling — see that module for the full rationale). `ConfigFileLock`
//! names the config-flock call sites (`grim add` / `login` / `install`
//! read-modify-write under the `<grimoire.toml>.lock` sidecar); the catalog
//! refresh coordinator uses [`AdvisoryFileLock`] directly for the
//! per-registry cache file.

use std::path::Path;

use crate::lock::advisory_lock::AdvisoryFileLock;
use crate::lock::lock_error::LockError;

/// A held exclusive advisory lock keyed by a config-file path.
///
/// Dropping the guard removes the sidecar and releases the lock (delegated
/// to [`AdvisoryFileLock`]).
#[derive(Debug)]
pub struct ConfigFileLock(#[allow(dead_code)] AdvisoryFileLock);

impl ConfigFileLock {
    /// Try to acquire the exclusive advisory lock for `config_path` (held on
    /// the `<file>.lock` sidecar). Non-blocking: another holder yields
    /// [`crate::lock::lock_error::LockErrorKind::Locked`] immediately.
    ///
    /// The config file itself need not exist; its parent directory does.
    ///
    /// # Errors
    ///
    /// - [`crate::lock::lock_error::LockErrorKind::Locked`] — another writer
    ///   holds the lock.
    /// - [`crate::lock::lock_error::LockErrorKind::Io`] — the config path is
    ///   a symlink, or the sidecar could not be opened.
    pub fn try_acquire(config_path: &Path) -> Result<Self, LockError> {
        AdvisoryFileLock::try_acquire(config_path).map(Self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lock::lock_error::LockErrorKind;

    #[test]
    fn second_acquire_on_held_config_is_locked() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("grimoire.toml");
        std::fs::write(&cfg, "[skills]\n").unwrap();

        let first = ConfigFileLock::try_acquire(&cfg).expect("first acquire succeeds");
        let err = ConfigFileLock::try_acquire(&cfg).expect_err("second acquire must fail");
        assert!(matches!(err.kind, LockErrorKind::Locked));

        drop(first);
        ConfigFileLock::try_acquire(&cfg).expect("acquire after release succeeds");
    }

    #[test]
    fn reader_is_unaffected_by_held_lock() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("grimoire.toml");
        std::fs::write(&cfg, "[skills]\nx = \"ghcr.io/acme/x:1\"\n").unwrap();

        let _guard = ConfigFileLock::try_acquire(&cfg).expect("acquire");
        // A reader does not lock — plain read must complete immediately.
        let content = std::fs::read_to_string(&cfg).expect("reader unaffected");
        assert!(content.contains("ghcr.io/acme/x:1"));
    }

    #[test]
    fn sidecar_appends_full_lock_suffix() {
        // `.lock` is appended to the whole file name, never substituted for
        // the extension: `grimoire.toml` → `grimoire.toml.lock`, never the
        // `grimoire.lock` package lockfile.
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("grimoire.toml");
        std::fs::write(&cfg, "[skills]\n").unwrap();

        let _guard = ConfigFileLock::try_acquire(&cfg).expect("acquire");
        assert!(dir.path().join("grimoire.toml.lock").exists());
        assert!(!dir.path().join("grimoire.lock").exists());
    }
}
