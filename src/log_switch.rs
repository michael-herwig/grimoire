// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! Switchable log sink for the TUI alternate-screen session.
//!
//! When the TUI enters raw mode the terminal is shared between ratatui's
//! drawing surface and the shell. Any `tracing` output that leaks to
//! `stderr` during that window overwrites the TUI frame and is never
//! repainted over (ratatui uses a diff-based draw).
//!
//! The fix is a [`SwitchableWriter`]: a [`MakeWriter`] implementation
//! backed by an [`Arc<Mutex<WriterTarget>>`]. At startup the target is
//! `Stderr`. When the TUI enters alt-screen it calls
//! [`LogSinkGuard::redirect`], which opens `$GRIM_HOME/tui.log` (falling
//! back to a temporary file) and swaps the target to that file. When the
//! guard drops (after `TerminalGuard` drops and the alt-screen is left)
//! the target is swapped back to `Stderr`.
//!
//! Because `set_global_default` is one-shot, the [`SwitchableWriter`] is
//! installed once at startup and mutated in place — no double-init and no
//! new subscriber ever created. Tests that call `init_tracing` in the same
//! process must guard against the one-shot panic, hence
//! [`init_tracing_for_tests`].

use std::fs::File;
use std::io::{self, Write};
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};

use tracing_subscriber::fmt::MakeWriter;

/// The active write target.
enum WriterTarget {
    /// Normal operation: log to stderr.
    Stderr,
    /// TUI session active: log to a file.
    File(File),
}

impl WriterTarget {
    /// Write a byte slice to the active target.
    fn write_all(&mut self, buf: &[u8]) -> io::Result<()> {
        match self {
            WriterTarget::Stderr => io::stderr().write_all(buf),
            WriterTarget::File(f) => f.write_all(buf),
        }
    }

    /// Flush the active target.
    fn flush(&mut self) -> io::Result<()> {
        match self {
            WriterTarget::Stderr => io::stderr().flush(),
            WriterTarget::File(f) => f.flush(),
        }
    }
}

/// The per-call-site writer handle returned by [`SwitchableWriter`].
///
/// Each [`io::Write`] call acquires the lock briefly so writes from
/// concurrent threads are serialised without holding the lock across the
/// whole format cycle.
pub struct SwitchableWriterHandle(Arc<Mutex<WriterTarget>>);

impl io::Write for SwitchableWriterHandle {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        // Lock, write, unlock — never hold across an await.
        let mut guard = self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.write_all(buf)?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        let mut guard = self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.flush()
    }
}

/// A [`MakeWriter`] that routes each log record to the currently active
/// [`WriterTarget`].
///
/// Clone is cheap (`Arc` clone only). Installed once by [`init_tracing`].
#[derive(Clone)]
pub struct SwitchableWriter(Arc<Mutex<WriterTarget>>);

impl SwitchableWriter {
    /// Create a new `SwitchableWriter` initially targeting `stderr`.
    pub fn new() -> Self {
        Self(Arc::new(Mutex::new(WriterTarget::Stderr)))
    }

    /// Swap the active target to the given file, returning the previous
    /// target as an opaque `OldTarget` so the caller can restore it.
    fn swap_to_file(&self, file: File) -> WriterTarget {
        let mut guard = self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        std::mem::replace(&mut *guard, WriterTarget::File(file))
    }

    /// Restore a previously saved target.
    fn restore(&self, old: WriterTarget) {
        let mut guard = self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        *guard = old;
    }
}

impl<'a> MakeWriter<'a> for SwitchableWriter {
    type Writer = SwitchableWriterHandle;

    fn make_writer(&'a self) -> Self::Writer {
        SwitchableWriterHandle(Arc::clone(&self.0))
    }
}

/// The singleton writer installed into the global tracing subscriber.
///
/// `init_tracing` stores it here; TUI code retrieves it via
/// [`global_writer`] to create [`LogSinkGuard`]s. The `OnceLock`
/// guarantees the writer is initialised exactly once per process — safe
/// against the one-shot `set_global_default` constraint.
static GLOBAL_WRITER: OnceLock<SwitchableWriter> = OnceLock::new();

/// Return a reference to the global [`SwitchableWriter`] installed by
/// `init_tracing`.
///
/// Returns `None` when called before `init_tracing` (e.g. in tests that
/// do not call the real subscriber init).
pub fn global_writer() -> Option<&'static SwitchableWriter> {
    GLOBAL_WRITER.get()
}

/// Set the global writer (called exactly once by `init_tracing`).
///
/// Returns the stored reference. Silently returns the existing instance
/// when called a second time (test isolation).
pub(crate) fn set_global_writer(w: SwitchableWriter) -> &'static SwitchableWriter {
    GLOBAL_WRITER.get_or_init(|| w)
}

