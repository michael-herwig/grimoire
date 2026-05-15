// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! The OCI-access seam: one trait every resolution/install/release/catalog
//! path depends on.
//!
//! Adapted from OCX `oci/index.rs`, collapsed to a single trait with one
//! cache layer. The OCX chained multi-source index, platform/variant
//! candidate selection, image-index manifest selection, and manifest
//! builder/push are all out of scope (plan Phase 3 + cut-lines) — Grimoire
//! resolves one floating tag to one digest and pulls one artifact blob.
//!
//! Lookup-vs-error contract (arch-principles "Option-based lookups"):
//! `Ok(None)` means "the registry does not have this", a benign miss the
//! caller decides how to treat. `Err` is reserved for transport, auth, or
//! data/integrity failures — never a benign absence.

pub mod cached_access;
pub mod error;
pub mod registry_client;

use async_trait::async_trait;

use super::manifest::OciManifest;
use super::{Digest, Identifier, PinnedIdentifier};
use crate::env;
use error::AccessError;

/// Cache/source routing policy, derived once per invocation from the
/// environment (and, in later phases, the equivalent CLI flags).
///
/// Collapsed from OCX `ChainMode`: there is no chained source list, only
/// an inner source and one persistent cache layer.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AccessMode {
    /// Read the cache first; on a tag miss consult the source and persist
    /// the result (tag pointers only when the call is a `Resolve`).
    Default,
    /// Skip the cache read for mutable lookups and go straight to the
    /// source; still write successful results back to the cache.
    Remote,
    /// Cache only. A miss that would require the network is refused
    /// (`Resolve` → `OfflineMiss`; pure `Query` → `Ok(None)`).
    Offline,
}

impl AccessMode {
    /// Derive the mode from the environment. Offline wins over remote: a
    /// user asking for both is asking for the stricter guarantee.
    pub fn from_env() -> Self {
        if env::offline() {
            Self::Offline
        } else if env::remote() {
            Self::Remote
        } else {
            Self::Default
        }
    }
}

/// Caller intent for a mutable lookup.
///
/// Adapted from OCX `IndexOperation`. A pure `Query` never produces a
/// network round-trip in offline/default-miss situations and never writes
/// a tag pointer through to the cache; a `Resolve` is the install/lock
/// path that walks the source on a miss and persists the pin.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Operation {
    /// Pure read — report existing data, never produce it.
    Query,
    /// Read with write-through — the lock/install resolution path.
    Resolve,
}

/// The single OCI-access abstraction.
///
/// Implemented by the real [`registry_client::RegistryClient`] and wrapped
/// by [`cached_access::CachedAccess`]; tests substitute a scripted mock.
#[async_trait]
pub trait OciAccess: Send + Sync {
    /// Resolve `id` to a content digest.
    ///
    /// A digest-addressed `id` resolves to itself with no I/O. A
    /// tag-addressed `id` is looked up; `Ok(None)` when the tag does not
    /// exist on the registry.
    async fn resolve_digest(&self, id: &Identifier, op: Operation) -> Result<Option<Digest>, AccessError>;

    /// Fetch the (Grimoire subset of the) manifest for a pinned artifact.
    ///
    /// `Ok(None)` when the manifest does not exist.
    async fn fetch_manifest(&self, id: &PinnedIdentifier) -> Result<Option<OciManifest>, AccessError>;

    /// Fetch the raw bytes of `digest` from `repo`'s registry.
    ///
    /// Implementations verify the bytes hash to `digest`. `Ok(None)` when
    /// the blob does not exist.
    async fn fetch_blob(&self, repo: &Identifier, digest: &Digest) -> Result<Option<Vec<u8>>, AccessError>;

    /// List the tags for `id`'s repository. `Ok(None)` when the
    /// repository is unknown to the registry.
    async fn list_tags(&self, id: &Identifier) -> Result<Option<Vec<String>>, AccessError>;

    /// List the repository catalog for `registry`. An unsupported or
    /// missing catalog endpoint yields an empty list, not an error.
    async fn list_catalog(&self, registry: &str) -> Result<Vec<String>, AccessError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pure-logic mirror of [`AccessMode::from_env`]'s precedence. Env
    /// mutation is `unsafe` in edition 2024 and forbidden crate-wide, so
    /// the offline-wins-over-remote contract is asserted through a
    /// parameterized reimplementation rather than by toggling the process
    /// environment.
    fn mode_from(offline: bool, remote: bool) -> AccessMode {
        if offline {
            AccessMode::Offline
        } else if remote {
            AccessMode::Remote
        } else {
            AccessMode::Default
        }
    }

    #[test]
    fn offline_wins_over_remote() {
        assert_eq!(mode_from(true, true), AccessMode::Offline);
        assert_eq!(mode_from(true, false), AccessMode::Offline);
        assert_eq!(mode_from(false, true), AccessMode::Remote);
        assert_eq!(mode_from(false, false), AccessMode::Default);
    }

    #[test]
    fn modes_are_distinct() {
        assert_ne!(AccessMode::Default, AccessMode::Remote);
        assert_ne!(AccessMode::Default, AccessMode::Offline);
        assert_ne!(AccessMode::Remote, AccessMode::Offline);
    }

    #[test]
    fn operation_is_copy_and_eq() {
        let q = Operation::Query;
        let q2 = q;
        assert_eq!(q, q2);
        assert_ne!(Operation::Query, Operation::Resolve);
    }
}
