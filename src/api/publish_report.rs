// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! `grim publish` output.
//!
//! Plain format: 5-column table (Kind | Ref | Digest | Tags | Status).
//!
//! JSON format: an array of `{kind, reference, digest, tags, status}`
//! objects (the report wraps a `Vec`, serialized to the bare array — no
//! wrapper object, per subsystem-cli-api.md).

use std::io::{self, Write};

use serde::{Serialize, Serializer};

use crate::cli::printer::{Printable, print_table};
use crate::oci::ArtifactKind;

/// The outcome of publishing one manifest entry.
///
/// Closed internal enum — the binary is the only consumer. `DryRun`
/// renders as `dry-run` in both plain and JSON output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublishStatus {
    /// The artifact was pushed to the registry.
    Pushed,
    /// The exact-version tag already existed; the entry was skipped
    /// (default skip-existing behavior).
    Skipped,
    /// `--dry-run` was active; nothing was pushed.
    DryRun,
    /// The push failed; the batch was stopped.
    Failed,
}

impl std::fmt::Display for PublishStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Pushed => "pushed",
            Self::Skipped => "skipped",
            Self::DryRun => "dry-run",
            Self::Failed => "failed",
        })
    }
}

impl Serialize for PublishStatus {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

/// One row in the publish report: the outcome of a single manifest entry.
#[derive(Debug, Serialize)]
pub struct PublishEntry {
    /// The OCI reference that was (or would be) published
    /// (`registry/repo:version`).
    #[serde(rename = "ref")]
    pub reference: String,
    /// The artifact kind.
    #[serde(serialize_with = "serialize_kind")]
    pub kind: ArtifactKind,
    /// The manifest digest of the pushed artifact, if available.
    pub digest: Option<String>,
    /// The cascade tag set pointed at the manifest.
    pub tags: Vec<String>,
    /// The outcome of this entry.
    pub status: PublishStatus,
}

fn serialize_kind<S: Serializer>(kind: &ArtifactKind, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(&kind.to_string())
}

/// The result of a `grim publish` run: one row per manifest entry
/// processed (including the failed entry on fail-fast; entries not
/// reached are absent from the report).
#[derive(Debug)]
pub struct PublishReport {
    /// Per-entry outcomes, in publish order.
    entries: Vec<PublishEntry>,
}

impl PublishReport {
    /// Build from operation results.
    pub fn new(entries: Vec<PublishEntry>) -> Self {
        Self { entries }
    }

    /// Per-entry outcomes, in publish order (read-only — the report is
    /// built once from operation results and never mutated).
    pub fn entries(&self) -> &[PublishEntry] {
        &self.entries
    }
}

impl Serialize for PublishReport {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.entries.serialize(serializer)
    }
}

/// Truncate a digest for plain-text table display.
///
/// Renders as `sha256:` + first 12 hex characters (e.g.
/// `sha256:a1b2c3d4e5f6`). The JSON output retains the full digest;
/// truncation is presentation-only.
fn truncate_digest(digest: &str) -> String {
    if let Some(hex) = digest.strip_prefix("sha256:") {
        let short: String = hex.chars().take(12).collect();
        format!("sha256:{short}")
    } else {
        // Non-sha256 digest (unlikely): keep as-is.
        digest.to_string()
    }
}

impl Printable for PublishReport {
    fn print_plain(&self, w: &mut impl Write) -> io::Result<()> {
        let rows: Vec<Vec<String>> = self
            .entries
            .iter()
            .map(|e| {
                vec![
                    e.kind.to_string(),
                    e.reference.clone(),
                    e.digest
                        .as_deref()
                        .map(truncate_digest)
                        .unwrap_or_else(|| "-".to_string()),
                    e.tags.join(","),
                    e.status.to_string(),
                ]
            })
            .collect();
        print_table(w, &["Kind", "Ref", "Digest", "Tags", "Status"], &rows)
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
    fn display_and_serialize_agree() {
        assert_eq!(PublishStatus::Pushed.to_string(), "pushed");
        assert_eq!(PublishStatus::Skipped.to_string(), "skipped");
        assert_eq!(PublishStatus::DryRun.to_string(), "dry-run");
        assert_eq!(PublishStatus::Failed.to_string(), "failed");
        assert_eq!(serde_json::to_string(&PublishStatus::DryRun).unwrap(), "\"dry-run\"");
    }

    #[test]
    fn plain_single_table() {
        let r = PublishReport::new(vec![PublishEntry {
            reference: "registry.example/acme/code-review:1.0.0".to_string(),
            kind: ArtifactKind::Skill,
            // Full 64-hex digest; plain output should truncate to first 12 chars.
            digest: Some("sha256:a1b2c3d4e5f6aabbccddeeff001122334455667788990011223344556677889900".to_string()),
            tags: vec![
                "1.0.0".to_string(),
                "1.0".to_string(),
                "1".to_string(),
                "latest".to_string(),
            ],
            status: PublishStatus::Pushed,
        }]);
        let mut buf = Vec::new();
        r.print_plain(&mut buf).unwrap();
        let out = String::from_utf8(buf).unwrap();
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].starts_with("Kind"));
        assert!(lines[1].contains("pushed"));
        assert!(lines[1].contains("code-review"));
        // Plain output must truncate to sha256: + 12 hex chars.
        assert!(
            lines[1].contains("sha256:a1b2c3d4e5f6"),
            "plain digest must be truncated, got: {}",
            lines[1]
        );
        assert!(
            !lines[1].contains("a1b2c3d4e5f6aabbccddeeff"),
            "plain digest must not contain full hex, got: {}",
            lines[1]
        );
    }

    #[test]
    fn truncate_digest_sha256() {
        assert_eq!(truncate_digest("sha256:a1b2c3d4e5f6aabbccdd"), "sha256:a1b2c3d4e5f6");
    }

    #[test]
    fn truncate_digest_non_sha256_passthrough() {
        assert_eq!(truncate_digest("md5:abc"), "md5:abc");
    }

    #[test]
    fn json_is_bare_array() {
        let r = PublishReport::new(vec![PublishEntry {
            reference: "registry.example/acme/my-rule:0.1.0".to_string(),
            kind: ArtifactKind::Rule,
            digest: None,
            tags: vec![],
            status: PublishStatus::DryRun,
        }]);
        let mut buf = Vec::new();
        r.print_json(&mut buf).unwrap();
        let v: serde_json::Value = serde_json::from_slice(&buf).unwrap();
        assert!(v.is_array());
        assert_eq!(v[0]["ref"], "registry.example/acme/my-rule:0.1.0");
        assert_eq!(v[0]["status"], "dry-run");
        assert!(v[0]["digest"].is_null());
    }
}
