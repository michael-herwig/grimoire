// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! Docker-compatible credential store over `~/.docker/config.json`.
//!
//! `grim` already *reads* credentials through this file in
//! `oci::access::registry_client`; this is the *write* half that
//! `grim login` / `grim logout` need. Resolution order matches docker /
//! oras:
//! 1. `credHelpers[registry]` — per-registry helper (highest priority)
//! 2. `credsStore` — global default helper
//! 3. `auths[registry]` — plaintext base64 fallback (gated by
//!    `allow_plaintext_put`, lowest priority)
//!
//! Helper store/erase go through the patched `docker_credential` fork.
//! Plaintext writes reuse the crate-wide [`atomic_write`] primitive (then
//! tighten the mode to `0600` — a credentials file must never inherit the
//! `0644` cap `atomic_write` applies to ordinary state files). Concurrent
//! `grim` writers serialize through [`ConfigFileLock`]; unknown JSON keys
//! written by `docker` / `oras` round-trip untouched.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use secrecy::zeroize::Zeroizing;
use secrecy::{ExposeSecret as _, SecretString};
use serde::{Deserialize, Serialize};

use crate::auth::auth_error::AuthError;
use crate::auth::credential::Credential;
use crate::auth::registry_url::canonicalize_registry;
use crate::lock::file_lock::ConfigFileLock;
use crate::lock::lock_error::LockErrorKind;
use crate::store::atomic_write::atomic_write;

/// Knobs controlling [`DockerCredentialStore`] behaviour.
#[derive(Debug, Default, Clone, Copy)]
pub struct StoreOptions {
    /// Allow `put` to fall through to a plaintext `auths[registry]` entry
    /// when no credential helper is configured. Default `false` (the safe
    /// default); `grim login --allow-insecure-store` opts in for headless
    /// environments with no native keychain.
    pub allow_plaintext_put: bool,
}

/// The three credential-store verbs (docker credential-helper protocol).
#[async_trait]
pub trait CredentialStore: Send + Sync {
    /// Fetch the credential for `registry`. `Ok(None)` when none is stored.
    async fn get(&self, registry: &str) -> Result<Option<Credential>, AuthError>;

    /// Persist `cred` for `registry`.
    async fn put(&self, registry: &str, cred: &Credential) -> Result<(), AuthError>;

    /// Remove the credential for `registry`. `Ok(())` whether or not one
    /// was present (idempotent, matching `docker logout`).
    async fn delete(&self, registry: &str) -> Result<(), AuthError>;
}

/// A [`CredentialStore`] backed by `~/.docker/config.json` plus the
/// configured credential helpers.
#[derive(Debug, Clone)]
pub struct DockerCredentialStore {
    config_path: PathBuf,
    allow_plaintext_put: bool,
}

impl DockerCredentialStore {
    /// Construct a store pointed at the resolved docker config path
    /// (`$DOCKER_CONFIG/config.json` or `~/.docker/config.json`).
    ///
    /// # Errors
    ///
    /// [`AuthError::NoConfigLocation`] when neither `$DOCKER_CONFIG` nor a
    /// home directory can be determined.
    pub fn new(opts: StoreOptions) -> Result<Self, AuthError> {
        let config_path = crate::env::docker_config_path().ok_or(AuthError::NoConfigLocation)?;
        Ok(Self {
            config_path,
            allow_plaintext_put: opts.allow_plaintext_put,
        })
    }

    /// Construct a store pointed at an explicit path (tests, and callers
    /// that resolve the path themselves).
    pub fn with_path(config_path: PathBuf, opts: StoreOptions) -> Self {
        Self {
            config_path,
            allow_plaintext_put: opts.allow_plaintext_put,
        }
    }

    /// The on-disk path this store reads and mutates.
    pub fn config_path(&self) -> &Path {
        &self.config_path
    }
}

