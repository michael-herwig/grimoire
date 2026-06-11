// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! The required YAML frontmatter of an agent definition.
//!
//! An agent is a single Markdown file whose leading `---`-delimited YAML
//! block carries the **canonical common fields** shared by every supported
//! client (Claude Code subagents, OpenCode agents, Copilot CLI custom
//! agents): the required `name` and `description`, the optional `model`
//! and `tools`, and an arbitrary string-valued `metadata` map. The body is
//! the agent's system prompt.
//!
//! Vendor-unique capabilities (Claude's `permissionMode`, OpenCode's
//! `temperature`, …) are NOT top-level fields: they are authored as
//! namespaced `metadata` keys (`<vendor>.<field>: "…"`) and projected into
//! each client's native frontmatter at install time by
//! [`crate::install::render`]. A `<vendor>.<field>` key whose native name
//! collides with a projected common field (e.g. `claude.model` vs `model`)
//! **overrides** the common value for that vendor.
//!
//! Unlike a rule, the frontmatter is required — every client needs at
//! least a description to route to the agent. Forward-compatible like the
//! other kinds: unknown keys are preserved via [`AgentFrontmatter::extra`].

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::skill_description::SkillDescription;
use super::skill_error::{SkillError, SkillErrorKind};
use super::skill_frontmatter::SkillFrontmatter;
use super::skill_name::SkillName;

/// The parsed frontmatter of an agent file.
///
/// Round-trips through serde: known keys are modelled, unknown keys are
/// captured in [`Self::extra`] and re-emitted on serialize.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentFrontmatter {
    /// Required: the agent name (must equal the file stem).
    pub name: SkillName,
    /// Required: when a client should delegate to this agent.
    pub description: SkillDescription,

    /// Optional model selector, passed through to each client verbatim
    /// (no alias translation). Override per vendor via `<vendor>.model`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,

    /// Optional comma-separated tool allowlist, projected into each
    /// client's native representation. Override via `<vendor>.tools`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<String>,

    /// Arbitrary key/value metadata (e.g. `keywords`, `summary`), plus
    /// tool-namespaced capability keys (`<vendor>.<field>`) projected into
    /// native client frontmatter at install time.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, String>,

    /// Forward-compat: any unknown frontmatter key, preserved verbatim.
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_yaml::Value>,
}

/// An agent document split into its frontmatter and Markdown body (the
/// system prompt).
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedAgent {
    /// The parsed frontmatter.
    pub frontmatter: AgentFrontmatter,
    /// The Markdown body — the agent's system prompt.
    pub body: String,
}

impl AgentFrontmatter {
    /// Parse an agent document into `(frontmatter, body)`.
    ///
    /// Unlike a rule, the `---`-delimited frontmatter block is required:
    /// an agent without `name`/`description` is meaningless to every
    /// client.
    ///
    /// # Errors
    ///
    /// [`SkillErrorKind::MissingFrontmatter`] when the document has no
    /// `---`-delimited block; [`SkillErrorKind::FrontmatterParse`] when
    /// the YAML is malformed or the required fields are missing/invalid.
    pub fn parse_doc(doc: &str, path: &std::path::Path) -> Result<ParsedAgent, SkillError> {
        let (fm_yaml, body) = SkillFrontmatter::split(doc, path)?;
        let frontmatter: AgentFrontmatter =
            serde_yaml::from_str(&fm_yaml).map_err(|e| SkillError::new(path, SkillErrorKind::FrontmatterParse(e)))?;
        Ok(ParsedAgent { frontmatter, body })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn p() -> &'static Path {
        Path::new("code-reviewer.md")
    }

    #[test]
    fn parses_minimal_required_fields() {
        let doc = "---\nname: code-reviewer\ndescription: Reviews diffs.\n---\nYou are a code reviewer.\n";
        let a = AgentFrontmatter::parse_doc(doc, p()).expect("parse");
        assert_eq!(a.frontmatter.name.as_str(), "code-reviewer");
        assert_eq!(a.frontmatter.description.as_str(), "Reviews diffs.");
        assert!(a.frontmatter.model.is_none());
        assert!(a.frontmatter.tools.is_none());
        assert_eq!(a.body, "You are a code reviewer.\n");
    }

    #[test]
    fn parses_common_fields_and_namespaced_metadata() {
        let doc = r#"---
name: code-reviewer
description: Reviews diffs.
model: sonnet
tools: Read,Grep,Bash
metadata:
  summary: terse blurb
  keywords: review,quality
  claude.permission-mode: plan
  opencode.temperature: "0.2"
---
body
"#;
        let a = AgentFrontmatter::parse_doc(doc, p()).expect("parse");
        assert_eq!(a.frontmatter.model.as_deref(), Some("sonnet"));
        assert_eq!(a.frontmatter.tools.as_deref(), Some("Read,Grep,Bash"));
        assert_eq!(
            a.frontmatter.metadata.get("summary").map(String::as_str),
            Some("terse blurb")
        );
        assert_eq!(
            a.frontmatter.metadata.get("claude.permission-mode").map(String::as_str),
            Some("plan")
        );
        assert_eq!(
            a.frontmatter.metadata.get("opencode.temperature").map(String::as_str),
            Some("0.2")
        );
        assert!(a.frontmatter.extra.is_empty(), "all keys were known");
    }

    #[test]
    fn missing_required_field_is_parse_error() {
        let doc = "---\nname: a\n---\nbody\n";
        let err = AgentFrontmatter::parse_doc(doc, p()).expect_err("missing description");
        assert!(matches!(err.kind, SkillErrorKind::FrontmatterParse(_)));
    }

    #[test]
    fn no_fence_is_missing_frontmatter() {
        let doc = "You are an agent without frontmatter.\n";
        let err = AgentFrontmatter::parse_doc(doc, p()).expect_err("no frontmatter");
        assert!(matches!(err.kind, SkillErrorKind::MissingFrontmatter));
    }

    #[test]
    fn invalid_name_is_parse_error() {
        let doc = "---\nname: Bad_Name\ndescription: d\n---\nbody\n";
        let err = AgentFrontmatter::parse_doc(doc, p()).expect_err("bad name");
        assert!(matches!(err.kind, SkillErrorKind::FrontmatterParse(_)));
    }

    #[test]
    fn unknown_keys_preserved_in_extra_and_round_trip() {
        let doc = "---\nname: a\ndescription: d\nfuture_field: hello\n---\nbody\n";
        let a = AgentFrontmatter::parse_doc(doc, p()).expect("forward-compat parse");
        assert!(a.frontmatter.extra.contains_key("future_field"));
        let yaml = serde_yaml::to_string(&a.frontmatter).unwrap();
        let again: AgentFrontmatter = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(again, a.frontmatter);
    }

    #[test]
    fn metadata_round_trips() {
        let doc = "---\nname: a\ndescription: d\nmodel: opus\nmetadata:\n  claude.memory: project\n---\nbody\n";
        let a = AgentFrontmatter::parse_doc(doc, p()).expect("parse");
        let yaml = serde_yaml::to_string(&a.frontmatter).unwrap();
        let again: AgentFrontmatter = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(again.metadata, a.frontmatter.metadata);
        assert_eq!(again.model.as_deref(), Some("opus"));
    }
}
