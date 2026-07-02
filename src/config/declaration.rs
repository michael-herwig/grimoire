// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! The declared set of skills, rules, agents, and bundles, with a
//! lazily-cached canonical declaration hash.
//!
//! Adapted from the OCX `ProjectConfig` cache pattern: the
//! declaration-hash cache is excluded from `Clone`/`PartialEq` (it speaks
//! to runtime state, not on-disk identity) and a clone starts with a fresh
//! empty cache so a mutated clone never leaks the original's hash.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::config::hash;
use crate::oci::Identifier;

/// The view mode the catalog browser opens in, as set in `[options.tui]`.
///
/// Typed enum so an invalid value (e.g. `default_view = "list"`) is rejected
/// as an unknown enum variant at deserialization — the value set is closed and
/// serde enforces it, so no manual validation is needed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum DefaultView {
    /// Flat list view (the built-in default when this field is absent).
    Flat,
    /// Grouped collapsible tree view.
    Tree,
}

/// TUI-specific display options, nested under `[options.tui]`.
///
/// `#[serde(deny_unknown_fields)]` so an unknown key (e.g. a typo'd field
/// name) surfaces as a parse error rather than a silent ignore. An invalid
/// value for `default_view` (e.g. `"list"`) is rejected as an unknown enum
/// variant at deserialization.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TuiOptions {
    /// The view mode to open with. `"tree"` starts the browser in grouped
    /// tree view; `"flat"` (or absent) starts in flat list mode.
    /// An invalid value is rejected as an unknown enum variant at deserialization.
    /// The runtime `t` key still toggles ephemerally — config is never
    /// rewritten.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_view: Option<DefaultView>,
    /// When true, insert a type-level group (skill / rule / agent / bundle)
    /// between the registry root and the path segments in tree view.
    #[serde(default)]
    pub group_by_type: bool,
    /// Characters on which the repository path is split into nested groups
    /// in tree view. When absent or empty, `/` is used at runtime. Each
    /// entry must be exactly one single-column printable character; empty,
    /// multi-character, control, whitespace, or zero-width entries are a
    /// parse error.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tree_separators: Vec<String>,
}

impl TuiOptions {
    /// True when no option has been set — used for `skip_serializing_if`
    /// so an unconfigured `[options.tui]` table is omitted from the
    /// serialized config.
    ///
    /// Derived from `PartialEq + Default` so any future field addition is
    /// automatically reflected here without a manual update.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// Optional config options shared by both scopes.
///
/// `#[serde(deny_unknown_fields)]` so schema drift surfaces as a parse
/// error rather than a silent ignore.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ConfigOptions {
    /// Default registry for short identifiers (lower priority than
    /// `GRIM_DEFAULT_REGISTRY`; see the registry-precedence chain in
    /// `command::resolve_default_registry`).
    ///
    /// **Deprecated for new writes** — `grim init` now emits a
    /// `[[registries]]` entry with `default = true` instead. This field is
    /// still read for back-compat and folded into the resolution chain; it
    /// is ignored for browse purposes when a `[[registries]]` array is
    /// present (the array is authoritative). No `#[deprecated]` attribute
    /// is added — the field is a serde key, not a callable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_registry: Option<String>,
    /// AI client targets install/update materialize into when `--client` is
    /// absent. A list so one declaration can generate for several clients
    /// at once (e.g. `["claude", "opencode"]`); empty triggers detection of
    /// the clients whose vendor dir is present, falling back to `claude`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub clients: Vec<String>,
    /// TUI display options (grouped tree view, separators, default mode).
    #[serde(default, skip_serializing_if = "TuiOptions::is_empty")]
    pub tui: TuiOptions,
}

