// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! `grim uninstall` output.
//!
//! Plain format: a single-row 3-column table (Kind | Name | Status).
//!
//! JSON format: a single object `{kind, name, status}` (not an array —
//! `uninstall` touches exactly one declared entry). Unlike `remove`,
//! `uninstall` also deletes the materialized client files and drops the
//! install-state record (full uninstall).

use std::io::{self, Write};

use serde::Serialize;

use crate::cli::printer::{Printable, print_table};
use crate::oci::ArtifactKind;

/// What `grim uninstall` did to the named entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum UninstallStatus {
    /// Files deleted, install-state record dropped, config + lock entry
    /// undeclared.
    Uninstalled,
    /// A declared bundle still provides this artifact and it was not directly
    /// declared, so its files are kept and nothing was undeclared — remove the
    /// bundle to remove it. The uninstall was intentionally a no-op.
    KeptByBundle,
    /// Nothing was installed or declared for this name (no-op).
    NotInstalled,
}

impl std::fmt::Display for UninstallStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Uninstalled => "uninstalled",
            Self::KeptByBundle => "kept-by-bundle",
            Self::NotInstalled => "not-installed",
        })
    }
}

/// The result of uninstalling one skill/rule.
#[derive(Debug, Serialize)]
pub struct UninstallReport {
    #[serde(serialize_with = "serialize_kind")]
    pub kind: ArtifactKind,
    pub name: String,
    pub status: UninstallStatus,
}

fn serialize_kind<S: serde::Serializer>(kind: &ArtifactKind, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(&kind.to_string())
}

impl UninstallReport {
    /// Build from operation results.
    pub fn new(kind: ArtifactKind, name: String, status: UninstallStatus) -> Self {
        Self { kind, name, status }
    }
}

impl Printable for UninstallReport {
    fn print_plain(&self, w: &mut impl Write) -> io::Result<()> {
        print_table(
            w,
            &["Kind", "Name", "Status"],
            &[vec![self.kind.to_string(), self.name.clone(), self.status.to_string()]],
        )
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
        let r = UninstallReport::new(
            ArtifactKind::Skill,
            "code-review".to_string(),
            UninstallStatus::Uninstalled,
        );
        let mut buf = Vec::new();
        r.print_plain(&mut buf).unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(out.lines().next().unwrap().starts_with("Kind"));
        assert!(out.contains("uninstalled"));
    }

    #[test]
    fn json_object_not_installed() {
        let r = UninstallReport::new(ArtifactKind::Rule, "gone".to_string(), UninstallStatus::NotInstalled);
        let mut buf = Vec::new();
        r.print_json(&mut buf).unwrap();
        let v: serde_json::Value = serde_json::from_slice(&buf).unwrap();
        assert_eq!(v["kind"], "rule");
        assert_eq!(v["status"], "not-installed");
    }
}
