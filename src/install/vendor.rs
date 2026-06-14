// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! The per-vendor materialization strategy seam.
//!
//! [`Vendor`] is the interface every supported AI client implements: it
//! owns the client's on-disk layout (project **and** global/native
//! user-level discovery paths), its known-field registries (the **only**
//! place vendor field knowledge lives), its index transforms, and its
//! config side-effects. [`super::client_target::ClientTarget`] stays the
//! closed identity enum (parse/display); behavior dispatches through the
//! vendor structs in `vendor_claude` / `vendor_opencode` /
//! `vendor_copilot`. Adding a client = one new struct + one enum arm.
//!
//! Design principle (owner decision): a capability **common to several
//! vendors** is authored once as a canonical top-level frontmatter field
//! and projected per vendor (e.g. a rule's `paths` → Claude `paths:`,
//! Copilot `applyTo:`); a capability **unique to one vendor** is authored
//! as a `<vendor>.<field>` string key inside the `metadata` map.
//!
//! Scope-aware layout: project-scope installs land under
//! `<workspace>/<root_dir>/…`; global-scope installs land in the vendor's
//! **native** user-level discovery directory (`~/.claude`,
//! `~/.config/opencode/skills`, `~/.copilot/skills`) so the tool actually
//! loads them — falling back to the workspace layout when the native
//! location cannot be resolved (no `$HOME`) or does not exist for the
//! artifact kind.

use std::io;
use std::path::{Path, PathBuf};

use crate::config::scope::ConfigScope;
use crate::oci::ArtifactKind;
use crate::skill::agent_frontmatter::ParsedAgent;
use crate::skill::rule_frontmatter::ParsedRule;

use super::install_state::InstallState;
use super::render::{RenderError, RenderedDoc};

/// The native YAML type a known namespaced field converts to.
#[derive(Debug, Clone, Copy)]
pub enum FieldType {
    /// `"true"` / `"false"` → native YAML bool; anything else errors.
    Bool,
    /// Passthrough string.
    String,
    /// Passthrough string validated against a closed set of literals.
    Enum(&'static [&'static str]),
    /// Base-10 integer literal → native YAML number; anything else errors.
    Integer,
    /// Finite float literal → native YAML number; anything else errors.
    Float,
    /// Comma-separated string → native YAML sequence (segments trimmed,
    /// empties dropped, input order kept). Never fails.
    CommaList,
}

/// One row of a vendor registry: the namespaced field name (the part
/// after `<vendor>.`), the native frontmatter key it lifts to, and its
/// native type.
pub struct KnownField {
    /// The metadata key suffix (`user-invocable` in `claude.user-invocable`).
    pub field: &'static str,
    /// The native frontmatter key the value is emitted under.
    pub native: &'static str,
    /// The native value type (drives conversion + validation).
    pub ty: FieldType,
}

/// A supported AI client's materialization strategy.
pub trait Vendor {
    /// The vendor name — the `metadata` namespace prefix and the
    /// `--client` identifier (`claude`, `opencode`, `copilot`, `codex`).
    fn name(&self) -> &'static str;

    /// The client root directory under a project workspace (`.claude`, …).
    fn root_dir(&self) -> &'static str;

    /// Whether this vendor has a native install target for `kind`. Default
    /// `true`; a vendor returns `false` to decline a kind (the installer
    /// warns + skips, records no output). Codex declines [`ArtifactKind::Rule`]
    /// — it has no faithful path-scoped instruction mechanism.
    fn supports_kind(&self, _kind: ArtifactKind) -> bool {
        true
    }

