// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! Claude Code's vendor strategy: the richest native frontmatter surface.
//!
//! Claude reads typed extension fields in `SKILL.md` (booleans, enums) —
//! the registry below maps each `claude.*` metadata key to its native
//! key and type, verified against the official frontmatter reference
//! (code.claude.com/docs/en/skills). Rules are near-canonical: `paths:`
//! is native (code.claude.com/docs/en/memory), so a plain rule installs
//! verbatim; a rule carrying tool-namespaced metadata is re-rendered to
//! the cleaned canonical shape (foreign vendor keys dropped).

use std::path::{Path, PathBuf};

use crate::config::scope::ConfigScope;
use crate::skill::rule_frontmatter::ParsedRule;

use super::render::{self, RenderError, RenderedDoc};
use super::vendor::{FieldType, KnownField, Vendor, env_dir, home_dir};

/// Claude Code.
pub struct ClaudeVendor;

/// `claude.*` skill fields → native Claude Code `SKILL.md` frontmatter.
///
/// `hooks` (an object) is deliberately absent: it cannot be expressed as a
/// single string metadata value; the separate hooks ADR owns that surface.
pub const CLAUDE_SKILL_FIELDS: &[KnownField] = &[
    KnownField {
        field: "disable-model-invocation",
        native: "disable-model-invocation",
        ty: FieldType::Bool,
    },
    KnownField {
        field: "user-invocable",
        native: "user-invocable",
        ty: FieldType::Bool,
    },
    KnownField {
        field: "model",
        native: "model",
        ty: FieldType::String,
    },
    KnownField {
        field: "effort",
        native: "effort",
        ty: FieldType::Enum(&["low", "medium", "high", "xhigh", "max"]),
    },
    KnownField {
        field: "context",
        native: "context",
        ty: FieldType::Enum(&["fork"]),
    },
    KnownField {
        field: "agent",
        native: "agent",
        ty: FieldType::String,
    },
    KnownField {
        field: "argument-hint",
        native: "argument-hint",
        ty: FieldType::String,
    },
    KnownField {
        // Note the native key uses an underscore — Claude reads
        // `when_to_use`, not `when-to-use`.
        field: "when-to-use",
        native: "when_to_use",
        ty: FieldType::String,
    },
    KnownField {
        field: "arguments",
        native: "arguments",
        ty: FieldType::String,
    },
    KnownField {
        field: "disallowed-tools",
        native: "disallowed-tools",
        ty: FieldType::String,
    },
    KnownField {
        field: "shell",
        native: "shell",
        ty: FieldType::Enum(&["bash", "powershell"]),
    },
    KnownField {
        field: "paths",
        native: "paths",
        ty: FieldType::String,
    },
];

impl Vendor for ClaudeVendor {
    fn name(&self) -> &'static str {
        "claude"
    }

    fn root_dir(&self) -> &'static str {
        ".claude"
    }

    fn skill_fields(&self) -> &'static [KnownField] {
        CLAUDE_SKILL_FIELDS
    }

    // Rules: `paths:` is native and authored canonically; Claude defines
    // no vendor-specific rule fields today, so the registry is empty.

    fn detect(&self, workspace: &Path, scope: ConfigScope) -> bool {
        match scope {
            ConfigScope::Project => workspace.join(".claude").exists(),
            // Global: the native user-level root Claude actually discovers
            // (or its `$CLAUDE_CONFIG_DIR` override) being present marks
            // Claude as a configured client on this machine.
            ConfigScope::Global => global_root(env_dir("CLAUDE_CONFIG_DIR"), home_dir()).is_some_and(|p| p.exists()),
        }
    }

    fn skills_root(&self, workspace: &Path, scope: ConfigScope) -> PathBuf {
        scope_root(workspace, scope).join("skills")
    }

    fn rule_path(&self, workspace: &Path, scope: ConfigScope, name: &str) -> PathBuf {
        scope_root(workspace, scope).join("rules").join(format!("{name}.md"))
    }

    fn skill_index(&self, doc: &str) -> Result<Option<RenderedDoc>, RenderError> {
        render::render_skill_doc(doc, self)
    }

    fn rule_index(&self, parsed: &ParsedRule, _pinned: &str) -> Result<Option<RenderedDoc>, RenderError> {
        // A plain rule installs verbatim (`paths:` is native). Only a rule
        // carrying tool-namespaced metadata is re-rendered: own-namespace
        // keys lift (none known today — unknown ones warn), foreign vendor
        // keys drop, plain keys stay.
        render::render_rule_canonical(parsed, self)
    }
}