#[async_trait]
impl CredentialStore for DockerCredentialStore {
    async fn get(&self, registry: &str) -> Result<Option<Credential>, AuthError> {
        let canonical = canonicalize_registry(registry);
        let path = self.config_path.clone();
        run_blocking(self.config_path.clone(), move || get_blocking(&path, &canonical)).await
    }

    async fn put(&self, registry: &str, cred: &Credential) -> Result<(), AuthError> {
        let canonical = canonicalize_registry(registry);
        let path = self.config_path.clone();
        let allow_plaintext = self.allow_plaintext_put;
        let cred = dup_credential(cred);
        run_blocking(self.config_path.clone(), move || {
            put_blocking(&path, &canonical, &cred, allow_plaintext)
        })
        .await
    }

    async fn delete(&self, registry: &str) -> Result<(), AuthError> {
        let canonical = canonicalize_registry(registry);
        let path = self.config_path.clone();
        run_blocking(self.config_path.clone(), move || delete_blocking(&path, &canonical)).await
    }
}

// ── blocking core (runs inside spawn_blocking) ─────────────────────────

/// Run a blocking store op on the blocking pool.
///
/// A panic inside the closure is re-raised on the caller thread
/// (`resume_unwind`) rather than masked as an I/O error — masking a panic
/// would hide a logic bug behind a benign-looking `StoreIo`. Only a genuine
/// cancellation falls through to `StoreIo`.
async fn run_blocking<T, F>(path: PathBuf, f: F) -> Result<T, AuthError>
where
    F: FnOnce() -> Result<T, AuthError> + Send + 'static,
    T: Send + 'static,
{
    match tokio::task::spawn_blocking(f).await {
        Ok(result) => result,
        Err(join) if join.is_panic() => std::panic::resume_unwind(join.into_panic()),
        Err(join) => Err(AuthError::StoreIo {
            path,
            source: std::io::Error::other(join),
        }),
    }
}

fn get_blocking(path: &Path, canonical: &str) -> Result<Option<Credential>, AuthError> {
    let config = read_config(path)?;
    match resolve_helper(&config, canonical) {
        Some(helper) => match docker_credential::credential_from_helper(canonical, &helper) {
            Ok(docker_credential::DockerCredential::UsernamePassword(user, pass)) => {
                Ok(Some(Credential::basic(user, SecretString::from(pass))))
            }
            Ok(docker_credential::DockerCredential::IdentityToken(tok)) => {
                Ok(Some(Credential::identity_token(SecretString::from(tok))))
            }
            Err(docker_credential::CredentialRetrievalError::NotFound) => Ok(None),
            Err(err) => Err(map_helper_err(&helper, err)),
        },
        None => Ok(read_plaintext(&config, canonical)),
    }
}

fn put_blocking(path: &Path, canonical: &str, cred: &Credential, allow_plaintext: bool) -> Result<(), AuthError> {
    // Resolve the tier from a lock-free read: the helper path does not touch
    // `config.json`, so it needs neither the file to exist nor the advisory
    // lock (and must NOT hold the lock across the helper subprocess).
    let config = read_config(path)?;

    if let Some(helper) = resolve_helper(&config, canonical) {
        return docker_credential::store_credential(canonical, &helper, &to_docker_credential(cred))
            .map_err(|err| map_helper_err(&helper, err));
    }
    if !allow_plaintext {
        return Err(AuthError::NoCredentialStore);
    }

    // Plaintext path: the only branch that writes `config.json`, so it is the
    // only branch that takes the lock and re-reads under it.
    ensure_config_file(path)?;
    let _lock = acquire_lock(path)?;
    let mut config = read_config(path)?;
    let entry = config.auths.entry(canonical.to_string()).or_default();
    if !cred.identity_token.expose_secret().is_empty() {
        entry.identity_token = Some(cred.identity_token.expose_secret().to_string());
        entry.auth = None;
    } else {
        // Zeroized so the cleartext `user:password` does not linger on the
        // heap after the base64 encode (defense in depth alongside `secrecy`).
        let user_pass = Zeroizing::new(format!("{}:{}", cred.username, cred.password.expose_secret()));
        entry.auth = Some(BASE64.encode(user_pass.as_bytes()));
        entry.identity_token = None;
    }
    write_config(path, &config)
}

