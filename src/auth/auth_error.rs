// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! Authentication-tier error type.
//!
//! Composes into the top-level [`crate::error::Error`] via `#[from]`;
//! [`crate::error::classify_error`] maps each variant to an [`ExitCode`].
//! Messages follow the Rust API Guidelines (lowercase, no trailing
//! punctuation) so they read cleanly in an `anyhow` `{:#}` chain.
//!
//! [`ExitCode`]: crate::cli::exit_code::ExitCode

use std::path::PathBuf;

/// An error raised while reading or writing registry credentials.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AuthError {
    /// Reading or writing `~/.docker/config.json` failed.
    #[error("failed to access credential store at {path}")]
    StoreIo {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// The on-disk `config.json` was not valid JSON.
    #[error("credential store at {path} is not valid JSON")]
    MalformedConfig {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },

    /// A credential-helper subprocess failed.
    #[error("credential helper failed")]
    Helper(#[source] docker_credential::CredentialRetrievalError),

    /// A credential helper exited non-zero. Distinct from [`Self::Helper`]
    /// because the underlying `HelperFailure` carries the helper's raw
    /// stdout/stderr (which can contain credentials) in its `Display`; this
    /// variant drops that payload so it never reaches a log line (CWE-532).
    #[error("credential helper '{helper}' exited non-zero")]
    HelperFailed { helper: String },

    /// No credential store is available: no helper is configured and the
    /// user did not opt into the plaintext fallback.
    #[error("no credential store available; configure a docker credential helper or pass --allow-insecure-store")]
    NoCredentialStore,

    /// The docker config location could not be resolved (no `$HOME` and no
    /// `$DOCKER_CONFIG`).
    #[error("cannot locate docker config; set $DOCKER_CONFIG or $HOME")]
    NoConfigLocation,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_are_lowercase_without_trailing_period() {
        for msg in [
            AuthError::NoCredentialStore.to_string(),
            AuthError::NoConfigLocation.to_string(),
        ] {
            let first = msg.chars().next().unwrap();
            assert!(first.is_lowercase(), "message must start lowercase: {msg:?}");
            assert!(!msg.ends_with('.'), "message must not end with a period: {msg:?}");
        }
    }
}
