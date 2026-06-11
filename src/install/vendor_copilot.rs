// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! GitHub Copilot's vendor strategy: universal skills, instructions rules.
//!
//! Copilot agent skills read only the universal agentskills `SKILL.md`
//! fields (docs.github.com → "about agent skills"; `allowed-tools` is the
//! universal experimental field and passes through canonically), so the
//! skill registry is empty and the render matches OpenCode's universal
//! shape. Rules become `.github/instructions/<name>.instructions.md`:
//! the canonical `paths` globs comma-join into the single `applyTo:`
//! string Copilot reads, and the vendor-unique `copilot.exclude-agent`
//! metadata key lifts to `excludeAgent:` (enum `code-review` /
//! `cloud-agent`, per docs.github.com "add repository instructions").

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use crate::config::scope::ConfigScope;
use crate::skill::rule_frontmatter::ParsedRule;

use super::render::{self, RenderError, RenderedDoc};
use super::vendor::{FieldType, KnownField, Vendor, env_dir, home_dir, provenance};

/// GitHub Copilot.
pub struct CopilotVendor;

/// `copilot.*` rule metadata fields → instructions-file frontmatter.
pub const COPILOT_RULE_FIELDS: &[KnownField] = &[KnownField {
    field: "exclude-agent",
    native: "excludeAgent",
    ty: FieldType::Enum(&["code-review", "cloud-agent"]),
}];

impl Vendor for CopilotVendor {
    fn name(&self) -> &'static str {
        "copilot"
    }

    fn root_dir(&self) -> &'static str {
        ".github"
    }

    // Skill registry empty: Copilot skills are agentskills-universal.

    fn rule_fields(&self) -> &'static [KnownField] {
        COPILOT_RULE_FIELDS
    }

    fn detect(&self, workspace: &Path, scope: ConfigScope) -> bool {
        match scope {
            // Project: a Copilot-SPECIFIC marker, NOT bare `.github` —
            // nearly every repo carries `.github/` for CI with nothing to
            // do with Copilot, so detection requires a
            // `.github/copilot-instructions.md` file or a
            // `.github/instructions/` directory.
            ConfigScope::Project => {
                let github = workspace.join(".github");
                github.join("copilot-instructions.md").is_file() || github.join("instructions").is_dir()
            }
            // Global: the native `~/.copilot` skills root (or its
            // `$COPILOT_HOME` override) being present marks Copilot CLI as a
            // configured client on this machine.
            ConfigScope::Global => global_skills_root(env_dir("COPILOT_HOME"), home_dir()).is_some_and(|p| p.exists()),
        }
    }

    fn skills_root(&self, workspace: &Path, scope: ConfigScope) -> PathBuf {
        match scope {
            ConfigScope::Project => workspace.join(".github").join("skills"),
            ConfigScope::Global => global_skills_root(env_dir("COPILOT_HOME"), home_dir())
                .unwrap_or_else(|| workspace.join(".github").join("skills")),
        }
    }

    fn rule_path(&self, workspace: &Path, scope: ConfigScope, name: &str) -> PathBuf {
        // Copilot documents no user-level instructions directory; global
        // rules stay under the workspace layout (inert for Copilot — the
        // installer warns).
        let _ = scope;
        workspace
            .join(".github")
            .join("instructions")
            .join(format!("{name}.instructions.md"))
    }

    fn skill_index(&self, doc: &str) -> Result<Option<RenderedDoc>, RenderError> {
        render::render_skill_doc(doc, self)
    }

    fn rule_index(&self, parsed: &ParsedRule, pinned: &str) -> Result<Option<RenderedDoc>, RenderError> {
        let projection = render::project_rule(&parsed.frontmatter, self)?;

        let mut document = String::new();
        let apply_to = parsed.frontmatter.paths.join(",");
        if !apply_to.is_empty() || !projection.lifted.is_empty() {
            document.push_str("---\n");
            if !apply_to.is_empty() {
                // Quoted: a glob's leading `*` would otherwise read as a
                // YAML alias indicator.
                let _ = writeln!(document, "applyTo: \"{}\"", apply_to.replace('"', "\\\""));
            }
            for (native, value) in &projection.lifted {
                if let serde_yaml::Value::String(s) = value {
                    let _ = writeln!(document, "{native}: \"{s}\"");
                }
            }
            document.push_str("---\n");
        }
        document.push_str(&provenance(pinned));
        document.push_str(&parsed.body);
        Ok(Some(RenderedDoc {
            document,
            warnings: projection.warnings,
        }))
    }
}

