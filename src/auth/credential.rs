// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! The in-memory credential a `grim login` collects and the store
//! persists.
//!
//! Secret fields are wrapped in [`SecretString`] so they are redacted from
//! `Debug` output and zeroized on drop — a `grim` panic or a stray
//! `tracing` line can never leak a token. The struct mirrors the docker
//! credential-helper wire shape (`{username, secret, identitytoken}`):
//!
//! - `username` + `password` → HTTP Basic
//! - `identity_token` alone → OAuth2 refresh token (wire `identitytoken`)

use secrecy::{ExposeSecret as _, SecretString};

/// A registry credential, secrets redacted in `Debug`.
#[derive(Debug, Default)]
pub struct Credential {
    /// Account name. Empty for an identity-token-only credential.
    pub username: String,
    /// Basic-auth secret. Empty when an identity token is used instead.
    pub password: SecretString,
    /// OAuth2 identity (refresh) token. Empty for basic auth.
    pub identity_token: SecretString,
}

impl Credential {
    /// Construct a basic-auth credential (the common `grim login` case).
    pub fn basic(username: impl Into<String>, password: SecretString) -> Self {
        Self {
            username: username.into(),
            password,
            identity_token: SecretString::default(),
        }
    }

    /// Construct an identity-token credential (OAuth2 refresh).
    pub fn identity_token(token: SecretString) -> Self {
        Self {
            username: String::new(),
            password: SecretString::default(),
            identity_token: token,
        }
    }

    /// True when every field is empty.
    pub fn is_empty(&self) -> bool {
        self.username.is_empty()
            && self.password.expose_secret().is_empty()
            && self.identity_token.expose_secret().is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_populates_username_and_password() {
        let c = Credential::basic("alice", SecretString::from("hunter2"));
        assert_eq!(c.username, "alice");
        assert_eq!(c.password.expose_secret(), "hunter2");
        assert!(c.identity_token.expose_secret().is_empty());
        assert!(!c.is_empty());
    }

    #[test]
    fn identity_token_leaves_basic_fields_empty() {
        let c = Credential::identity_token(SecretString::from("tok"));
        assert!(c.username.is_empty());
        assert!(c.password.expose_secret().is_empty());
        assert_eq!(c.identity_token.expose_secret(), "tok");
    }

    #[test]
    fn default_is_empty() {
        assert!(Credential::default().is_empty());
    }

    #[test]
    fn debug_redacts_secret_fields() {
        let c = Credential::basic("alice", SecretString::from("DO_NOT_LEAK"));
        let dbg = format!("{c:?}");
        assert!(!dbg.contains("DO_NOT_LEAK"), "Debug leaked the secret: {dbg}");
    }
}
