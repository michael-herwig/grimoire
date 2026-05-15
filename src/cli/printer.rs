// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! Shared output layer.
//!
//! Structured command results render through [`Printable`] so the
//! plain-text table and `--format json` always derive from one source.
//! Each `print_plain` implementation must make exactly one
//! [`print_table`] call (single-table rule, see `subsystem-cli-api.md`).

use std::io::{self, Write};

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
}