/// One configured browse source in the top-level `[[registries]]` array.
///
/// Additive over the single `[options].default_registry`: a config that
/// declares no `[[registries]]` keeps the legacy single-registry behavior.
/// When present, the array is the authoritative browse set for
/// `search`/`tui`/`mcp`, and its `default = true` entry (else the first)
/// is the primary registry short identifiers expand against.
///
/// Exactly one of [`Self::oci`] / [`Self::index`] must be set (enforced by
/// `validate_registries`). An `oci` entry lists packages via the OCI
/// `_catalog` endpoint; an `index` entry lists packages from a package
/// index whose entries carry their own fully-qualified registry refs — so
/// pairing an index with an OCI registry ref would be meaningless.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RegistryConfig {
    /// Optional short alias used in qualified `alias/repo` references and
    /// shown as the tree-root label. Must be non-empty when present and
    /// unique across the array.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alias: Option<String>,
    /// A plain OCI registry ref — host (and optional namespace), e.g.
    /// `ghcr.io` or `ghcr.io/acme`. Same shape as
    /// `[options].default_registry`. Mutually exclusive with
    /// [`Self::index`]. Accepts the pre-0.7.0 key `url` as a
    /// deserialization alias.
    #[serde(default, alias = "url", skip_serializing_if = "Option::is_none")]
    pub oci: Option<String>,
    /// A package-index locator replacing the `_catalog` listing: an
    /// `http(s)://` base serving compiled static files (`all.json`), or a
    /// git repository (`git+…`, `ssh://`, `git@…`, or a URL ending in
    /// `.git`) holding `index/**/metadata.json`. Mutually exclusive with
    /// [`Self::oci`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index: Option<String>,
    /// Marks this registry as the primary one short identifiers expand
    /// against. Exactly one entry MAY set it; setting it on two or more
    /// entries is a parse error. When none set it, the first entry is
    /// primary at resolution time.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub default: bool,
}

impl RegistryConfig {
    /// The entry's locator — the `oci` registry ref or the `index` locator,
    /// whichever is set (`oci` wins if both are, which validation rejects).
    /// Empty only for an invalid entry that validation would reject.
    pub fn locator(&self) -> &str {
        self.oci.as_deref().or(self.index.as_deref()).unwrap_or("")
    }
}

/// The declared skills, rules, and agents.
///
/// `skills` / `rules` / `agents` are `(name → fully-qualified identifier)`
/// maps. The declaration hash (RFC 8785 JCS + SHA-256) is cached on first
/// access via [`Self::declaration_hash_cached`]; in-place mutators must call
/// [`Self::invalidate_declaration_hash_cache`] to keep the cache coherent.
#[derive(Debug, Default)]
pub struct DesiredSet {
    /// Declared skills, keyed by config name.
    pub skills: BTreeMap<String, Identifier>,
    /// Declared rules, keyed by config name.
    pub rules: BTreeMap<String, Identifier>,
    /// Declared agents, keyed by config name.
    pub agents: BTreeMap<String, Identifier>,
    /// Declared bundles, keyed by config name. A bundle expands into its
    /// member skills/rules/agents at resolve time; the identifier is the
    /// bundle artifact reference (floating tag or pinned digest).
    pub bundles: BTreeMap<String, Identifier>,

    /// Lazily-computed canonical declaration hash.
    ///
    /// `OnceLock` (not `OnceCell`) so the type stays `Send + Sync`.
    /// Excluded from `Clone` / `PartialEq` — those speak to the declared
    /// content, not the derived cache. A clone resets it (fresh `OnceLock`)
    /// so an independently-mutated clone cannot serve the original's hash.
    declaration_hash_cache: OnceLock<String>,
}

impl Clone for DesiredSet {
    fn clone(&self) -> Self {
        // Fresh cache on clone: the clone may be mutated independently;
        // sharing the cached hash would leak a stale value through to the
        // divergent clone and defeat the staleness gate.
        Self {
            skills: self.skills.clone(),
            rules: self.rules.clone(),
            agents: self.agents.clone(),
            bundles: self.bundles.clone(),
            declaration_hash_cache: OnceLock::new(),
        }
    }
}

impl PartialEq for DesiredSet {
    fn eq(&self, other: &Self) -> bool {
        // The cache is derived; equality speaks to declared content only.
        self.skills == other.skills
            && self.rules == other.rules
            && self.agents == other.agents
            && self.bundles == other.bundles
    }
}

impl Eq for DesiredSet {}

