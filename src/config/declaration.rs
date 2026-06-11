// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! The declared set of skills and rules, with a lazily-cached canonical
//! declaration hash.
//!
//! Adapted from the OCX `ProjectConfig` cache pattern: the
//! declaration-hash cache is excluded from `Clone`/`PartialEq` (it speaks
//! to runtime state, not on-disk identity) and a clone starts with a fresh
//! empty cache so a mutated clone never leaks the original's hash.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use crate::config::hash;
use crate::oci::Identifier;

/// Optional config options shared by both scopes.
///
/// `#[serde(deny_unknown_fields)]` so schema drift surfaces as a parse
/// error rather than a silent ignore.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigOptions {
    /// Default registry for short identifiers (lower priority than
    /// `GRIM_DEFAULT_REGISTRY`; see the registry-precedence chain in
    /// `command::resolve_default_registry`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_registry: Option<String>,
    /// AI client targets install/update materialize into when `--client` is
    /// absent. A list so one declaration can generate for several clients
    /// at once (e.g. `["claude", "opencode"]`); empty triggers detection of
    /// the clients whose vendor dir is present, falling back to `claude`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub clients: Vec<String>,
}

/// The declared skills and rules.
///
/// `skills` / `rules` are `(name → fully-qualified identifier)` maps. The
/// declaration hash (RFC 8785 JCS + SHA-256) is cached on first access via
/// [`Self::declaration_hash_cached`]; in-place mutators must call
/// [`Self::invalidate_declaration_hash_cache`] to keep the cache coherent.
#[derive(Debug, Default)]
pub struct DesiredSet {
    /// Declared skills, keyed by config name.
    pub skills: BTreeMap<String, Identifier>,
    /// Declared rules, keyed by config name.
    pub rules: BTreeMap<String, Identifier>,
    /// Declared bundles, keyed by config name. A bundle expands into its
    /// member skills/rules at resolve time; the identifier is the bundle
    /// artifact reference (floating tag or pinned digest).
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
            bundles: self.bundles.clone(),
            declaration_hash_cache: OnceLock::new(),
        }
    }
}

impl PartialEq for DesiredSet {
    fn eq(&self, other: &Self) -> bool {
        // The cache is derived; equality speaks to declared content only.
        self.skills == other.skills && self.rules == other.rules && self.bundles == other.bundles
    }
}

impl Eq for DesiredSet {}

impl DesiredSet {
    /// Construct from explicit skill/rule maps with no bundles (fixtures,
    /// programmatic callers). The declaration-hash cache starts empty.
    pub fn from_parts(skills: BTreeMap<String, Identifier>, rules: BTreeMap<String, Identifier>) -> Self {
        Self::from_parts_with_bundles(skills, rules, BTreeMap::new())
    }

    /// Construct from explicit skill, rule, and bundle maps.
    pub fn from_parts_with_bundles(
        skills: BTreeMap<String, Identifier>,
        rules: BTreeMap<String, Identifier>,
        bundles: BTreeMap<String, Identifier>,
    ) -> Self {
        Self {
            skills,
            rules,
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
    /// `skills` / `rules` MUST call this or the staleness gate compares
    /// against a pre-mutation hash.
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
