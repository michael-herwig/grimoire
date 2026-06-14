// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! OpenAI Codex's vendor strategy: universal skills, TOML agents, no rules.
//!
//! Codex reads only the universal agentskills `SKILL.md` fields and
//! auto-discovers skills from the cross-vendor open standard directory
//! (`.agents/skills/<name>/` — project `<repo>/.agents/skills`, global
//! `$HOME/.agents/skills`, **independent of `$CODEX_HOME`**), so its skill
//! registry is empty and the render matches OpenCode's universal shape
//! (developers.openai.com/codex/skills).
//!
//! Codex subagents are auto-discovered TOML files at `.codex/agents/<name>.toml`
//! — the **first TOML-emitting vendor**. The native keys are `name`,
//! `description`, `developer_instructions` (= the grim agent body) and an
//! optional `model` (developers.openai.com/codex/subagents).
//!
//! Codex has **no native rule target**: AGENTS.md is always-on and
//! directory-granular with no path-glob / `applyTo` scoping anywhere, and
//! hooks cannot synthesize it (`PreToolUse` rejects `additionalContext`).
//! So [`CodexVendor::supports_kind`] declines [`ArtifactKind::Rule`] and the
//! installer warns + skips rather than writing an inert file.

use std::path::{Path, PathBuf};

use crate::config::scope::ConfigScope;
use crate::oci::ArtifactKind;
use crate::skill::agent_frontmatter::ParsedAgent;
use crate::skill::rule_frontmatter::ParsedRule;

use super::render::{self, RenderError, RenderedDoc};
use super::vendor::{FieldType, KnownField, Vendor, env_dir, home_dir, toml_provenance};

/// OpenAI Codex CLI.
pub struct CodexVendor;

/// `codex.*` agent fields → native Codex subagent TOML keys
/// (developers.openai.com/codex/config — model knobs). `model` shadows the
/// projected canonical common field — the per-vendor override escape hatch.
/// Object-valued fields are deliberately absent: they cannot be expressed
/// as a single string metadata value.
pub const CODEX_AGENT_FIELDS: &[KnownField] = &[
    KnownField {
        field: "model",
        native: "model",
        ty: FieldType::String,
    },
    KnownField {
        field: "reasoning-effort",
        native: "model_reasoning_effort",
        ty: FieldType::Enum(&["minimal", "low", "medium", "high", "xhigh"]),
    },
    KnownField {
        field: "sandbox-mode",
        native: "sandbox_mode",
        ty: FieldType::Enum(&["read-only", "workspace-write", "danger-full-access"]),
    },
];

/// A Codex subagent TOML table. Field order is the emitted key order
/// (`toml` serializes a struct in declaration order, deterministically):
/// the identity keys first, the long free-form body last.
#[derive(serde::Serialize)]
struct CodexAgent {
    name: String,
    description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    model_reasoning_effort: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    sandbox_mode: Option<String>,
    developer_instructions: String,
}

