// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! OCI domain types: identifiers, digests, pinned identifiers, and the
//! artifact kind/reference model.

pub mod access;
pub mod artifact_kind;
pub mod digest;
pub mod identifier;
pub mod manifest;
pub mod pinned_identifier;
pub mod reference;
pub mod tag_cache;

// Convenience re-exports for the subsystems landing in Phases 2–6.
// Unused until those call sites exist (see the crate-level Phase 1 note).
#[allow(unused_imports)]
pub use artifact_kind::ArtifactKind;
#[allow(unused_imports)]
pub use digest::{Algorithm, Digest};
#[allow(unused_imports)]
pub use identifier::Identifier;
#[allow(unused_imports)]
pub use pinned_identifier::PinnedIdentifier;
#[allow(unused_imports)]
pub use reference::ArtifactRef;
