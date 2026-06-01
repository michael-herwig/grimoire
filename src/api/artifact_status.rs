// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! Typed status / action enums shared by the command reports.
//!
//! Every command reports operation results through one of these closed
//! enums (never a raw `String`), each with a lowercase `Display` and a
//! lowercase `Serialize` so the plain table and the JSON array agree.

use serde::Serialize;

/// The state of a declared artifact relative to lock + install state.
///
/// Closed internal enum — the binary is the only consumer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ArtifactStatus {
    /// Locked, installed, content intact, pin matches the lock.
    Installed,
    /// The lock's declaration hash no longer matches the config — a
    /// `grim lock` is required before install reflects the config.
    Stale,
    /// Installed but the on-disk content drifted from what was recorded.
    Modified,
    /// Declared (and locked) but not installed.
    Missing,
    /// Installed, but the installed digest differs from the lock digest.
    Outdated,
}

impl std::fmt::Display for ArtifactStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Installed => "installed",
            Self::Stale => "stale",
            Self::Modified => "modified",
            Self::Missing => "missing",
            Self::Outdated => "outdated",
        })
    }
}

/// What `grim lock` did to one entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LockAction {
    /// Newly pinned or re-pinned to a different digest.
    Locked,
    /// Already pinned to the same digest — carried forward unchanged.
    Unchanged,
}

impl std::fmt::Display for LockAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Locked => "locked",
            Self::Unchanged => "unchanged",
        })
    }
}

/// What `grim install` did to one entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum InstallStatus {
    /// Freshly installed.
    Installed,
    /// Reinstalled over a different prior pin / content.
    Updated,
    /// Already installed, pin and content intact — no-op.
    Unchanged,
    /// Refused: locally modified and `--force` not given.
    Refused,
    /// Skipped for a benign reason.
    Skipped,
}

impl std::fmt::Display for InstallStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Installed => "installed",
            Self::Updated => "updated",
            Self::Unchanged => "unchanged",
            Self::Refused => "refused",
            Self::Skipped => "skipped",
        })
    }
}

/// What `grim update` did to one entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum UpdateAction {
    /// The pin changed (and the artifact was re-materialized).
    Updated,
    /// The pin was unchanged.
    Unchanged,
    /// The artifact left the lock (e.g. a bundle dropped it) and its
    /// materialized files were pruned.
    Removed,
    /// The artifact left the lock but was locally modified, so it was
    /// preserved (re-run with `--force` to prune it).
    KeptModified,
}

impl std::fmt::Display for UpdateAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Updated => "updated",
            Self::Unchanged => "unchanged",
            Self::Removed => "removed",
            Self::KeptModified => "kept-modified",
        })
    }
}

/// What `grim init` did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum InitStatus {
    /// A fresh config file was created.
    Created,
}

impl std::fmt::Display for InitStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Created => "created",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_and_serialize_are_lowercase_and_agree() {
        assert_eq!(ArtifactStatus::Outdated.to_string(), "outdated");
        assert_eq!(
            serde_json::to_string(&ArtifactStatus::Modified).unwrap(),
            "\"modified\""
        );
        assert_eq!(LockAction::Unchanged.to_string(), "unchanged");
        assert_eq!(serde_json::to_string(&InstallStatus::Refused).unwrap(), "\"refused\"");
        assert_eq!(UpdateAction::Updated.to_string(), "updated");
        assert_eq!(UpdateAction::KeptModified.to_string(), "kept-modified");
        assert_eq!(
            serde_json::to_string(&UpdateAction::KeptModified).unwrap(),
            "\"kept-modified\""
        );
        assert_eq!(serde_json::to_string(&UpdateAction::Removed).unwrap(), "\"removed\"");
        assert_eq!(serde_json::to_string(&InitStatus::Created).unwrap(), "\"created\"");
    }
}