impl Vendor for CodexVendor {
    fn name(&self) -> &'static str {
        "codex"
    }

    fn root_dir(&self) -> &'static str {
        ".codex"
    }

    fn supports_kind(&self, kind: ArtifactKind) -> bool {
        // Codex has no path-scoped instruction mechanism (no globs/applyTo
        // anywhere; hooks cannot supply file-aware context). Rules are
        // skipped rather than materialized as inert files.
        !matches!(kind, ArtifactKind::Rule)
    }

    // Skill registry empty: Codex skills are agentskills-universal.

    fn agent_fields(&self) -> &'static [KnownField] {
        CODEX_AGENT_FIELDS
    }

    fn detect(&self, workspace: &Path, scope: ConfigScope) -> bool {
        match scope {
            // Project: a real Codex-user signal is `<ws>/.codex`. The shared
            // `.agents/skills` dir is a weak/cross-vendor marker (like
            // Copilot's bare `.github` caveat), so it does NOT count alone.
            ConfigScope::Project => workspace.join(".codex").exists(),
            // Global: the native `$CODEX_HOME|~/.codex` config root being
            // present marks Codex as a configured client on this machine.
            ConfigScope::Global => codex_root(env_dir("CODEX_HOME"), home_dir()).is_some_and(|p| p.exists()),
        }
    }

    fn skills_root(&self, workspace: &Path, scope: ConfigScope) -> PathBuf {
        match scope {
            // The cross-vendor open standard: project skills live at
            // `<repo>/.agents/skills`, NOT under `.codex`.
            ConfigScope::Project => workspace.join(".agents").join("skills"),
            // Global skills live at `$HOME/.agents/skills` — independent of
            // `$CODEX_HOME` (the standard is keyed on `$HOME`).
            ConfigScope::Global => {
                global_skills_root(home_dir()).unwrap_or_else(|| workspace.join(".agents").join("skills"))
            }
        }
    }

    fn rule_path(&self, workspace: &Path, _scope: ConfigScope, name: &str) -> PathBuf {
        // Dead path: [`Self::supports_kind`] declines `Rule`, so the
        // installer skips Codex before `path_for` ever calls this. Returns a
        // defensive in-workspace location so the trait stays total.
        workspace.join(".codex").join("rules").join(format!("{name}.md"))
    }

    fn agent_path(&self, workspace: &Path, scope: ConfigScope, name: &str) -> PathBuf {
        let root = match scope {
            ConfigScope::Project => workspace.join(".codex").join("agents"),
            // Codex discovers user-level subagents from `$CODEX_HOME|~/.codex`
            // + `agents/` (developers.openai.com/codex/subagents).
            ConfigScope::Global => codex_root(env_dir("CODEX_HOME"), home_dir())
                .map(|r| r.join("agents"))
                .unwrap_or_else(|| workspace.join(".codex").join("agents")),
        };
        root.join(format!("{name}.toml"))
    }

    fn skill_index(&self, doc: &str) -> Result<Option<RenderedDoc>, RenderError> {
        // Empty registries ⇒ universal-shape render (verbatim fast path for a
        // plain skill), identical to OpenCode/Copilot.
        render::render_skill_doc(doc, self)
    }

    fn rule_index(&self, _parsed: &ParsedRule, _pinned: &str) -> Result<Option<RenderedDoc>, RenderError> {
        // Never called: rules are skipped at the installer's `supports_kind`
        // gate. Defensive `None` (would install verbatim) keeps the trait total.
        Ok(None)
    }

    fn agent_index(&self, parsed: &ParsedAgent, pinned: &str) -> Result<Option<RenderedDoc>, RenderError> {
        // Codex agents are always a transform: the canonical Markdown agent
        // becomes a TOML subagent. The filename carries identity but Codex
        // also reads a `name` key; `description` and the body
        // (`developer_instructions`) are required; `model` and the optional
        // `codex.*` knobs are emitted when present. `tools` has no Codex
        // equivalent — dropped with a warning.
        let projection = render::project_agent(&parsed.frontmatter, self)?;
        let mut warnings = projection.warnings;

        if projection.cleaned.tools.is_some() {
            warnings.push(format!(
                "agent field 'tools' has no Codex equivalent; dropped for agent '{}'",
                projection.cleaned.name
            ));
        }

        // Start from the projected common `model`; a lifted `codex.*` key
        // (registry order) overrides it silently — the documented escape
        // hatch. All `CODEX_AGENT_FIELDS` are String/Enum ⇒ `Value::String`.
        let mut model = projection.cleaned.model.clone();
        let mut model_reasoning_effort = None;
        let mut sandbox_mode = None;
        for (native, value) in &projection.lifted {
            if let serde_yaml::Value::String(s) = value {
                match *native {
                    "model" => model = Some(s.clone()),
                    "model_reasoning_effort" => model_reasoning_effort = Some(s.clone()),
                    "sandbox_mode" => sandbox_mode = Some(s.clone()),
                    _ => {}
                }
            }
        }

        let agent = CodexAgent {
            name: projection.cleaned.name.to_string(),
            description: projection.cleaned.description.to_string(),
            model,
            model_reasoning_effort,
            sandbox_mode,
            developer_instructions: parsed.body.clone(),
        };

        // A flat table of string scalars always serializes to TOML — there
        // is no nested table or non-string key that could fail. The error is
        // surfaced (not `.expect()`-panicked) to keep this path panic-free.
        let table = toml::to_string(&agent).map_err(|e| RenderError::Serialization {
            format: "TOML",
            source: Box::new(e),
        })?;

        let mut document = toml_provenance(pinned);
        document.push_str(&table);
        Ok(Some(RenderedDoc { document, warnings }))
    }
}

