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
//! The registry-catalog listing delegates to `oci-client`'s `catalog`
//! endpoint (added in 0.17), which runs the same `WWW-Authenticate` token
//! handshake as every other call — most registries gate `_catalog` behind
//! a token, and without one it 401s and the catalog comes back empty. We
//! only add bounded pagination over the spec `last` cursor; an unsupported
//! or forbidden endpoint degrades to an empty list rather than an error.

use async_trait::async_trait;

use super::error::{AccessError, AccessErrorKind};
use crate::oci::manifest::OciManifest;
use crate::oci::{Digest, Identifier, PinnedIdentifier};

use super::super::access::{OciAccess, Operation};

use oci_client::Reference;
use oci_client::client::{Client, ClientConfig, ClientProtocol};
use oci_client::errors::{OciDistributionError, OciErrorCode};
use oci_client::manifest::{OciDescriptor, OciImageManifest};
use oci_client::secrets::RegistryAuth;

use crate::oci::manifest::Descriptor;

/// The media type of a Grimoire artifact layer (single uncompressed tar).
const GRIMOIRE_LAYER_MEDIA_TYPE: &str = "application/vnd.grimoire.artifact.layer.v1.tar";
/// The OCI empty config descriptor media type. Stamped on every pushed
/// manifest's config descriptor (the blob is the byte-identical `{}` the OCI
/// spec blesses for artifacts that carry an `artifactType`). The custom
/// per-kind config type the project used before `adr_oci_empty_config_compat.md`
/// is off GitLab's referenced-media-type allowlist and made GitLab reject the
/// manifest (`400 MANIFEST_INVALID`); the empty type is on every registry's
/// allowlist. The kind now rides on the `artifactType` plus the
/// `com.grimoire.kind` annotation, never the config descriptor.
const OCI_EMPTY_CONFIG_MEDIA_TYPE: &str = "application/vnd.oci.empty.v1+json";
/// The media type of an OCI image manifest.
const OCI_MANIFEST_MEDIA_TYPE: &str = "application/vnd.oci.image.manifest.v1+json";

/// Real OCI registry access backed by `oci-client`.
pub struct RegistryClient {
    client: Client,
}

impl RegistryClient {
    /// Construct a client for production use.
    ///
    /// The conventional loopback forms (`localhost` / `127.0.0.1`, bare and
    /// on `:5000`) are contacted over plain HTTP so a local test registry
    /// "just works"; any host listed in `GRIM_INSECURE_REGISTRIES`
    /// (comma-separated) is likewise plain HTTP; everything else uses HTTPS.
    /// `oci-client`'s `HttpsExcept` matches by *exact* `host:port`, so a
    /// loopback registry on another port (e.g. the manual rig on `:5050`)
    /// opts in through that env var.
    pub fn new() -> Self {
        // `HttpsExcept` is exact-match per `host:port`, so enumerate the
        // zero-config loopback forms (bare host + the conventional :5000)
        // and add every `GRIM_INSECURE_REGISTRIES` entry — that env is how
        // a loopback registry on a non-default port (e.g. the manual rig
        // on :5050) opts into plain HTTP.
        let mut exceptions = vec![
            "localhost".to_string(),
            "localhost:5000".to_string(),
            "127.0.0.1".to_string(),
            "127.0.0.1:5000".to_string(),
        ];
        for r in insecure_registries() {
            if !exceptions.contains(&r) {
                exceptions.push(r);
            }
        }
        let config = ClientConfig {
            protocol: ClientProtocol::HttpsExcept(exceptions),
            ..Default::default()
        };
        Self {
            client: Client::new(config),
        }
    }

