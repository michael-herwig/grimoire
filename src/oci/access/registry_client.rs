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
use oci_client::manifest::{OciDescriptor, OciImageManifest};
use oci_client::secrets::RegistryAuth;

use crate::oci::manifest::Descriptor;

/// The media type of a Grimoire artifact layer (single uncompressed tar).
const GRIMOIRE_LAYER_MEDIA_TYPE: &str = "application/vnd.grimoire.artifact.layer.v1.tar";
/// The media type of the OCI image config blob.
const OCI_CONFIG_MEDIA_TYPE: &str = "application/vnd.oci.image.config.v1+json";
/// The media type of an OCI image manifest.
const OCI_MANIFEST_MEDIA_TYPE: &str = "application/vnd.oci.image.manifest.v1+json";

/// Real OCI registry access backed by `oci-client`.
pub struct RegistryClient {
    client: Client,
    http: reqwest::Client,
}

impl RegistryClient {
    /// Construct a client for production use.
    ///
    /// Loopback registries (`localhost` / `127.0.0.1`, *any* port) are
    /// contacted over plain HTTP so a local test registry "just works" on
    /// whatever port it binds; any host listed in `GRIM_INSECURE_REGISTRIES`
    /// (comma-separated) is likewise plain HTTP; everything else uses HTTPS.
    ///
    /// `oci-client`'s `HttpsExcept` matches the registry by *exact*
    /// `host:port` string, so it cannot express "loopback on any port".
    /// The exception list is therefore materialised from the same
    /// [`Self::plain_http`] rule for the concrete registries known up front
    /// (the loopback defaults plus the `GRIM_INSECURE_REGISTRIES` entries);
    /// the raw catalog client uses the predicate directly.
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

        // Canonicalize the lookup key so a credential written by
        // `grim login` (scheme / `/vN` stripped, docker.io aliased) is
        // found here. Single source of truth: `auth::canonicalize_registry`.
        let registry = crate::auth::canonicalize_registry(registry);
        match docker_credential::get_credential(&registry) {
            Ok(DockerCredential::IdentityToken(token)) => Ok(RegistryAuth::Bearer(token)),
            Ok(DockerCredential::UsernamePassword(user, pass)) => Ok(RegistryAuth::Basic(user, pass)),
            // "No credential for this registry" is a benign anonymous-access
            // case, not an error. The patched `docker_credential` fork
            // surfaces a credential-helper miss as `NotFound` (upstream rolled
            // it into `HelperFailure`), so it joins the anonymous group too.
            Err(
                CredentialRetrievalError::NoCredentialConfigured
                | CredentialRetrievalError::ConfigNotFound
                | CredentialRetrievalError::ConfigReadError
                | CredentialRetrievalError::NotFound
                | CredentialRetrievalError::HelperFailure { .. },
            ) => Ok(RegistryAuth::Anonymous),
            Err(e) => Err(AccessErrorKind::Authentication(Box::new(e))),
        }
    }

    /// Whether `registry` is contacted over plain HTTP: any loopback host
    /// (`localhost` / `127.0.0.1`, *any* port) or a host explicitly listed
    /// in `GRIM_INSECURE_REGISTRIES`. Single source of truth for the
    /// HTTP/HTTPS decision — the `oci-client` `HttpsExcept` list in
    /// [`Self::new`] is materialised from this same rule.
    fn plain_http(registry: &str) -> bool {
        let host = registry.split(':').next().unwrap_or(registry);
        if host == "localhost" || host == "127.0.0.1" {
            return true;
        }
        insecure_registries().iter().any(|r| r == registry)
    }

    /// HTTP scheme for plain-HTTP registries (see [`Self::plain_http`]);
    /// HTTPS otherwise.
    fn scheme_for(registry: &str) -> &'static str {
        if Self::plain_http(registry) { "http" } else { "https" }
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
    }
}

