// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! Config-tier errors (parse, schema, discovery, declaration hash).
//!
//! Three-layer shape: the top-level [`crate::error::Error`] wraps a
//! context-bearing [`ConfigError`] (which file failed) wrapping a
//! discriminant [`ConfigErrorKind`]. Path-less constructions
//! (`from_toml_str`) carry an empty path so the chain does not lead with
//! a bare `: ` separator.

use std::path::PathBuf;

use crate::oci::identifier::error::IdentifierError;

/// A config-tier operation failed on a specific file.
#[derive(Debug)]
pub struct ConfigError {
    /// The file the failure occurred on (empty for in-memory parses).
    pub path: PathBuf,
    /// The specific failure.
    pub kind: ConfigErrorKind,
}

impl ConfigError {
    /// Attach `path` context to `kind`.
    pub fn new(path: impl Into<PathBuf>, kind: ConfigErrorKind) -> Self {
        Self {
            path: path.into(),
            kind,
        }
    }
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.path.as_os_str().is_empty() {
            write!(f, "{}", self.kind)
        } else {
            write!(f, "{}: {}", self.path.display(), self.kind)
        }
    }
}

impl std::error::Error for ConfigError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        // `Display` already embeds the kind's message ("{path}: {kind}"), so
        // the chain skips the kind layer and exposes its underlying cause
        // directly — otherwise `{:#}` rendering would print the kind twice.
        self.kind.source()
    }
}

/// Inner discriminant for config-tier failures.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ConfigErrorKind {
    /// Failed to read the config file from disk.
    #[error("I/O error")]
    Io(#[source] std::io::Error),

    /// TOML parse failure (also fires on `deny_unknown_fields`).
    #[error("invalid TOML")]
    TomlParse(#[source] toml::de::Error),

    /// The file on disk exceeds the 64 KiB size cap.
    #[error("file too large: {size} bytes exceeds limit of {limit} bytes")]
    FileTooLarge { size: u64, limit: u64 },

    /// A `[skills]` / `[rules]` value is missing an explicit registry.
    ///
    /// Bare-tag values like `code-review = "stable"` are rejected: the
    /// declaration requires fully-qualified identifiers so resolution is
    /// reproducible regardless of `GRIM_DEFAULT_REGISTRY`.
    #[error(
        "artifact '{name}': value '{value}' is missing a registry; expected 'registry/repo:tag' (e.g. 'ghcr.io/acme/code-review:stable')"
    )]
    ArtifactValueMissingRegistry { name: String, value: String },

    /// A `[skills]` / `[rules]` value failed to parse as an identifier for
    /// a reason other than missing registry (invalid chars, malformed
    /// digest, uppercase repo). Carries the underlying error via `#[source]`.
    #[error("artifact '{name}': value '{value}' is not a valid identifier")]
    ArtifactValueInvalid {
        name: String,
        value: String,
        #[source]
        source: IdentifierError,
    },

    /// `grim init` was called where a config already exists. Surfaced so
    /// a hand-edited file is never silently overwritten.
    #[error("config already exists")]
    ConfigAlreadyExists,

    /// The lock's canonicalization-contract version is from a newer
    /// release; reading is refused rather than comparing against a hash
    /// this build would compute differently.
    #[error("unsupported declaration_hash_version {version}; this build understands version 1")]
    UnsupportedDeclarationHashVersion { version: u8 },

    /// No `grimoire.toml` was found by walking up from the working
    /// directory (project scope).
    #[error("no grimoire.toml found by walking up from the working directory")]
    NotDiscovered,

    /// A `[[registries]]` entry is malformed: an empty `url`, an empty
    /// `alias`, or a duplicate `alias` across the array.
    #[error("invalid [[registries]] entry: {reason}")]
    RegistryInvalid { reason: String },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oci::identifier::error::IdentifierErrorKind;

    #[test]
    fn display_with_path_uses_prefix_separator() {
        let err = ConfigError::new(
            PathBuf::from("/tmp/grimoire.toml"),
            ConfigErrorKind::ConfigAlreadyExists,
        );
        assert!(err.to_string().starts_with("/tmp/grimoire.toml: "));
    }

    #[test]
    fn display_without_path_omits_leading_separator() {
        let err = ConfigError::new(PathBuf::new(), ConfigErrorKind::NotDiscovered);
        let rendered = err.to_string();
        assert!(!rendered.starts_with(':'));
        assert!(!rendered.starts_with(' '));
    }

    #[test]
    fn anyhow_alternate_format_prints_kind_message_once() {
        // Regression: `Display` embeds the kind AND `source()` exposed it,
        // so `{:#}` printed "no grimoire.toml found …" twice on one line.
        let err = ConfigError::new(PathBuf::from("/w"), ConfigErrorKind::NotDiscovered);
        let any: anyhow::Error = crate::error::Error::from(err).into();
        let rendered = format!("{any:#}");
        assert_eq!(
            rendered.matches("no grimoire.toml found").count(),
            1,
            "kind message must render exactly once, got: {rendered}"
        );
    }

    #[test]
    fn artifact_value_invalid_exposes_source_once() {
        use std::error::Error;

        let ident = IdentifierError::new("bad//v", IdentifierErrorKind::InvalidFormat);
        let ident_msg = ident.to_string();
        let kind = ConfigErrorKind::ArtifactValueInvalid {
            name: "x".to_string(),
            value: "bad//v".to_string(),
            source: ident,
        };
        // The kind's own Display must NOT embed the source message.
        assert!(!kind.to_string().contains(&ident_msg));
        // But it must be reachable via source().
        assert!(kind.source().is_some());
    }
}