    /// Known `<vendor>.*` skill metadata fields lifted into native
    /// `SKILL.md` frontmatter. Empty ⇒ the vendor reads only universal
    /// agentskills fields (any own-namespace key is a typo: warn + drop).
    fn skill_fields(&self) -> &'static [KnownField] {
        &[]
    }

    /// Known `<vendor>.*` rule metadata fields. Same semantics as
    /// [`Self::skill_fields`], for rule frontmatter `metadata`.
    fn rule_fields(&self) -> &'static [KnownField] {
        &[]
    }

    /// Known `<vendor>.*` agent metadata fields. Same semantics as
    /// [`Self::skill_fields`], for agent frontmatter `metadata`. A lifted
    /// key whose native name collides with a projected common field
    /// (`model`, `tools`) **overrides** it — the documented escape hatch.
    fn agent_fields(&self) -> &'static [KnownField] {
        &[]
    }

    /// Whether this client is *detected* for `scope` — its vendor
    /// directory / config marker is present — so a default install (no
    /// `--client`, no `[options].clients`) should target it. Pure existence
    /// checks; no I/O beyond `stat`.
    ///
    /// The default probes the project root dir (`<workspace>/<root_dir>`)
    /// for project scope and returns `false` for global scope. Each vendor
    /// overrides this to own its native user-level discovery knowledge for
    /// the global scope (and, for Copilot, a tighter project marker than
    /// the broadly-present `.github` dir).
    fn detect(&self, workspace: &Path, scope: ConfigScope) -> bool {
        match scope {
            ConfigScope::Project => workspace.join(self.root_dir()).exists(),
            ConfigScope::Global => false,
        }
    }

    /// The directory skill trees install under for `scope`.
    fn skills_root(&self, workspace: &Path, scope: ConfigScope) -> PathBuf;

    /// The install path of the rule index `<name>` for `scope`.
    fn rule_path(&self, workspace: &Path, scope: ConfigScope, name: &str) -> PathBuf;

    /// The install path of the agent file `<name>` for `scope`. Every
    /// vendor has a native agents directory (project and user level), so
    /// there is no default — each vendor owns its layout.
    fn agent_path(&self, workspace: &Path, scope: ConfigScope, name: &str) -> PathBuf;

    /// Render the `SKILL.md` index for this vendor, or `None` when the
    /// canonical bytes should install verbatim (no tool-namespaced
    /// metadata, or not parseable as a skill).
    ///
    /// # Errors
    ///
    /// [`RenderError`] when a known `<vendor>.<field>` metadata key
    /// carries an unconvertible literal.
    fn skill_index(&self, doc: &str) -> Result<Option<RenderedDoc>, RenderError>;

    /// Render the rule index document for this vendor, or `None` when the
    /// canonical bytes should install verbatim. A `Some` document is
    /// written `generated: true` (integrity-anchored on the rendered
    /// bytes) and must be deterministic.
    ///
    /// # Errors
    ///
    /// [`RenderError`] when a known `<vendor>.<field>` metadata key
    /// carries an unconvertible literal.
    fn rule_index(&self, parsed: &ParsedRule, pinned: &str) -> Result<Option<RenderedDoc>, RenderError>;

    /// Render the agent document for this vendor, or `None` when the
    /// canonical bytes should install verbatim. Same `generated`/
    /// determinism contract as [`Self::rule_index`]. The projected common
    /// fields (`name`/`description`/`model`/`tools`) follow the per-vendor
    /// emit matrix; a lifted `<vendor>.*` key overrides its common field.
    ///
    /// # Errors
    ///
    /// [`RenderError`] when a known `<vendor>.<field>` metadata key
    /// carries an unconvertible literal.
    fn agent_index(&self, parsed: &ParsedAgent, pinned: &str) -> Result<Option<RenderedDoc>, RenderError>;

    /// Converge vendor-owned configuration on the current install state —
    /// the reversible config-registration seam (hooks ADR pattern).
    /// Called after install/update/uninstall mutated `state` for every
    /// involved vendor. Default: no-op.
    ///
    /// # Errors
    ///
    /// An I/O failure editing the vendor config (the operation that
    /// triggered the sync still completed; callers surface the error).
    fn sync_config(&self, _state: &InstallState, _workspace: &Path, _scope: ConfigScope) -> io::Result<()> {
        Ok(())
    }
}

/// The shared provenance header generated rule transforms prepend.
pub fn provenance(pinned: &str) -> String {
    format!("<!-- generated by grim from {pinned}; edits will be overwritten -->\n")
}

/// The provenance header generated TOML transforms prepend. TOML uses `#`
/// line comments — the HTML-comment [`provenance`] header is invalid in
/// TOML, so Codex agent files get this variant instead.
pub fn toml_provenance(pinned: &str) -> String {
    format!("# generated by grim from {pinned}; edits will be overwritten\n")
}

/// `$HOME`, when set and non-empty.
pub fn home_dir() -> Option<PathBuf> {
    env_dir("HOME")
}

/// The value of `var` as a path, when set and non-empty. An empty value
/// is treated as unset, matching common env-override conventions.
pub fn env_dir(var: &str) -> Option<PathBuf> {
    std::env::var_os(var).filter(|v| !v.is_empty()).map(PathBuf::from)
}

/// `$XDG_CONFIG_HOME`, else `$HOME/.config`, when resolvable.
pub fn xdg_config_dir() -> Option<PathBuf> {
    env_dir("XDG_CONFIG_HOME").or_else(|| home_dir().map(|h| h.join(".config")))
}
