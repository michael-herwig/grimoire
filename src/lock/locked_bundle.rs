// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! One declared bundle's cached expansion result in the lock.
//!
//! The `[[bundle]]` section makes declaration mutations computable
//! **offline**: `remove`/`uninstall`/the TUI delete action derive the
//! before/after effective desired sets from the cached member lists
//! instead of re-fetching bundle manifests. See
//! `.claude/artifacts/adr_effective_set_mutations.md`.

use serde::{Deserialize, Serialize};

use crate::oci::PinnedIdentifier;
use crate::oci::bundle::BundleMember;

/// A declared bundle's resolution snapshot: which binding declared it,
/// where it resolved to, and the member list its manifest carried.
///
/// `pinned` records the bundle manifest digest, which also gives the TUI a
/// baseline for floating-tag "outdated" re-checks on bundle rows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LockedBundle {
    /// Config binding name (TOML key from `[bundles]`).
    pub name: String,
    /// The bundle's `registry/repo`.
    pub repo: String,
    /// The declared tag this expansion resolved (or the short digest for a
    /// digest-only declaration — mirrors the member provenance tag).
    pub tag: String,
    /// Resolved bundle manifest digest (`registry/repo@sha256:…`).
    pub pinned: PinnedIdentifier,
    /// The member list the bundle manifest carried at resolution time.
    #[serde(default, rename = "member")]
    pub members: Vec<BundleMember>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oci::{ArtifactKind, Digest, Identifier};

    #[test]
    fn round_trips_through_toml() {
        let id = Identifier::new_registry("acme/bundles/stack", "ghcr.io")
            .clone_with_tag("1")
            .clone_with_digest(Digest::Sha256("a".repeat(64)));
        let bundle = LockedBundle {
            name: "stack".to_string(),
            repo: "ghcr.io/acme/bundles/stack".to_string(),
            tag: "1".to_string(),
            pinned: PinnedIdentifier::try_from(id).unwrap(),
            members: vec![BundleMember {
                kind: ArtifactKind::Skill,
                name: "code-review".to_string(),
                id: "ghcr.io/acme/code-review:1".to_string(),
            }],
        };
        let toml = toml::to_string_pretty(&bundle).expect("serialize");
        assert!(toml.contains("[[member]]"), "{toml}");
        let back: LockedBundle = toml::from_str(&toml).expect("reparse");
        assert_eq!(back, bundle);
    }
}
