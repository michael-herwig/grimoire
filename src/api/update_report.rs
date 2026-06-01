// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! `grim update` output.
//!
//! Plain format: 5-column table (Kind | Name | Old | New | Action).
//!
//! JSON format: an array of `{kind, name, old, new, action}` objects (the
//! report wraps a `Vec`, serialized to the bare array — no wrapper
//! object, per subsystem-cli-api.md). `old` is `null` for an artifact
//! that had no previous lock entry.

use std::io::{self, Write};

use serde::{Serialize, Serializer};

use crate::cli::printer::{Printable, print_table};
use crate::oci::{ArtifactKind, Digest};

use super::artifact_status::UpdateAction;

/// One updated artifact row.
#[derive(Debug, Serialize)]
pub struct UpdateEntry {
    #[serde(serialize_with = "serialize_kind")]
    pub kind: ArtifactKind,
    pub name: String,
    /// Previous digest, if the artifact was previously locked.
    #[serde(serialize_with = "serialize_opt_digest")]
    pub old: Option<Digest>,
    /// New digest, or `null` for a pruned/kept artifact that left the lock.
    #[serde(serialize_with = "serialize_opt_digest")]
    pub new: Option<Digest>,
    pub action: UpdateAction,
}

fn serialize_kind<S: Serializer>(kind: &ArtifactKind, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(&kind.to_string())
}

fn serialize_opt_digest<S: Serializer>(digest: &Option<Digest>, s: S) -> Result<S::Ok, S::Error> {
    match digest {
        Some(d) => s.serialize_some(&d.to_string()),
        None => s.serialize_none(),
    }
}

/// The result of an update pass: one row per re-resolved/carried artifact.
#[derive(Debug)]
pub struct UpdateReport {
    entries: Vec<UpdateEntry>,
}

impl UpdateReport {
    /// Build from operation results.
    pub fn new(entries: Vec<UpdateEntry>) -> Self {
        Self { entries }
    }
}

impl Serialize for UpdateReport {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.entries.serialize(serializer)
    }
}

impl Printable for UpdateReport {
    fn print_plain(&self, w: &mut impl Write) -> io::Result<()> {
        let rows: Vec<Vec<String>> = self
            .entries
            .iter()
            .map(|e| {
                vec![
                    e.kind.to_string(),
                    e.name.clone(),
                    e.old
                        .as_ref()
                        .map(Digest::to_short_string)
                        .unwrap_or_else(|| "-".to_string()),
                    e.new
                        .as_ref()
                        .map(Digest::to_short_string)
                        .unwrap_or_else(|| "-".to_string()),
                    e.action.to_string(),
                ]
            })
            .collect();
        print_table(w, &["Kind", "Name", "Old", "New", "Action"], &rows)
    }

    fn print_json(&self, w: &mut impl Write) -> io::Result<()> {
        let json = serde_json::to_string_pretty(self).map_err(io::Error::other)?;
        writeln!(w, "{json}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oci::Algorithm;

    #[test]
    fn plain_single_table_with_old_dash_when_absent() {
        let r = UpdateReport::new(vec![UpdateEntry {
            kind: ArtifactKind::Skill,
            name: "code-review".to_string(),
            old: None,
            new: Some(Algorithm::Sha256.hash(b"new")),
            action: UpdateAction::Updated,
        }]);
        let mut buf = Vec::new();
        r.print_plain(&mut buf).unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(out.lines().next().unwrap().starts_with("Kind"));
        assert!(out.contains("code-review"));
        assert!(out.contains("updated"));
        assert!(out.contains(" - "));
    }

    #[test]
    fn json_old_is_null_when_absent_and_string_when_present() {
        let old = Algorithm::Sha256.hash(b"old");
        let r = UpdateReport::new(vec![
            UpdateEntry {
                kind: ArtifactKind::Rule,
                name: "a".to_string(),
                old: None,
                new: Some(Algorithm::Sha256.hash(b"x")),
                action: UpdateAction::Updated,
            },
            UpdateEntry {
                kind: ArtifactKind::Rule,
                name: "b".to_string(),
                old: Some(old.clone()),
                new: Some(old),
                action: UpdateAction::Unchanged,
            },
        ]);
        let mut buf = Vec::new();
        r.print_json(&mut buf).unwrap();
        let v: serde_json::Value = serde_json::from_slice(&buf).unwrap();
        assert!(v.is_array());
        assert!(v[0]["old"].is_null());
        assert!(v[1]["old"].as_str().unwrap().starts_with("sha256:"));
        assert_eq!(v[1]["action"], "unchanged");
    }
}
