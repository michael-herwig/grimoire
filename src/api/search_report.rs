// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! `grim search` output.
//!
//! Plain format: a five-column table
//! (Kind | Repo | Summary | Version | Status). The `Summary` cell shows the
//! short summary, falling back to the (truncated) description; the `Version`
//! cell shows the highest concrete (non-rolling) tag, falling back to the
//! representative tag. A deprecated row carries a comma-suffixed `deprecated`
//! in its `Status` cell (e.g. `installed,deprecated`).
//!
//! JSON format: an array of
//! `{kind, repo, summary, description, version, latest_tag, repository,
//! deprecated, status}` objects (the report wraps a `Vec`, serialized to the
//! bare array — no wrapper object, per subsystem-cli-api.md). The
//! `description` stays full and untruncated; both `version` and the
//! representative `latest_tag` are kept; `repository` is the HTTPS source URL
//! or `null`; `deprecated` is the deprecation message or `null`.

use std::io::{self, Write};

use serde::{Serialize, Serializer};

use crate::cli::printer::{Printable, print_table, terminal_width, truncate_ellipsis};
use crate::install::status_badge::StatusBadge;

/// One catalog match annotated with its install status.
#[derive(Debug, Clone)]
pub struct SearchEntry {
    /// `skill` / `rule`, or `None` if the manifest declared no kind.
    pub kind: Option<String>,
    /// The `registry/repository` reference.
    pub repo: String,
    /// The catalog description, if any.
    pub description: Option<String>,
    /// The short catalog summary, if any. Preferred over `description`
    /// for the plain-text column; the full `description` stays in JSON.
    pub summary: Option<String>,
    /// The HTTPS source-repository URL from the catalog read-back guard,
    /// if any. JSON-only — never shown as its own plain-table column.
    pub repository: Option<String>,
    /// The representative tag the metadata was read from (may be the moving
    /// `latest` pointer). Kept in JSON for fidelity; the plain table shows
    /// `version` instead.
    pub latest_tag: Option<String>,
    /// The highest concrete (non-rolling) version tag, if any tag parses as
    /// semver. Shown in the plain `Version` column in preference to the
    /// moving `latest_tag`.
    pub version: Option<String>,
    /// The publisher's deprecation message when the artifact is deprecated;
    /// `None` otherwise. Surfaced as a comma-suffixed `deprecated` on the plain
    /// `Status` cell and a dedicated `deprecated` field in JSON.
    pub deprecated: Option<String>,
    /// How the repository relates to the current scope.
    pub status: StatusBadge,
}

impl Serialize for SearchEntry {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut s = serializer.serialize_struct("SearchEntry", 9)?;
        s.serialize_field("kind", &self.kind)?;
        s.serialize_field("repo", &self.repo)?;
        s.serialize_field("summary", &self.summary)?;
        s.serialize_field("description", &self.description)?;
        s.serialize_field("version", &self.version)?;
        s.serialize_field("latest_tag", &self.latest_tag)?;
        s.serialize_field("repository", &self.repository)?;
        s.serialize_field("deprecated", &self.deprecated)?;
        s.serialize_field("status", &self.status.to_string())?;
        s.end()
    }
}

/// The result of a catalog search: one row per matching repository.
#[derive(Debug)]
pub struct SearchReport {
    entries: Vec<SearchEntry>,
}

impl SearchReport {
    /// Build from operation results.
    pub fn new(entries: Vec<SearchEntry>) -> Self {
        Self { entries }
    }
}

impl Serialize for SearchReport {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.entries.serialize(serializer)
    }
}

/// The blurb column's character budget for a terminal `width` columns wide,
/// given the `fixed` width the other columns and gaps consume. Clamped to a
/// readable window so the column never collapses to nothing or sprawls.
fn blurb_budget(width: usize, fixed: usize) -> usize {
    const MIN: usize = 24;
    const MAX: usize = 60;
    width.saturating_sub(fixed).clamp(MIN, MAX)
}

