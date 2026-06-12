// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! RAII raw-mode guard shared by every full-screen TUI session.
//!
//! Both the main catalog browser ([`super::app`]) and the pre-session
//! init dialog ([`super::init_dialog`]) run inside the alternate screen
//! in raw mode; this guard is the single place that transition lives so
//! a panic or early return never leaves the user's shell in raw mode.

use std::io;

use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode};

/// Restores the terminal on drop — even if the body panics or returns an
/// error — so a crash never leaves the user's shell in raw mode.
pub struct TerminalGuard;

impl TerminalGuard {
    /// Enter raw mode and the alternate screen.
    ///
    /// # Errors
    ///
    /// Any terminal-setup I/O failure from crossterm.
    pub fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        crossterm::execute!(io::stdout(), EnterAlternateScreen)?;
        Ok(Self)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = crossterm::execute!(io::stdout(), LeaveAlternateScreen);
        let _ = disable_raw_mode();
    }
}
