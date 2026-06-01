// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! Command-tier precondition errors that do not belong to a single
//! subsystem.
//!
//! `grim install` / `grim update` enforce "a fresh lock must exist"
//! before doing any work. That precondition failure is neither a config
//! nor a lock *parse* failure — it is a workflow-state error with its own
//! exit-code mapping (missing lock ⇒ NotFound 79, stale lock ⇒ DataError
//! 65). A small dedicated error keeps the classifier exhaustive without
//! overloading the lock taxonomy.

/// A command-level precondition was not met.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CommandError {
    /// `install`/`update` requires a `grimoire.lock`, but none exists.
    #[error("no grimoire.lock found at {path}; run `grim lock` first")]
    LockMissing { path: std::path::PathBuf },

    /// The lock's declaration hash no longer matches the live config.
    #[error(
        "grimoire.lock is stale (declaration_hash {locked} does not match current {current}); run `grim lock` before installing"
    )]
    LockStale { locked: String, current: String },

    /// `search` / `tui` need a registry but none could be resolved from
    /// `--registry`, the config `default_registry` option, or
    /// `GRIM_DEFAULT_REGISTRY`.
    #[error("no registry to search; pass --registry or set GRIM_DEFAULT_REGISTRY")]
    NoRegistry,

    /// `login` / `logout` need a registry but none was given and no
    /// default is configured.
    #[error("no registry given; pass a registry argument or set GRIM_DEFAULT_REGISTRY")]
    NoLoginRegistry,

    /// `login` could not obtain a required credential input — typically a
    /// non-interactive shell missing `--username` / `--password-stdin`.
    #[error("{0}")]
    LoginInput(&'static str),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_are_actionable_and_lowercase_start() {
        let m = CommandError::LockMissing {
            path: std::path::PathBuf::from("/w/grimoire.lock"),
        };
        assert!(m.to_string().starts_with("no grimoire.lock"));
        assert!(m.to_string().contains("grim lock"));

        let s = CommandError::LockStale {
            locked: "sha256:aaa".to_string(),
            current: "sha256:bbb".to_string(),
        };
        assert!(s.to_string().contains("stale"));
        assert!(s.to_string().contains("grim lock"));
    }
}