fn delete_blocking(path: &Path, canonical: &str) -> Result<(), AuthError> {
    // Lock-free read: a missing file resolves to the default (empty) config,
    // so an absent store is a clean no-op without a `path.exists()` pre-check
    // (which would race the lock acquisition).
    let config = read_config(path)?;

    // Helper erase needs no config.json lock — the helper owns its store, and
    // `erase_credential` already maps the not-found sentinel to `Ok`.
    if let Some(helper) = resolve_helper(&config, canonical) {
        docker_credential::erase_credential(canonical, &helper).map_err(|err| map_helper_err(&helper, err))?;
    }

    // Only the plaintext removal mutates config.json, so only it takes the
    // lock and re-reads under it.
    if config.auths.contains_key(canonical) {
        let _lock = acquire_lock(path)?;
        let mut config = read_config(path)?;
        if config.auths.remove(canonical).is_some() {
            write_config(path, &config)?;
        }
    }
    Ok(())
}

/// Convert a credential-helper error into an [`AuthError`], **redacting** the
/// `HelperFailure` variant.
///
/// The upstream `HelperFailure` `Display` dumps the helper's raw `stdout` /
/// `stderr`, which on a `get` can contain credential JSON. That text would
/// otherwise reach the `tracing::error!("{err:#}")` chain in `main` (CWE-532).
/// Every other variant's `Display` is credential-free and passes through.
fn map_helper_err(helper: &str, err: docker_credential::CredentialRetrievalError) -> AuthError {
    match err {
        docker_credential::CredentialRetrievalError::HelperFailure { .. } => AuthError::HelperFailed {
            helper: helper.to_string(),
        },
        other => AuthError::Helper(other),
    }
}

// ── docker config.json schema (unknown keys preserved) ─────────────────

#[derive(Debug, Default, Serialize, Deserialize)]
struct DockerConfig {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    auths: BTreeMap<String, AuthEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "credsStore")]
    creds_store: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty", rename = "credHelpers")]
    cred_helpers: BTreeMap<String, String>,
    /// Any key `docker` / `oras` / `podman` wrote that we do not model.
    #[serde(flatten)]
    other: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct AuthEntry {
    /// base64(`username:password`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    auth: Option<String>,
    /// OAuth2 identity token (mutually exclusive with `auth` in practice).
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "identitytoken")]
    identity_token: Option<String>,
    #[serde(flatten)]
    other: serde_json::Map<String, serde_json::Value>,
}

// ── helpers ────────────────────────────────────────────────────────────

/// The helper configured for `canonical`: per-registry first, then global.
fn resolve_helper(config: &DockerConfig, canonical: &str) -> Option<String> {
    config
        .cred_helpers
        .get(canonical)
        .cloned()
        .or_else(|| config.creds_store.clone())
}

fn read_plaintext(config: &DockerConfig, canonical: &str) -> Option<Credential> {
    let entry = config.auths.get(canonical)?;
    if let Some(token) = &entry.identity_token {
        return Some(Credential::identity_token(SecretString::from(token.clone())));
    }
    let decoded = BASE64.decode(entry.auth.as_ref()?.as_bytes()).ok()?;
    let decoded = String::from_utf8(decoded).ok()?;
    let (user, pass) = decoded.split_once(':')?;
    Some(Credential::basic(
        user.to_string(),
        SecretString::from(pass.to_string()),
    ))
}

fn to_docker_credential(cred: &Credential) -> docker_credential::DockerCredential {
    if !cred.identity_token.expose_secret().is_empty() {
        docker_credential::DockerCredential::IdentityToken(cred.identity_token.expose_secret().to_string())
    } else {
        docker_credential::DockerCredential::UsernamePassword(
            cred.username.clone(),
            cred.password.expose_secret().to_string(),
        )
    }
}

