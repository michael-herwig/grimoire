// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! The one cache layer wrapping an inner [`OciAccess`].
//!
//! Adapted from OCX `oci/index.rs` `ChainedIndex` + `ChainMode`, collapsed
//! to a single source plus a persistent [`TagCache`] and the
//! content-addressed [`BlobStore`]. There is no chained source list and no
//! platform-candidate handling — the routing matrix below is the whole
//! policy.
//!
//! | mode    | tag resolve (Query)        | tag resolve (Resolve)            | blob fetch                 |
//! |---------|----------------------------|----------------------------------|----------------------------|
//! | Offline | cache; miss ⇒ `Ok(None)`   | cache; miss ⇒ `OfflineMiss`      | store; miss ⇒ `OfflineMiss`|
//! | Remote  | inner; then write cache    | inner; then write cache          | store; miss ⇒ inner ⇒ store|
//! | Default | cache; miss ⇒ inner; write | cache; miss ⇒ inner; write       | store; miss ⇒ inner ⇒ store|
//!
//! "write cache" for tag pointers only happens on `Resolve` in `Default`
//! mode (a pure `Query` must not persist a pin), matching OCX semantics.

use async_trait::async_trait;

use super::super::access::{AccessMode, OciAccess, Operation};
use super::error::{AccessError, AccessErrorKind};
use crate::oci::manifest::OciManifest;
use crate::oci::tag_cache::TagCache;
use crate::oci::{Digest, Identifier, PinnedIdentifier};
use crate::store::BlobStore;

/// An [`OciAccess`] that consults a persistent tag cache and blob store
/// before falling through to `inner`, routed by [`AccessMode`].
pub struct CachedAccess<A: OciAccess> {
    inner: A,
    tags: TagCache,
    blobs: BlobStore,
    mode: AccessMode,
}

impl<A: OciAccess> CachedAccess<A> {
    /// Wrap `inner` with the given cache, blob store, and routing mode.
    pub fn new(inner: A, tags: TagCache, blobs: BlobStore, mode: AccessMode) -> Self {
        Self {
            inner,
            tags,
            blobs,
            mode,
        }
    }

    fn io_err(id: &Identifier, source: std::io::Error) -> AccessError {
        AccessError::with_identifier(id.clone(), AccessErrorKind::Io { path: None, source })
    }
}

#[async_trait]
impl<A: OciAccess> OciAccess for CachedAccess<A> {
    async fn resolve_digest(&self, id: &Identifier, op: Operation) -> Result<Option<Digest>, AccessError> {
        // Digest-addressed input is immutable — no cache, no I/O.
        if let Some(digest) = id.digest() {
            return Ok(Some(digest));
        }

        match self.mode {
            AccessMode::Offline => {
                if let Some(digest) = self.tags.get(id).map_err(|e| Self::io_err(id, e))? {
                    return Ok(Some(digest));
                }
                match op {
                    Operation::Resolve => Err(AccessError::with_identifier(id.clone(), AccessErrorKind::OfflineMiss)),
                    Operation::Query => Ok(None),
                }
            }
            AccessMode::Remote => {
                // Skip the cache read; go straight to the source, then
                // persist a successful pin.
                match self.inner.resolve_digest(id, op).await? {
                    Some(digest) => {
                        if op == Operation::Resolve {
                            self.tags.put(id, &digest).map_err(|e| Self::io_err(id, e))?;
                        }
                        Ok(Some(digest))
                    }
                    None => Ok(None),
                }
            }
            AccessMode::Default => {
                if let Some(digest) = self.tags.get(id).map_err(|e| Self::io_err(id, e))? {
                    return Ok(Some(digest));
                }
                match self.inner.resolve_digest(id, op).await? {
                    Some(digest) => {
                        // Persist the pin only for a resolve; a pure query
                        // must not write a tag pointer.
                        if op == Operation::Resolve {
                            self.tags.put(id, &digest).map_err(|e| Self::io_err(id, e))?;
                        }
                        Ok(Some(digest))
                    }
                    None => Ok(None),
                }
            }
        }
    }

    async fn fetch_manifest(&self, id: &PinnedIdentifier) -> Result<Option<OciManifest>, AccessError> {
        if self.mode == AccessMode::Offline {
            // No manifest cache in Phase 3 — a manifest fetch always needs
            // the network, which offline forbids.
            return Err(AccessError::with_identifier(
                id.as_identifier().clone(),
                AccessErrorKind::OfflineMiss,
            ));
        }
        self.inner.fetch_manifest(id).await
    }