    /// Resolve credentials for `registry`: anonymous unless Docker
    /// credentials are configured for the host.
    ///
    /// Infallible by design: a credential is an opportunistic enhancement,
    /// never a precondition. A broken credential store (helper binary not
    /// on PATH, locked keyring, malformed helper output, …) must not block
    /// access to a registry that allows anonymous pulls — those failures
    /// degrade to anonymous with a warning. A registry that actually
    /// requires auth then fails the request itself with a classified
    /// authentication error pointing at `grim login`.
    fn auth_for(registry: &str) -> RegistryAuth {
        use docker_credential::{CredentialRetrievalError, DockerCredential};

        // Canonicalize the lookup key so a credential written by
        // `grim login` (scheme / `/vN` stripped, docker.io aliased) is
        // found here. Single source of truth: `auth::canonicalize_registry`.
        let registry = crate::auth::canonicalize_registry(registry);
        match docker_credential::get_credential(&registry) {
            Ok(DockerCredential::IdentityToken(token)) => RegistryAuth::Bearer(token),
            Ok(DockerCredential::UsernamePassword(user, pass)) => RegistryAuth::Basic(user, pass),
            // "No credential for this registry" is a benign anonymous-access
            // case — stay silent. The patched `docker_credential` fork
            // surfaces a credential-helper miss as `NotFound` (upstream rolled
            // it into `HelperFailure`), so it joins the silent group too.
            Err(
                CredentialRetrievalError::NoCredentialConfigured
                | CredentialRetrievalError::ConfigNotFound
                | CredentialRetrievalError::ConfigReadError
                | CredentialRetrievalError::NotFound
                | CredentialRetrievalError::HelperFailure { .. },
            ) => RegistryAuth::Anonymous,
            // Anything else is a broken credential store — degrade loudly.
            Err(e) => {
                tracing::warn!("credential lookup for {registry} failed ({e}); continuing anonymously");
                RegistryAuth::Anonymous
            }
        }
    }
}

/// Hosts the user has opted into plain HTTP for, from
/// `GRIM_INSECURE_REGISTRIES` (comma-separated, entries trimmed, empties
/// dropped). Empty/unset yields an empty list.
fn insecure_registries() -> Vec<String> {
    parse_insecure_registries(&std::env::var("GRIM_INSECURE_REGISTRIES").unwrap_or_default())
}

