// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! Persistent per-`(registry, repository)` tag → digest cache.
//!
//! Adapted from OCX `oci/index/local_index/tag_lock.rs`, simplified to a
//! standalone on-disk cache (no `Repository` domain type, no chained
//! index). One JSON file per repository at
//! `$GRIM_HOME/tags/<registry>/<repo>/tags.json`, version-enveloped via
//! `serde_repr` (an unknown version is rejected, never silently reset)
//! and written through the shared atomic-write primitive.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_repr::{Deserialize_repr, Serialize_repr};

use crate::oci::{Digest, Identifier};
use crate::store::atomic_write::atomic_write;

/// On-disk tag-cache envelope version.
///
/// Closed internal on-disk discriminant — not `#[non_exhaustive]`, per the
/// project convention. An unknown discriminant fails deserialization at
/// the `serde_repr` layer with no silent fallback.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize_repr, Deserialize_repr)]
#[repr(u8)]
pub enum TagCacheVersion {
    /// Version 1 of the on-disk format.
    V1 = 1,
}

/// Versioned envelope persisted at `tags/<registry>/<repo>/tags.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TagCacheFile {
    version: TagCacheVersion,
    registry: String,
    repository: String,
    /// Tag → resolved digest.
    tags: BTreeMap<String, Digest>,
}

/// A persistent tag → digest cache rooted at a `tags` directory.
#[derive(Debug, Clone)]
pub struct TagCache {
    root: PathBuf,
}

impl TagCache {
    /// Construct a cache rooted at `tags_dir` (see [`crate::store::GrimPaths::tags_dir`]).
    pub fn new(tags_dir: impl Into<PathBuf>) -> Self {
        Self { root: tags_dir.into() }
    }

    /// The `tags.json` path for `id`'s repository.
    fn file_for(&self, id: &Identifier) -> PathBuf {
        self.root
            .join(sanitize(id.registry()))
            .join(sanitize(id.repository()))
            .join("tags.json")
    }

    /// Read and validate the cache file for `id`'s repository.
    ///
    /// A missing file is `Ok(None)`. A corrupt or wrong-version file is an
    /// error so a stale/incompatible cache surfaces rather than silently
    /// behaving as a cold cache.
    ///
    /// # Errors
    ///
    /// Returns an [`std::io::Error`] for read failures and a parse/version
    /// rejection mapped to [`std::io::ErrorKind::InvalidData`].
    fn read_file(&self, id: &Identifier) -> std::io::Result<Option<TagCacheFile>> {
        let path = self.file_for(id);
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        let file: TagCacheFile =
            serde_json::from_slice(&bytes).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        Ok(Some(file))
    }

    /// The cached digest for `id`'s exact tag, if any.
    ///
    /// A digest-addressed identifier is immutable and returns its own
    /// digest without touching disk. An identifier with neither tag nor
    /// digest is treated as `latest`.
    ///
    /// # Errors
    ///
    /// Propagates a read/parse failure from [`Self::read_file`].
    pub fn get(&self, id: &Identifier) -> std::io::Result<Option<Digest>> {
        if let Some(digest) = id.digest() {
            return Ok(Some(digest));
        }
        let tag = id.tag_or_latest().to_string();
        Ok(self.read_file(id)?.and_then(|f| f.tags.get(&tag).cloned()))
    }

    /// Insert/update the digest for `id`'s tag and persist atomically.
    ///
    /// A digest-addressed identifier carries no mutable tag pointer and is
    /// a no-op (the pin is already immutable).
    ///
    /// # Errors
    ///
    /// Propagates read, serialize, or atomic-write I/O failures.
    pub fn put(&self, id: &Identifier, digest: &Digest) -> std::io::Result<()> {
        if id.digest().is_some() {
            return Ok(());
        }
        let tag = id.tag_or_latest().to_string();
        let mut file = self.read_file(id)?.unwrap_or_else(|| TagCacheFile {
            version: TagCacheVersion::V1,
            registry: id.registry().to_string(),
            repository: id.repository().to_string(),
            tags: BTreeMap::new(),
        });
        file.tags.insert(tag, digest.clone());
        self.write_file(id, &file)
    }

    /// All cached tags for `id`'s repository, if the file exists.
    ///
    /// # Errors
    ///
    /// Propagates a read/parse failure from [`Self::read_file`].
    pub fn get_tags(&self, id: &Identifier) -> std::io::Result<Option<Vec<String>>> {
        Ok(self.read_file(id)?.map(|f| f.tags.keys().cloned().collect::<Vec<_>>()))
    }

    /// Replace the cached tag set for `id`'s repository.
    ///
    /// Tags without a known digest are recorded with no pointer is not
    /// possible (the map is tag → digest); `put_tags` therefore only
    /// records names it can pair, leaving digest resolution to `put`.
    /// In practice this is used to seed the *names* a `list_tags` call
    /// discovered; digests fill in lazily via `put` on resolve. Names
    /// already carrying a digest entry are preserved.
    ///
    /// # Errors
    ///
    /// Propagates read, serialize, or atomic-write I/O failures.
    pub fn put_tags(&self, id: &Identifier, tags: &[String]) -> std::io::Result<()> {
        let mut file = self.read_file(id)?.unwrap_or_else(|| TagCacheFile {
            version: TagCacheVersion::V1,
            registry: id.registry().to_string(),
            repository: id.repository().to_string(),
            tags: BTreeMap::new(),
        });
        // Drop digest entries for tags no longer present; keep digests for
        // tags still listed so a subsequent `get` stays a cache hit.
        file.tags.retain(|name, _| tags.iter().any(|t| t == name));
        self.write_file(id, &file)
    }