/// Reconstruct an owned [`Credential`] so it can move into the blocking
/// closure (`SecretString` is intentionally not `Clone`). Each secret is
/// briefly exposed to a heap `String` during the move — the exposure window
/// is construction-only (immediately re-wrapped in `SecretString`), but it
/// is not zero. `secrecy` zeroizes both the source and the rewrapped copy on
/// drop.
fn dup_credential(cred: &Credential) -> Credential {
    Credential {
        username: cred.username.clone(),
        password: SecretString::from(cred.password.expose_secret().to_string()),
        identity_token: SecretString::from(cred.identity_token.expose_secret().to_string()),
    }
}

/// Read and parse the docker config. A missing or empty file is the
/// default (empty) config; malformed JSON is an error.
fn read_config(path: &Path) -> Result<DockerConfig, AuthError> {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(DockerConfig::default()),
        Err(source) => {
            return Err(AuthError::StoreIo {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    if bytes.is_empty() {
        return Ok(DockerConfig::default());
    }
    serde_json::from_slice(&bytes).map_err(|source| AuthError::MalformedConfig {
        path: path.to_path_buf(),
        source,
    })
}

/// Serialize and atomically replace the docker config, then tighten the
/// mode to owner-only (`0600`) — credentials must not inherit the `0644`
/// cap [`atomic_write`] applies to ordinary state files.
fn write_config(path: &Path, config: &DockerConfig) -> Result<(), AuthError> {
    let bytes = serde_json::to_vec_pretty(config).map_err(|source| AuthError::MalformedConfig {
        path: path.to_path_buf(),
        source,
    })?;
    atomic_write(path, &bytes).map_err(|source| AuthError::StoreIo {
        path: path.to_path_buf(),
        source,
    })?;
    restrict_permissions(path)
}

/// Ensure the config file (and its parent directory) exist so the advisory
/// lock has something to open. A freshly created file is owner-only.
fn ensure_config_file(path: &Path) -> Result<(), AuthError> {
    let io = |source| AuthError::StoreIo {
        path: path.to_path_buf(),
        source,
    };
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent).map_err(io)?;
    }
    if !path.exists() {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            opts.mode(0o600);
        }
        opts.open(path).map_err(io)?;
    }
    Ok(())
}

/// Acquire the exclusive advisory lock on the config file, mapping the
/// lock-subsystem error into an auth-tier store error.
fn acquire_lock(path: &Path) -> Result<ConfigFileLock, AuthError> {
    ConfigFileLock::try_acquire(path).map_err(|err| {
        let source = match err.kind {
            LockErrorKind::Locked => std::io::Error::new(
                std::io::ErrorKind::WouldBlock,
                "credential store is locked by another process",
            ),
            LockErrorKind::Io(io) => io,
            other => std::io::Error::other(other.to_string()),
        };
        AuthError::StoreIo {
            path: path.to_path_buf(),
            source,
        }
    })
}

