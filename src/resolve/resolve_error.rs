// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! Resolution-tier errors.
//!
//! Three-layer shape: top [`crate::error::Error`] → context-bearing
//! [`ResolveError`] (carries the boxed [`ArtifactRef`] the failure is
//! about — boxed to keep the kind small, mirroring OCX's precedent) →
//! discriminant [`ResolveErrorKind`].

use crate::oci::access::error::AccessError;
use crate::oci::reference::ArtifactRef;

/// A resolution failed for one declared artifact.
#[derive(Debug)]
pub struct ResolveError {
    /// The artifact the failure is about. Boxed so [`ResolveErrorKind`]
    /// stays small (avoids a `clippy::result_large_err` suppression).
    pub reference: Box<ArtifactRef>,
    /// The specific failure.
    pub kind: ResolveErrorKind,
}

impl ResolveError {
    /// Construct from an artifact reference and a failure kind.
    pub fn new(reference: ArtifactRef, kind: ResolveErrorKind) -> Self {
        Self {
            reference: Box::new(reference),
            kind,
        }
    }
}

impl std::fmt::Display for ResolveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} '{}' ({}): {}",
            self.reference.kind, self.reference.name, self.reference.id, self.kind
        )
    }
}

impl std::error::Error for ResolveError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.kind)
    }
}

/// Inner discriminant for resolution-tier failures.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ResolveErrorKind {
    /// The declared tag does not exist on the registry (`Ok(None)` from
    /// the access layer). Not retried.
    #[error("tag not found")]
    TagNotFound,

    /// The registry rejected the request for authentication reasons.
    /// Terminal — not retried.
    #[error("authentication failed")]
    AuthFailure(#[source] AccessError),

    /// The registry was unreachable after exhausting the retry budget, or
    /// an offline miss blocked the resolve.
    #[error("registry unreachable")]
    RegistryUnreachable(#[source] AccessError),

    /// Resolution for one artifact exceeded the per-artifact timeout.
    #[error("resolve timed out")]
    ResolveTimeout,

    /// Partial-resolve refused: the predecessor lock's declaration hash
    /// does not match the current declaration. Both are surfaced so an
    /// operator can diff the lock against the live config.
    #[error(
        "partial-resolve refused: lock declaration_hash {previous_hash} does not match current {current_hash}; retry with a full resolve"
    )]
    StaleLock {
        previous_hash: String,
        current_hash: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oci::{ArtifactKind, Identifier};

    fn artifact_ref() -> ArtifactRef {
        ArtifactRef {
            kind: ArtifactKind::Skill,
            name: "code-review".to_string(),
            id: Identifier::parse("ghcr.io/acme/code-review:stable").unwrap(),
        }
    }

    #[test]
    fn display_includes_artifact_context() {
        let err = ResolveError::new(artifact_ref(), ResolveErrorKind::TagNotFound);
        let s = err.to_string();
        assert!(s.contains("skill"));
        assert!(s.contains("code-review"));
        assert!(s.contains("tag not found"));
    }

    #[test]
    fn source_chain_reaches_kind() {
        use std::error::Error;
        let err = ResolveError::new(artifact_ref(), ResolveErrorKind::ResolveTimeout);
        assert!(err.source().is_some());
    }
}
