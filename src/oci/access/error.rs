// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! OCI-access-tier errors.
//!
//! Three-layer shape mirroring [`crate::config::config_error`] and
//! [`crate::lock::lock_error`]: top [`crate::error::Error`] →
//! context-bearing [`AccessError`] (carries the identifier the failure
//! happened on) → discriminant [`AccessErrorKind`].
//!
//! `Option::None` is "not found" at this layer (see arch-principles
//! "Option-based lookups"); an `Err` only ever means transport, auth, or
//! a data/integrity failure — never a benign miss.

use std::io;
use std::path::PathBuf;

use crate::oci::{Digest, Identifier};

/// An OCI-access operation failed, optionally on a specific identifier.
///
/// The identifier is `None` for registry-wide operations (catalog
/// listing) where no single artifact context applies.
#[derive(Debug)]
pub struct AccessError {
    /// The artifact identifier the failure occurred on, when applicable.
    pub identifier: Option<Identifier>,
    /// The specific failure.
    pub kind: AccessErrorKind,
}

impl AccessError {
    /// Attach `identifier` context to `kind`.
    pub fn with_identifier(identifier: Identifier, kind: AccessErrorKind) -> Self {
        Self {
            identifier: Some(identifier),
            kind,
        }
    }

    /// Construct without identifier context (registry-wide operations).
    pub fn without_identifier(kind: AccessErrorKind) -> Self {
        Self { identifier: None, kind }
    }
}

impl std::fmt::Display for AccessError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.identifier {
            Some(id) => write!(f, "{id}: {}", self.kind),
            None => write!(f, "{}", self.kind),
        }
    }
}

impl std::error::Error for AccessError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.kind)
    }
}

/// Inner discriminant for OCI-access-tier failures.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AccessErrorKind {
    /// A registry operation failed (network, 5xx, malformed response).
    #[error("registry operation failed")]
    Registry(#[source] Box<dyn std::error::Error + Send + Sync>),

    /// The registry rejected the request for authentication reasons
    /// (401/403 or an equivalent policy denial). Terminal — not retried.
    #[error("registry authentication failed")]
    Authentication(#[source] Box<dyn std::error::Error + Send + Sync>),

    /// The requested manifest does not exist on the registry.
    #[error("manifest not found")]
    ManifestNotFound,

    /// The requested blob does not exist on the registry.
    #[error("blob not found")]
    BlobNotFound,

    /// A manifest was fetched but could not be parsed into the Grimoire
    /// subset (e.g. an image index where an image manifest was expected).
    #[error("invalid manifest: {0}")]
    InvalidManifest(String),

    /// Fetched blob bytes did not hash to the requested digest.
    #[error("digest mismatch: expected {expected}, got {actual}")]
    DigestMismatch { expected: Digest, actual: Digest },

    /// A local I/O error (tag cache, blob store) with optional path.
    #[error("I/O error{}", .path.as_ref().map(|p| format!(" for {}", p.display())).unwrap_or_default())]
    Io {
        path: Option<PathBuf>,
        #[source]
        source: io::Error,
    },

    /// Offline mode blocked a network operation that the cache could not
    /// satisfy. Deliberate policy, not a fault.
    #[error("offline mode blocked a required network operation")]
    OfflineMiss,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_with_identifier_uses_prefix() {
        let id = Identifier::parse("ghcr.io/acme/code-review:stable").unwrap();
        let err = AccessError::with_identifier(id, AccessErrorKind::ManifestNotFound);
        assert!(err.to_string().starts_with("ghcr.io/acme/code-review:stable: "));
    }

    #[test]
    fn display_without_identifier_no_leading_separator() {
        let err = AccessError::without_identifier(AccessErrorKind::OfflineMiss);
        assert!(!err.to_string().starts_with(':'));
        assert!(!err.to_string().starts_with(' '));
    }

    #[test]
    fn io_message_includes_path_when_present() {
        let err = AccessErrorKind::Io {
            path: Some(PathBuf::from("/grim/tags/x.json")),
            source: io::Error::other("boom"),
        };
        assert!(err.to_string().contains("/grim/tags/x.json"));
    }

    #[test]
    fn source_chain_reaches_kind() {
        use std::error::Error;
        let id = Identifier::parse("ghcr.io/acme/x:1").unwrap();
        let err = AccessError::with_identifier(id, AccessErrorKind::OfflineMiss);
        assert!(err.source().is_some());
    }
}
