// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! The kinds of artifact Grimoire manages: skills, rules, agents, and
//! bundles.

use serde::{Deserialize, Serialize};

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
    /// An agent: a single markdown file whose required frontmatter
    /// (`name`, `description`) plus optional common fields (`model`,
    /// `tools`) define an AI agent; the body is the system prompt.
    /// Projected per client at install time.
    Agent,
    /// A bundle: a curated set of skill/rule/agent members, declared in
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
            Self::Agent => "agents",
            Self::Bundle => "bundles",
        }
    }

    /// Parse the lowercase kind string (`skill`/`rule`/`agent`/`bundle`)
    /// into a kind.
    /// `None` for any other string. Used to interpret the `--kind` CLI flag;
    /// the on-the-wire discriminator is the OCI `artifactType` (see
    /// [`Self::artifact_type`]), not this string.
    pub fn from_kind_str(s: &str) -> Option<Self> {
        match s {
            "skill" => Some(Self::Skill),
            "rule" => Some(Self::Rule),
            "agent" => Some(Self::Agent),
            "bundle" => Some(Self::Bundle),
            _ => None,
        }
    }

    /// The OCI `artifactType` media type stamped on a published manifest and
    /// read back to infer the kind. The authoritative wire discriminator and
    /// the single source of truth for the per-kind type string.
    pub fn artifact_type(self) -> &'static str {
        match self {
            Self::Skill => "application/vnd.grimoire.skill.v1",
            Self::Rule => "application/vnd.grimoire.rule.v1",
            Self::Agent => "application/vnd.grimoire.agent.v1",
            Self::Bundle => "application/vnd.grimoire.bundle.v1",
        }
    }

    /// The OCI config-descriptor media type stamped on a published manifest;
    /// the pre-1.1 fallback discriminator read when `artifactType` is absent.
    pub fn config_media_type(self) -> &'static str {
        match self {
            Self::Skill => "application/vnd.grimoire.skill.config.v1+json",
            Self::Rule => "application/vnd.grimoire.rule.config.v1+json",
            Self::Agent => "application/vnd.grimoire.agent.config.v1+json",
            Self::Bundle => "application/vnd.grimoire.bundle.config.v1+json",
        }
    }

    /// Parse an OCI `artifactType` media type back into a kind. `None` for any
    /// non-Grimoire type.
    pub fn from_artifact_type(s: &str) -> Option<Self> {
        [Self::Skill, Self::Rule, Self::Agent, Self::Bundle]
            .into_iter()
            .find(|k| k.artifact_type() == s)
    }

    /// Parse an OCI config media type back into a kind (the fallback read
    /// path). `None` for the generic OCI image config or any non-Grimoire type.
    pub fn from_config_media_type(s: &str) -> Option<Self> {
        [Self::Skill, Self::Rule, Self::Agent, Self::Bundle]
            .into_iter()
            .find(|k| k.config_media_type() == s)
    }

    /// Whether the artifact materializes as a directory tree (skill) rather
    /// than a single file (rule, agent). Bundles never materialize.
    pub fn is_dir_artifact(self) -> bool {
        match self {
            Self::Skill => true,
            Self::Rule | Self::Agent | Self::Bundle => false,
        }
    }
}

impl std::fmt::Display for ArtifactKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Skill => "skill",
            Self::Rule => "rule",
            Self::Agent => "agent",
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
        assert_eq!(ArtifactKind::Agent.subdir(), "agents");
        assert_eq!(ArtifactKind::Bundle.subdir(), "bundles");
        assert!(ArtifactKind::Skill.is_dir_artifact());
        assert!(!ArtifactKind::Rule.is_dir_artifact());
        assert!(!ArtifactKind::Agent.is_dir_artifact());
        assert!(!ArtifactKind::Bundle.is_dir_artifact());
    }

    #[test]
    fn from_kind_str_round_trips_and_rejects_unknown() {
        assert_eq!(ArtifactKind::from_kind_str("skill"), Some(ArtifactKind::Skill));
        assert_eq!(ArtifactKind::from_kind_str("rule"), Some(ArtifactKind::Rule));
        assert_eq!(ArtifactKind::from_kind_str("agent"), Some(ArtifactKind::Agent));
        assert_eq!(ArtifactKind::from_kind_str("bundle"), Some(ArtifactKind::Bundle));
        assert_eq!(ArtifactKind::from_kind_str("Skill"), None);
        assert_eq!(ArtifactKind::from_kind_str("widget"), None);
        // Display ⇄ from_kind_str round-trip for every kind.
        for k in [
            ArtifactKind::Skill,
            ArtifactKind::Rule,
            ArtifactKind::Agent,
            ArtifactKind::Bundle,
        ] {
            assert_eq!(ArtifactKind::from_kind_str(&k.to_string()), Some(k));
        }
    }

    #[test]
    fn artifact_type_and_config_media_type_round_trip() {
        for k in [
            ArtifactKind::Skill,
            ArtifactKind::Rule,
            ArtifactKind::Agent,
            ArtifactKind::Bundle,
        ] {
            assert_eq!(ArtifactKind::from_artifact_type(k.artifact_type()), Some(k));
            assert_eq!(ArtifactKind::from_config_media_type(k.config_media_type()), Some(k));
        }
        // Exact wire strings (the published contract).
        assert_eq!(ArtifactKind::Skill.artifact_type(), "application/vnd.grimoire.skill.v1");
        assert_eq!(
            ArtifactKind::Skill.config_media_type(),
            "application/vnd.grimoire.skill.config.v1+json"
        );
        assert_eq!(ArtifactKind::Agent.artifact_type(), "application/vnd.grimoire.agent.v1");
        assert_eq!(
            ArtifactKind::Agent.config_media_type(),
            "application/vnd.grimoire.agent.config.v1+json"
        );
        // The generic OCI image config and foreign types are not a kind.
        assert_eq!(
            ArtifactKind::from_config_media_type("application/vnd.oci.image.config.v1+json"),
            None
        );
        assert_eq!(
            ArtifactKind::from_artifact_type("application/vnd.cncf.helm.config.v1+json"),
            None
        );
    }

    #[test]
    fn display_and_serde_are_lowercase_and_agree() {
        assert_eq!(ArtifactKind::Skill.to_string(), "skill");
        assert_eq!(ArtifactKind::Rule.to_string(), "rule");
        assert_eq!(ArtifactKind::Agent.to_string(), "agent");
        assert_eq!(ArtifactKind::Bundle.to_string(), "bundle");
        assert_eq!(serde_json::to_string(&ArtifactKind::Skill).unwrap(), "\"skill\"");
        assert_eq!(
            serde_json::from_str::<ArtifactKind>("\"agent\"").unwrap(),
            ArtifactKind::Agent
        );
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
