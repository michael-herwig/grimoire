// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! Login / logout operations — the seam between the CLI commands and the
//! credential store.
//!
//! Kept as free functions (not methods on the store trait) so the trait
//! stays the three minimal protocol verbs. The registry argument is
//! canonicalized by [`CredentialStore`] implementations themselves, so
//! these wrappers pass it straight through.
//!
//! v1 does not verify the credential against the registry before storing
//! it (matching `docker login` with a credential helper, which also stores
//! optimistically). The function shape leaves room for a future
//! `--verify` pre-flight without changing call sites.

use crate::auth::auth_error::AuthError;
use crate::auth::credential::Credential;
use crate::auth::store::CredentialStore;

/// Persist `cred` for `registry` via `store`.
///
/// # Errors
///
/// Any [`AuthError`] the store raises while writing.
pub async fn login(registry: &str, cred: &Credential, store: &dyn CredentialStore) -> Result<(), AuthError> {
    store.put(registry, cred).await
}

/// Remove any stored credential for `registry` via `store`. Idempotent:
/// `Ok(())` whether or not one was present.
///
/// # Errors
///
/// Any [`AuthError`] the store raises while erasing.
pub async fn logout(registry: &str, store: &dyn CredentialStore) -> Result<(), AuthError> {
    store.delete(registry).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use secrecy::SecretString;
    use std::sync::Mutex;

    #[derive(Default)]
    struct MockStore {
        puts: Mutex<Vec<String>>,
        deletes: Mutex<Vec<String>>,
    }

    #[async_trait::async_trait]
    impl CredentialStore for MockStore {
        async fn get(&self, _registry: &str) -> Result<Option<Credential>, AuthError> {
            Ok(None)
        }
        async fn put(&self, registry: &str, _cred: &Credential) -> Result<(), AuthError> {
            self.puts.lock().unwrap().push(registry.to_string());
            Ok(())
        }
        async fn delete(&self, registry: &str) -> Result<(), AuthError> {
            self.deletes.lock().unwrap().push(registry.to_string());
            Ok(())
        }
    }

    #[tokio::test]
    async fn login_invokes_store_put() {
        let store = MockStore::default();
        let cred = Credential::basic("u", SecretString::from("p"));
        login("ghcr.io", &cred, &store).await.expect("login");
        assert_eq!(store.puts.lock().unwrap().as_slice(), &["ghcr.io".to_string()]);
    }

    #[tokio::test]
    async fn logout_invokes_store_delete() {
        let store = MockStore::default();
        logout("ghcr.io", &store).await.expect("logout");
        assert_eq!(store.deletes.lock().unwrap().as_slice(), &["ghcr.io".to_string()]);
    }
}