impl RegistryClient {
    /// A tiny, deterministic OCI image config blob. Grimoire artifacts
    /// carry their real metadata in manifest annotations; the config blob
    /// only has to exist and be content-addressable.
    fn config_blob() -> Vec<u8> {
        br#"{"architecture":"","os":""}"#.to_vec()
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
        // to an empty list.
        //
        // The `_catalog` response is paginated: registries cap the page
        // size and advertise the next page via a `Link: <…>; rel="next"`
        // header (distribution spec §catalog). Follow the cursor, but only
        // up to a bounded number of pages: a registry can advertise tens
        // of thousands of repositories and walking every page is neither
        // fast nor useful for a *bounded* catalog (the catalog layer caps
        // and prefilters anyway). A truncated listing is an explicit
        // cut-line — the catalog is a best-effort index, not a mirror.
        const PAGE_SIZE: usize = 1000;
        const MAX_PAGES: usize = 8;

        #[derive(serde::Deserialize)]
        struct Catalog {
            #[serde(default)]
            repositories: Vec<String>,
        }

        // The `_catalog` endpoint lives on the bare registry HOST. A
        // configured default registry may carry a namespace
        // (`ghcr.io/acme`); using it verbatim would build the malformed URL
        // `https://ghcr.io/acme/v2/_catalog` and silently return nothing.
        // Extract the host for both the URL and the HTTP/HTTPS decision.
        let host = registry_host(registry);
        let scheme = Self::scheme_for(host);
        let mut next: Option<String> = Some(format!("{scheme}://{host}/v2/_catalog?n={PAGE_SIZE}"));
        let mut all: Vec<String> = Vec::new();
        let mut pages = 0;

        while let Some(url) = next.take() {
            pages += 1;
            if pages > MAX_PAGES {
                break;
            }
            let resp = match self.http.get(&url).send().await {
                Ok(r) => r,
                Err(e) => {
                    // A mid-walk transport failure on a *busy* catalog is
                    // not worth aborting search/tui for: return what was
                    // collected so far (degrade, never crash).
                    if all.is_empty() {
                        return Err(AccessError::without_identifier(AccessErrorKind::Registry(Box::new(e))));
                    }
                    break;
                }
            };
            if resp.status() == reqwest::StatusCode::NOT_FOUND || !resp.status().is_success() {
                // An unsupported endpoint on page 1 ⇒ empty catalog.
                break;
            }
            // Parse the `Link` header before consuming the body.
            let link_next = resp
                .headers()
                .get(reqwest::header::LINK)
                .and_then(|v| v.to_str().ok())
                .and_then(parse_next_link)
                .map(|rel| absolutize_link(scheme, host, &rel));
            match resp.json::<Catalog>().await {
                Ok(catalog) => {
                    if catalog.repositories.is_empty() {
                        break;
                    }
                    all.extend(catalog.repositories);
                }
                // A non-JSON / unexpected body from a registry that does
                // not implement the catalog is not a hard failure.
                Err(_) => break,
            }
            next = link_next;
        }
        Ok(all)
    }

