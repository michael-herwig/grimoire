// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! OpenCode's vendor strategy: universal skills, config-wired rules.
//!
//! OpenCode reads only the universal agentskills `SKILL.md` fields
//! (opencode.ai/docs/skills) — its registries are empty, so a skill
//! renders to the clean universal shape (identical to Copilot's). It has
//! no per-file rule scoping: the rule index is rewritten to provenance +
//! body, and loading is wired through the managed `instructions` entry in
//! `opencode.json` (see [`super::opencode_config`]).

use std::io;
use std::path::{Path, PathBuf};

use crate::config::scope::ConfigScope;
use crate::skill::agent_frontmatter::ParsedAgent;
use crate::skill::rule_frontmatter::ParsedRule;

use super::install_state::InstallState;
use super::opencode_config;
use super::render::{self, RenderError, RenderedDoc};
use super::vendor::{FieldType, KnownField, Vendor, env_dir, provenance, xdg_config_dir};

/// OpenCode.
pub struct OpenCodeVendor;

/// `opencode.*` agent fields → native OpenCode agent frontmatter
/// (opencode.ai/docs/agents). `model` shadows the projected canonical
/// common field — the per-vendor override escape hatch (OpenCode expects
/// `provider/model-id`, which the canonical value may not be).
/// Object-valued fields (`permission`, the deprecated `tools` map) are
/// deliberately absent: they cannot be expressed as a single string
/// metadata value.
pub const OPENCODE_AGENT_FIELDS: &[KnownField] = &[
    KnownField {
        field: "model",
        native: "model",
        ty: FieldType::String,
    },
    KnownField {
        field: "mode",
        native: "mode",
        ty: FieldType::Enum(&["primary", "subagent", "all"]),
    },
    KnownField {
        field: "temperature",
        native: "temperature",
        ty: FieldType::Float,
    },
    KnownField {
        field: "top-p",
        native: "top_p",
        ty: FieldType::Float,
    },
    KnownField {
        field: "steps",
        native: "steps",
        ty: FieldType::Integer,
    },
    KnownField {
        field: "prompt",
        native: "prompt",
        ty: FieldType::String,
    },
    KnownField {
        field: "disable",
        native: "disable",
        ty: FieldType::Bool,
    },
    KnownField {
        field: "hidden",
        native: "hidden",
        ty: FieldType::Bool,
    },
    KnownField {
        field: "color",
        native: "color",
        ty: FieldType::String,
    },
];

/// The common agent fields a lifted `opencode.*` key may silently override.
const OPENCODE_AGENT_OVERRIDES: &[&str] = &["model"];