impl Printable for SearchReport {
    fn print_plain(&self, w: &mut impl Write) -> io::Result<()> {
        // The displayed blurb prefers the short summary, falling back to
        // the long description. On a real terminal it is clamped to a
        // readable window so a verbose description can't wrap the table;
        // piped/non-TTY output stays full and byte-deterministic.
        const HEADERS: [&str; 5] = ["Kind", "Repo", "Summary", "Version", "Status"];
        /// Two-space gaps between the five columns.
        const GAP_TOTAL: usize = (HEADERS.len() - 1) * 2;
        /// Index of the blurb column the budget applies to.
        const BLURB: usize = 2;

        let mut rows: Vec<Vec<String>> = self
            .entries
            .iter()
            .map(|e| {
                let base = e.summary.as_deref().or(e.description.as_deref()).unwrap_or("-");
                // Deprecation rides a comma-suffixed Status cell (e.g.
                // `installed,deprecated`) — greppable, and keeping the Summary
                // cell unmarked.
                let status = if e.deprecated.is_some() {
                    format!("{},deprecated", e.status)
                } else {
                    e.status.to_string()
                };
                vec![
                    e.kind.clone().unwrap_or_else(|| "-".to_string()),
                    e.repo.clone(),
                    base.to_string(),
                    // Prefer the concrete (non-rolling) version; fall back to
                    // the representative tag only when no semver tag exists.
                    e.version
                        .clone()
                        .or_else(|| e.latest_tag.clone())
                        .unwrap_or_else(|| "-".to_string()),
                    status,
                ]
            })
            .collect();

        if let Some(width) = terminal_width() {
            // Width the fixed columns (everything but the blurb) consume.
            let col_width = |i: usize| {
                rows.iter()
                    .map(|r| r[i].chars().count())
                    .chain(std::iter::once(HEADERS[i].chars().count()))
                    .max()
                    .unwrap_or(0)
            };
            let fixed = col_width(0) + col_width(1) + col_width(3) + col_width(4) + GAP_TOTAL;
            let budget = blurb_budget(width, fixed);
            for r in &mut rows {
                r[BLURB] = truncate_ellipsis(&r[BLURB], budget).into_owned();
            }
        }

        print_table(w, &HEADERS, &rows)
    }