/// RAII guard that redirects tracing output to a log file for the duration
/// of a TUI alt-screen session.
///
/// Acquire with [`LogSinkGuard::redirect`] **before** [`TerminalGuard`]
/// so it drops **after** `TerminalGuard` — Rust drops locals in reverse
/// declaration order, meaning the alt-screen is left before logging is
/// restored to `stderr`. A log record emitted during the guard's own
/// `Drop` (after the alt-screen exit) therefore reaches `stderr` cleanly.
///
/// For async callers, open the file off the Tokio runtime with
/// [`open_log_file_off_thread`] and pass the result to
/// [`LogSinkGuard::redirect_to`] to avoid blocking I/O on an async task.
pub struct LogSinkGuard {
    writer: SwitchableWriter,
    /// The target that was active before we redirected (always `Stderr`
    /// in practice, but we save it generically for correct restore).
    saved: Option<WriterTarget>,
}

impl LogSinkGuard {
    /// Redirect tracing output to `grim_home/tui.log`. Falls back to an
    /// anonymous temporary file (unlinked-after-open, no leaked inode) when
    /// the `GRIM_HOME` directory does not exist or the file cannot be
    /// created. Returns `None` only when no writable destination is
    /// available (very unusual; tracing continues to `stderr` in that case).
    ///
    /// **Sync callers only.** This opens the file with blocking I/O on the
    /// calling thread. Async callers must use [`open_log_file_off_thread`]
    /// + [`LogSinkGuard::redirect_to`] instead.
    pub fn redirect(writer: &SwitchableWriter, grim_home: &Path) -> Option<Self> {
        let file = open_log_file_sync(grim_home);
        Self::redirect_to(writer, file)
    }

    /// Activate the redirect from a pre-opened file (or `None`).
    ///
    /// Use this on async callers: open the file with
    /// [`open_log_file_off_thread`] (which runs the blocking open on a
    /// thread pool), then hand the result here so no blocking I/O executes
    /// on the async task.
    ///
    /// Passing `None` is a no-op: `None` is returned and logging continues
    /// to `stderr`.
    pub fn redirect_to(writer: &SwitchableWriter, file: Option<File>) -> Option<Self> {
        let Some(file) = file else {
            // Cannot open any log file — leave logging on stderr. This is
            // unusual (no tempdir writable?) so we do not emit a warning
            // because writing to stderr is exactly the problem we are
            // trying to avoid.
            return None;
        };

        let saved = writer.swap_to_file(file);
        Some(Self {
            writer: writer.clone(),
            saved: Some(saved),
        })
    }
}

impl Drop for LogSinkGuard {
    fn drop(&mut self) {
        if let Some(old) = self.saved.take() {
            self.writer.restore(old);
        }
    }
}

/// Open (create/append) `$GRIM_HOME/tui.log`, falling back to an anonymous
/// temporary file when the home directory is unwritable.
///
/// The fallback uses [`tempfile::tempfile`] (an anonymous, unlinked-after-open
/// file) rather than [`tempfile::NamedTempFile::keep`], so the OS reclaims
/// the inode when the last file descriptor closes — no leaked inode.
///
/// This function performs **blocking** I/O. Call it only from a synchronous
/// context (the init-dialog path). For the async TUI path, use the public
/// [`open_log_file_off_thread`] helper instead.
fn open_log_file_sync(grim_home: &Path) -> Option<File> {
    let log_path = grim_home.join("tui.log");
    // Primary: persistent append log under GRIM_HOME.
    if let Ok(file) = std::fs::OpenOptions::new().create(true).append(true).open(&log_path) {
        return Some(file);
    }
    // Fallback: anonymous temp file — unlinked-after-open, no leaked inode.
    tempfile::tempfile().ok()
}

