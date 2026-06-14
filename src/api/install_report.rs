// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! `grim install` output.
//!
//! Plain format: 4-column table (Kind | Name | Target | Status). The
//! Target cell is `—` when nothing was written (every selected client
//! declined the kind).
//!
//! JSON format: an array of `{kind, name, target, status}` objects (the
//! report wraps a `Vec`, serialized to the bare array — no wrapper
//! object, per subsystem-cli-api.md). `target` is `null` when no client
//! wrote a file.

use std::io::{self, Write};
use std::path::PathBuf;

use serde::{Serialize, Serializer};

use crate::cli::printer::{Printable, print_table};
use crate::oci::ArtifactKind;

use super::artifact_status::InstallStatus;

/// One installed artifact row.
#[derive(Debug, Serialize)]
pub struct InstallEntry {
    #[serde(serialize_with = "serialize_kind")]
    pub kind: ArtifactKind,
    pub name: String,
    /// The on-disk path written, or `None` when every selected client
    /// declined the kind (serialized as `null`, rendered as `—`).
    pub target: Option<PathBuf>,
    pub status: InstallStatus,
}

fn serialize_kind<S: Serializer>(kind: &ArtifactKind, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(&kind.to_string())
}

/// The result of an install pass: one row per locked artifact.
#[derive(Debug)]
pub struct InstallReport {
    entries: Vec<InstallEntry>,
}

impl InstallReport {
    /// Build from operation results.
    pub fn new(entries: Vec<InstallEntry>) -> Self {
        Self { entries }
    }
}

impl Serialize for InstallReport {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.entries.serialize(serializer)
    }
}

impl Printable for InstallReport {
    fn print_plain(&self, w: &mut impl Write) -> io::Result<()> {
        let rows: Vec<Vec<String>> = self
            .entries
            .iter()
            .map(|e| {
                vec![
                    e.kind.to_string(),
                    e.name.clone(),
                    e.target
                        .as_ref()
                        .map_or_else(|| "—".to_string(), |p| p.display().to_string()),
                    e.status.to_string(),
                ]
            })
            .collect();
        print_table(w, &["Kind", "Name", "Target", "Status"], &rows)
    }

    fn print_json(&self, w: &mut impl Write) -> io::Result<()> {
        let json = serde_json::to_string_pretty(self).map_err(io::Error::other)?;
        writeln!(w, "{json}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_single_table() {
        let r = InstallReport::new(vec![InstallEntry {
            kind: ArtifactKind::Skill,
            name: "code-review".to_string(),
            target: Some(PathBuf::from("/w/.claude/skills/code-review")),
            status: InstallStatus::Installed,
        }]);
        let mut buf = Vec::new();
        r.print_plain(&mut buf).unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(out.lines().next().unwrap().starts_with("Kind"));
        assert!(out.contains("code-review"));
        assert!(out.contains("installed"));
    }

    #[test]
    fn json_is_bare_array() {
        let r = InstallReport::new(vec![InstallEntry {
            kind: ArtifactKind::Rule,
            name: "rust-style".to_string(),
            target: Some(PathBuf::from("/w/.claude/rules/rust-style.md")),
            status: InstallStatus::Refused,
        }]);
        let mut buf = Vec::new();
        r.print_json(&mut buf).unwrap();
        let v: serde_json::Value = serde_json::from_slice(&buf).unwrap();
        assert!(v.is_array());
        assert_eq!(v[0]["kind"], "rule");
        assert_eq!(v[0]["status"], "refused");
    }

    #[test]
    fn none_target_renders_dash_and_null() {
        // A declined-only install (every selected client declines the kind)
        // has no on-disk path: plain shows `—`, JSON shows `null`.
        let r = InstallReport::new(vec![InstallEntry {
            kind: ArtifactKind::Rule,
            name: "rust-style".to_string(),
            target: None,
            status: InstallStatus::Skipped,
        }]);
        let mut plain = Vec::new();
        r.print_plain(&mut plain).unwrap();
        assert!(String::from_utf8(plain).unwrap().contains('—'));
        let mut json = Vec::new();
        r.print_json(&mut json).unwrap();
        let v: serde_json::Value = serde_json::from_slice(&json).unwrap();
        assert!(v[0]["target"].is_null());
    }
}