    fn print_json(&self, w: &mut impl Write) -> io::Result<()> {
        let json = serde_json::to_string_pretty(self).map_err(io::Error::other)?;
        writeln!(w, "{json}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(repo: &str, status: StatusBadge) -> SearchEntry {
        SearchEntry {
            kind: Some("skill".to_string()),
            repo: repo.to_string(),
            summary: None,
            repository: None,
            description: Some("desc".to_string()),
            latest_tag: Some("latest".to_string()),
            version: None,
            deprecated: None,
            status,
        }
    }

    #[test]
    fn plain_single_table_with_header() {
        let r = SearchReport::new(vec![entry("localhost:5000/acme/x", StatusBadge::Installed)]);
        let mut buf = Vec::new();
        r.print_plain(&mut buf).unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(out.lines().next().unwrap().starts_with("Kind"));
        assert!(out.contains("installed"));
        assert!(out.contains("acme/x"));
    }

    #[test]
    fn json_is_bare_array() {
        let r = SearchReport::new(vec![entry("localhost:5000/acme/x", StatusBadge::NotInstalled)]);
        let mut buf = Vec::new();
        r.print_json(&mut buf).unwrap();
        let v: serde_json::Value = serde_json::from_slice(&buf).unwrap();
        assert!(v.is_array());
        assert_eq!(v[0]["kind"], "skill");
        assert_eq!(v[0]["status"], "not-installed");
        assert_eq!(v[0]["repo"], "localhost:5000/acme/x");
    }

    #[test]
    fn blurb_budget_clamps_to_readable_window() {
        // Narrow terminal ⇒ floor; the blurb never collapses below MIN.
        assert_eq!(blurb_budget(40, 30), 24);
        // Roomy terminal ⇒ ceiling; never sprawls past MAX.
        assert_eq!(blurb_budget(500, 30), 60);
        // In-window ⇒ exact remaining width.
        assert_eq!(blurb_budget(80, 30), 50);
        // Fixed columns already exceed the width ⇒ saturates to the floor.
        assert_eq!(blurb_budget(20, 100), 24);
    }

    #[test]
    fn plain_prefers_version_over_rolling_latest_tag() {
        let e = SearchEntry {
            kind: Some("skill".to_string()),
            repo: "localhost:5000/acme/x".to_string(),
            summary: Some("blurb".to_string()),
            repository: None,
            description: None,
            latest_tag: Some("latest".to_string()),
            version: Some("2.1.0".to_string()),
            deprecated: None,
            status: StatusBadge::Installed,
        };
        let mut buf = Vec::new();
        SearchReport::new(vec![e]).print_plain(&mut buf).unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(out.lines().next().unwrap().contains("Version"), "header renamed");
        assert!(out.contains("2.1.0"), "concrete version shown, not the moving tag");
        assert!(!out.contains("latest"), "the rolling 'latest' pointer is not shown");
    }

    #[test]
    fn plain_version_falls_back_to_latest_tag() {
        // No semver tag ⇒ the representative tag is the only thing to show.
        let e = SearchEntry {
            kind: Some("rule".to_string()),
            repo: "localhost:5000/acme/y".to_string(),
            summary: None,
            repository: None,
            description: Some("d".to_string()),
            latest_tag: Some("stable".to_string()),
            version: None,
            deprecated: None,
            status: StatusBadge::NotInstalled,
        };
        let mut buf = Vec::new();
        SearchReport::new(vec![e]).print_plain(&mut buf).unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(out.contains("stable"));
    }

    #[test]
    fn plain_prefers_summary_over_description() {
        let e = SearchEntry {
            kind: Some("skill".to_string()),
            repo: "localhost:5000/acme/x".to_string(),
            summary: Some("short blurb".to_string()),
            repository: None,
            description: Some("a much longer description that should be hidden".to_string()),
            latest_tag: Some("latest".to_string()),
            version: None,
            deprecated: None,
            status: StatusBadge::Installed,
        };
        let mut buf = Vec::new();
        SearchReport::new(vec![e]).print_plain(&mut buf).unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(out.contains("short blurb"));
        assert!(!out.contains("much longer description"));
    }

    #[test]
    fn plain_falls_back_to_description_without_summary() {
        let e = SearchEntry {
            kind: Some("skill".to_string()),
            repo: "localhost:5000/acme/x".to_string(),
            summary: None,
            repository: None,
            description: Some("the description text".to_string()),
            latest_tag: Some("latest".to_string()),
            version: None,
            deprecated: None,
            status: StatusBadge::NotInstalled,
        };
        let mut buf = Vec::new();
        SearchReport::new(vec![e]).print_plain(&mut buf).unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(out.contains("the description text"));
    }

    #[test]
    fn plain_does_not_truncate_when_not_a_tty() {
        // Tests capture stdout to a pipe, so `terminal_width()` is `None`
        // and the full blurb is emitted untruncated (no ellipsis).
        let long = "x".repeat(200);
        let e = SearchEntry {
            kind: Some("skill".to_string()),
            repo: "localhost:5000/acme/x".to_string(),
            summary: Some(long.clone()),
            repository: None,
            description: None,
            latest_tag: Some("latest".to_string()),
            version: None,
            deprecated: None,
            status: StatusBadge::Installed,
        };
        let mut buf = Vec::new();
        SearchReport::new(vec![e]).print_plain(&mut buf).unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(out.contains(&long), "piped output keeps full text");
        assert!(!out.contains('…'), "no ellipsis when piped");
    }

    #[test]
    fn json_includes_summary_and_full_description() {
        let e = SearchEntry {
            kind: Some("skill".to_string()),
            repo: "localhost:5000/acme/x".to_string(),
            summary: Some("short".to_string()),
            repository: None,
            description: Some("the full long description".to_string()),
            latest_tag: Some("latest".to_string()),
            version: Some("1.2.0".to_string()),
            deprecated: None,
            status: StatusBadge::Installed,
        };
        let mut buf = Vec::new();
        SearchReport::new(vec![e]).print_json(&mut buf).unwrap();
        let v: serde_json::Value = serde_json::from_slice(&buf).unwrap();
        assert_eq!(v[0]["summary"], "short");
        assert_eq!(v[0]["description"], "the full long description");
        // Both the concrete version and the representative tag round-trip.
        assert_eq!(v[0]["version"], "1.2.0");
        assert_eq!(v[0]["latest_tag"], "latest");
    }

    #[test]
    fn json_carries_repository_url_plain_table_does_not() {
        let mut e = entry("localhost:5000/acme/x", StatusBadge::Installed);
        e.repository = Some("https://github.com/acme/x".to_string());
        let mut buf = Vec::new();
        SearchReport::new(vec![e.clone()]).print_json(&mut buf).unwrap();
        let v: serde_json::Value = serde_json::from_slice(&buf).unwrap();
        assert_eq!(v[0]["repository"], "https://github.com/acme/x");
        // Absent ⇒ explicit null, key always present for stable consumers.
        let mut buf = Vec::new();
        SearchReport::new(vec![entry("localhost:5000/acme/y", StatusBadge::Installed)])
            .print_json(&mut buf)
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&buf).unwrap();
        assert!(v[0]["repository"].is_null());
        // The plain table stays five columns — no URL leaks into it.
        let mut buf = Vec::new();
        SearchReport::new(vec![e]).print_plain(&mut buf).unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(!out.contains("github.com"), "plain table unchanged");
    }

