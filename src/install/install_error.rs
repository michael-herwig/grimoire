// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! Install-tier errors.
//!
//! Three-layer shape mirroring [`crate::config::config_error`],
//! [`crate::lock::lock_error`], and [`crate::oci::access::error`]: top
//! [`crate::error::Error`] → context-bearing [`InstallError`] (carries the
//! boxed [`ArtifactRef`] the failure is about, when one applies) →
//! discriminant [`InstallErrorKind`].

use std::io;
use std::path::PathBuf;

use crate::oci::Digest;
use crate::oci::reference::ArtifactRef;

/// An install-tier operation failed, optionally on a specific artifact.
///
/// The reference is `None` for store-wide failures (install-state I/O not
/// attributable to one artifact); it is `Some` for per-artifact failures.
#[derive(Debug)]
pub struct InstallError {
    /// The artifact the failure is about. Boxed so [`InstallErrorKind`]
    /// stays small (avoids a `clippy::result_large_err` suppression).
    pub reference: Option<Box<ArtifactRef>>,
    /// The specific failure.
    pub kind: InstallErrorKind,
}

impl InstallError {
    /// Attach `reference` context to `kind`.
    pub fn with_reference(reference: ArtifactRef, kind: InstallErrorKind) -> Self {
        Self {
            reference: Some(Box::new(reference)),
            kind,
        }
    }

    /// Construct without artifact context (store-wide failures).
    pub fn without_reference(kind: InstallErrorKind) -> Self {
        Self { reference: None, kind }
    }
}

impl std::fmt::Display for InstallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.reference {
            Some(r) => write!(f, "{} '{}' ({}): {}", r.kind, r.name, r.id, self.kind),
            None => write!(f, "{}", self.kind),
        }
    }
}

impl std::error::Error for InstallError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.kind)
    }
}

/// Inner discriminant for install-tier failures.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum InstallErrorKind {
    /// The pinned blob is absent from the registry and the cache.
    #[error("blob not found in registry or local cache")]
    BlobMissing,

    /// A previously installed artifact was modified on disk: the recorded
    /// content hash no longer matches what is on disk. Refused unless the
    /// caller forces the reinstall.
    #[error(
        "installed artifact was modified locally: recorded {recorded}, found {actual}; rerun with --force to overwrite"
    )]
    IntegrityMismatch { recorded: Digest, actual: Digest },

    /// A filesystem operation on an install target failed.
    #[error("I/O error for {path}")]
    TargetIo {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    /// The blob could not be materialized (corrupt tar, unsafe entry).
    #[error("failed to materialize artifact: {0}")]
    MaterializeFailed(String),

    /// Fetched blob bytes did not hash to the pinned digest.
    #[error("blob digest mismatch: expected {expected}, got {actual}")]
    BlobDigestMismatch { expected: Digest, actual: Digest },

    /// The configured client target is not supported by this build.
    #[error("unsupported client target '{0}'; supported clients are 'claude', 'opencode', 'copilot'")]
    UnsupportedClient(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oci::{Algorithm, ArtifactKind, Identifier};

    fn artifact_ref() -> ArtifactRef {
        ArtifactRef {
            kind: ArtifactKind::Skill,
            name: "code-review".to_string(),
            id: Identifier::parse("ghcr.io/acme/code-review:stable").unwrap(),
        }
    }

    #[test]
    fn display_with_reference_uses_prefix() {
        let err = InstallError::with_reference(artifact_ref(), InstallErrorKind::BlobMissing);
        let s = err.to_string();
        assert!(s.contains("skill"));
        assert!(s.contains("code-review"));
        assert!(s.contains("blob not found"));
    }

    #[test]
    fn display_without_reference_no_leading_separator() {
        let err = InstallError::without_reference(InstallErrorKind::UnsupportedClient("vscode".to_string()));
        assert!(!err.to_string().starts_with(':'));
        assert!(!err.to_string().starts_with(' '));
        assert!(err.to_string().contains("vscode"));
    }

    #[test]
    fn integrity_mismatch_renders_both_digests() {
        let recorded = Algorithm::Sha256.hash(b"a");
        let actual = Algorithm::Sha256.hash(b"b");
        let kind = InstallErrorKind::IntegrityMismatch {
            recorded: recorded.clone(),
            actual: actual.clone(),
        };
        let s = kind.to_string();
        assert!(s.contains(&recorded.to_string()));
        assert!(s.contains(&actual.to_string()));
    }

    #[test]
    fn source_chain_reaches_kind() {
        use std::error::Error;
        let err = InstallError::with_reference(artifact_ref(), InstallErrorKind::BlobMissing);
        assert!(err.source().is_some());
    }
}