    async fn fetch_blob(&self, repo: &Identifier, digest: &Digest) -> Result<Option<Vec<u8>>, AccessError> {
        // Content-addressed: a present blob is authoritative.
        match self.blobs.get(digest) {
            Ok(Some(bytes)) => return Ok(Some(bytes)),
            Ok(None) => {}
            Err(e) => return Err(Self::io_err(repo, e)),
        }

        if self.mode == AccessMode::Offline {
            return Err(AccessError::with_identifier(repo.clone(), AccessErrorKind::OfflineMiss));
        }

        let Some(bytes) = self.inner.fetch_blob(repo, digest).await? else {
            return Ok(None);
        };

        // `RegistryClient` already digest-verifies; `BlobStore::put` also
        // verifies before writing, so a corrupt blob can never be cached.
        // Map the verification failure into the access taxonomy.
        if let Err(e) = self.blobs.put(digest, &bytes) {
            if e.kind() == std::io::ErrorKind::InvalidData {
                let actual = digest.algorithm().hash(&bytes);
                return Err(AccessError::with_identifier(
                    repo.clone(),
                    AccessErrorKind::DigestMismatch {
                        expected: digest.clone(),
                        actual,
                    },
                ));
            }
            return Err(Self::io_err(repo, e));
        }
        Ok(Some(bytes))
    }

    async fn list_tags(&self, id: &Identifier) -> Result<Option<Vec<String>>, AccessError> {
        if self.mode == AccessMode::Offline {
            return self.tags.get_tags(id).map_err(|e| Self::io_err(id, e));
        }
        match self.inner.list_tags(id).await? {
            Some(tags) => {
                self.tags.put_tags(id, &tags).map_err(|e| Self::io_err(id, e))?;
                Ok(Some(tags))
            }
            None => Ok(None),
        }
    }

