// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! The two kinds of artifact Grimoire manages: skills and rules.

use serde::{Deserialize, Serialize};

/// The OCI manifest annotation key the artifact kind is persisted under at
/// publish time and read back from on pull. Single source of truth for the
/// key string (writers in `annotations.rs`, readers in the catalog + `add`).
pub const KIND_ANNOTATION: &str = "com.grimoire.kind";

/// A Grimoire-managed artifact kind.
///
/// Closed internal enum: the binary is the only consumer, so matches stay
/// total — no `#[non_exhaustive]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ArtifactKind {
    /// An Agent Skill: a `SKILL.md` directory with YAML frontmatter.
    ///
    /// Also the `Default`: the lock layer's `LockedArtifact::kind` is
    /// `#[serde(skip)]` and re-stamped from the array it was read from, so
    /// the deserialization placeholder is never observed.
    #[default]
    Skill,
    /// A rule: a single `paths:`-scoped markdown file.
    Rule,
    /// A bundle: a curated set of skill/rule members, declared in
    /// `[bundles]` and expanded into its members at resolve time. A bundle
    /// is never materialized or written to the lock itself — only the
    /// members it expands to are.
    Bundle,
}

impl ArtifactKind {
    /// The `$GRIM_HOME`/install subdirectory for this kind.
    pub fn subdir(self) -> &'static str {
        match self {
            Self::Skill => "skills",
            Self::Rule => "rules",
            Self::Bundle => "bundles",
        }
    }

    /// Parse the lowercase annotation/string form (`skill`/`rule`/`bundle`)
    /// into a kind. `None` for any other string. The single source of truth
    /// for the string→enum mapping (`add`, `build`, catalog read).
    pub fn from_annotation(s: &str) -> Option<Self> {
        match s {
            "skill" => Some(Self::Skill),
            "rule" => Some(Self::Rule),
            "bundle" => Some(Self::Bundle),
            _ => None,
        }
    }

    /// Whether the artifact materializes as a directory tree (skill) rather
    /// than a single file (rule). Bundles never materialize.
    pub fn is_dir_artifact(self) -> bool {
        match self {
            Self::Skill => true,
            Self::Rule | Self::Bundle => false,
        }
    }
}

impl std::fmt::Display for ArtifactKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Skill => "skill",
            Self::Rule => "rule",
            Self::Bundle => "bundle",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subdir_and_dir_artifact() {
        assert_eq!(ArtifactKind::Skill.subdir(), "skills");
        assert_eq!(ArtifactKind::Rule.subdir(), "rules");
        assert_eq!(ArtifactKind::Bundle.subdir(), "bundles");
        assert!(ArtifactKind::Skill.is_dir_artifact());
        assert!(!ArtifactKind::Rule.is_dir_artifact());
        assert!(!ArtifactKind::Bundle.is_dir_artifact());
    }

    #[test]
    fn from_annotation_round_trips_and_rejects_unknown() {
        assert_eq!(ArtifactKind::from_annotation("skill"), Some(ArtifactKind::Skill));
        assert_eq!(ArtifactKind::from_annotation("rule"), Some(ArtifactKind::Rule));
        assert_eq!(ArtifactKind::from_annotation("bundle"), Some(ArtifactKind::Bundle));
        assert_eq!(ArtifactKind::from_annotation("Skill"), None);
        assert_eq!(ArtifactKind::from_annotation("widget"), None);
        // Display ⇄ from_annotation round-trip for every kind.
        for k in [ArtifactKind::Skill, ArtifactKind::Rule, ArtifactKind::Bundle] {
            assert_eq!(ArtifactKind::from_annotation(&k.to_string()), Some(k));
        }
    }

    #[test]
    fn display_and_serde_are_lowercase_and_agree() {
        assert_eq!(ArtifactKind::Skill.to_string(), "skill");
        assert_eq!(ArtifactKind::Rule.to_string(), "rule");
        assert_eq!(ArtifactKind::Bundle.to_string(), "bundle");
        assert_eq!(serde_json::to_string(&ArtifactKind::Skill).unwrap(), "\"skill\"");
        assert_eq!(
            serde_json::from_str::<ArtifactKind>("\"rule\"").unwrap(),
            ArtifactKind::Rule
        );
        assert_eq!(
            serde_json::from_str::<ArtifactKind>("\"bundle\"").unwrap(),
            ArtifactKind::Bundle
        );
    }
}
