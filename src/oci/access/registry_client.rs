// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! The real [`OciAccess`] implementation over the `oci-client` crate.
//!
//! Adapted from OCX `oci/client*` (the `NativeTransport` + `auth`
//! modules), trimmed: no chunked-push progress, no platform resolution,
//! no manifest builder. Auth tries anonymous first and falls back to
//! Docker credentials (`~/.docker/config.json` / credential helpers) via
//! the `docker_credential` crate, the same approach OCX uses.
//!
//! `oci-client` 0.16 has no `_catalog` endpoint, so the registry-catalog
//! listing issues the distribution-spec `GET /v2/_catalog` request
//! directly through `reqwest`; an unsupported endpoint degrades to an
//! empty list rather than an error (catalog is a Phase 6 surface — the
//! method exists here only to satisfy the seam).

use async_trait::async_trait;

use super::error::{AccessError, AccessErrorKind};
use crate::oci::manifest::OciManifest;
use crate::oci::{Digest, Identifier, PinnedIdentifier};

use super::super::access::{OciAccess, Operation};

use oci_client::Reference;
use oci_client::client::{Client, ClientConfig, ClientProtocol};
use oci_client::errors::{OciDistributionError, OciErrorCode};
use oci_client::secrets::RegistryAuth;

/// Real OCI registry access backed by `oci-client`.
pub struct RegistryClient {
    client: Client,
    http: reqwest::Client,
}

impl RegistryClient {
    /// Construct a client for production use.
    ///
    /// `localhost` / `127.0.0.1` registries are contacted over plain HTTP
    /// (matching the local test-registry workflow); everything else uses
    /// HTTPS. This mirrors OCX's `ClientProtocol::HttpsExcept` policy with
    /// the loopback host hard-wired so a local registry "just works".
    pub fn new() -> Self {
        let config = ClientConfig {
            protocol: ClientProtocol::HttpsExcept(vec![
                "localhost".to_string(),
                "localhost:5000".to_string(),
                "127.0.0.1".to_string(),
                "127.0.0.1:5000".to_string(),
            ]),
            ..Default::default()
        };
        Self {
            client: Client::new(config),
            http: reqwest::Client::new(),
        }
    }

    /// Resolve credentials for `registry`: anonymous unless Docker
    /// credentials are configured for the host.
    ///
    /// A genuine credential-helper failure is surfaced; "no credentials
    /// configured" is a normal anonymous-access case, not an error.
    fn auth_for(registry: &str) -> Result<RegistryAuth, AccessErrorKind> {
        use docker_credential::{CredentialRetrievalError, DockerCredential};

        match docker_credential::get_credential(registry) {
            Ok(DockerCredential::IdentityToken(token)) => Ok(RegistryAuth::Bearer(token)),
            Ok(DockerCredential::UsernamePassword(user, pass)) => Ok(RegistryAuth::Basic(user, pass)),
            Err(
                CredentialRetrievalError::NoCredentialConfigured
                | CredentialRetrievalError::ConfigNotFound
                | CredentialRetrievalError::ConfigReadError
                | CredentialRetrievalError::HelperFailure { .. },
            ) => Ok(RegistryAuth::Anonymous),
            Err(e) => Err(AccessErrorKind::Authentication(Box::new(e))),
        }
    }

    /// HTTP scheme used for the loopback registry; HTTPS otherwise. Kept
    /// in lockstep with the [`ClientProtocol::HttpsExcept`] list in
    /// [`Self::new`].
    fn scheme_for(registry: &str) -> &'static str {
        let host = registry.split(':').next().unwrap_or(registry);
        if host == "localhost" || host == "127.0.0.1" {
            "http"
        } else {
            "https"
        }
    }
}

impl Default for RegistryClient {
    fn default() -> Self {
        Self::new()
    }
}

/// Build an `oci-client` [`Reference`] from a Grimoire [`Identifier`].
///
/// This is the `From<&Identifier>`-style conversion Phase 1 deliberately
/// deferred to the access seam (see `oci/identifier.rs` module docs). It
/// lives here, private to the registry client, so the domain type stays
/// transport-agnostic.
fn reference_for(id: &Identifier) -> Reference {
    let registry = id.registry().to_string();
    let repository = id.repository().to_string();
    match (id.tag(), id.digest()) {
        (Some(tag), Some(digest)) => {
            Reference::with_tag_and_digest(registry, repository, tag.to_string(), digest.to_string())
        }
        (Some(tag), None) => Reference::with_tag(registry, repository, tag.to_string()),
        (None, Some(digest)) => Reference::with_digest(registry, repository, digest.to_string()),
        (None, None) => Reference::with_tag(registry, repository, "latest".to_string()),
    }
}