    fn write_file(&self, id: &Identifier, file: &TagCacheFile) -> std::io::Result<()> {
        let path = self.file_for(id);
        let bytes =
            serde_json::to_vec_pretty(file).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        atomic_write(&path, &bytes)
    }
}

/// Strip path separators and traversal segments from a registry/repo
/// component so it can be a safe directory name. `Identifier` already
/// rejects `.`/`..` segments at parse time, but defence-in-depth keeps a
/// hand-built identifier from escaping the tags root.
fn sanitize(component: &str) -> String {
    component.replace(['/', '\\'], "_").replace("..", "_")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oci::Digest;

    fn cache() -> (tempfile::TempDir, TagCache) {
        let dir = tempfile::tempdir().unwrap();
        let cache = TagCache::new(dir.path().join("tags"));
        (dir, cache)
    }

    fn sha(b: char) -> Digest {
        Digest::Sha256(std::iter::repeat_n(b, 64).collect())
    }

    #[test]
    fn round_trip_get_put() {
        let (_d, cache) = cache();
        let id = Identifier::parse("ghcr.io/acme/code-review:stable").unwrap();
        assert_eq!(cache.get(&id).unwrap(), None);
        cache.put(&id, &sha('a')).unwrap();
        assert_eq!(cache.get(&id).unwrap(), Some(sha('a')));
    }

    #[test]
    fn distinct_tags_isolated() {
        let (_d, cache) = cache();
        let stable = Identifier::parse("ghcr.io/acme/x:stable").unwrap();
        let edge = Identifier::parse("ghcr.io/acme/x:edge").unwrap();
        cache.put(&stable, &sha('a')).unwrap();
        cache.put(&edge, &sha('b')).unwrap();
        assert_eq!(cache.get(&stable).unwrap(), Some(sha('a')));
        assert_eq!(cache.get(&edge).unwrap(), Some(sha('b')));
        let mut tags = cache.get_tags(&stable).unwrap().unwrap();
        tags.sort();
        assert_eq!(tags, vec!["edge".to_string(), "stable".to_string()]);
    }

    #[test]
    fn digest_addressed_get_is_immutable_no_io() {
        let (_d, cache) = cache();
        let id = Identifier::parse(&format!("ghcr.io/acme/x@sha256:{}", "a".repeat(64))).unwrap();
        // No file written; the digest comes straight from the identifier.
        assert_eq!(cache.get(&id).unwrap(), Some(sha('a')));
        cache.put(&id, &sha('b')).unwrap();
        // put is a no-op for a pinned id — still no file.
        assert!(!cache.file_for(&id).exists());
    }

    #[test]
    fn bare_identifier_uses_latest() {
        let (_d, cache) = cache();
        let bare = Identifier::parse_with_default_registry("acme/x", "ghcr.io").unwrap();
        cache.put(&bare, &sha('a')).unwrap();
        let latest = Identifier::parse("ghcr.io/acme/x:latest").unwrap();
        assert_eq!(cache.get(&latest).unwrap(), Some(sha('a')));
    }

    #[test]
    fn rejects_unknown_version() {
        let (_d, cache) = cache();
        let id = Identifier::parse("ghcr.io/acme/x:stable").unwrap();
        let path = cache.file_for(&id);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            r#"{"version":99,"registry":"ghcr.io","repository":"acme/x","tags":{}}"#,
        )
        .unwrap();
        let err = cache.get(&id).expect_err("unknown version must reject");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    }

    #[test]
    fn put_tags_prunes_stale_digests() {
        let (_d, cache) = cache();
        let stable = Identifier::parse("ghcr.io/acme/x:stable").unwrap();
        let edge = Identifier::parse("ghcr.io/acme/x:edge").unwrap();
        cache.put(&stable, &sha('a')).unwrap();
        cache.put(&edge, &sha('b')).unwrap();
        // Registry now only reports `stable`.
        cache.put_tags(&stable, &["stable".to_string()]).unwrap();
        assert_eq!(cache.get(&stable).unwrap(), Some(sha('a')));
        assert_eq!(cache.get(&edge).unwrap(), None);
    }

    #[test]
    fn write_is_atomic_round_trip() {
        // The atomic-write primitive is unit-tested in store::atomic_write;
        // here we only assert the persisted file reloads byte-faithfully.
        let (_d, cache) = cache();
        let id = Identifier::parse("ghcr.io/acme/x:stable").unwrap();
        cache.put(&id, &sha('c')).unwrap();
        let reloaded = TagCache::new(cache.root.clone());
        assert_eq!(reloaded.get(&id).unwrap(), Some(sha('c')));
    }
}