/// Copilot CLI's personal skills dir. `$COPILOT_HOME` "replaces the entire
/// ~/.copilot path" (docs.github.com → Copilot CLI config-dir reference),
/// else `~/.copilot`. `$XDG_CONFIG_HOME` interplay is undocumented and
/// inconsistent upstream (github/copilot-cli#1750) — not honored here.
fn global_skills_root(copilot_home: Option<PathBuf>, home: Option<PathBuf>) -> Option<PathBuf> {
    copilot_home
        .or_else(|| home.map(|h| h.join(".copilot")))
        .map(|d| d.join("skills"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skill::RuleFrontmatter;
    use std::path::Path;

    #[test]
    fn global_skills_root_resolution_order() {
        assert_eq!(
            global_skills_root(Some(PathBuf::from("/custom/cop")), Some(PathBuf::from("/home/u"))),
            Some(PathBuf::from("/custom/cop/skills")),
            "COPILOT_HOME replaces ~/.copilot entirely"
        );
        assert_eq!(
            global_skills_root(None, Some(PathBuf::from("/home/u"))),
            Some(PathBuf::from("/home/u/.copilot/skills"))
        );
        assert_eq!(global_skills_root(None, None), None);
    }

    fn parsed(doc: &str) -> ParsedRule {
        RuleFrontmatter::parse_doc(doc, Path::new("rust-style.md")).unwrap()
    }

    #[test]
    fn detect_project_needs_tighter_marker_than_bare_github() {
        let tmp = tempfile::tempdir().unwrap();
        let w = tmp.path();
        // A bare `.github` dir (CI workflows) must NOT count as Copilot.
        std::fs::create_dir_all(w.join(".github").join("workflows")).unwrap();
        assert!(
            !CopilotVendor.detect(w, ConfigScope::Project),
            "bare .github is not a Copilot signal"
        );
        // The instructions dir IS a Copilot signal.
        std::fs::create_dir_all(w.join(".github").join("instructions")).unwrap();
        assert!(CopilotVendor.detect(w, ConfigScope::Project));
    }

    #[test]
    fn detect_project_by_copilot_instructions_file() {
        let tmp = tempfile::tempdir().unwrap();
        let w = tmp.path();
        std::fs::create_dir_all(w.join(".github")).unwrap();
        std::fs::write(w.join(".github").join("copilot-instructions.md"), "# x\n").unwrap();
        assert!(CopilotVendor.detect(w, ConfigScope::Project));
    }

    #[test]
    fn rule_index_maps_paths_to_apply_to() {
        let doc = "---\npaths:\n  - \"**/*.rs\"\n  - \"Cargo.toml\"\n---\n# Rust Style\n\nUse 4 spaces.\n";
        let out = CopilotVendor
            .rule_index(&parsed(doc), "ghcr.io/acme/rust-style@sha256:abc")
            .unwrap()
            .unwrap();
        let expected = "---\napplyTo: \"**/*.rs,Cargo.toml\"\n---\n<!-- generated by grim from ghcr.io/acme/rust-style@sha256:abc; edits will be overwritten -->\n# Rust Style\n\nUse 4 spaces.\n";
        assert_eq!(out.document, expected);
        assert!(!out.document.contains("paths:"), "canonical frontmatter must not leak");
    }

    #[test]
    fn rule_index_emits_exclude_agent_from_metadata() {
        let doc = "---\npaths: [\"a\"]\nmetadata:\n  copilot.exclude-agent: code-review\n---\nbody\n";
        let out = CopilotVendor.rule_index(&parsed(doc), "p").unwrap().unwrap();
        assert!(
            out.document
                .starts_with("---\napplyTo: \"a\"\nexcludeAgent: \"code-review\"\n---\n")
        );
        assert!(out.warnings.is_empty());
    }

    #[test]
    fn rule_index_rejects_bad_exclude_agent() {
        let doc = "---\nmetadata:\n  copilot.exclude-agent: everything\n---\nbody\n";
        let err = CopilotVendor.rule_index(&parsed(doc), "p").unwrap_err();
        assert!(err.to_string().contains("copilot.exclude-agent"), "{err}");
    }

    #[test]
    fn bare_rule_has_no_frontmatter_block() {
        let doc = "# Just A Rule\nguidance\n";
        let out = CopilotVendor.rule_index(&parsed(doc), "r@sha256:d").unwrap().unwrap();
        assert_eq!(
            out.document,
            "<!-- generated by grim from r@sha256:d; edits will be overwritten -->\n# Just A Rule\nguidance\n"
        );
    }

    #[test]
    fn rule_index_is_deterministic() {
        let doc = "---\npaths: [\"a\"]\n---\nbody line\n";
        let a = CopilotVendor.rule_index(&parsed(doc), "r@sha256:d").unwrap().unwrap();
        let b = CopilotVendor.rule_index(&parsed(doc), "r@sha256:d").unwrap().unwrap();
        assert_eq!(a.document, b.document, "regeneration must be byte-identical");
    }
}