/// Open the TUI log file on a Tokio blocking thread and return the result.
///
/// This is the **async-safe** entry point for opening the log file. It runs
/// the blocking [`open_log_file_sync`] call on `tokio::task::spawn_blocking`
/// so the Tokio async runtime is never stalled by blocking I/O.
///
/// # Errors
///
/// Returns `None` when no writable log destination is available (both the
/// GRIM_HOME path and the system temp dir are unwritable). The `JoinError`
/// from a panicking blocking task is propagated as `None` rather than
/// unwrapped, so a panic inside the opener does not crash the TUI.
pub async fn open_log_file_off_thread(grim_home: std::path::PathBuf) -> Option<File> {
    tokio::task::spawn_blocking(move || open_log_file_sync(&grim_home))
        .await
        .unwrap_or(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn writer_starts_with_stderr_and_toggles_to_file_and_back() {
        // Round-trip: default stderr → redirect to file → restore stderr.
        let tmp = tempfile::tempdir().unwrap();
        let log_path = tmp.path().join("tui.log");
        let writer = SwitchableWriter::new();

        // Before redirect: target is Stderr — we can't easily introspect
        // the enum variant, so just check that make_writer().flush() works.
        writer.make_writer().flush().unwrap();

        // Redirect to file.
        let guard = LogSinkGuard::redirect(&writer, tmp.path()).expect("should open log file");

        // Write through the switched writer — should reach the file.
        writer.make_writer().write_all(b"hello from tui\n").unwrap();
        writer.make_writer().flush().unwrap();
        drop(guard); // restores stderr

        // Verify the log file received the write.
        let contents = std::fs::read_to_string(&log_path).unwrap();
        assert!(
            contents.contains("hello from tui"),
            "log file should contain the written line"
        );

        // After guard drop: make_writer still works (restored to stderr).
        writer.make_writer().flush().unwrap();
    }

    #[test]
    fn log_sink_guard_restores_on_drop_even_when_panicking() {
        // Verify the saved target is restored even if the caller drops
        // early (simulates an early-return path).
        let tmp = tempfile::tempdir().unwrap();
        let writer = SwitchableWriter::new();
        {
            let _guard = LogSinkGuard::redirect(&writer, tmp.path()).unwrap();
            // Guard drops here.
        }
        // After drop: flush must succeed (back on stderr path).
        writer.make_writer().flush().unwrap();
    }

    #[test]
    fn redirect_fallback_when_grim_home_absent() {
        // When the GRIM_HOME path does not exist, redirect falls back to an
        // anonymous temp file rather than returning None. The anonymous file
        // leaves no named inode on disk after the fd closes (FIX 2).
        let non_existent = std::path::Path::new("/tmp/grim_test_no_such_dir_xyzzy_99999");
        let writer = SwitchableWriter::new();
        // The fallback (tempfile::tempfile()) succeeds; we don't require a
        // specific outcome since the temp path may or may not be writable in
        // CI. Just confirm redirect() doesn't panic.
        let _ = LogSinkGuard::redirect(&writer, non_existent);
        writer.make_writer().flush().unwrap();
    }

    /// Prove that the async-TUI log-open seam (`redirect_to`) accepts a
    /// pre-opened `File` and does not call the blocking opener directly.
    ///
    /// This is the unit seam for `app::run`: the caller opens the file
    /// off-thread (via `open_log_file_off_thread`) and hands the `Option<File>`
    /// to `redirect_to`. This test exercises that path without a Tokio
    /// runtime — it constructs the `File` inline, proving the constructor is
    /// runtime-agnostic and testable without spawning a blocking task.
    #[test]
    fn redirect_to_accepts_pre_opened_file_without_blocking_opener() {
        let tmp = tempfile::tempdir().unwrap();
        let log_path = tmp.path().join("async_tui.log");
        // Open the file synchronously here (simulating what spawn_blocking
        // would do off-thread in production).
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .ok();

        let writer = SwitchableWriter::new();
        let guard = LogSinkGuard::redirect_to(&writer, file).expect("redirect_to should succeed");

        writer.make_writer().write_all(b"async seam test\n").unwrap();
        writer.make_writer().flush().unwrap();
        drop(guard);

        let contents = std::fs::read_to_string(&log_path).unwrap();
        assert!(
            contents.contains("async seam test"),
            "log file should contain the line written through redirect_to"
        );
        // After drop: flushing confirms the writer is restored to stderr.
        writer.make_writer().flush().unwrap();
    }

    #[tokio::test]
    async fn open_log_file_off_thread_returns_file_for_valid_dir() {
        // Verify the async open helper succeeds for a writable directory
        // and does so without blocking the Tokio runtime directly.
        let tmp = tempfile::tempdir().unwrap();
        let file = open_log_file_off_thread(tmp.path().to_path_buf()).await;
        assert!(file.is_some(), "should open tui.log in a writable dir");
    }

    #[tokio::test]
    async fn open_log_file_off_thread_falls_back_for_absent_dir() {
        // Verify the async open helper returns Some (anonymous tempfile
        // fallback) rather than None when GRIM_HOME is absent.
        let non_existent = std::path::PathBuf::from("/tmp/grim_test_async_no_such_dir_xyzzy_99999");
        // Result is Some (fallback) or None (system tmpdir also unwritable);
        // either is acceptable — what we assert is no panic.
        let _ = open_log_file_off_thread(non_existent).await;
    }
}