    async fn list_catalog(&self, registry: &str) -> Result<Vec<String>, AccessError> {
        if self.mode == AccessMode::Offline {
            // No persistent catalog cache in Phase 3 — degrade to empty.
            return Ok(Vec::new());
        }
        self.inner.list_catalog(registry).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oci::Algorithm;
    use crate::store::GrimPaths;
    use std::sync::Arc;
    use std::sync::Mutex;

    /// Minimal scripted mock used only for the cache-routing tests. The
    /// shared resolver-level `MockAccess` lives in `resolve/resolver.rs`;
    /// duplicating a tiny one here keeps this module's tests self-contained
    /// (DAMP over DRY for test scaffolding, per quality-core).
    struct CountingInner {
        digest: Digest,
        calls: Arc<Mutex<usize>>,
        blob: Vec<u8>,
    }

    #[async_trait]
    impl OciAccess for CountingInner {
        async fn resolve_digest(&self, _id: &Identifier, _op: Operation) -> Result<Option<Digest>, AccessError> {
            *self.calls.lock().unwrap() += 1;
            Ok(Some(self.digest.clone()))
        }
        async fn fetch_manifest(&self, _id: &PinnedIdentifier) -> Result<Option<OciManifest>, AccessError> {
            Ok(None)
        }
        async fn fetch_blob(&self, _repo: &Identifier, _digest: &Digest) -> Result<Option<Vec<u8>>, AccessError> {
            *self.calls.lock().unwrap() += 1;
            Ok(Some(self.blob.clone()))
        }
        async fn list_tags(&self, _id: &Identifier) -> Result<Option<Vec<String>>, AccessError> {
            Ok(Some(vec!["stable".to_string()]))
        }
        async fn list_catalog(&self, _registry: &str) -> Result<Vec<String>, AccessError> {
            Ok(vec!["acme/x".to_string()])
        }
    }

    fn paths() -> (tempfile::TempDir, TagCache, BlobStore) {
        let dir = tempfile::tempdir().unwrap();
        let p = GrimPaths::new(dir.path().join("home"));
        p.ensure_layout().unwrap();
        (dir, TagCache::new(p.tags_dir()), BlobStore::new(p.blobs_dir()))
    }

    fn id() -> Identifier {
        Identifier::parse("ghcr.io/acme/x:stable").unwrap()
    }

    #[tokio::test]
    async fn offline_cold_resolve_is_offline_miss() {
        let (_d, tags, blobs) = paths();
        let calls = Arc::new(Mutex::new(0));
        let inner = CountingInner {
            digest: Algorithm::Sha256.hash(b"d"),
            calls: calls.clone(),
            blob: vec![],
        };
        let access = CachedAccess::new(inner, tags, blobs, AccessMode::Offline);
        let err = access
            .resolve_digest(&id(), Operation::Resolve)
            .await
            .expect_err("cold offline resolve must fail");
        assert!(matches!(err.kind, AccessErrorKind::OfflineMiss));
        assert_eq!(*calls.lock().unwrap(), 0, "inner must not be contacted offline");
    }

    #[tokio::test]
    async fn offline_cold_query_is_none() {
        let (_d, tags, blobs) = paths();
        let calls = Arc::new(Mutex::new(0));
        let inner = CountingInner {
            digest: Algorithm::Sha256.hash(b"d"),
            calls: calls.clone(),
            blob: vec![],
        };
        let access = CachedAccess::new(inner, tags, blobs, AccessMode::Offline);
        assert_eq!(access.resolve_digest(&id(), Operation::Query).await.unwrap(), None);
        assert_eq!(*calls.lock().unwrap(), 0);
    }

    #[tokio::test]
    async fn warm_tag_cache_hit_skips_inner() {
        let (_d, tags, blobs) = paths();
        let digest = Algorithm::Sha256.hash(b"d");
        tags.put(&id(), &digest).unwrap();
        let calls = Arc::new(Mutex::new(0));
        let inner = CountingInner {
            digest: digest.clone(),
            calls: calls.clone(),
            blob: vec![],
        };
        let access = CachedAccess::new(inner, tags, blobs, AccessMode::Default);
        assert_eq!(
            access.resolve_digest(&id(), Operation::Resolve).await.unwrap(),
            Some(digest)
        );
        assert_eq!(*calls.lock().unwrap(), 0, "warm cache must not call inner");
    }

    #[tokio::test]
    async fn digest_addressed_input_does_no_io() {
        let (_d, tags, blobs) = paths();
        let digest = Algorithm::Sha256.hash(b"d");
        let pinned = Identifier::parse(&format!("ghcr.io/acme/x@sha256:{}", digest.hex())).unwrap();
        let calls = Arc::new(Mutex::new(0));
        let inner = CountingInner {
            digest: digest.clone(),
            calls: calls.clone(),
            blob: vec![],
        };
        let access = CachedAccess::new(inner, tags, blobs, AccessMode::Offline);
        // Even offline + Resolve resolves with no error and no inner call.
        assert_eq!(
            access.resolve_digest(&pinned, Operation::Resolve).await.unwrap(),
            Some(digest)
        );
        assert_eq!(*calls.lock().unwrap(), 0);
    }

    #[tokio::test]
    async fn default_resolve_writes_cache_query_does_not() {
        let (_d, tags, blobs) = paths();
        let digest = Algorithm::Sha256.hash(b"d");
        let calls = Arc::new(Mutex::new(0));
        let inner = CountingInner {
            digest: digest.clone(),
            calls: calls.clone(),
            blob: vec![],
        };
        let access = CachedAccess::new(inner, tags.clone(), blobs, AccessMode::Default);

        // Query: must not persist a pin.
        assert_eq!(
            access.resolve_digest(&id(), Operation::Query).await.unwrap(),
            Some(digest.clone())
        );
        assert_eq!(tags.get(&id()).unwrap(), None, "query must not write the cache");

        // Resolve: persists.
        assert_eq!(
            access.resolve_digest(&id(), Operation::Resolve).await.unwrap(),
            Some(digest.clone())
        );
        assert_eq!(tags.get(&id()).unwrap(), Some(digest));
    }

    #[tokio::test]
    async fn blob_cache_hit_skips_inner() {
        let (_d, tags, blobs) = paths();
        let payload = b"artifact tar".to_vec();
        let digest = Algorithm::Sha256.hash(&payload);
        blobs.put(&digest, &payload).unwrap();
        let calls = Arc::new(Mutex::new(0));
        let inner = CountingInner {
            digest: digest.clone(),
            calls: calls.clone(),
            blob: payload.clone(),
        };
        let access = CachedAccess::new(inner, tags, blobs, AccessMode::Default);
        assert_eq!(access.fetch_blob(&id(), &digest).await.unwrap(), Some(payload));
        assert_eq!(*calls.lock().unwrap(), 0, "warm blob cache must not call inner");
    }

    #[tokio::test]
    async fn blob_digest_mismatch_is_error() {
        let (_d, tags, blobs) = paths();
        // The inner returns bytes that do NOT hash to the requested digest.
        let requested = Algorithm::Sha256.hash(b"expected");
        let calls = Arc::new(Mutex::new(0));
        let inner = CountingInner {
            digest: requested.clone(),
            calls: calls.clone(),
            blob: b"corrupt".to_vec(),
        };
        let access = CachedAccess::new(inner, tags, blobs, AccessMode::Default);
        let err = access
            .fetch_blob(&id(), &requested)
            .await
            .expect_err("mismatched blob must error");
        assert!(matches!(err.kind, AccessErrorKind::DigestMismatch { .. }));
    }

    #[tokio::test]
    async fn offline_blob_cold_is_offline_miss() {
        let (_d, tags, blobs) = paths();
        let digest = Algorithm::Sha256.hash(b"d");
        let calls = Arc::new(Mutex::new(0));
        let inner = CountingInner {
            digest: digest.clone(),
            calls: calls.clone(),
            blob: vec![],
        };
        let access = CachedAccess::new(inner, tags, blobs, AccessMode::Offline);
        let err = access
            .fetch_blob(&id(), &digest)
            .await
            .expect_err("cold offline blob must fail");
        assert!(matches!(err.kind, AccessErrorKind::OfflineMiss));
    }
}
