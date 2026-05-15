// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! The two kinds of artifact Grimoire manages: skills and rules.

use serde::{Deserialize, Serialize};

/// A Grimoire-managed artifact kind.
///
/// Closed internal enum: the binary is the only consumer, so matches stay
/// total — no `#[non_exhaustive]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ArtifactKind {
    /// An Agent Skill: a `SKILL.md` directory with YAML frontmatter.
    Skill,
    /// A rule: a single `paths:`-scoped markdown file.
    Rule,
}

impl ArtifactKind {
    /// The `$GRIM_HOME`/install subdirectory for this kind.
    pub fn subdir(self) -> &'static str {
        match self {
            Self::Skill => "skills",
            Self::Rule => "rules",
        }
    }

    /// Whether the artifact materializes as a directory tree (skill) rather
    /// than a single file (rule).
    pub fn is_dir_artifact(self) -> bool {
        match self {
            Self::Skill => true,
            Self::Rule => false,
        }
    }
}

impl std::fmt::Display for ArtifactKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Skill => "skill",
            Self::Rule => "rule",
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
        assert!(ArtifactKind::Skill.is_dir_artifact());
        assert!(!ArtifactKind::Rule.is_dir_artifact());
    }

    #[test]
    fn display_and_serde_are_lowercase_and_agree() {
        assert_eq!(ArtifactKind::Skill.to_string(), "skill");
        assert_eq!(ArtifactKind::Rule.to_string(), "rule");
        assert_eq!(serde_json::to_string(&ArtifactKind::Skill).unwrap(), "\"skill\"");
        assert_eq!(
            serde_json::from_str::<ArtifactKind>("\"rule\"").unwrap(),
            ArtifactKind::Rule
        );
    }
}