impl DesiredSet {
    /// Construct from explicit skill/rule maps with no bundles (fixtures,
    /// programmatic callers). The declaration-hash cache starts empty.
    pub fn from_parts(skills: BTreeMap<String, Identifier>, rules: BTreeMap<String, Identifier>) -> Self {
        Self::from_parts_with_bundles(skills, rules, BTreeMap::new())
    }

    /// Construct from explicit skill, rule, and bundle maps with no agents.
    pub fn from_parts_with_bundles(
        skills: BTreeMap<String, Identifier>,
        rules: BTreeMap<String, Identifier>,
        bundles: BTreeMap<String, Identifier>,
    ) -> Self {
        Self::from_maps(skills, rules, BTreeMap::new(), bundles)
    }

    /// Construct from explicit skill, rule, agent, and bundle maps.
    pub fn from_maps(
        skills: BTreeMap<String, Identifier>,
        rules: BTreeMap<String, Identifier>,
        agents: BTreeMap<String, Identifier>,
        bundles: BTreeMap<String, Identifier>,
    ) -> Self {
        Self {
            skills,
            rules,
            agents,
            bundles,
            declaration_hash_cache: OnceLock::new(),
        }
    }

    /// The lazily-cached canonical declaration hash.
    ///
    /// First call computes via [`hash::declaration_hash`]; later calls
    /// return the cached `&str` for free.
    pub fn declaration_hash_cached(&self) -> &str {
        self.declaration_hash_cache.get_or_init(|| hash::declaration_hash(self))
    }

    /// Drop any cached hash so the next [`Self::declaration_hash_cached`]
    /// recomputes from current state. In-place mutators that change
    /// `skills` / `rules` / `agents` MUST call this or the staleness gate
    /// compares against a pre-mutation hash.
    pub fn invalidate_declaration_hash_cache(&mut self) {
        self.declaration_hash_cache.take();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(s: &str) -> Identifier {
        Identifier::parse(s).expect("valid identifier")
    }

    #[test]
    fn cached_hash_matches_free_function() {
        let mut skills = BTreeMap::new();
        skills.insert("code-review".to_string(), id("ghcr.io/acme/code-review:stable"));
        let set = DesiredSet::from_parts(skills, BTreeMap::new());
        let cached = set.declaration_hash_cached().to_string();
        assert_eq!(cached, hash::declaration_hash(&set));
        // Cheap path returns the same value.
        assert_eq!(cached, set.declaration_hash_cached());
    }

    #[test]
    fn cache_invalidated_on_mutation() {
        let mut skills = BTreeMap::new();
        skills.insert("code-review".to_string(), id("ghcr.io/acme/code-review:stable"));
        let mut set = DesiredSet::from_parts(skills, BTreeMap::new());
        let before = set.declaration_hash_cached().to_string();

        set.skills.insert("docs".to_string(), id("ghcr.io/acme/docs:1"));
        set.invalidate_declaration_hash_cache();

        assert_ne!(before, set.declaration_hash_cached());
    }

    #[test]
    fn clone_resets_cache_so_mutated_clone_reflects_new_state() {
        let mut skills = BTreeMap::new();
        skills.insert("code-review".to_string(), id("ghcr.io/acme/code-review:stable"));
        let set = DesiredSet::from_parts(skills, BTreeMap::new());
        let original_hash = set.declaration_hash_cached().to_string();

        let mut cloned = set.clone();
        cloned.skills.insert("docs".to_string(), id("ghcr.io/acme/docs:1"));
        // No invalidate call — the clone's cache started empty so the
        // first fill must reflect the mutated state.
        assert_ne!(original_hash, cloned.declaration_hash_cached());
    }

    #[test]
    fn eq_ignores_cache_state() {
        let mut skills = BTreeMap::new();
        skills.insert("x".to_string(), id("ghcr.io/acme/x:1"));
        let a = DesiredSet::from_parts(skills.clone(), BTreeMap::new());
        let b = DesiredSet::from_parts(skills, BTreeMap::new());
        // Fill a's cache but not b's — equality must still hold.
        let _ = a.declaration_hash_cached();
        assert_eq!(a, b);
    }
}
