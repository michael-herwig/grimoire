// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! Shared output layer.
//!
//! Structured command results render through [`Printable`] so the
//! plain-text table and `--format json` always derive from one source.
//! Each `print_plain` implementation must make exactly one
//! [`print_table`] call (single-table rule, see `subsystem-cli-api.md`).

use std::borrow::Cow;
use std::io::{self, IsTerminal, Write};

/// A command result that can render as a plain table or as JSON.
pub trait Printable {
    /// Renders a human-readable aligned table.
    ///
    /// # Errors
    ///
    /// Returns any I/O error from writing to `w`.
    fn print_plain(&self, w: &mut impl Write) -> io::Result<()>;

    /// Renders machine-readable pretty JSON.
    ///
    /// # Errors
    ///
    /// Returns any I/O or serialization error encountered while writing.
    fn print_json(&self, w: &mut impl Write) -> io::Result<()>;
}

/// Writes a column-aligned table.
///
/// Columns are padded to the widest cell (header or data). Headers are
/// static `&str` slices; callers add columns rather than formatting
/// dynamic headers.
///
/// # Errors
///
/// Returns any I/O error from writing to `w`.
pub fn print_table(w: &mut impl Write, headers: &[&str], rows: &[Vec<String>]) -> io::Result<()> {
    const GAP: &str = "  ";

    let mut widths: Vec<usize> = headers.iter().map(|h| h.len()).collect();
    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            if i < widths.len() {
                widths[i] = widths[i].max(cell.len());
            } else {
                widths.push(cell.len());
            }
        }
    }

    write_row(w, &widths, headers.iter().map(|h| h.as_ref()), GAP)?;
    for row in rows {
        write_row(w, &widths, row.iter().map(String::as_str), GAP)?;
    }
    Ok(())
}

fn write_row<'a>(
    w: &mut impl Write,
    widths: &[usize],
    cells: impl Iterator<Item = &'a str>,
    gap: &str,
) -> io::Result<()> {
    let mut line = String::new();
    for (i, cell) in cells.enumerate() {
        if i > 0 {
            line.push_str(gap);
        }
        let width = widths.get(i).copied().unwrap_or(0);
        line.push_str(cell);
        for _ in cell.len()..width {
            line.push(' ');
        }
    }
    // Trailing padding on the last column is not meaningful; trim it so
    // table output stays byte-stable regardless of column order.
    writeln!(w, "{}", line.trim_end())
}

/// The current terminal width in columns, or `None` when stdout is not a
/// terminal (piped to a file, a pager, or a test harness).
///
/// Callers treat `None` as "do not truncate" so non-interactive output
/// stays complete and byte-deterministic (the git/ls convention).
#[must_use]
pub fn terminal_width() -> Option<usize> {
    if !io::stdout().is_terminal() {
        return None;
    }
    crossterm::terminal::size().ok().map(|(cols, _)| cols as usize)
}

/// Truncate `s` to at most `max` characters, appending an ellipsis (`…`)
/// when it overflows.
///
/// Operates on `char` boundaries so multi-byte text never splits
/// mid-codepoint. The ellipsis counts toward `max`. Returns the input
/// borrowed unchanged when it already fits.
#[must_use]
pub fn truncate_ellipsis(s: &str, max: usize) -> Cow<'_, str> {
    if max == 0 {
        return Cow::Borrowed("");
    }
    if s.chars().count() <= max {
        return Cow::Borrowed(s);
    }
    let keep: String = s.chars().take(max - 1).collect();
    Cow::Owned(format!("{keep}…"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn print_table_aligns_columns() {
        let mut buf = Vec::new();
        print_table(
            &mut buf,
            &["Name", "Kind"],
            &[
                vec!["code-review".to_string(), "skill".to_string()],
                vec!["rust-style".to_string(), "rule".to_string()],
            ],
        )
        .unwrap();
        let out = String::from_utf8(buf).unwrap();
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines[0], "Name         Kind");
        assert_eq!(lines[1], "code-review  skill");
        assert_eq!(lines[2], "rust-style   rule");
    }

    #[test]
    fn print_table_header_wider_than_data() {
        let mut buf = Vec::new();
        print_table(&mut buf, &["Header"], &[vec!["hi".to_string()]]).unwrap();
        let out = String::from_utf8(buf).unwrap();
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines[0], "Header");
        assert_eq!(lines[1], "hi");
    }

    #[test]
    fn print_table_no_rows_writes_header_only() {
        let mut buf = Vec::new();
        print_table(&mut buf, &["A", "B"], &[]).unwrap();
        assert_eq!(String::from_utf8(buf).unwrap(), "A  B\n");
    }

    #[test]
    fn truncate_ellipsis_leaves_short_strings_borrowed() {
        let out = truncate_ellipsis("short", 10);
        assert_eq!(out, "short");
        assert!(matches!(out, Cow::Borrowed(_)), "no allocation when it fits");
    }

    #[test]
    fn truncate_ellipsis_appends_ellipsis_on_overflow() {
        // 1 ellipsis char counts toward the budget ⇒ 4 kept + "…" = 5 chars.
        assert_eq!(truncate_ellipsis("abcdefgh", 5), "abcd…");
        assert_eq!(truncate_ellipsis("abcdefgh", 5).chars().count(), 5);
    }

    #[test]
    fn truncate_ellipsis_respects_char_boundaries() {
        // Multi-byte chars must never be split mid-codepoint.
        let out = truncate_ellipsis("héllo wörld", 6);
        assert_eq!(out, "héllo…");
        assert_eq!(out.chars().count(), 6);
    }

    #[test]
    fn truncate_ellipsis_zero_max_is_empty() {
        assert_eq!(truncate_ellipsis("anything", 0), "");
    }
}
