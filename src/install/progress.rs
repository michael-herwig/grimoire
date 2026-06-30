// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! Install progress seam.
//!
//! The installer loop drives an [`InstallProgress`] sink once per artifact
//! so a front-end can render feedback without the install module knowing
//! how (DIP). `grim install` injects a stderr bar
//! ([`crate::cli::progress::StderrBar`]); the TUI (owns the terminal),
//! `update`, and unit tests use [`SilentProgress`] so nothing is written
//! over a full-screen frame or into captured test output.

/// A sink the installer notifies as it works through the locked artifacts.
///
/// Call sequence: one [`start`](Self::start), then one
/// [`advance`](Self::advance) immediately before each artifact installs
/// (1-based `position`), then one [`finish`](Self::finish). All methods
/// take `&self`; a rendering implementation uses interior mutability.
pub trait InstallProgress {
    /// Total number of artifacts about to be processed.
    fn start(&self, total: usize);
    /// About to install the `position`-th artifact (1-based), labelled `label`.
    fn advance(&self, position: usize, label: &str);
    /// Every artifact processed — clear or finalize any rendered line.
    fn finish(&self);
}

/// A no-op sink: the installer runs silently. Used by the TUI (which owns
/// the terminal), `update`, and unit tests.
pub struct SilentProgress;

impl InstallProgress for SilentProgress {
    fn start(&self, _total: usize) {}
    fn advance(&self, _position: usize, _label: &str) {}
    fn finish(&self) {}
}