impl Vendor for OpenCodeVendor {
    fn name(&self) -> &'static str {
        "opencode"
    }

    fn root_dir(&self) -> &'static str {
        ".opencode"
    }

    // Skill/rule registries empty: OpenCode reads only universal fields.

    fn agent_fields(&self) -> &'static [KnownField] {
        OPENCODE_AGENT_FIELDS
    }

    fn detect(&self, workspace: &Path, scope: ConfigScope) -> bool {
        match scope {
            ConfigScope::Project => workspace.join(".opencode").exists(),
            // Global: a present native skills dir (or its
            // `$OPENCODE_CONFIG_DIR` override) OR a present global
            // `opencode.json` config file. A configured-but-empty OpenCode
            // user — only an `opencode.json`, no skills dir yet — still
            // counts as a real OpenCode user.
            ConfigScope::Global => {
                global_skills_root(env_dir("OPENCODE_CONFIG_DIR"), xdg_config_dir()).is_some_and(|p| p.exists())
                    || opencode_config::config_path_for_scope(workspace, scope).is_some_and(|p| p.is_file())
            }
        }
    }

    fn skills_root(&self, workspace: &Path, scope: ConfigScope) -> PathBuf {
        match scope {
            ConfigScope::Project => workspace.join(".opencode").join("skills"),
            ConfigScope::Global => global_skills_root(env_dir("OPENCODE_CONFIG_DIR"), xdg_config_dir())
                .unwrap_or_else(|| workspace.join(".opencode").join("skills")),
        }
    }

    fn rule_path(&self, workspace: &Path, scope: ConfigScope, name: &str) -> PathBuf {
        // Rules stay under the workspace for BOTH scopes: OpenCode has no
        // native rules directory — loading is wired through the managed
        // `instructions` entry (absolute glob for the global scope), so
        // the files themselves live in grim's own layout.
        let _ = scope;
        workspace.join(".opencode").join("rules").join(format!("{name}.md"))
    }

    fn agent_path(&self, workspace: &Path, scope: ConfigScope, name: &str) -> PathBuf {
        let root = match scope {
            ConfigScope::Project => workspace.join(".opencode").join("agents"),
            // OpenCode discovers user-level agents natively from
            // `<config dir>/agents/` — same override order as skills.
            ConfigScope::Global => global_agents_root(env_dir("OPENCODE_CONFIG_DIR"), xdg_config_dir())
                .unwrap_or_else(|| workspace.join(".opencode").join("agents")),
        };
        root.join(format!("{name}.md"))
    }

    fn skill_index(&self, doc: &str) -> Result<Option<RenderedDoc>, RenderError> {
        render::render_skill_doc(doc, self)
    }

    fn rule_index(&self, parsed: &ParsedRule, pinned: &str) -> Result<Option<RenderedDoc>, RenderError> {
        // Frontmatter is meaningless to OpenCode — always rewrite to
        // provenance + body. The projection still runs for its typo-guard
        // warnings (an `opencode.*` rule key is unknown by definition).
        let projection = render::project_rule(&parsed.frontmatter, self)?;
        let mut document = provenance(pinned);
        document.push_str(&parsed.body);
        Ok(Some(RenderedDoc {
            document,
            warnings: projection.warnings,
        }))
    }

    fn agent_index(&self, parsed: &ParsedAgent, pinned: &str) -> Result<Option<RenderedDoc>, RenderError> {
        // OpenCode agents are always a transform: the filename carries the
        // identity (the canonical `name` is dropped) and only the fields
        // OpenCode reads are emitted — `description` plus the pass-through
        // `model` (caveat documented: OpenCode expects `provider/model-id`;
        // `opencode.model` overrides). `tools` is dropped (deprecated
        // upstream in favor of the object-valued `permission`).
        let projection = render::project_agent(&parsed.frontmatter, self)?;
        let mut warnings = projection.warnings;

        let mut natives: Vec<(&'static str, serde_yaml::Value)> = vec![(
            "description",
            serde_yaml::Value::String(projection.cleaned.description.to_string()),
        )];
        if let Some(model) = &projection.cleaned.model {
            natives.push(("model", serde_yaml::Value::String(model.clone())));
        }

        let mut document = render::agent_frontmatter_block(
            natives,
            projection.lifted,
            self.name(),
            OPENCODE_AGENT_OVERRIDES,
            &mut warnings,
        );
        document.push_str(&provenance(pinned));
        document.push_str(&parsed.body);
        Ok(Some(RenderedDoc { document, warnings }))
    }

    fn sync_config(&self, state: &InstallState, workspace: &Path, scope: ConfigScope) -> io::Result<()> {
        let outcome = opencode_config::sync_for_state(state, workspace, scope)?;
        tracing::debug!("opencode instructions sync: {outcome:?}");
        Ok(())
    }
}

/// OpenCode's user-level skills dir. `$OPENCODE_CONFIG_DIR` is OpenCode's
/// **additive** extra scan directory (opencode.ai/docs/config — searched
/// with the `{skill,skills}/**/SKILL.md` pattern alongside the always-
/// scanned global config dir): when the user set it, grim installs there
/// to respect the explicit override; else the default
/// `$XDG_CONFIG_HOME|~/.config/opencode/skills`. `$OPENCODE_CONFIG` (a
/// config **file** path) deliberately plays no role — it does not affect
/// OpenCode's skill discovery (sst/opencode#3432).
fn global_skills_root(config_dir_override: Option<PathBuf>, xdg_config: Option<PathBuf>) -> Option<PathBuf> {
    config_dir_override
        .map(|d| d.join("skills"))
        .or_else(|| xdg_config.map(|c| c.join("opencode").join("skills")))
}