/// Classify an `oci-client` error into the access taxonomy.
///
/// 404 / `MANIFEST_UNKNOWN` / `NAME_UNKNOWN` indicates a benign miss and
/// is reported by the caller as `Ok(None)`; 401/403 / `UNAUTHORIZED` /
/// `DENIED` is a terminal auth failure; everything else is a transport
/// failure (`Registry`).
enum Classified {
    NotFound,
    Auth(OciDistributionError),
    Registry(OciDistributionError),
}

fn classify(err: OciDistributionError) -> Classified {
    match &err {
        OciDistributionError::ImageManifestNotFoundError(_) => Classified::NotFound,
        OciDistributionError::AuthenticationFailure(_) | OciDistributionError::UnauthorizedError { .. } => {
            Classified::Auth(err)
        }
        OciDistributionError::ServerError { code, .. } => match code {
            404 => Classified::NotFound,
            401 | 403 => Classified::Auth(err),
            _ => Classified::Registry(err),
        },
        OciDistributionError::RegistryError { envelope, .. } => {
            let codes = || envelope.errors.iter().map(|e| &e.code);
            if codes().any(|c| {
                matches!(
                    c,
                    OciErrorCode::ManifestUnknown
                        | OciErrorCode::NameUnknown
                        | OciErrorCode::NotFound
                        | OciErrorCode::BlobUnknown
                )
            }) {
                Classified::NotFound
            } else if codes().any(|c| matches!(c, OciErrorCode::Unauthorized | OciErrorCode::Denied)) {
                Classified::Auth(err)
            } else {
                Classified::Registry(err)
            }
        }
        _ => Classified::Registry(err),
    }
}

/// Map a classified error to an [`AccessErrorKind`], or `None` for the
/// benign not-found case (the caller then reports `Ok(None)`).
///
/// Returns the small `AccessErrorKind` rather than a fully-built
/// `AccessError`: the latter embeds an `Identifier` and would trip
/// `clippy::result_large_err` on this free function. The caller attaches
/// identifier context.
fn lookup_failure(err: OciDistributionError) -> Option<AccessErrorKind> {
    match classify(err) {
        Classified::NotFound => None,
        Classified::Auth(e) => Some(AccessErrorKind::Authentication(Box::new(e))),
        Classified::Registry(e) => Some(AccessErrorKind::Registry(Box::new(e))),
    }
}

#[async_trait]
impl OciAccess for RegistryClient {
    async fn resolve_digest(&self, id: &Identifier, _op: Operation) -> Result<Option<Digest>, AccessError> {
        if let Some(digest) = id.digest() {
            // Digest-addressed input is immutable — resolves to itself.
            return Ok(Some(digest));
        }
        let reference = reference_for(id);
        let auth = Self::auth_for(id.registry()).map_err(|kind| AccessError::with_identifier(id.clone(), kind))?;

        match self.client.fetch_manifest_digest(&reference, &auth).await {
            Ok(digest_str) => {
                let digest = Digest::try_from(digest_str.as_str()).map_err(|_| {
                    AccessError::with_identifier(
                        id.clone(),
                        AccessErrorKind::InvalidManifest(format!("registry returned an invalid digest: {digest_str}")),
                    )
                })?;
                Ok(Some(digest))
            }
            Err(e) => match lookup_failure(e) {
                None => Ok(None),
                Some(kind) => Err(AccessError::with_identifier(id.clone(), kind)),
            },
        }
    }

    async fn fetch_manifest(&self, id: &PinnedIdentifier) -> Result<Option<OciManifest>, AccessError> {
        let identifier = id.as_identifier().clone();
        let reference = reference_for(&identifier);
        let auth = Self::auth_for(identifier.registry())
            .map_err(|kind| AccessError::with_identifier(identifier.clone(), kind))?;

        let (manifest, _digest) = match self.client.pull_manifest(&reference, &auth).await {
            Ok(pair) => pair,
            Err(e) => {
                return match lookup_failure(e) {
                    None => Ok(None),
                    Some(kind) => Err(AccessError::with_identifier(identifier, kind)),
                };
            }
        };

        let parsed = OciManifest::try_from(manifest)
            .map_err(|e| AccessError::with_identifier(identifier.clone(), AccessErrorKind::InvalidManifest(e.0)))?;
        Ok(Some(parsed))
    }