    async fn push_blob(&self, repo: &Identifier, bytes: &[u8]) -> Result<Digest, AccessError> {
        let reference = reference_for(repo);
        let auth = Self::auth_for(repo.registry()).map_err(|kind| AccessError::with_identifier(repo.clone(), kind))?;
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
        let auth = Self::auth_for(repo.registry()).map_err(|kind| AccessError::with_identifier(repo.clone(), kind))?;
        let registry_ref = reference_for(repo);
        self.client
            .store_auth_if_needed(registry_ref.resolve_registry(), &auth)
            .await;

        // The config blob is pushed inline; Grimoire's real metadata lives
        // in the manifest annotations, not the config.
        let config = Self::config_blob();
        let config_digest = self.push_blob(repo, &config).await?;

        let image = OciImageManifest {
            schema_version: 2,
            media_type: Some(OCI_MANIFEST_MEDIA_TYPE.to_string()),
            config: OciDescriptor {
                media_type: OCI_CONFIG_MEDIA_TYPE.to_string(),
                digest: config_digest.to_string(),
                size: i64::try_from(config.len()).unwrap_or(i64::MAX),
                urls: None,
                annotations: None,
            },
            layers: manifest.layers.iter().map(oci_descriptor).collect(),
            subject: None,
            artifact_type: None,
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
        let auth = Self::auth_for(repo.registry()).map_err(|kind| AccessError::with_identifier(repo.clone(), kind))?;
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

/// Extract the `rel="next"` target from an RFC 8288 `Link` header.
///
/// A catalog `Link` looks like `</v2/_catalog?last=foo&n=100>; rel="next"`
/// (possibly comma-separated with other relations). Returns the raw URL
/// reference between the angle brackets of the `next` link, if present.
fn parse_next_link(header: &str) -> Option<String> {
    for part in header.split(',') {
        let part = part.trim();
        let (target, params) = part.split_once('>')?;
        let target = target.trim_start_matches('<');
        if params.split(';').any(|p| {
            let p = p.trim().replace(['"', ' '], "");
            p == "rel=next"
        }) {
            return Some(target.to_string());
        }
    }
    None
}

/// The bare registry host (first path segment) of a possibly-namespaced
/// registry string: `ghcr.io/acme` → `ghcr.io`; `localhost:5000` →
/// `localhost:5000`. The OCI distribution API (`/v2/_catalog`, auth scope)
/// is served by the host, not a host+namespace prefix.
fn registry_host(registry: &str) -> &str {
    registry.split_once('/').map_or(registry, |(host, _)| host)
}

/// Resolve a possibly-relative `Link` target against the registry origin.
/// Registries return an absolute-path reference (`/v2/_catalog?…`); a
/// fully-qualified URL is passed through unchanged.
fn absolutize_link(scheme: &str, registry: &str, link: &str) -> String {
    if link.starts_with("http://") || link.starts_with("https://") {
        link.to_string()
    } else if let Some(rest) = link.strip_prefix('/') {
        format!("{scheme}://{registry}/{rest}")
    } else {
        format!("{scheme}://{registry}/{link}")
    }
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

    /// Regression: a loopback registry on a non-default port (the manual
    /// rig moved off the shared :5000 to :5050) must still be plain HTTP.
    /// Before the fix the hardcoded `HttpsExcept` list only knew :5000, so
    /// :5050 went to HTTPS and failed the TLS handshake against `registry:2`.
    #[test]
    fn loopback_any_port_is_http() {
        assert_eq!(RegistryClient::scheme_for("localhost:5050"), "http");
        assert_eq!(RegistryClient::scheme_for("127.0.0.1:5050"), "http");
        assert!(RegistryClient::plain_http("localhost:5050"));
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
    fn parse_next_link_extracts_next_target() {
        let h = r#"</v2/_catalog?last=grim-test%2Fzz&n=100>; rel="next""#;
        assert_eq!(
            parse_next_link(h),
            Some("/v2/_catalog?last=grim-test%2Fzz&n=100".to_string())
        );
    }

    #[test]
    fn parse_next_link_ignores_other_relations_and_absent_next() {
        assert_eq!(parse_next_link(r#"</v2/_catalog?n=1>; rel="prev""#), None);
        assert_eq!(parse_next_link(""), None);
        // Multi-relation header: pick only the `next` one.
        let h = r#"</a>; rel="prev", </v2/_catalog?last=x>; rel="next""#;
        assert_eq!(parse_next_link(h), Some("/v2/_catalog?last=x".to_string()));
    }

    #[test]
    fn absolutize_link_handles_relative_and_absolute() {
        assert_eq!(
            absolutize_link("http", "localhost:5000", "/v2/_catalog?last=x"),
            "http://localhost:5000/v2/_catalog?last=x"
        );
        assert_eq!(
            absolutize_link("https", "ghcr.io", "https://ghcr.io/v2/_catalog?last=y"),
            "https://ghcr.io/v2/_catalog?last=y"
        );
        assert_eq!(
            absolutize_link("http", "localhost:5000", "v2/_catalog"),
            "http://localhost:5000/v2/_catalog"
        );
    }

    #[test]
    fn auth_for_unknown_registry_is_anonymous() {
        // No Docker config for a bogus host ⇒ anonymous, not an error.
        let auth = RegistryClient::auth_for("nonexistent.invalid").expect("anonymous fallback");
        assert!(matches!(auth, RegistryAuth::Anonymous));
    }
}
