// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! Stderr install progress bar.
//!
//! A single in-place line redrawn with a carriage return as each artifact
//! installs, then erased when the pass finishes so the result table
//! (stdout) starts on a clean line. Rendered only when stderr is a
//! terminal — the caller gates on `is_terminal`, so piped / non-interactive
//! runs use [`crate::install::progress::SilentProgress`] and keep machine
//! output and captured test streams free of control codes.

use std::cell::Cell;
use std::io::{self, Write};

use crate::install::progress::InstallProgress;

use super::printer::truncate_ellipsis;

/// Width of the textual bar (the `[####----]` field), in cells.
const BAR_WIDTH: usize = 20;
/// Fallback terminal width when the size cannot be queried.
const FALLBACK_COLS: usize = 80;

/// Renders install progress as a redrawing stderr line.
#[derive(Default)]
pub struct StderrBar {
    /// Total artifact count, learned from [`InstallProgress::start`].
    total: Cell<usize>,
}

impl InstallProgress for StderrBar {
    fn start(&self, total: usize) {
        self.total.set(total);
    }

    fn advance(&self, position: usize, label: &str) {
        let cols = crossterm::terminal::size().map_or(FALLBACK_COLS, |(c, _)| c as usize);
        let line = render_bar(position, self.total.get(), label, cols);
        // `\r` returns to column 0; `\x1b[K` erases to end of line so a
        // shorter label never leaves stale tail characters from a longer one.
        // ponytail: raw ANSI (no indicatif dep); a rare `tracing::warn!` to
        // stderr mid-pass can smear one frame — cosmetic, redrawn on the next
        // advance. Reach for indicatif's `suspend()` only if logs interleave
        // often enough to matter.
        // Best-effort: write/flush errors are ignored — a broken pipe on a
        // cosmetic bar must never fail an install.
        let mut err = io::stderr().lock();
        let _ = write!(err, "\r{line}\x1b[K");
        let _ = err.flush();
    }

    fn finish(&self) {
        // Erase the bar so the result table (stdout) starts on a clean line.
        // Best-effort (see `advance`): a write/flush failure here is ignored.
        let mut err = io::stderr().lock();
        let _ = write!(err, "\r\x1b[K");
        let _ = err.flush();
    }
}

/// Build the progress line `[####----] p/total label`, clamped to `cols`
/// so it never wraps (a wrapped line breaks the carriage-return redraw).
fn render_bar(position: usize, total: usize, label: &str, cols: usize) -> String {
    let total = total.max(1);
    let position = position.min(total);
    let filled = position * BAR_WIDTH / total;
    let mut bar = String::with_capacity(BAR_WIDTH);
    for cell in 0..BAR_WIDTH {
        bar.push(if cell < filled { '#' } else { '-' });
    }
    let prefix = format!("[{bar}] {position}/{total} ");
    // The label takes whatever space remains after the fixed-width prefix.
    let budget = cols.saturating_sub(prefix.chars().count());
    format!("{prefix}{}", truncate_ellipsis(label, budget))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_bar_fills_proportionally() {
        // 1 of 4 ⇒ 1*20/4 = 5 filled cells.
        let line = render_bar(1, 4, "skill code-review", 80);
        assert!(line.starts_with("[#####---------------] 1/4 "), "got: {line}");
        assert!(line.ends_with("skill code-review"));
    }

    #[test]
    fn render_bar_full_at_completion() {
        let line = render_bar(3, 3, "rule rust-style", 80);
        assert!(line.starts_with("[####################] 3/3 "), "got: {line}");
    }

    #[test]
    fn render_bar_truncates_label_to_terminal_width() {
        // The 27-col prefix fits; the long label is clamped to the remaining
        // 13 cols so the whole line stays within the 40-col terminal and
        // never wraps (a wrapped line breaks the carriage-return redraw).
        let line = render_bar(1, 1, "a-very-long-artifact-name-that-overflows", 40);
        assert_eq!(line.chars().count(), 40, "line must fit the terminal: {line}");
        assert!(line.contains('…'), "overflowing label is ellipsized: {line}");
    }

    #[test]
    fn render_bar_clamps_position_over_total() {
        // Defensive: a position past the total clamps so the counter and the
        // bar never overflow (unreachable from the installer, guarded anyway).
        let line = render_bar(5, 3, "x", 80);
        assert!(line.starts_with("[####################] 3/3 "), "got: {line}");
    }

    #[test]
    fn render_bar_zero_total_does_not_panic() {
        // An empty lock yields total 0; treat as 1 so the divide is safe.
        let line = render_bar(0, 0, "", 80);
        assert!(line.starts_with("[--------------------] 0/1 "), "got: {line}");
    }
}
