// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! Top-level error type and the error → exit-code classifier.
//!
//! [`classify_error`] is a free function (not a trait method) so the
//! dependency direction stays clean: errors do not depend on the exit-code
//! taxonomy. It walks the `anyhow` chain, downcasts to [`Error`], and maps
//! each known kind to a typed [`ExitCode`].

use crate::cli::exit_code::ExitCode;
use crate::oci::digest::error::DigestError;
use crate::oci::identifier::error::IdentifierError;
use crate::oci::pinned_identifier::PinnedIdentifierError;

/// Top-level Grimoire error. Subsystem errors compose in via `#[from]`.
///
/// `#[error(transparent)]` on every arm: there is nothing to add at this
/// layer — the inner error already carries the full message and source.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    #[error(transparent)]
    Identifier(#[from] IdentifierError),

    #[error(transparent)]
    Digest(#[from] DigestError),

    #[error(transparent)]
    PinnedIdentifier(#[from] PinnedIdentifierError),
}

/// Maps an error chain to a process exit code.
///
/// Walks `err.chain()`, downcasts each cause to [`Error`], and
/// exhaustively maps every Phase 1 variant. Anything not classified falls
/// through to [`ExitCode::Failure`]; the fall-through is locked by a test
/// so it cannot silently change.
pub fn classify_error(err: &anyhow::Error) -> ExitCode {
    for cause in err.chain() {
        if let Some(e) = cause.downcast_ref::<Error>() {
            // Exhaustive match: a new variant fails to compile until it is
            // explicitly classified here.
            return match e {
                Error::Identifier(_) => ExitCode::DataError,
                Error::Digest(_) => ExitCode::DataError,
                Error::PinnedIdentifier(_) => ExitCode::DataError,
            };
        }
    }
    ExitCode::Failure
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oci::Identifier;
    use crate::oci::digest::Digest;
    use crate::oci::identifier::error::IdentifierErrorKind;
    use crate::oci::pinned_identifier::PinnedIdentifier;

    #[test]
    fn identifier_error_classifies_as_data_error() {
        let inner = IdentifierError::new("bad", IdentifierErrorKind::MissingRegistry);
        let err: anyhow::Error = Error::from(inner).into();
        assert_eq!(classify_error(&err), ExitCode::DataError);
    }

    #[test]
    fn digest_error_classifies_as_data_error() {
        let inner = DigestError::Invalid("nope".to_string());
        let err: anyhow::Error = Error::from(inner).into();
        assert_eq!(classify_error(&err), ExitCode::DataError);
    }

    #[test]
    fn pinned_identifier_error_classifies_as_data_error() {
        let id = Identifier::new_registry("cmake", "example.com");
        let inner = PinnedIdentifier::try_from(id).unwrap_err();
        let err: anyhow::Error = Error::from(inner).into();
        assert_eq!(classify_error(&err), ExitCode::DataError);
    }

    #[test]
    fn classification_survives_anyhow_context_layers() {
        let inner = DigestError::Invalid("nope".to_string());
        let err = anyhow::Error::from(Error::from(inner)).context("while resolving lock");
        assert_eq!(classify_error(&err), ExitCode::DataError);
    }

    #[test]
    fn unclassified_error_falls_through_to_failure() {
        // Locks the documented v1 fall-through behaviour: any error that is
        // not a Grimoire `Error` maps to Failure (1), never a semantic code.
        let err = anyhow::anyhow!("some unrelated failure");
        assert_eq!(classify_error(&err), ExitCode::Failure);

        // A bare std::io::Error is also unclassified in Phase 1.
        let io = std::io::Error::other("disk gone");
        let err: anyhow::Error = io.into();
        assert_eq!(classify_error(&err), ExitCode::Failure);
    }

    #[test]
    fn from_impls_round_trip_into_top_level_error() {
        let _: Error = DigestError::Invalid("x".into()).into();
        let _: Error = IdentifierError::new("x", IdentifierErrorKind::Empty).into();
        let id = Identifier::new_registry("c", "e");
        let _: Error = PinnedIdentifier::try_from(id).unwrap_err().into();
        // Smoke: the Digest type stays reachable through the error module's
        // re-export path used by callers building pinned identifiers.
        let _ = Digest::Sha256("a".repeat(64));
    }
}