/// Codex's user-level config root. `$CODEX_HOME` replaces `~/.codex` when
/// set, else `~/.codex` (developers.openai.com/codex/config). Hosts the
/// `agents/` subdir; the [`PathAnchor`](super::path_anchor) `CodexRoot`
/// anchor is rooted here. Note: this does **not** relocate Codex skills —
/// those follow `$HOME/.agents/skills` (see [`global_skills_root`]).
pub(crate) fn codex_root(codex_home: Option<PathBuf>, home: Option<PathBuf>) -> Option<PathBuf> {
    codex_home.or_else(|| home.map(|h| h.join(".codex")))
}

/// Codex's user-level skills dir: the cross-vendor open standard
/// `$HOME/.agents/skills`. Keyed on `$HOME` only — `$CODEX_HOME` does NOT
/// move it. The [`PathAnchor`](super::path_anchor) `AgentsSkills` anchor is
/// rooted here.
pub(crate) fn global_skills_root(home: Option<PathBuf>) -> Option<PathBuf> {
    home.map(|h| h.join(".agents").join("skills"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn supports_kind_declines_only_rule() {
        assert!(CodexVendor.supports_kind(ArtifactKind::Skill));
        assert!(CodexVendor.supports_kind(ArtifactKind::Agent));
        assert!(
            !CodexVendor.supports_kind(ArtifactKind::Rule),
            "Codex has no rule target"
        );
    }

    #[test]
    fn codex_root_resolution_order() {
        assert_eq!(
            codex_root(Some(PathBuf::from("/custom/cx")), Some(PathBuf::from("/home/u"))),
            Some(PathBuf::from("/custom/cx")),
            "CODEX_HOME replaces ~/.codex"
        );
        assert_eq!(
            codex_root(None, Some(PathBuf::from("/home/u"))),
            Some(PathBuf::from("/home/u/.codex"))
        );
        assert_eq!(codex_root(None, None), None);
    }

    #[test]
    fn global_skills_root_is_home_agents_skills_not_codex_home() {
        assert_eq!(
            global_skills_root(Some(PathBuf::from("/home/u"))),
            Some(PathBuf::from("/home/u/.agents/skills"))
        );
        assert_eq!(global_skills_root(None), None);
    }

    #[test]
    fn skills_root_project_is_agents_skills_not_dot_codex() {
        assert_eq!(
            CodexVendor.skills_root(Path::new("/w"), ConfigScope::Project),
            PathBuf::from("/w/.agents/skills")
        );
    }

    #[test]
    fn agent_path_project_is_dot_codex_toml() {
        assert_eq!(
            CodexVendor.agent_path(Path::new("/w"), ConfigScope::Project, "rev"),
            PathBuf::from("/w/.codex/agents/rev.toml")
        );
    }

    #[test]
    fn detect_project_scope_follows_dot_codex_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let w = tmp.path();
        assert!(
            !CodexVendor.detect(w, ConfigScope::Project),
            "absent .codex ⇒ not detected"
        );
        // The shared `.agents/skills` dir alone must NOT mark Codex present.
        std::fs::create_dir_all(w.join(".agents").join("skills")).unwrap();
        assert!(
            !CodexVendor.detect(w, ConfigScope::Project),
            ".agents/skills is a weak cross-vendor marker, not a Codex signal"
        );
        std::fs::create_dir_all(w.join(".codex")).unwrap();
        assert!(CodexVendor.detect(w, ConfigScope::Project));
    }

    fn parsed_agent(doc: &str) -> ParsedAgent {
        crate::skill::AgentFrontmatter::parse_doc(doc, Path::new("code-reviewer.md")).unwrap()
    }

    #[test]
    fn agent_index_emits_toml_with_developer_instructions_and_drops_tools() {
        let doc =
            "---\nname: code-reviewer\ndescription: Reviews diffs.\nmodel: gpt-5\ntools: Read,Grep\n---\nYou review.\n";
        let out = CodexVendor
            .agent_index(&parsed_agent(doc), "r@sha256:d")
            .unwrap()
            .unwrap();

        // Provenance is a TOML comment, not an HTML comment.
        assert!(
            out.document
                .starts_with("# generated by grim from r@sha256:d; edits will be overwritten\n"),
            "{}",
            out.document
        );
        assert!(!out.document.contains("<!--"), "HTML comment is invalid in TOML");
        // Parses as TOML and carries the native keys.
        let value: toml::Value = out.document.parse().expect("valid TOML");
        let table = value.as_table().unwrap();
        assert_eq!(table["name"].as_str(), Some("code-reviewer"));
        assert_eq!(table["description"].as_str(), Some("Reviews diffs."));
        assert_eq!(table["model"].as_str(), Some("gpt-5"));
        assert_eq!(table["developer_instructions"].as_str(), Some("You review.\n"));
        assert!(!table.contains_key("tools"), "tools has no Codex equivalent");
        // The drop is surfaced as a warning.
        assert_eq!(out.warnings.len(), 1);
        assert!(out.warnings[0].contains("tools"));
    }

    #[test]
    fn agent_index_is_deterministic() {
        let doc = "---\nname: rev\ndescription: d\nmodel: gpt-5\n---\nbody line\n";
        let a = CodexVendor.agent_index(&parsed_agent(doc), "p").unwrap().unwrap();
        let b = CodexVendor.agent_index(&parsed_agent(doc), "p").unwrap().unwrap();
        assert_eq!(a.document, b.document, "regeneration must be byte-identical");
    }

    #[test]
    fn agent_index_lifts_codex_knobs_and_overrides_model() {
        let doc = "---\nname: rev\ndescription: d\nmodel: gpt-5\nmetadata:\n  codex.model: gpt-5-codex\n  codex.reasoning-effort: high\n  codex.sandbox-mode: workspace-write\n---\nbody\n";
        let out = CodexVendor.agent_index(&parsed_agent(doc), "p").unwrap().unwrap();
        let value: toml::Value = out.document.parse().expect("valid TOML");
        let table = value.as_table().unwrap();
        assert_eq!(table["model"].as_str(), Some("gpt-5-codex"), "codex.model overrides");
        assert_eq!(table["model_reasoning_effort"].as_str(), Some("high"));
        assert_eq!(table["sandbox_mode"].as_str(), Some("workspace-write"));
        assert!(out.warnings.is_empty(), "override is silent: {:?}", out.warnings);
    }

    #[test]
    fn agent_index_rejects_bad_codex_literal() {
        let doc = "---\nname: rev\ndescription: d\nmetadata:\n  codex.reasoning-effort: turbo\n---\nbody\n";
        assert!(CodexVendor.agent_index(&parsed_agent(doc), "p").is_err());
    }

    #[test]
    fn agent_index_model_absent_omits_optional_fields() {
        // A frontmatter with only `name` and `description` — no `model`,
        // no `codex.*` knobs — exercises the `skip_serializing_if = "Option::is_none"`
        // paths for `model`, `model_reasoning_effort`, and `sandbox_mode`.
        let doc = "---\nname: minimal-agent\ndescription: Does the bare minimum.\n---\nKeep it simple.\n";
        let out = CodexVendor
            .agent_index(&parsed_agent(doc), "<pinned>")
            .unwrap()
            .unwrap();

        let value: toml::Value = out.document.parse().expect("valid TOML");
        let table = value.as_table().unwrap();

        // Required fields must be present.
        assert_eq!(table["name"].as_str(), Some("minimal-agent"));
        assert_eq!(table["description"].as_str(), Some("Does the bare minimum."));
        assert_eq!(table["developer_instructions"].as_str(), Some("Keep it simple.\n"));

        // Optional fields must be absent when not supplied.
        assert!(
            !table.contains_key("model"),
            "model must be absent when not set in frontmatter"
        );
        assert!(
            !table.contains_key("model_reasoning_effort"),
            "model_reasoning_effort must be absent when not set"
        );
        assert!(
            !table.contains_key("sandbox_mode"),
            "sandbox_mode must be absent when not set"
        );

        // No warnings expected for a clean minimal agent.
        assert!(out.warnings.is_empty(), "unexpected warnings: {:?}", out.warnings);
    }

    #[test]
    fn rule_index_is_none_defensive() {
        let parsed =
            crate::skill::RuleFrontmatter::parse_doc("---\npaths: [\"a\"]\n---\nbody\n", Path::new("r.md")).unwrap();
        assert!(CodexVendor.rule_index(&parsed, "p").unwrap().is_none());
    }
}