/// Parse a `GRIM_INSECURE_REGISTRIES` value: comma-separated, entries
/// trimmed, empties dropped. Pure so it is testable without mutating the
/// process environment (the crate forbids `unsafe`, so `set_var` is out).
fn parse_insecure_registries(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
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

/// Build the `oci-client` descriptor for one Grimoire [`Descriptor`].
fn oci_descriptor(d: &Descriptor) -> OciDescriptor {
    OciDescriptor {
        media_type: d.media_type.clone(),
        digest: d.digest.to_string(),
        size: i64::try_from(d.size).unwrap_or(i64::MAX),
        urls: None,
        annotations: None,
        artifact_type: None,
    }
}

impl RegistryClient {
    /// A tiny, deterministic config blob (`{}`, digest
    /// `sha256:44136fa3...8a`, size 2 — the OCI empty descriptor). The
    /// artifact's identity is carried by the manifest `artifactType` and the
    /// `com.grimoire.kind` annotation, and its metadata by the manifest
    /// annotations — the config blob itself only has to exist and be
    /// content-addressable, so it stays the OCI empty config
    /// ([`OCI_EMPTY_CONFIG_MEDIA_TYPE`]) rather than masquerading as a runnable
    /// image config or carrying a custom type a registry allowlist may reject.
    fn config_blob() -> Vec<u8> {
        b"{}".to_vec()
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
        let auth = Self::auth_for(id.registry());

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
        let auth = Self::auth_for(identifier.registry());

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
        let auth = Self::auth_for(repo.registry());
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
        let auth = Self::auth_for(id.registry());

        match self.client.list_tags(&reference, &auth, None, None).await {
            Ok(response) => Ok(Some(response.tags)),
            Err(e) => match lookup_failure(e) {
                None => Ok(None),
                Some(kind) => Err(AccessError::with_identifier(id.clone(), kind)),
            },
        }
    }

    async fn list_catalog(&self, registry: &str) -> Result<Vec<String>, AccessError> {
        // Bound the walk: a registry can advertise tens of thousands of
        // repositories, and the catalog layer caps and prefilters anyway —
        // a truncated listing is an explicit cut-line, the catalog is a
        // best-effort index, not a mirror.
        const PAGE_SIZE: usize = 1000;
        const MAX_PAGES: usize = 8;

        // The `_catalog` endpoint lives on the bare registry HOST. A
        // configured default registry may carry a namespace
        // (`ghcr.io/acme`); the namespace is filtered upstream, so list
        // against the host. The reference's repository is unused by the
        // catalog request itself — it only drives oci-client's
        // `repository:…:pull` token scope, and the token a registry mints
        // carries the caller's read access, which covers `_catalog`.
        let host = registry_host(registry);
        let auth = Self::auth_for(host);
        let reference = Reference::with_tag(host.to_string(), "_catalog".to_string(), "latest".to_string());

        // `oci-client`'s `catalog` runs the WWW-Authenticate token handshake
        // (so a private registry resolves) but returns one page with no
        // cursor, so paginate over the distribution-spec `last` marker.
        let mut all: Vec<String> = Vec::new();
        let mut last: Option<String> = None;
        for _ in 0..MAX_PAGES {
            let page = match self
                .client
                .catalog(&reference, &auth, Some(PAGE_SIZE), last.as_deref())
                .await
            {
                Ok(page) => page.repositories,
                // Degrade vs. abort, mirroring the rest of this seam: an
                // unsupported (404) or forbidden/auth-gated catalog yields
                // whatever was collected (empty on page 1); only a genuine
                // transport fault aborts an otherwise-empty walk.
                Err(e) => match classify(e) {
                    Classified::NotFound | Classified::Auth(_) => break,
                    Classified::Registry(e) => {
                        if all.is_empty() {
                            return Err(AccessError::without_identifier(AccessErrorKind::Registry(Box::new(e))));
                        }
                        break;
                    }
                },
            };
            // A short page is the last page: the spec returns up to
            // PAGE_SIZE repositories, so fewer than that means the registry
            // has none left. When the total is an exact multiple of PAGE_SIZE
            // the final request comes back empty (0 < PAGE_SIZE) and this same
            // check terminates on it — no separate empty-page case needed.
            let is_last_page = page.len() < PAGE_SIZE;
            last = page.last().cloned();
            all.extend(page);
            if is_last_page {
                break;
            }
        }
        Ok(all)
    }

    async fn push_blob(&self, repo: &Identifier, bytes: &[u8]) -> Result<Digest, AccessError> {
        let reference = reference_for(repo);
        let auth = Self::auth_for(repo.registry());
        self.client
            .store_auth_if_needed(reference.resolve_registry(), &auth)
            .await;

        let digest = crate::oci::Algorithm::Sha256.hash(bytes);
        let digest_str = digest.to_string();
        // `oci-client` skips the upload when the blob already exists, so a
        // re-push of identical content is an idempotent no-op success.
        self.client
            .push_blob(&reference, bytes.to_vec(), &digest_str)
            .await
            .map_err(|e| AccessError::with_identifier(repo.clone(), registry_or_auth(e)))?;
        Ok(digest)
    }

    async fn push_manifest(&self, repo: &Identifier, manifest: &OciManifest) -> Result<Digest, AccessError> {
        let auth = Self::auth_for(repo.registry());
        let registry_ref = reference_for(repo);
        self.client
            .store_auth_if_needed(registry_ref.resolve_registry(), &auth)
            .await;

        // The config blob is pushed inline; the kind rides on the manifest
        // `artifactType` + the `com.grimoire.kind` annotation, not the config
        // blob bytes. The config descriptor's media type is ALWAYS the OCI
        // empty type (ignoring `manifest.config_media_type`): a custom per-kind
        // config type is off GitLab's referenced-media-type allowlist and made
        // GitLab reject the whole manifest. The empty type is universally
        // allow-listed (`adr_oci_empty_config_compat.md`).
        let config = Self::config_blob();
        let config_digest = self.push_blob(repo, &config).await?;

        let image = OciImageManifest {
            schema_version: 2,
            media_type: Some(OCI_MANIFEST_MEDIA_TYPE.to_string()),
            config: OciDescriptor {
                media_type: OCI_EMPTY_CONFIG_MEDIA_TYPE.to_string(),
                digest: config_digest.to_string(),
                size: i64::try_from(config.len()).unwrap_or(i64::MAX),
                urls: None,
                annotations: None,
                artifact_type: None,
            },
            layers: manifest.layers.iter().map(oci_descriptor).collect(),
            subject: None,
            artifact_type: manifest.artifact_type.clone(),
            annotations: if manifest.annotations.is_empty() {
                None
            } else {
                Some(manifest.annotations.clone())
            },
        };

        // Serialize the manifest ourselves and PUT those exact bytes by
        // digest: the manifest digest a registry stores is the hash of the
        // bytes it received, so controlling the bytes makes the returned
        // digest deterministic and the push idempotent.
        let body = serde_json::to_vec(&image).map_err(|e| {
            AccessError::with_identifier(
                repo.clone(),
                AccessErrorKind::InvalidManifest(format!("cannot serialize manifest: {e}")),
            )
        })?;
        let manifest_digest = crate::oci::Algorithm::Sha256.hash(&body);

        let by_digest = Reference::with_digest(
            repo.registry().to_string(),
            repo.repository().to_string(),
            manifest_digest.to_string(),
        );
        let content_type = OCI_MANIFEST_MEDIA_TYPE.parse().map_err(|_| {
            AccessError::with_identifier(
                repo.clone(),
                AccessErrorKind::InvalidManifest("bad content type".to_string()),
            )
        })?;
        self.client
            .push_manifest_raw(&by_digest, body, content_type)
            .await
            .map_err(|e| AccessError::with_identifier(repo.clone(), registry_or_auth(e)))?;
        Ok(manifest_digest)
    }

    async fn put_tag(&self, repo: &Identifier, tag: &str, manifest_digest: &Digest) -> Result<(), AccessError> {
        let auth = Self::auth_for(repo.registry());
        let registry_ref = reference_for(repo);
        self.client
            .store_auth_if_needed(registry_ref.resolve_registry(), &auth)
            .await;

        // Pull the manifest by digest and re-PUT the identical bytes under
        // `tag` so the floating tag points at exactly `manifest_digest`.
        let by_digest = Reference::with_digest(
            repo.registry().to_string(),
            repo.repository().to_string(),
            manifest_digest.to_string(),
        );
        let (body, _digest) = self
            .client
            .pull_manifest_raw(&by_digest, &auth, &[OCI_MANIFEST_MEDIA_TYPE, GRIMOIRE_LAYER_MEDIA_TYPE])
            .await
            .map_err(|e| AccessError::with_identifier(repo.clone(), registry_or_auth(e)))?;

        let by_tag = Reference::with_tag(
            repo.registry().to_string(),
            repo.repository().to_string(),
            tag.to_string(),
        );
        let content_type = OCI_MANIFEST_MEDIA_TYPE.parse().map_err(|_| {
            AccessError::with_identifier(
                repo.clone(),
                AccessErrorKind::InvalidManifest("bad content type".to_string()),
            )
        })?;
        self.client
            .push_manifest_raw(&by_tag, body.to_vec(), content_type)
            .await
            .map_err(|e| AccessError::with_identifier(repo.clone(), registry_or_auth(e)))?;
        Ok(())
    }
}

/// The bare registry host (first path segment) of a possibly-namespaced
/// registry string: `ghcr.io/acme` → `ghcr.io`; `localhost:5000` →
/// `localhost:5000`. The OCI distribution API (`/v2/_catalog`, auth scope)
/// is served by the host, not a host+namespace prefix.
fn registry_host(registry: &str) -> &str {
    registry.split_once('/').map_or(registry, |(host, _)| host)
}

/// Classify an `oci-client` error from a push/pull path: an auth failure
/// stays terminal, everything else (including a rare push-time
/// not-found) is a transport (`Registry`) failure — a push has no benign
/// absence.
fn registry_or_auth(err: OciDistributionError) -> AccessErrorKind {
    match &err {
        OciDistributionError::AuthenticationFailure(_) | OciDistributionError::UnauthorizedError { .. } => {
            AccessErrorKind::Authentication(Box::new(err))
        }
        OciDistributionError::ServerError { code: 401 | 403, .. } => AccessErrorKind::Authentication(Box::new(err)),
        OciDistributionError::RegistryError { envelope, .. }
            if envelope
                .errors
                .iter()
                .any(|e| matches!(e.code, OciErrorCode::Unauthorized | OciErrorCode::Denied)) =>
        {
            AccessErrorKind::Authentication(Box::new(err))
        }
        _ => AccessErrorKind::Registry(Box::new(err)),
    }
}

#[cfg(test)]
impl RegistryClient {
    /// Test constructor that forces `registry` to plain HTTP. The default
    /// exception list only knows the conventional loopback `:5000` forms,
    /// but a mock binds a random port, so add it explicitly.
    fn with_plain_http(registry: &str) -> Self {
        let config = ClientConfig {
            protocol: ClientProtocol::HttpsExcept(vec![registry.to_string()]),
            ..Default::default()
        };
        Self {
            client: Client::new(config),
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

    /// `GRIM_INSECURE_REGISTRIES` parsing: comma-separated, trimmed,
    /// empties dropped. Drives the plain-HTTP opt-in for non-loopback
    /// hosts (e.g. an internal registry) and the `oci-client` exception
    /// list built in [`RegistryClient::new`].
    #[test]
    fn parse_insecure_registries_trims_and_drops_empties() {
        assert_eq!(
            parse_insecure_registries("registry.internal:5000, , localhost:5050 "),
            vec!["registry.internal:5000".to_string(), "localhost:5050".to_string()]
        );
        assert!(parse_insecure_registries("").is_empty());
        assert!(parse_insecure_registries("  ,  ").is_empty());
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
    fn registry_host_strips_namespace() {
        assert_eq!(registry_host("ghcr.io/acme"), "ghcr.io");
        assert_eq!(registry_host("localhost:5000"), "localhost:5000");
        assert_eq!(registry_host("localhost:5000/a/b"), "localhost:5000");
    }

    #[test]
    fn auth_for_unknown_registry_is_anonymous() {
        // No Docker config for a bogus host ⇒ anonymous, not an error.
        let auth = RegistryClient::auth_for("nonexistent.invalid");
        assert!(matches!(auth, RegistryAuth::Anonymous));
    }

    // ── token-gated catalog listing ──────────────────────────────────

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// A throwaway HTTP/1.1 registry that gates `_catalog` behind the OCI
    /// token handshake: `GET /v2/` answers `401` + a `Bearer` challenge
    /// whose realm is `/token`, `/token` mints a token, and `_catalog`
    /// returns the repository list only once that bearer token is
    /// presented. This exercises `oci-client`'s real `auth` path. The task
    /// runs until aborted by the test.
    async fn spawn_token_gated_registry(repositories_json: &str) -> (String, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let host = format!("127.0.0.1:{}", listener.local_addr().unwrap().port());
        let realm = format!("http://{host}/token");
        let catalog_body = format!(r#"{{"repositories":[{repositories_json}]}}"#);
        let handle = tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    return;
                };
                // Read the request head (GET has no body) to the blank line.
                let mut req = Vec::new();
                let mut buf = [0u8; 1024];
                loop {
                    let n = sock.read(&mut buf).await.unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    req.extend_from_slice(&buf[..n]);
                    if req.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                let req = String::from_utf8_lossy(&req).to_ascii_lowercase();
                let first = req.lines().next().unwrap_or("").to_string();
                let has_bearer = req.contains("authorization: bearer ");
                let ok_json = |body: &str| {
                    format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    )
                };
                let response = if first.contains("/token") {
                    ok_json(r#"{"token":"test-token"}"#)
                } else if first.contains("/v2/_catalog") && has_bearer {
                    // The caller terminates on a short page, so this single
                    // sub-PAGE_SIZE page ends the walk after one request. The
                    // `last=` branch stays faithful to a real registry (an
                    // exhausted cursor yields no repositories) as a backstop
                    // in case the walk ever issues a follow-up request.
                    if first.contains("last=") {
                        ok_json(r#"{"repositories":[]}"#)
                    } else {
                        ok_json(&catalog_body)
                    }
                } else {
                    // Unauthenticated `/v2/` or `/v2/_catalog` ⇒ challenge.
                    format!(
                        "HTTP/1.1 401 Unauthorized\r\nwww-authenticate: Bearer realm=\"{realm}\",service=\"reg\"\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
                    )
                };
                let _ = sock.write_all(response.as_bytes()).await;
                let _ = sock.shutdown().await;
            }
        });
        (host, handle)
    }

    /// Regression: a registry that gates `_catalog` behind a bearer token
    /// must be walked by reusing `oci-client`'s token handshake. Before the
    /// fix `list_catalog` issued a single unauthenticated GET, took the
    /// `401` as an unsupported endpoint, and returned an empty list — so
    /// `grim search`/`grim tui` showed nothing against a private registry
    /// while every other operation (via `oci-client`) authed fine.
    #[tokio::test]
    async fn list_catalog_authenticates_via_oci_client_token() {
        let (host, handle) = spawn_token_gated_registry(r#""glab","acme/code-review""#).await;
        let client = RegistryClient::with_plain_http(&host);
        let repos = client.list_catalog(&host).await.expect("catalog listing");
        assert_eq!(repos, vec!["glab".to_string(), "acme/code-review".to_string()]);
        handle.abort();
    }
}