    async fn fetch_blob(&self, repo: &Identifier, digest: &Digest) -> Result<Option<Vec<u8>>, AccessError> {
        let reference = reference_for(repo);
        let auth = Self::auth_for(repo.registry()).map_err(|kind| AccessError::with_identifier(repo.clone(), kind))?;
        // Ensure auth is primed before the (internally-authenticated)
        // blob pull, matching OCX's NativeTransport ordering.
        self.client
            .store_auth_if_needed(reference.resolve_registry(), &auth)
            .await;

        let mut bytes: Vec<u8> = Vec::new();
        let digest_str = digest.to_string();
        match self.client.pull_blob(&reference, digest_str.as_str(), &mut bytes).await {
            Ok(()) => {}
            Err(e) => {
                return match lookup_failure(e) {
                    None => Ok(None),
                    Some(kind) => Err(AccessError::with_identifier(repo.clone(), kind)),
                };
            }
        }

        // Defence in depth: verify the bytes hash to the requested digest
        // before handing them up. Reuses the Phase-1 `Algorithm`.
        let actual = digest.algorithm().hash(&bytes);
        if &actual != digest {
            return Err(AccessError::with_identifier(
                repo.clone(),
                AccessErrorKind::DigestMismatch {
                    expected: digest.clone(),
                    actual,
                },
            ));
        }
        Ok(Some(bytes))
    }

    async fn list_tags(&self, id: &Identifier) -> Result<Option<Vec<String>>, AccessError> {
        let reference = reference_for(id);
        let auth = Self::auth_for(id.registry()).map_err(|kind| AccessError::with_identifier(id.clone(), kind))?;

        match self.client.list_tags(&reference, &auth, None, None).await {
            Ok(response) => Ok(Some(response.tags)),
            Err(e) => match lookup_failure(e) {
                None => Ok(None),
                Some(kind) => Err(AccessError::with_identifier(id.clone(), kind)),
            },
        }
    }

    async fn list_catalog(&self, registry: &str) -> Result<Vec<String>, AccessError> {
        // `oci-client` 0.16 has no catalog API. Issue the distribution
        // spec request directly; an unsupported/absent endpoint degrades
        // to an empty list (Phase 6 owns real catalog handling).
        #[derive(serde::Deserialize)]
        struct Catalog {
            #[serde(default)]
            repositories: Vec<String>,
        }

        let url = format!("{}://{registry}/v2/_catalog", Self::scheme_for(registry));
        let resp = match self.http.get(&url).send().await {
            Ok(r) => r,
            Err(e) => {
                return Err(AccessError::without_identifier(AccessErrorKind::Registry(Box::new(e))));
            }
        };
        if resp.status() == reqwest::StatusCode::NOT_FOUND || !resp.status().is_success() {
            return Ok(Vec::new());
        }
        match resp.json::<Catalog>().await {
            Ok(catalog) => Ok(catalog.repositories),
            // A non-JSON or unexpected body from a registry that does not
            // implement the catalog is not a hard failure here.
            Err(_) => Ok(Vec::new()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reference_for_tagged_identifier() {
        let id = Identifier::parse("ghcr.io/acme/code-review:stable").unwrap();
        let r = reference_for(&id);
        assert_eq!(r.registry(), "ghcr.io");
        assert_eq!(r.repository(), "acme/code-review");
        assert_eq!(r.tag(), Some("stable"));
    }

    #[test]
    fn reference_for_digest_identifier() {
        let hex = "a".repeat(64);
        let id = Identifier::parse(&format!("ghcr.io/acme/x@sha256:{hex}")).unwrap();
        let r = reference_for(&id);
        assert_eq!(r.digest(), Some(format!("sha256:{hex}").as_str()));
    }

    #[test]
    fn reference_for_bare_identifier_defaults_latest() {
        let id = Identifier::parse_with_default_registry("acme/x", "ghcr.io").unwrap();
        let r = reference_for(&id);
        assert_eq!(r.tag(), Some("latest"));
    }

    #[test]
    fn scheme_for_loopback_is_http() {
        assert_eq!(RegistryClient::scheme_for("localhost"), "http");
        assert_eq!(RegistryClient::scheme_for("localhost:5000"), "http");
        assert_eq!(RegistryClient::scheme_for("127.0.0.1:5000"), "http");
        assert_eq!(RegistryClient::scheme_for("ghcr.io"), "https");
    }

    #[test]
    fn classify_manifest_unknown_is_not_found() {
        let err = OciDistributionError::ImageManifestNotFoundError("x".to_string());
        assert!(matches!(classify(err), Classified::NotFound));
    }

    #[test]
    fn classify_unauthorized_is_auth() {
        let err = OciDistributionError::UnauthorizedError {
            url: "https://r/x".to_string(),
        };
        assert!(matches!(classify(err), Classified::Auth(_)));
    }

    #[test]
    fn classify_server_503_is_registry() {
        let err = OciDistributionError::ServerError {
            code: 503,
            url: "https://r/x".to_string(),
            message: "down".to_string(),
        };
        assert!(matches!(classify(err), Classified::Registry(_)));
    }

    #[test]
    fn auth_for_unknown_registry_is_anonymous() {
        // No Docker config for a bogus host ⇒ anonymous, not an error.
        let auth = RegistryClient::auth_for("nonexistent.invalid").expect("anonymous fallback");
        assert!(matches!(auth, RegistryAuth::Anonymous));
    }
}