    #[test]
    fn plain_marks_deprecated_in_status_json_carries_message() {
        let mut e = entry("localhost:5000/acme/x", StatusBadge::Installed);
        e.summary = Some("a reviewer".to_string());
        e.deprecated = Some("use acme/x-2".to_string());
        // Plain: the Status cell gains a comma-suffixed `deprecated`; the blurb
        // cell stays unmarked (no `[deprecated]` prefix).
        let mut buf = Vec::new();
        SearchReport::new(vec![e.clone()]).print_plain(&mut buf).unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(
            out.contains("installed,deprecated"),
            "Status cell carries the deprecated suffix: {out}"
        );
        assert!(out.contains("a reviewer"), "blurb is still shown: {out}");
        assert!(!out.contains("[deprecated]"), "the blurb prefix is gone: {out}");
        // JSON: the deprecation message rides a dedicated field.
        let mut buf = Vec::new();
        SearchReport::new(vec![e]).print_json(&mut buf).unwrap();
        let v: serde_json::Value = serde_json::from_slice(&buf).unwrap();
        assert_eq!(v[0]["deprecated"], "use acme/x-2");
        // A non-deprecated row carries no suffix and a null field.
        let mut buf = Vec::new();
        SearchReport::new(vec![entry("localhost:5000/acme/y", StatusBadge::Installed)])
            .print_json(&mut buf)
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&buf).unwrap();
        assert!(v[0]["deprecated"].is_null(), "key present, null when not deprecated");
        let mut buf = Vec::new();
        SearchReport::new(vec![entry("localhost:5000/acme/y", StatusBadge::Installed)])
            .print_plain(&mut buf)
            .unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(!out.contains("deprecated"), "non-deprecated row is unmarked");
    }

    #[test]
    fn empty_results_serialize_as_empty_array() {
        let r = SearchReport::new(vec![]);
        let mut buf = Vec::new();
        r.print_json(&mut buf).unwrap();
        let v: serde_json::Value = serde_json::from_slice(&buf).unwrap();
        assert_eq!(v, serde_json::json!([]));
    }
}