#[cfg(unix)]
fn restrict_permissions(path: &Path) -> Result<(), AuthError> {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).map_err(|source| AuthError::StoreIo {
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(not(unix))]
fn restrict_permissions(_path: &Path) -> Result<(), AuthError> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        (dir, path)
    }

    fn opts(allow_plaintext_put: bool) -> StoreOptions {
        StoreOptions { allow_plaintext_put }
    }

    fn rt() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap()
    }

    #[test]
    fn get_returns_none_on_empty_store() {
        let (_d, path) = tmp();
        let store = DockerCredentialStore::with_path(path, opts(false));
        let got = rt().block_on(store.get("ghcr.io"));
        assert!(matches!(got, Ok(None)), "fresh store must be empty: {got:?}");
    }

    #[test]
    fn put_refuses_plaintext_without_opt_in() {
        let (_d, path) = tmp();
        let store = DockerCredentialStore::with_path(path, opts(false));
        let cred = Credential::basic("u", SecretString::from("p"));
        let res = rt().block_on(store.put("ghcr.io", &cred));
        assert!(
            matches!(res, Err(AuthError::NoCredentialStore)),
            "no helper + no opt-in must refuse: {res:?}"
        );
    }

    #[test]
    fn put_then_get_round_trips_plaintext() {
        let (_d, path) = tmp();
        let store = DockerCredentialStore::with_path(path.clone(), opts(true));
        let cred = Credential::basic("alice", SecretString::from("hunter2"));
        rt().block_on(store.put("ghcr.io", &cred)).expect("put");

        let raw = std::fs::read_to_string(&path).unwrap();
        let json: serde_json::Value = serde_json::from_str(&raw).unwrap();
        let auth = json.pointer("/auths/ghcr.io/auth").and_then(|v| v.as_str()).unwrap();
        let decoded = String::from_utf8(BASE64.decode(auth).unwrap()).unwrap();
        assert_eq!(decoded, "alice:hunter2");

        let got = rt().block_on(store.get("ghcr.io")).unwrap().unwrap();
        assert_eq!(got.username, "alice");
        assert_eq!(got.password.expose_secret(), "hunter2");
    }

    #[test]
    fn put_canonicalizes_registry_key() {
        let (_d, path) = tmp();
        let store = DockerCredentialStore::with_path(path.clone(), opts(true));
        let cred = Credential::basic("u", SecretString::from("p"));
        rt().block_on(store.put("https://ghcr.io/v2/", &cred)).expect("put");
        let json: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(
            json.pointer("/auths/ghcr.io/auth").is_some(),
            "key must be canonical ghcr.io"
        );
    }

    #[test]
    fn delete_removes_plaintext_entry() {
        let (_d, path) = tmp();
        let store = DockerCredentialStore::with_path(path.clone(), opts(true));
        let cred = Credential::basic("u", SecretString::from("p"));
        rt().block_on(store.put("ghcr.io", &cred)).expect("put");
        rt().block_on(store.delete("ghcr.io")).expect("delete");
        let json: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(
            json.pointer("/auths/ghcr.io").is_none(),
            "entry must be gone after delete"
        );
    }

    #[test]
    fn delete_is_noop_on_absent_store() {
        let (_d, path) = tmp();
        let store = DockerCredentialStore::with_path(path, opts(false));
        assert!(matches!(rt().block_on(store.delete("ghcr.io")), Ok(())));
    }

    #[test]
    fn put_preserves_unknown_top_level_keys() {
        let (_d, path) = tmp();
        std::fs::write(&path, r#"{"currentContext":"default","experimental":true}"#).unwrap();
        let store = DockerCredentialStore::with_path(path.clone(), opts(true));
        let cred = Credential::basic("u", SecretString::from("p"));
        rt().block_on(store.put("ghcr.io", &cred)).expect("put");
        let json: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(json.get("currentContext").and_then(|v| v.as_str()), Some("default"));
        assert_eq!(json.get("experimental").and_then(|v| v.as_bool()), Some(true));
    }

    #[test]
    fn malformed_config_surfaces_error() {
        let (_d, path) = tmp();
        std::fs::write(&path, "not json {").unwrap();
        let store = DockerCredentialStore::with_path(path, opts(true));
        let cred = Credential::basic("u", SecretString::from("p"));
        let res = rt().block_on(store.put("ghcr.io", &cred));
        assert!(matches!(res, Err(AuthError::MalformedConfig { .. })), "got: {res:?}");
    }

    #[test]
    #[cfg(unix)]
    fn plaintext_put_creates_file_mode_0600() {
        use std::os::unix::fs::PermissionsExt as _;
        let (_d, path) = tmp();
        let store = DockerCredentialStore::with_path(path.clone(), opts(true));
        let cred = Credential::basic("u", SecretString::from("p"));
        rt().block_on(store.put("ghcr.io", &cred)).expect("put");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "credentials file must be owner-only");
    }
}
