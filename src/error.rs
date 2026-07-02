// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! Top-level error type and the error → exit-code classifier.
//!
//! [`classify_error`] is a free function (not a trait method) so the
//! dependency direction stays clean: errors do not depend on the exit-code
//! taxonomy. It walks the `anyhow` chain, downcasts to [`Error`], and maps
//! each known kind to a typed [`ExitCode`].

use crate::auth::auth_error::AuthError;
use crate::catalog::catalog_error::{CatalogError, CatalogErrorKind};
use crate::catalog::index_announce::AnnounceError;
use crate::cli::exit_code::ExitCode;
use crate::command::command_error::CommandError;
use crate::config::config_error::{ConfigError, ConfigErrorKind};
use crate::install::install_error::{InstallError, InstallErrorKind};
use crate::install::path_anchor::AnchorError;
use crate::lock::lock_error::{LockError, LockErrorKind};
use crate::oci::access::error::{AccessError, AccessErrorKind};
use crate::oci::digest::error::DigestError;
use crate::oci::identifier::error::IdentifierError;
use crate::oci::pinned_identifier::PinnedIdentifierError;
use crate::oci::release::{ReleaseError, ReleaseErrorKind};
use crate::resolve::resolve_error::{ResolveError, ResolveErrorKind};
use crate::skill::skill_error::{SkillError, SkillErrorKind};

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

    #[error(transparent)]
    Config(#[from] ConfigError),

    #[error(transparent)]
    Lock(#[from] LockError),

    #[error(transparent)]
    Access(#[from] AccessError),

    #[error(transparent)]
    Resolve(#[from] ResolveError),

    #[error(transparent)]
    Install(#[from] InstallError),

    #[error(transparent)]
    Anchor(#[from] AnchorError),

    #[error(transparent)]
    Skill(#[from] SkillError),

    #[error(transparent)]
    Release(#[from] ReleaseError),

    #[error(transparent)]
    Catalog(#[from] CatalogError),

    #[error(transparent)]
    Auth(#[from] AuthError),

    #[error(transparent)]
    Command(#[from] CommandError),

    #[error(transparent)]
    Announce(#[from] AnnounceError),
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
                Error::Config(ce) => classify_config(ce),
                Error::Lock(le) => classify_lock(le),
                Error::Access(ae) => classify_access(ae),
                Error::Resolve(re) => classify_resolve(re),
                Error::Install(ie) => classify_install(ie),
                Error::Anchor(ae) => classify_anchor(ae),
                Error::Skill(se) => classify_skill(se),
                Error::Release(re) => classify_release(re),
                Error::Catalog(ce) => classify_catalog(ce),
                Error::Auth(ae) => classify_auth(ae),
                Error::Command(ce) => match ce {
                    CommandError::LockMissing { .. } => ExitCode::NotFound,
                    CommandError::LockStale { .. } => ExitCode::DataError,
                    CommandError::NoLoginRegistry => ExitCode::ConfigError,
                    CommandError::LoginInput(_) => ExitCode::UsageError,
                    CommandError::KindInferenceFailed { .. } => ExitCode::DataError,
                    CommandError::ConfigUsage(_) => ExitCode::UsageError,
                    CommandError::ConfigValue(_) => ExitCode::DataError,
                },
                // Announce needs remote resources (the index repository, the
                // GitHub API); a local I/O fault classifies as I/O.
                Error::Announce(ae) => match ae {
                    AnnounceError::Io(io) => classify_io(io),
                    AnnounceError::Git { .. } | AnnounceError::OwnerLookup { .. } => ExitCode::Unavailable,
                },
            };
        }
    }
    ExitCode::Failure
}

/// Map a config-tier error to an exit code.
fn classify_config(err: &ConfigError) -> ExitCode {
    match &err.kind {
        ConfigErrorKind::TomlParse(_)
        | ConfigErrorKind::FileTooLarge { .. }
        | ConfigErrorKind::UnsupportedDeclarationHashVersion { .. }
        | ConfigErrorKind::RegistryInvalid { .. }
        | ConfigErrorKind::TreeSeparatorInvalid { .. } => ExitCode::ConfigError,
        ConfigErrorKind::NotDiscovered => ExitCode::NotFound,
        ConfigErrorKind::ArtifactValueMissingRegistry { .. } | ConfigErrorKind::ArtifactValueInvalid { .. } => {
            ExitCode::DataError
        }
        ConfigErrorKind::ConfigAlreadyExists => ExitCode::UsageError,
        ConfigErrorKind::Io(io) => classify_io(io),
    }
}

/// Map a lock-tier error to an exit code.
fn classify_lock(err: &LockError) -> ExitCode {
    match &err.kind {
        LockErrorKind::Locked => ExitCode::TempFail,
        LockErrorKind::TomlParse(_)
        | LockErrorKind::TomlSerialize(_)
        | LockErrorKind::FileTooLarge { .. }
        | LockErrorKind::UnsupportedVersion { .. } => ExitCode::ConfigError,
        LockErrorKind::StaleLockOnPartial { .. } => ExitCode::DataError,
        LockErrorKind::Io(io) => classify_io(io),
    }
}

/// Map an OCI-access-tier error to an exit code.
fn classify_access(err: &AccessError) -> ExitCode {
    match &err.kind {
        AccessErrorKind::Authentication(_) => ExitCode::AuthError,
        AccessErrorKind::Registry(_) => ExitCode::Unavailable,
        AccessErrorKind::OfflineMiss => ExitCode::OfflineBlocked,
        AccessErrorKind::ManifestNotFound | AccessErrorKind::BlobNotFound => ExitCode::NotFound,
        AccessErrorKind::DigestMismatch { .. } | AccessErrorKind::InvalidManifest(_) => ExitCode::DataError,
        AccessErrorKind::Io { source, .. } => classify_io(source),
    }
}

/// Map a resolution-tier error to an exit code.
fn classify_resolve(err: &ResolveError) -> ExitCode {
    match &err.kind {
        ResolveErrorKind::TagNotFound | ResolveErrorKind::BundleNotFound => ExitCode::NotFound,
        ResolveErrorKind::AuthFailure(_) => ExitCode::AuthError,
        ResolveErrorKind::RegistryUnreachable(_) | ResolveErrorKind::ResolveTimeout => ExitCode::Unavailable,
        ResolveErrorKind::StaleLock { .. } | ResolveErrorKind::BundleInvalid(_) => ExitCode::DataError,
        // A bundle conflict is a misconfiguration of the user's own
        // declaration (two bundles disagree), not malformed external data.
        ResolveErrorKind::BundleConflict { .. } => ExitCode::ConfigError,
    }
}

/// Map an install-tier error to an exit code.
fn classify_install(err: &InstallError) -> ExitCode {
    match &err.kind {
        InstallErrorKind::BlobMissing => ExitCode::NotFound,
        InstallErrorKind::IntegrityMismatch { .. }
        | InstallErrorKind::BlobDigestMismatch { .. }
        | InstallErrorKind::MaterializeFailed(_) => ExitCode::DataError,
        InstallErrorKind::TargetIo { source, .. } => classify_io(source),
        InstallErrorKind::UnsupportedClient(_) => ExitCode::ConfigError,
    }
}

/// Map an anchor-tier error to an exit code. A traversal/escape is bad
/// on-disk state data (65); an I/O failure is I/O (74); an unclassifiable or
/// unresolvable anchor falls through to the generic failure (1).
fn classify_anchor(err: &AnchorError) -> ExitCode {
    match err {
        AnchorError::TraversalAttempt { .. } | AnchorError::EscapedAnchor { .. } => ExitCode::DataError,
        AnchorError::Io { .. } => ExitCode::IoError,
        AnchorError::UnknownAnchor { .. } | AnchorError::AnchorRootAbsent { .. } => ExitCode::Failure,
    }
}

/// Map a skill-standard-tier error to an exit code. A spec/parse/mismatch
/// failure is bad input data (65); an I/O failure is I/O / NoPermission.
fn classify_skill(err: &SkillError) -> ExitCode {
    match &err.kind {
        SkillErrorKind::MissingSkillMd
        | SkillErrorKind::NameMismatch { .. }
        | SkillErrorKind::NameInvalid(_)
        | SkillErrorKind::DescriptionInvalid(_)
        | SkillErrorKind::FrontmatterParse(_)
        | SkillErrorKind::MissingFrontmatter
        | SkillErrorKind::MetadataInvalid(_)
        | SkillErrorKind::ValidationFailed(_)
        | SkillErrorKind::GitProvenance(_) => ExitCode::DataError,
        SkillErrorKind::Io(io) => classify_io(io),
    }
}

/// Map a release-tier error to an exit code. A bad version, a missing tag,
/// or a refused tag overwrite is a data error (65).
fn classify_release(err: &ReleaseError) -> ExitCode {
    match &err.kind {
        ReleaseErrorKind::InvalidVersion { .. } | ReleaseErrorKind::MissingTag | ReleaseErrorKind::TagExists { .. } => {
            ExitCode::DataError
        }
    }
}

/// Map a catalog-tier error to an exit code. A parse / unknown-version
/// failure is bad on-disk data (65); an I/O failure is I/O / NoPermission;
/// an OCI-access failure delegates to the access classifier.
fn classify_catalog(err: &CatalogError) -> ExitCode {
    match &err.kind {
        CatalogErrorKind::Parse(_) | CatalogErrorKind::UnsupportedVersion { .. } => ExitCode::DataError,
        CatalogErrorKind::Io(io) => classify_io(io),
        CatalogErrorKind::Access(ae) => classify_access(ae),
        // An index fetch failure is a remote-resource fault: the index
        // host is unreachable or served a non-success status.
        CatalogErrorKind::IndexFetch { .. } => ExitCode::Unavailable,
    }
}

/// Map an auth-tier error to an exit code. Store I/O delegates to the I/O
/// classifier; malformed on-disk config is bad data (65); a missing store
/// or config location is a configuration problem (78); helper failures map
/// per the underlying `docker_credential` error kind.
fn classify_auth(err: &AuthError) -> ExitCode {
    use docker_credential::CredentialRetrievalError as Helper;
    match err {
        AuthError::StoreIo { source, .. } => classify_io(source),
        AuthError::MalformedConfig { .. } => ExitCode::DataError,
        AuthError::NoCredentialStore | AuthError::NoConfigLocation => ExitCode::ConfigError,
        AuthError::HelperFailed { .. } => ExitCode::AuthError,
        AuthError::Helper(inner) => match inner {
            Helper::NotOnPath { .. } | Helper::UnsafePath { .. } => ExitCode::ConfigError,
            Helper::Timeout { .. } => ExitCode::TempFail,
            Helper::InvalidJson(_) | Helper::MalformedHelperResponse | Helper::CredentialDecodingError => {
                ExitCode::DataError
            }
            Helper::HelperCommunicationError => ExitCode::IoError,
            // OutputTooLarge / HelperFailure / the config-miss sentinels are
            // all treated as auth failures (the miss variants are never
            // wrapped in `Helper` — `map_helper_err`/`get_blocking` divert
            // them — but the arm keeps the match total).
            _ => ExitCode::AuthError,
        },
    }
}

/// `PermissionDenied` → `NoPermission` (77); any other I/O → `IoError` (74).
fn classify_io(io: &std::io::Error) -> ExitCode {
    if io.kind() == std::io::ErrorKind::PermissionDenied {
        ExitCode::NoPermission
    } else {
        ExitCode::IoError
    }
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
    fn auth_errors_classify_per_kind() {
        let cases = [
            (
                AuthError::StoreIo {
                    path: std::path::PathBuf::from("/x/config.json"),
                    source: std::io::Error::other("disk full"),
                },
                ExitCode::IoError,
            ),
            (
                AuthError::StoreIo {
                    path: std::path::PathBuf::from("/x/config.json"),
                    source: std::io::Error::from(std::io::ErrorKind::PermissionDenied),
                },
                ExitCode::NoPermission,
            ),
            (
                AuthError::MalformedConfig {
                    path: std::path::PathBuf::from("/x/config.json"),
                    source: serde_json::from_str::<serde_json::Value>("{").expect_err("must err"),
                },
                ExitCode::DataError,
            ),
            (AuthError::NoCredentialStore, ExitCode::ConfigError),
            (AuthError::NoConfigLocation, ExitCode::ConfigError),
            (
                AuthError::HelperFailed {
                    helper: "test".to_string(),
                },
                ExitCode::AuthError,
            ),
        ];
        for (inner, expected) in cases {
            let err: anyhow::Error = Error::from(inner).into();
            assert_eq!(classify_error(&err), expected);
        }
    }

    #[test]
    fn helper_error_kinds_classify_per_variant() {
        use docker_credential::CredentialRetrievalError as Helper;
        let cases = [
            (Helper::NotOnPath { name: "x".into() }, ExitCode::ConfigError),
            (Helper::Timeout { seconds: 30 }, ExitCode::TempFail),
            (Helper::MalformedHelperResponse, ExitCode::DataError),
            (Helper::CredentialDecodingError, ExitCode::DataError),
            (Helper::HelperCommunicationError, ExitCode::IoError),
        ];
        for (helper, expected) in cases {
            let err: anyhow::Error = Error::from(AuthError::Helper(helper)).into();
            assert_eq!(classify_error(&err), expected);
        }
    }

    #[test]
    fn command_login_errors_classify_per_kind() {
        let no_registry: anyhow::Error = Error::from(CommandError::NoLoginRegistry).into();
        assert_eq!(classify_error(&no_registry), ExitCode::ConfigError);

        let usage: anyhow::Error = Error::from(CommandError::LoginInput("bad input")).into();
        assert_eq!(classify_error(&usage), ExitCode::UsageError);
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
    fn install_errors_classify_per_kind() {
        use crate::install::install_error::{InstallError, InstallErrorKind};

        let cases = [
            (InstallErrorKind::BlobMissing, ExitCode::NotFound),
            (
                InstallErrorKind::IntegrityMismatch {
                    recorded: Digest::Sha256("a".repeat(64)),
                    actual: Digest::Sha256("b".repeat(64)),
                },
                ExitCode::DataError,
            ),
            (
                InstallErrorKind::MaterializeFailed("bad tar".to_string()),
                ExitCode::DataError,
            ),
            (
                InstallErrorKind::UnsupportedClient("vscode".to_string()),
                ExitCode::ConfigError,
            ),
            (
                InstallErrorKind::TargetIo {
                    path: std::path::PathBuf::from("/x"),
                    source: std::io::Error::other("disk full"),
                },
                ExitCode::IoError,
            ),
        ];
        for (kind, expected) in cases {
            let err: anyhow::Error = Error::from(InstallError::without_reference(kind)).into();
            assert_eq!(classify_error(&err), expected);
        }
    }

    #[test]
    fn anchor_errors_classify_per_kind() {
        use crate::install::path_anchor::{AnchorError, PathAnchor};

        let cases = [
            (
                AnchorError::TraversalAttempt {
                    relative: "../escape".to_string(),
                },
                ExitCode::DataError,
            ),
            (
                AnchorError::EscapedAnchor {
                    anchor: PathAnchor::Workspace,
                    resolved: std::path::PathBuf::from("/outside"),
                },
                ExitCode::DataError,
            ),
            (
                AnchorError::Io {
                    path: std::path::PathBuf::from("/x"),
                    source: std::io::Error::other("disk full"),
                },
                ExitCode::IoError,
            ),
            (
                AnchorError::UnknownAnchor {
                    path: std::path::PathBuf::from("/other/path"),
                },
                ExitCode::Failure,
            ),
            (
                AnchorError::AnchorRootAbsent {
                    anchor: PathAnchor::ClaudeRoot,
                },
                ExitCode::Failure,
            ),
        ];
        for (inner, expected) in cases {
            let err: anyhow::Error = Error::from(inner).into();
            assert_eq!(classify_error(&err), expected);
        }
    }

    #[test]
    fn skill_git_provenance_error_classifies_as_data_error() {
        use crate::oci::git_provenance::GitProvenanceError;
        // The `--git` opt-in surfaces a missing `git` as a path-attributed
        // SkillError; it must classify as a DataError (65), never a generic
        // failure — the user explicitly asked for provenance.
        let inner = SkillError::new(
            "/w/skill",
            SkillErrorKind::GitProvenance(GitProvenanceError::GitNotFound),
        );
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