/// Claude's layout root for a scope: the project `.claude` dir, or the
/// native user-level config root Claude Code actually discovers (falling
/// back to the workspace layout when neither `$CLAUDE_CONFIG_DIR` nor
/// `$HOME` resolves).
fn scope_root(workspace: &Path, scope: ConfigScope) -> PathBuf {
    match scope {
        ConfigScope::Project => workspace.join(".claude"),
        ConfigScope::Global => {
            global_root(env_dir("CLAUDE_CONFIG_DIR"), home_dir()).unwrap_or_else(|| workspace.join(".claude"))
        }
    }
}

/// Claude Code's user-level config root. `$CLAUDE_CONFIG_DIR` replaces the
/// **entire** `~/.claude` tree when set — "every ~/.claude path … lives
/// under that directory instead" (code.claude.com/docs/en/claude-directory)
/// — so skills and rules both follow it; else `~/.claude`.
fn global_root(config_dir_override: Option<PathBuf>, home: Option<PathBuf>) -> Option<PathBuf> {
    config_dir_override.or_else(|| home.map(|h| h.join(".claude")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn global_root_resolution_order() {
        assert_eq!(
            global_root(Some(PathBuf::from("/custom/cc")), Some(PathBuf::from("/home/u"))),
            Some(PathBuf::from("/custom/cc")),
            "CLAUDE_CONFIG_DIR replaces ~/.claude entirely"
        );
        assert_eq!(
            global_root(None, Some(PathBuf::from("/home/u"))),
            Some(PathBuf::from("/home/u/.claude"))
        );
        assert_eq!(
            global_root(None, None),
            None,
            "no override, no home ⇒ caller falls back"
        );
    }

    #[test]
    fn detect_project_scope_follows_dot_claude_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let w = tmp.path();
        assert!(
            !ClaudeVendor.detect(w, ConfigScope::Project),
            "absent .claude ⇒ not detected"
        );
        std::fs::create_dir_all(w.join(".claude")).unwrap();
        assert!(
            ClaudeVendor.detect(w, ConfigScope::Project),
            "present .claude ⇒ detected"
        );
    }

    #[test]
    fn docs_reference_matches_claude_registry() {
        // Doc/registry parity: `docs/src/vendor-metadata.md` must document
        // exactly the `claude.*` keys the registry knows, so the reference
        // page cannot silently drift from the renderer.
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/docs/src/vendor-metadata.md");
        let doc = std::fs::read_to_string(path).expect("docs/src/vendor-metadata.md exists (doc/registry parity)");
        let mut documented = std::collections::BTreeSet::new();
        // Backtick-delimited tokens: odd segments of a backtick split.
        for token in doc.split('`').skip(1).step_by(2) {
            if let Some(field) = token.strip_prefix("claude.")
                && !field.is_empty()
                && field.chars().all(|c| c.is_ascii_lowercase() || c == '-')
            {
                documented.insert(field.to_string());
            }
        }
        let registry: std::collections::BTreeSet<String> =
            CLAUDE_SKILL_FIELDS.iter().map(|f| f.field.to_string()).collect();
        assert_eq!(
            documented, registry,
            "vendor-metadata.md must document exactly the claude.* registry fields"
        );
    }
}