/// OpenCode's user-level agents dir — same resolution order as
/// [`global_skills_root`], with the native `agents/` subdirectory
/// (opencode.ai/docs/agents: global agents live in
/// `~/.config/opencode/agents/`).
fn global_agents_root(config_dir_override: Option<PathBuf>, xdg_config: Option<PathBuf>) -> Option<PathBuf> {
    config_dir_override
        .map(|d| d.join("agents"))
        .or_else(|| xdg_config.map(|c| c.join("opencode").join("agents")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skill::RuleFrontmatter;
    use std::path::Path;

    #[test]
    fn rule_index_strips_frontmatter_and_adds_provenance() {
        let doc = "---\npaths: [\"**/*.rs\"]\n---\n# Rust Style\nbody\n";
        let parsed = RuleFrontmatter::parse_doc(doc, Path::new("r.md")).unwrap();
        let out = OpenCodeVendor.rule_index(&parsed, "r@sha256:d").unwrap().unwrap();
        assert_eq!(
            out.document,
            "<!-- generated by grim from r@sha256:d; edits will be overwritten -->\n# Rust Style\nbody\n"
        );
        assert!(!out.document.contains("paths:"), "OpenCode has no rule frontmatter");
        assert!(out.warnings.is_empty());
    }

    #[test]
    fn global_skills_root_resolution_order() {
        use std::path::PathBuf;
        assert_eq!(
            global_skills_root(Some(PathBuf::from("/custom/oc")), Some(PathBuf::from("/xdg"))),
            Some(PathBuf::from("/custom/oc/skills")),
            "OPENCODE_CONFIG_DIR wins when set"
        );
        assert_eq!(
            global_skills_root(None, Some(PathBuf::from("/xdg"))),
            Some(PathBuf::from("/xdg/opencode/skills"))
        );
        assert_eq!(global_skills_root(None, None), None);
    }

    #[test]
    fn detect_project_scope_follows_dot_opencode_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let w = tmp.path();
        assert!(!OpenCodeVendor.detect(w, ConfigScope::Project));
        std::fs::create_dir_all(w.join(".opencode")).unwrap();
        assert!(OpenCodeVendor.detect(w, ConfigScope::Project));
    }

    fn parsed_agent(doc: &str) -> ParsedAgent {
        crate::skill::AgentFrontmatter::parse_doc(doc, Path::new("code-reviewer.md")).unwrap()
    }

    #[test]
    fn agent_index_drops_name_and_tools_emits_description_and_model() {
        let doc = "---\nname: code-reviewer\ndescription: Reviews diffs.\nmodel: sonnet\ntools: Read,Grep\n---\nYou review.\n";
        let out = OpenCodeVendor
            .agent_index(&parsed_agent(doc), "r@sha256:d")
            .unwrap()
            .unwrap();
        assert_eq!(
            out.document,
            "---\ndescription: Reviews diffs.\nmodel: sonnet\n---\n<!-- generated by grim from r@sha256:d; edits will be overwritten -->\nYou review.\n"
        );
        assert!(!out.document.contains("name:"), "filename carries the identity");
        assert!(!out.document.contains("tools"), "tools is deprecated upstream");
        assert!(out.warnings.is_empty());
    }

    #[test]
    fn agent_index_vendor_model_overrides_common_silently() {
        let doc = "---\nname: code-reviewer\ndescription: d\nmodel: sonnet\nmetadata:\n  opencode.model: anthropic/claude-sonnet-4-5\n  opencode.mode: subagent\n  opencode.temperature: \"0.2\"\n---\nbody\n";
        let out = OpenCodeVendor.agent_index(&parsed_agent(doc), "p").unwrap().unwrap();
        assert!(out.document.contains("model: anthropic/claude-sonnet-4-5"));
        assert!(!out.document.contains("model: sonnet"));
        assert!(out.document.contains("mode: subagent"));
        assert!(out.document.contains("temperature: 0.2"), "{}", out.document);
        assert!(
            out.warnings.is_empty(),
            "expected override is silent: {:?}",
            out.warnings
        );
    }

    #[test]
    fn agent_index_rejects_bad_literals() {
        for doc in [
            "---\nname: a\ndescription: d\nmetadata:\n  opencode.mode: pilot\n---\n",
            "---\nname: a\ndescription: d\nmetadata:\n  opencode.temperature: warm\n---\n",
            "---\nname: a\ndescription: d\nmetadata:\n  opencode.steps: few\n---\n",
        ] {
            let parsed = crate::skill::AgentFrontmatter::parse_doc(doc, Path::new("a.md")).unwrap();
            assert!(OpenCodeVendor.agent_index(&parsed, "p").is_err(), "{doc}");
        }
    }

    #[test]
    fn global_agents_root_resolution_order() {
        use std::path::PathBuf;
        assert_eq!(
            global_agents_root(Some(PathBuf::from("/custom/oc")), Some(PathBuf::from("/xdg"))),
            Some(PathBuf::from("/custom/oc/agents")),
            "OPENCODE_CONFIG_DIR wins when set"
        );
        assert_eq!(
            global_agents_root(None, Some(PathBuf::from("/xdg"))),
            Some(PathBuf::from("/xdg/opencode/agents"))
        );
        assert_eq!(global_agents_root(None, None), None);
    }

    #[test]
    fn agent_path_project_scope() {
        assert_eq!(
            OpenCodeVendor.agent_path(Path::new("/w"), ConfigScope::Project, "rev"),
            PathBuf::from("/w/.opencode/agents/rev.md")
        );
    }

    #[test]
    fn own_namespace_rule_key_warns() {
        let doc = "---\nmetadata:\n  opencode.future: \"x\"\n---\nbody\n";
        let parsed = RuleFrontmatter::parse_doc(doc, Path::new("r.md")).unwrap();
        let out = OpenCodeVendor.rule_index(&parsed, "p").unwrap().unwrap();
        assert_eq!(out.warnings.len(), 1);
        assert!(out.warnings[0].contains("opencode.future"));
    }
}
