// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! The persisted registry catalog.
//!
//! One entry per repository in a *bounded, query-scoped* window: its
//! kind, a short description, keywords, the chosen "latest"-ish tag, and
//! when it was fetched. Built exclusively through the [`OciAccess`] seam —
//! list the catalog, prefilter the repository names cheaply (no network),
//! cap at [`MAX_CATALOG_REPOS`], then for each survivor list its tags,
//! pick a representative tag, and read the Grimoire/OCI annotations off
//! that tag's manifest. No blob is ever pulled (catalog is metadata-only).
//! Walking an entire (potentially huge) registry is an explicit cut-line.
//!
//! The per-registry cache file at `$GRIM_HOME/catalog/<hash>.json` is
//! version-enveloped via
//! `serde_repr` (an unknown version is rejected, never silently reset) and
//! written through the shared atomic-write primitive. A 1 hour TTL governs
//! freshness; `--refresh` forces a rebuild. Offline never errors here: a
//! cached catalog is served as-is (marked stale by its age), and a cold
//! cache yields an empty catalog rather than a failure.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_repr::{Deserialize_repr, Serialize_repr};

use crate::lock::advisory_lock::AdvisoryFileLock;
use crate::oci::access::OciAccess;
use crate::oci::{Identifier, PinnedIdentifier};
use crate::store::atomic_write::atomic_write;

use super::catalog_error::CatalogError;
use super::search_match::SearchQuery;

/// Catalog freshness window: a catalog older than this is stale and is
/// rebuilt on the next online `search`/`tui` (offline still serves it).
pub const CATALOG_TTL_SECONDS: i64 = 3600;

/// Hard cap on repositories metadata-fetched in one catalog build.
///
/// A registry's `_catalog` can list tens of thousands of repositories;
/// fetching three round-trips of metadata for every one is infeasible (a
/// real registry, not just the shared test one). The catalog is therefore
/// *bounded*: a targeted query first prefilters the repository list by
/// name (cheap, no network) so a search stays O(matches); an empty query
/// (browse / TUI) caps to the first `MAX_CATALOG_REPOS` lexicographic
/// repositories. Walking the entire registry is an explicit cut-line.
pub const MAX_CATALOG_REPOS: usize = 500;

/// Registries that gate the host-level OCI `_catalog` browse endpoint, so a
/// `grim search` browse against them legitimately returns nothing. Single
/// source of truth for the user-facing list shared by `grim search` (stderr
/// warning) and the TUI (status line).
pub const CATALOG_GATED_REGISTRIES: &str = "GitLab SaaS, GHCR, Docker Hub";

/// Docs anchor for the registry-compatibility table (which registries support
/// `_catalog` browse vs. explicit-reference operations). Single source of truth
/// for the link emitted by `grim search` and the TUI.
pub const REGISTRY_COMPAT_DOCS_URL: &str = "https://grimoire.rs/configuration.html#registry-compatibility";

/// On-disk catalog envelope version.
///
/// Closed internal on-disk discriminant — not `#[non_exhaustive]`, per the
/// project convention. An unknown discriminant fails deserialization at
/// the `serde_repr` layer with no silent fallback.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize_repr, Deserialize_repr)]
#[repr(u8)]
pub enum CatalogVersion {
    /// Version 1 of the on-disk format.
    V1 = 1,
}

/// One repository's catalog record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogEntry {
    /// The registry host the repository lives on.
    pub registry: String,
    /// The repository path within the registry.
    pub repository: String,
    /// The artifact kind from the OCI `artifactType` (`skill`/`rule`/`bundle`),
    /// if the manifest declared it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// `org.opencontainers.image.description`, if present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// `com.grimoire.summary`, a short single-line blurb shown in the
    /// catalog, if present. Distinct from the longer `description`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// `com.grimoire.keywords` split on commas (trimmed, empties dropped).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keywords: Vec<String>,
    /// `org.opencontainers.image.source`, kept only when it is an HTTPS
    /// repository URL (older artifacts carry a non-URL release ref there;
    /// the prefix guard drops those instead of surfacing garbage).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository_url: Option<String>,
    /// `org.opencontainers.image.revision` — the publishing commit SHA (a
    /// `-dirty` suffix marks an uncommitted working tree), present only when
    /// the artifact was released with `--git`. Surfaced in the TUI detail pane.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
    /// `org.opencontainers.image.created` — the publishing commit date
    /// (RFC3339), present only when released with `--git`. Surfaced alongside
    /// the revision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created: Option<String>,
    /// `com.grimoire.deprecated`, the publisher's deprecation message when
    /// the representative tag's manifest marks the artifact deprecated.
    /// `None` when not deprecated. Surfaced as the search / TUI highlight.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deprecated: Option<String>,
    /// The representative tag the metadata was read from (may be the
    /// moving `latest` pointer).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_tag: Option<String>,
    /// The highest *concrete* semver tag, if any tag parses as semver.
    /// Distinct from [`Self::latest_tag`]: this never returns the moving
    /// `latest` pointer, so the UI can show an explicit version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// RFC3339 UTC timestamp this entry was fetched.
    pub fetched_at: String,
}

impl CatalogEntry {
    /// The fully-qualified `registry/repository` reference.
    pub fn repo(&self) -> String {
        format!("{}/{}", self.registry, self.repository)
    }

    /// Whether `query` matches this entry, delegating to the shared
    /// [`SearchQuery`] matcher (AND-of-terms over kind / repo / summary /
    /// description / keywords, plus bare kind-keyword filters). The query is
    /// parsed once by the caller so each catalog row costs no re-parse.
    pub fn matches(&self, query: &SearchQuery) -> bool {
        query.matches_fields(
            self.kind.as_deref(),
            &self.repo(),
            self.summary.as_deref().unwrap_or(""),
            self.description.as_deref().unwrap_or(""),
            &self.keywords,
        )
    }
}

/// Versioned envelope persisted per registry at `$GRIM_HOME/catalog/<hash>.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CatalogFile {
    version: CatalogVersion,
    /// The registry the catalog was built for.
    registry: String,
    /// The lowercased name-prefilter this catalog was built under (empty
    /// = a capped browse window). A cache built for a different scope is
    /// rebuilt online, but still served (filtered) offline.
    #[serde(default)]
    scope: String,
    /// Whether the build hit [`MAX_CATALOG_REPOS`] with candidates left over
    /// — i.e. this window is incomplete and a non-empty query may be missing
    /// matches past the cap. Persisted so an offline-served cache still warns.
    #[serde(default)]
    truncated: bool,
    /// RFC3339 UTC timestamp of the last (re)build.
    built_at: String,
    /// Entries keyed by repository path for stable, deduplicated output.
    entries: BTreeMap<String, CatalogEntry>,
}

/// An in-memory catalog for one registry.
#[derive(Debug, Clone)]
pub struct Catalog {
    registry: String,
    /// The lowercased name-prefilter this catalog was built under.
    scope: String,
    /// Whether the build was truncated at [`MAX_CATALOG_REPOS`] (see
    /// [`Self::truncated`]).
    truncated: bool,
    built_at: String,
    entries: BTreeMap<String, CatalogEntry>,
}

/// The outcome of the coordinated refresh's blocking pre-flight: either a
/// catalog ready to serve, or a decision to rebuild (with the advisory lock
/// when one was won — `None` is the uncoordinated fallback after a lock I/O
/// fault).
enum PreFlight {
    Serve(Catalog),
    Rebuild(Option<AdvisoryFileLock>),
}

/// Run a blocking catalog op on the blocking pool, surfacing a panic via
/// `resume_unwind` (a panic is a bug, not a cache condition) and mapping a
/// genuine task cancellation to an I/O error keyed to `path`. Mirrors the
/// `auth::store` blocking bridge.
async fn run_blocking<T, F>(path: PathBuf, f: F) -> Result<T, CatalogError>
where
    F: FnOnce() -> Result<T, CatalogError> + Send + 'static,
    T: Send + 'static,
{
    match tokio::task::spawn_blocking(f).await {
        Ok(result) => result,
        Err(join) if join.is_panic() => std::panic::resume_unwind(join.into_panic()),
        Err(join) => Err(CatalogError::io(&path, std::io::Error::other(join))),
    }
}

impl Catalog {
    /// An empty catalog for `registry` (cold cache / offline miss).
    pub fn empty(registry: &str) -> Self {
        Self {
            registry: registry.to_string(),
            scope: String::new(),
            truncated: false,
            built_at: now_rfc3339(),
            entries: BTreeMap::new(),
        }
    }

    /// The registry this catalog indexes.
    pub fn registry(&self) -> &str {
        &self.registry
    }

    /// When this catalog was last fully built (RFC3339).
    pub fn built_at(&self) -> &str {
        &self.built_at
    }

    /// Whether the build hit [`MAX_CATALOG_REPOS`] with more candidates left
    /// unwalked. When `true` the window is a *prefix* of the matching
    /// repositories, so a non-empty query filtered in memory may be missing
    /// matches that live past the cap — callers should surface a truncation
    /// hint (the `search` report / TUI status line) so results are not read as
    /// exhaustive. `false` means the whole candidate set fit within the cap.
    pub fn truncated(&self) -> bool {
        self.truncated
    }

    /// Entries sorted by repository path.
    pub fn entries(&self) -> impl Iterator<Item = &CatalogEntry> {
        self.entries.values()
    }

    /// Whether the catalog is within the freshness window relative to
    /// `now` (an RFC3339 instant). An unparseable timestamp is treated as
    /// stale so a corrupt clock cannot pin a cache forever.
    pub fn is_fresh(&self, now: chrono::DateTime<chrono::Utc>) -> bool {
        is_fresh_at(&self.built_at, now)
    }

    /// Load the catalog from `path`, if present and for `registry`.
    ///
    /// A missing file or a file built for a different registry yields
    /// `Ok(None)` (treat as a cold cache, not an error). A corrupt or
    /// unknown-version file is an error so a stale/incompatible cache
    /// surfaces rather than silently behaving as cold.
    ///
    /// # Errors
    ///
    /// [`CatalogError`] for a read failure or a parse / version rejection.
    pub fn load(path: &Path, registry: &str) -> Result<Option<Self>, CatalogError> {
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(CatalogError::io(path, e)),
        };
        let file: CatalogFile = serde_json::from_slice(&bytes).map_err(|e| CatalogError::parse(path, e))?;
        if file.registry != registry {
            return Ok(None);
        }
        Ok(Some(Self {
            registry: file.registry,
            scope: file.scope,
            truncated: file.truncated,
            built_at: file.built_at,
            entries: file.entries,
        }))
    }

    /// Atomically persist the catalog to `path`.
    ///
    /// # Errors
    ///
    /// Serialization or atomic-write I/O failure.
    pub fn save(&self, path: &Path) -> Result<(), CatalogError> {
        let file = CatalogFile {
            version: CatalogVersion::V1,
            registry: self.registry.clone(),
            scope: self.scope.clone(),
            truncated: self.truncated,
            built_at: self.built_at.clone(),
            entries: self.entries.clone(),
        };
        let bytes = serde_json::to_vec_pretty(&file).map_err(|e| CatalogError::parse(path, e))?;
        atomic_write(path, &bytes).map_err(|e| CatalogError::io(path, e))
    }

    /// Load a fresh, query-scoped cached catalog, or (re)build it.
    ///
    /// `query` is a case-insensitive repository-name prefilter (empty =
    /// capped browse window). The cache records the scope it was built
    /// under; reuse requires the same registry AND scope AND freshness.
    ///
    /// Resolution policy:
    /// - A cached catalog that is fresh, same-scope, and not `force`d is
    ///   returned.
    /// - Offline: whatever is cached is returned as-is (regardless of
    ///   scope — degrade, never reach the network, never error). A cold
    ///   cache yields an empty catalog.
    /// - Online: a stale / different-scope / absent cache (or `force`)
    ///   triggers a bounded rebuild, which is then persisted.
    ///
    /// # Errors
    ///
    /// [`CatalogError`] for a cache parse/version failure, or a genuine
    /// registry transport/auth failure during an online rebuild.
    pub async fn load_or_refresh(
        path: &Path,
        registry: &str,
        query: &str,
        access: &std::sync::Arc<dyn OciAccess>,
        offline: bool,
        force: bool,
    ) -> Result<Self, CatalogError> {
        let scope = query.to_lowercase();
        let cached = Self::load(path, registry)?;

        if offline {
            // Degrade: serve whatever is cached (any scope), never reach
            // the network, never error. A cold cache is empty.
            return Ok(cached.unwrap_or_else(|| Self::empty(registry)));
        }

        if !force
            && let Some(c) = &cached
            && c.scope == scope
            && c.is_fresh(chrono::Utc::now())
        {
            return Ok(cached.unwrap_or_else(|| Self::empty(registry)));
        }

        let rebuilt = Self::build(registry, &scope, access)
            .await
            .map_err(|e| CatalogError::access(path, e))?;
        rebuilt.save(path)?;
        Ok(rebuilt)
    }

    /// Like [`Self::load_or_refresh`], but coordinates the rebuild across
    /// concurrent processes via an advisory lock on the cache file so a long-
    /// lived MCP server, a CLI invocation, and the TUI sharing one
    /// `$GRIM_HOME` do not each walk the registry at once.
    ///
    /// The fresh-cache path takes **no lock** — readers are never blocked.
    /// When a refresh is needed the rebuild is gated by a non-blocking
    /// [`AdvisoryFileLock`] on the cache file:
    /// - **Won the lock:** re-read the cache (a peer may have refreshed it
    ///   while we contended); serve it if now fresh, else rebuild + persist.
    /// - **Lost the lock (a peer is refreshing):** serve the stale cache (or
    ///   an empty catalog on a cold miss) instead of joining a thundering
    ///   herd of redundant registry walks.
    ///
    /// The lock is an OS file handle held across the async rebuild — not a
    /// `MutexGuard`, so it is sound to hold across `.await`. A genuine lock
    /// I/O fault (e.g. a symlinked cache path) degrades to an uncoordinated
    /// rebuild rather than failing the operation; the atomic write still
    /// prevents corruption.
    ///
    /// # Errors
    ///
    /// [`CatalogError`] for a cache parse/version failure, or a genuine
    /// registry transport/auth failure during a rebuild this process owns.
    pub async fn load_or_refresh_coordinated(
        path: &Path,
        registry: &str,
        query: &str,
        access: &std::sync::Arc<dyn OciAccess>,
        offline: bool,
        force: bool,
    ) -> Result<Self, CatalogError> {
        let scope = query.to_lowercase();
        let guard = match Self::coordinate(path, registry, &scope, offline, force).await? {
            PreFlight::Serve(catalog) => return Ok(catalog),
            PreFlight::Rebuild(guard) => guard,
        };

        // Phase 2 — the network rebuild, the only genuinely async work, runs on
        // the async executor. The advisory lock (an OS file handle, not a
        // `MutexGuard`) is held across this await: sound to hold across `.await`.
        let rebuilt = match Self::build(registry, &scope, access).await {
            Ok(rebuilt) => rebuilt,
            Err(build_err) => {
                Self::dispose_guard(path, guard).await;
                return Err(CatalogError::access(path, build_err));
            }
        };

        Self::commit(path, guard, rebuilt).await
    }

    /// Like [`Self::load_or_refresh_coordinated`], but for an index-backed
    /// source: the same cache file, TTL, offline degrade, and advisory-lock
    /// coordination — only the rebuild differs (the package index is
    /// fetched over HTTP or a git shallow clone instead of walking the
    /// registry's `_catalog`). The cache is keyed by the index `locator`.
    ///
    /// # Errors
    ///
    /// [`CatalogError`] for a cache parse/version failure, or an index
    /// fetch/parse failure during a rebuild this process owns.
    pub async fn load_or_refresh_index_coordinated(
        path: &Path,
        locator: &str,
        kind: crate::config::registry_resolve::SourceKind,
        query: &str,
        git_dir: &Path,
        offline: bool,
        force: bool,
    ) -> Result<Self, CatalogError> {
        let scope = query.to_lowercase();
        let guard = match Self::coordinate(path, locator, &scope, offline, force).await? {
            PreFlight::Serve(catalog) => return Ok(catalog),
            PreFlight::Rebuild(guard) => guard,
        };

        let rebuilt = match Self::build_from_index(locator, kind, &scope, git_dir, path).await {
            Ok(rebuilt) => rebuilt,
            Err(build_err) => {
                Self::dispose_guard(path, guard).await;
                return Err(build_err);
            }
        };

        Self::commit(path, guard, rebuilt).await
    }

    /// Phase 1 — blocking pre-flight on the blocking pool: read the cache,
    /// decide, and (when a rebuild is needed) acquire the advisory lock and
    /// double-check. The cache read, the flock syscall, and the Windows
    /// delete-pending retry `sleep` are all blocking, so they must never
    /// run on a Tokio worker (quality-rust: no blocking I/O in async).
    ///
    /// `key` is the cache identity — the registry url for `_catalog`
    /// sources, the index locator for index sources.
    async fn coordinate(
        path: &Path,
        key: &str,
        scope: &str,
        offline: bool,
        force: bool,
    ) -> Result<PreFlight, CatalogError> {
        let path = path.to_path_buf();
        let key = key.to_string();
        let scope = scope.to_string();
        run_blocking(path.clone(), move || {
            let cached = Catalog::load(&path, &key)?;

            // Offline: serve whatever is cached (any scope), never lock or
            // reach the network. A cold cache is empty.
            if offline {
                return Ok(PreFlight::Serve(cached.unwrap_or_else(|| Catalog::empty(&key))));
            }

            // Fast path: a fresh, same-scope cache serves with no lock
            // taken, so the common case never contends.
            if !force
                && let Some(c) = &cached
                && c.scope == scope
                && c.is_fresh(chrono::Utc::now())
            {
                return Ok(PreFlight::Serve(c.clone()));
            }

            // The cache file's parent (the per-registry `catalog/` dir)
            // must exist for the sidecar and the atomic write; best-effort.
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }

            match AdvisoryFileLock::try_acquire(&path) {
                Ok(guard) => {
                    // Double-check: a peer may have rebuilt while we
                    // contended for the lock. Serve their fresh build
                    // instead of redoing it.
                    if !force
                        && let Some(c) = Catalog::load(&path, &key)?
                        && c.scope == scope
                        && c.is_fresh(chrono::Utc::now())
                    {
                        return Ok(PreFlight::Serve(c));
                    }
                    Ok(PreFlight::Rebuild(Some(guard)))
                }
                // A peer owns the refresh — serve stale (or empty) and move
                // on rather than walking the registry redundantly.
                Err(e) if matches!(e.kind, crate::lock::lock_error::LockErrorKind::Locked) => {
                    Ok(PreFlight::Serve(cached.unwrap_or_else(|| Catalog::empty(&key))))
                }
                // A real lock I/O fault: fall back to an uncoordinated
                // rebuild (the atomic write still prevents corruption).
                Err(_) => Ok(PreFlight::Rebuild(None)),
            }
        })
        .await
    }

    /// Dispose the advisory-lock guard on the blocking pool — its `Drop`
    /// unlinks the sidecar (a blocking syscall that must not run on a
    /// Tokio worker). Used on the rebuild error path; the success path
    /// drops the guard inside [`Self::commit`]'s blocking closure.
    async fn dispose_guard(path: &Path, guard: Option<AdvisoryFileLock>) {
        let lock_path = path.to_path_buf();
        let _ = run_blocking(lock_path, move || {
            drop(guard);
            Ok(())
        })
        .await;
    }

    /// Phase 3 — blocking commit on the blocking pool: atomically write the
    /// rebuilt cache and release the lock (its `Drop` unlinks the sidecar).
    /// A save error exits via `?`; the guard then drops at the end of this
    /// closure — still on the blocking pool — so the sidecar unlink never
    /// runs on the async executor on either the success or the error path.
    async fn commit(path: &Path, guard: Option<AdvisoryFileLock>, rebuilt: Catalog) -> Result<Catalog, CatalogError> {
        let write_path = path.to_path_buf();
        run_blocking(write_path.clone(), move || {
            rebuilt.save(&write_path)?;
            drop(guard);
            Ok(rebuilt)
        })
        .await
    }

    /// Build an index-backed catalog: fetch the package index (HTTP static
    /// files or a git shallow clone), apply the same lowercased name
    /// prefilter and [`MAX_CATALOG_REPOS`] cap as the `_catalog` walk, and
    /// key entries by their full `registry/repository` ref (index entries
    /// span registries, so the bare repository path is not unique).
    async fn build_from_index(
        locator: &str,
        kind: crate::config::registry_resolve::SourceKind,
        scope: &str,
        git_dir: &Path,
        cache_path: &Path,
    ) -> Result<Self, CatalogError> {
        let fetched_at = now_rfc3339();
        let fetched =
            crate::catalog::index_source::fetch_index_entries(locator, kind, git_dir, cache_path, &fetched_at).await?;

        let mut selected: Vec<CatalogEntry> = fetched
            .into_iter()
            .filter(|e| scope.is_empty() || e.repo().to_lowercase().contains(scope))
            .take(MAX_CATALOG_REPOS + 1)
            .collect();
        let truncated = selected.len() > MAX_CATALOG_REPOS;
        selected.truncate(MAX_CATALOG_REPOS);

        let entries: BTreeMap<String, CatalogEntry> = selected.into_iter().map(|e| (e.repo(), e)).collect();
        Ok(Self {
            registry: locator.to_string(),
            scope: scope.to_string(),
            truncated,
            built_at: fetched_at,
            entries,
        })
    }

    /// Build a bounded, query-scoped catalog over the access seam.
    ///
    /// `scope` is a lowercased repository-name prefilter applied *before*
    /// any per-repo network call: only repositories whose name contains it
    /// are metadata-fetched, so a targeted `search <kw>` is O(matches),
    /// not O(registry). An empty scope (browse / TUI) takes the first
    /// [`MAX_CATALOG_REPOS`] lexicographic repositories. Either way the
    /// number of repos walked is capped — walking an entire (potentially
    /// 60k-repo) registry is an explicit cut-line.
    ///
    /// For each selected repository: list its tags, pick a representative
    /// tag, fetch that tag's manifest, read the annotations. No blob is
    /// pulled. Only a failure of `list_catalog` itself aborts the build; a
    /// per-repository lookup failure (foreign image index, private/403,
    /// transient) degrades that one entry to a bare record.
    ///
    /// Per-repository metadata is fetched in bounded parallel via a
    /// [`tokio::task::JoinSet`] (capped repos × three round-trips each —
    /// sequential is unusable). Output is keyed by repository in a
    /// `BTreeMap`, so the result is deterministic regardless of completion
    /// order (quality-rust `JoinSet` rule).
    ///
    /// When the post-prefilter candidate list exceeds the cap the window is
    /// only a prefix of the matches; the build records that on
    /// [`Self::truncated`] so a non-empty query can warn that results past the
    /// cap are not shown.
    async fn build(
        registry: &str,
        scope: &str,
        access: &std::sync::Arc<dyn OciAccess>,
    ) -> Result<Self, crate::oci::access::error::AccessError> {
        /// Concurrent per-repository metadata lookups.
        const CONCURRENCY: usize = 16;

        // A configured default registry may carry a namespace
        // (`ghcr.io/acme`). The OCI `_catalog` endpoint lives on the bare
        // host, so list against the host and scope the result to the
        // namespace prefix. Entries are keyed by the host (matching how
        // identifiers parse `registry/repository`).
        let (host, namespace) = split_host_namespace(registry);
        let repos = access.list_catalog(registry).await?;
        // Namespace scope (no network) + cheap name prefilter, then the hard
        // cap. Take one past the cap so an overflow is detectable without
        // materializing a (potentially 60k-repo) list.
        let mut selected: Vec<String> = repos
            .into_iter()
            .filter(|r| namespace.is_none_or(|ns| r == ns || r.starts_with(&format!("{ns}/"))))
            .filter(|r| scope.is_empty() || r.to_lowercase().contains(scope))
            .take(MAX_CATALOG_REPOS + 1)
            .collect();
        // More candidates than the cap ⇒ the window is a prefix; drop the
        // probe element and flag the build as truncated.
        let truncated = selected.len() > MAX_CATALOG_REPOS;
        selected.truncate(MAX_CATALOG_REPOS);

        let mut entries = BTreeMap::new();
        let mut iter = selected.into_iter();
        let mut set: tokio::task::JoinSet<(String, CatalogEntry)> = tokio::task::JoinSet::new();

        // Prime the window.
        for _ in 0..CONCURRENCY {
            let Some(repository) = iter.next() else { break };
            spawn_entry(&mut set, host, repository, access);
        }
        while let Some(joined) = set.join_next().await {
            // A task panic is a bug, not a registry condition; surface it
            // by skipping that repo rather than poisoning the whole walk.
            if let Ok((repository, entry)) = joined {
                entries.insert(repository, entry);
            }
            if let Some(repository) = iter.next() {
                spawn_entry(&mut set, host, repository, access);
            }
        }

        Ok(Self {
            registry: registry.to_string(),
            scope: scope.to_string(),
            truncated,
            built_at: now_rfc3339(),
            entries,
        })
    }

    /// Build one repository's entry: pick a tag, read its manifest
    /// annotations (no blob pull).
    ///
    /// Infallible by design — every failure mode (no tags, unresolvable
    /// tag, a foreign / image-index / unparseable manifest, a per-repo
    /// transport or auth fault) degrades to a metadata-less entry so a
    /// shared registry full of non-Grimoire repos still yields a catalog.
    async fn build_entry(registry: &str, repository: &str, access: &dyn OciAccess) -> CatalogEntry {
        let fetched_at = now_rfc3339();
        let bare = |latest_tag: Option<String>, version: Option<String>| CatalogEntry {
            registry: registry.to_string(),
            repository: repository.to_string(),
            kind: None,
            description: None,
            summary: None,
            keywords: Vec::new(),
            repository_url: None,
            revision: None,
            created: None,
            deprecated: None,
            latest_tag,
            version,
            fetched_at: fetched_at.clone(),
        };

        let base = Identifier::new_registry(repository.to_string(), registry.to_string());

        let tags = match access.list_tags(&base).await {
            Ok(t) => t.unwrap_or_default(),
            Err(_) => return bare(None, None),
        };
        // Highest concrete semver (never the moving `latest`); reused for
        // every degraded path below now that the tag list is known.
        let version = pick_highest_version(&tags);
        let Some(tag) = pick_latest_tag(&tags) else {
            return bare(None, None);
        };

        let tagged = base.clone_with_tag(tag.clone());
        // Resolve the tag to a digest, then read the manifest (no blob).
        let digest = match access
            .resolve_digest(&tagged, crate::oci::access::Operation::Query)
            .await
        {
            Ok(Some(d)) => d,
            Ok(None) | Err(_) => return bare(Some(tag), version.clone()),
        };
        let pinned = match PinnedIdentifier::try_from(tagged.clone_with_digest(digest)) {
            Ok(p) => p,
            // Unreachable in practice (we just attached a digest); be
            // defensive rather than panic in a catalog walk.
            Err(_) => return bare(Some(tag), version.clone()),
        };

        // A foreign repo (image index, private, transient) ⇒ bare entry,
        // never a hard catalog failure.
        let manifest = match access.fetch_manifest(&pinned).await {
            Ok(m) => m,
            Err(_) => return bare(Some(tag), version.clone()),
        };
        let (kind, description, summary, keywords, repository_url, revision, created, deprecated) = manifest
            .map(|m| {
                let kind = crate::oci::annotations::kind_from_manifest(&m).map(|k| k.to_string());
                let description = m.annotations.get("org.opencontainers.image.description").cloned();
                let summary = m.annotations.get("com.grimoire.summary").cloned();
                let keywords = m
                    .annotations
                    .get("com.grimoire.keywords")
                    .map(|k| {
                        k.split(',')
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                            .map(str::to_string)
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                // Pre-repository artifacts carry the release ref here —
                // the prefix guard keeps only real HTTPS repo URLs.
                let repository_url = m
                    .annotations
                    .get("org.opencontainers.image.source")
                    .filter(|s| s.starts_with("https://"))
                    .cloned();
                // Git provenance (the `--git` publish opt-in). Absent on an
                // ordinary release.
                let revision = m.annotations.get("org.opencontainers.image.revision").cloned();
                let created = m.annotations.get("org.opencontainers.image.created").cloned();
                // A non-empty `com.grimoire.deprecated` marks the artifact
                // deprecated (the single read seam normalizes/trims).
                let deprecated = crate::oci::annotations::deprecation_message(&m.annotations);
                (
                    kind,
                    description,
                    summary,
                    keywords,
                    repository_url,
                    revision,
                    created,
                    deprecated,
                )
            })
            .unwrap_or((None, None, None, Vec::new(), None, None, None, None));

        CatalogEntry {
            registry: registry.to_string(),
            repository: repository.to_string(),
            kind,
            description,
            summary,
            keywords,
            repository_url,
            revision,
            created,
            deprecated,
            latest_tag: Some(tag),
            version,
            fetched_at,
        }
    }
}

/// Split a configured registry string into its host (the first path
/// segment) and an optional namespace prefix (the rest).
///
/// `ghcr.io/acme` → (`ghcr.io`, `Some("acme")`); `localhost:5000` →
/// (`localhost:5000`, `None`); `host/a/b` → (`host`, `Some("a/b")`). The
/// OCI `_catalog` endpoint lives on the bare host; the namespace scopes
/// which repositories the catalog keeps. Used by [`Catalog::build`] so a
/// namespaced default registry still discovers its own repositories.
fn split_host_namespace(registry: &str) -> (&str, Option<&str>) {
    match registry.split_once('/') {
        Some((host, ns)) if !ns.is_empty() => (host, Some(ns)),
        _ => (registry, None),
    }
}

/// Pick the representative tag from `tags`: prefer `latest`, else the
/// highest semver, else the first (lexicographically, for determinism).
pub fn pick_latest_tag(tags: &[String]) -> Option<String> {
    if tags.is_empty() {
        return None;
    }
    if tags.iter().any(|t| t == "latest") {
        return Some("latest".to_string());
    }
    let mut highest: Option<(semver::Version, &String)> = None;
    for t in tags {
        // OCI tags normalize `+` → `_`; semver build metadata uses `+`.
        let candidate = t.replace('_', "+");
        if let Ok(v) = semver::Version::parse(&candidate) {
            match &highest {
                Some((hv, _)) if &v <= hv => {}
                _ => highest = Some((v, t)),
            }
        }
    }
    if let Some((_, t)) = highest {
        return Some(t.clone());
    }
    let mut sorted: Vec<&String> = tags.iter().collect();
    sorted.sort();
    sorted.first().map(|s| (*s).clone())
}

/// Pick the highest *concrete* semver tag from `tags`, ignoring the moving
/// `latest` pointer entirely. `None` when no tag parses as semver — the UI
/// then falls back to whatever [`pick_latest_tag`] chose. Unlike
/// [`pick_latest_tag`] this never returns `latest`, so callers can show an
/// explicit version a user can pin.
pub fn pick_highest_version(tags: &[String]) -> Option<String> {
    let mut highest: Option<(semver::Version, &String)> = None;
    for t in tags {
        if t == "latest" {
            continue;
        }
        // OCI tags normalize `+` → `_`; semver build metadata uses `+`.
        let candidate = t.replace('_', "+");
        if let Ok(v) = semver::Version::parse(&candidate) {
            match &highest {
                Some((hv, _)) if &v <= hv => {}
                _ => highest = Some((v, t)),
            }
        }
    }
    highest.map(|(_, t)| t.clone())
}

/// Whether an RFC3339 `built_at` is within [`CATALOG_TTL_SECONDS`] of
/// `now`. An unparseable timestamp is stale (fail closed).
pub fn is_fresh_at(built_at: &str, now: chrono::DateTime<chrono::Utc>) -> bool {
    match chrono::DateTime::parse_from_rfc3339(built_at) {
        Ok(t) => {
            let age = now.signed_duration_since(t.with_timezone(&chrono::Utc));
            age.num_seconds() >= 0 && age.num_seconds() < CATALOG_TTL_SECONDS
        }
        Err(_) => false,
    }
}

/// Current UTC time as an RFC3339 string (`%Y-%m-%dT%H:%M:%SZ`), matching
/// the lock layer's timestamp format for consistency on disk.
fn now_rfc3339() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// Spawn one repository's metadata lookup onto `set`. The task owns an
/// `Arc` clone of the access seam and yields `(repository, entry)` so the
/// caller can re-key deterministically regardless of completion order.
fn spawn_entry(
    set: &mut tokio::task::JoinSet<(String, CatalogEntry)>,
    registry: &str,
    repository: String,
    access: &std::sync::Arc<dyn OciAccess>,
) {
    let registry = registry.to_string();
    let access = std::sync::Arc::clone(access);
    set.spawn(async move {
        let entry = Catalog::build_entry(&registry, &repository, access.as_ref()).await;
        (repository, entry)
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oci::access::memory_registry::MemoryRegistry;
    use crate::oci::manifest::{Descriptor, OciManifest};
    use crate::oci::{Algorithm, Identifier};
    use async_trait::async_trait;
    use std::sync::Mutex;

    fn ts(offset_secs: i64) -> String {
        (chrono::Utc::now() - chrono::Duration::seconds(offset_secs))
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string()
    }

    // ── pick_latest_tag ──────────────────────────────────────────────

    #[test]
    fn pick_prefers_literal_latest() {
        let tags = vec!["1.0.0".to_string(), "latest".to_string(), "2.0.0".to_string()];
        assert_eq!(pick_latest_tag(&tags), Some("latest".to_string()));
    }

    #[test]
    fn pick_highest_semver_when_no_latest() {
        let tags = vec!["1.0.0".to_string(), "2.3.1".to_string(), "2.0.0".to_string()];
        assert_eq!(pick_latest_tag(&tags), Some("2.3.1".to_string()));
    }

    #[test]
    fn pick_first_lexicographic_when_no_semver() {
        let tags = vec!["zeta".to_string(), "alpha".to_string(), "stable".to_string()];
        assert_eq!(pick_latest_tag(&tags), Some("alpha".to_string()));
    }

    #[test]
    fn pick_none_when_empty() {
        assert_eq!(pick_latest_tag(&[]), None);
    }

    // ── pick_highest_version ─────────────────────────────────────────

    #[test]
    fn highest_version_ignores_latest_pointer() {
        let tags = vec!["latest".to_string(), "1.0.0".to_string(), "2.3.1".to_string()];
        // `pick_latest_tag` returns `latest`; the concrete picker does not.
        assert_eq!(pick_latest_tag(&tags), Some("latest".to_string()));
        assert_eq!(pick_highest_version(&tags), Some("2.3.1".to_string()));
    }

    #[test]
    fn highest_version_none_without_semver() {
        let tags = vec!["latest".to_string(), "stable".to_string()];
        assert_eq!(pick_highest_version(&tags), None);
        assert_eq!(pick_highest_version(&[]), None);
    }

    // ── TTL freshness ────────────────────────────────────────────────

    #[test]
    fn fresh_within_ttl_stale_after() {
        let now = chrono::Utc::now();
        assert!(is_fresh_at(&ts(60), now), "60s old is fresh");
        assert!(!is_fresh_at(&ts(CATALOG_TTL_SECONDS + 5), now), "over TTL is stale");
        assert!(!is_fresh_at("not-a-timestamp", now), "unparseable is stale");
    }

    #[test]
    fn future_timestamp_is_stale() {
        // A built_at in the future (clock skew) must not pin the cache.
        let now = chrono::Utc::now();
        let future = (now + chrono::Duration::seconds(120))
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string();
        assert!(!is_fresh_at(&future, now));
    }

    // ── atomic round-trip + version rejection ────────────────────────

    #[test]
    fn round_trips_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("catalog.json");
        let mut entries = BTreeMap::new();
        entries.insert(
            "acme/code-review".to_string(),
            CatalogEntry {
                registry: "localhost:5000".to_string(),
                repository: "acme/code-review".to_string(),
                kind: Some("skill".to_string()),
                description: Some("Review code.".to_string()),
                summary: Some("review skill".to_string()),
                keywords: vec!["review".to_string(), "quality".to_string()],
                repository_url: Some("https://github.com/acme/code-review".to_string()),
                revision: Some("abc123def456-dirty".to_string()),
                created: Some("2026-06-29T12:00:00+00:00".to_string()),
                deprecated: Some("use acme/code-review-2".to_string()),
                latest_tag: Some("latest".to_string()),
                version: Some("1.2.0".to_string()),
                fetched_at: ts(10),
            },
        );
        let cat = Catalog {
            registry: "localhost:5000".to_string(),
            scope: String::new(),
            truncated: true,
            built_at: ts(10),
            entries,
        };
        cat.save(&path).unwrap();
        let loaded = Catalog::load(&path, "localhost:5000").unwrap().expect("present");
        assert_eq!(loaded.entries().count(), 1);
        assert!(loaded.truncated(), "truncated flag round-trips through disk");
        let e = loaded.entries().next().unwrap();
        assert_eq!(e.kind.as_deref(), Some("skill"));
        assert_eq!(e.keywords, vec!["review", "quality"]);
        assert_eq!(
            e.revision.as_deref(),
            Some("abc123def456-dirty"),
            "git revision round-trips through disk"
        );
        assert_eq!(
            e.created.as_deref(),
            Some("2026-06-29T12:00:00+00:00"),
            "git commit date round-trips through disk"
        );
        assert_eq!(
            e.deprecated.as_deref(),
            Some("use acme/code-review-2"),
            "deprecation message round-trips through disk"
        );
        // Different registry ⇒ treated as cold cache.
        assert!(Catalog::load(&path, "ghcr.io").unwrap().is_none());
    }

    #[test]
    fn unknown_version_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("catalog.json");
        std::fs::write(
            &path,
            r#"{"version":99,"registry":"localhost:5000","built_at":"x","entries":{}}"#,
        )
        .unwrap();
        let err = Catalog::load(&path, "localhost:5000").expect_err("unknown version rejects");
        assert!(matches!(
            err.kind,
            super::super::catalog_error::CatalogErrorKind::Parse(_)
        ));
    }

    #[test]
    fn absent_file_is_cold_not_error() {
        let dir = tempfile::tempdir().unwrap();
        let got = Catalog::load(&dir.path().join("nope.json"), "localhost:5000").unwrap();
        assert!(got.is_none());
    }

    // ── build / refresh over the access seam ─────────────────────────

    fn skill_manifest(kw: &str, desc: &str) -> OciManifest {
        let mut annotations = BTreeMap::new();
        annotations.insert("com.grimoire.keywords".to_string(), kw.to_string());
        annotations.insert("org.opencontainers.image.description".to_string(), desc.to_string());
        annotations.insert("com.grimoire.summary".to_string(), "short summary".to_string());
        annotations.insert(
            "org.opencontainers.image.source".to_string(),
            "https://github.com/acme/code-review".to_string(),
        );
        // grim's REAL wire shape since `adr_oci_empty_config_compat.md`: NO
        // `artifactType`, OCI empty config, kind carried solely by the
        // `com.grimoire.kind` annotation. Catalog kind resolution therefore
        // exercises read tier 3 here — the path grim actually publishes —
        // rather than the legacy `artifactType` tier.
        annotations.insert("com.grimoire.kind".to_string(), "skill".to_string());
        OciManifest {
            media_type: Some("application/vnd.oci.image.manifest.v1+json".to_string()),
            artifact_type: None,
            config_media_type: Some("application/vnd.oci.empty.v1+json".to_string()),
            layers: vec![Descriptor {
                digest: Algorithm::Sha256.hash(b"payload"),
                media_type: "application/vnd.grimoire.artifact.layer.v1.tar".to_string(),
                size: 7,
            }],
            annotations,
        }
    }

    /// A `MemoryRegistry` wrapper that also serves a repository catalog
    /// list (the base double returns an empty list).
    struct CatalogRegistry {
        inner: MemoryRegistry,
        repos: Vec<String>,
        registry: String,
        blob_pulled: std::sync::Arc<Mutex<bool>>,
    }

    impl CatalogRegistry {
        fn blob_pulled_handle(&self) -> std::sync::Arc<Mutex<bool>> {
            std::sync::Arc::clone(&self.blob_pulled)
        }
    }

    #[async_trait]
    impl OciAccess for CatalogRegistry {
        async fn resolve_digest(
            &self,
            id: &Identifier,
            op: crate::oci::access::Operation,
        ) -> Result<Option<crate::oci::Digest>, crate::oci::access::error::AccessError> {
            self.inner.resolve_digest(id, op).await
        }
        async fn fetch_manifest(
            &self,
            id: &PinnedIdentifier,
        ) -> Result<Option<OciManifest>, crate::oci::access::error::AccessError> {
            self.inner.fetch_manifest(id).await
        }
        async fn fetch_blob(
            &self,
            repo: &Identifier,
            digest: &crate::oci::Digest,
        ) -> Result<Option<Vec<u8>>, crate::oci::access::error::AccessError> {
            *self.blob_pulled.lock().unwrap() = true;
            self.inner.fetch_blob(repo, digest).await
        }
        async fn list_tags(
            &self,
            id: &Identifier,
        ) -> Result<Option<Vec<String>>, crate::oci::access::error::AccessError> {
            self.inner.list_tags(id).await
        }
        async fn list_catalog(&self, registry: &str) -> Result<Vec<String>, crate::oci::access::error::AccessError> {
            if registry == self.registry {
                Ok(self.repos.clone())
            } else {
                Ok(Vec::new())
            }
        }
        async fn push_blob(
            &self,
            repo: &Identifier,
            bytes: &[u8],
        ) -> Result<crate::oci::Digest, crate::oci::access::error::AccessError> {
            self.inner.push_blob(repo, bytes).await
        }
        async fn push_manifest(
            &self,
            repo: &Identifier,
            manifest: &OciManifest,
        ) -> Result<crate::oci::Digest, crate::oci::access::error::AccessError> {
            self.inner.push_manifest(repo, manifest).await
        }
        async fn put_tag(
            &self,
            repo: &Identifier,
            tag: &str,
            manifest_digest: &crate::oci::Digest,
        ) -> Result<(), crate::oci::access::error::AccessError> {
            self.inner.put_tag(repo, tag, manifest_digest).await
        }
    }

    async fn seed() -> CatalogRegistry {
        let inner = MemoryRegistry::new();
        let reg = "localhost:5000";
        // Publish one skill repo with a `latest` tag + annotations.
        let id = Identifier::new_registry("acme/code-review".to_string(), reg.to_string());
        let manifest = skill_manifest("review,quality", "Review code.");
        let mdigest = inner.push_manifest(&id, &manifest).await.unwrap();
        inner.put_tag(&id, "latest", &mdigest).await.unwrap();
        inner.put_tag(&id, "1.0.0", &mdigest).await.unwrap();
        CatalogRegistry {
            inner,
            repos: vec!["acme/code-review".to_string()],
            registry: reg.to_string(),
            blob_pulled: std::sync::Arc::new(Mutex::new(false)),
        }
    }

    #[tokio::test]
    async fn build_reads_annotations_without_blob_pull() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("catalog.json");
        let reg = std::sync::Arc::new(seed().await);
        let blob_flag = reg.blob_pulled_handle();
        let access: std::sync::Arc<dyn OciAccess> = reg;

        let cat = Catalog::load_or_refresh(&path, "localhost:5000", "", &access, false, true)
            .await
            .unwrap();
        let e = cat.entries().next().expect("one entry");
        assert_eq!(e.repository, "acme/code-review");
        assert_eq!(e.kind.as_deref(), Some("skill"));
        assert_eq!(e.description.as_deref(), Some("Review code."));
        assert_eq!(e.summary.as_deref(), Some("short summary"));
        assert_eq!(e.keywords, vec!["review", "quality"]);
        assert_eq!(
            e.repository_url.as_deref(),
            Some("https://github.com/acme/code-review"),
            "HTTPS source annotation is kept as the repository URL"
        );
        assert_eq!(e.latest_tag.as_deref(), Some("latest"));
        assert!(!*blob_flag.lock().unwrap(), "catalog must not pull a blob");
        // Persisted for reuse.
        assert!(path.exists());
    }

    #[tokio::test]
    async fn build_drops_non_https_source_annotation() {
        // Pre-repository artifacts carry the tagless release ref in the
        // source annotation — the https:// guard must drop it, not surface
        // it as a clickable repository URL.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("catalog.json");
        let inner = MemoryRegistry::new();
        let reg = "localhost:5000";
        let id = Identifier::new_registry("acme/legacy".to_string(), reg.to_string());
        let mut manifest = skill_manifest("k", "d");
        manifest.annotations.insert(
            "org.opencontainers.image.source".to_string(),
            "localhost:5000/acme/legacy".to_string(),
        );
        let mdigest = inner.push_manifest(&id, &manifest).await.unwrap();
        inner.put_tag(&id, "latest", &mdigest).await.unwrap();
        let access: std::sync::Arc<dyn OciAccess> = std::sync::Arc::new(CatalogRegistry {
            inner,
            repos: vec!["acme/legacy".to_string()],
            registry: reg.to_string(),
            blob_pulled: std::sync::Arc::new(Mutex::new(false)),
        });
        let cat = Catalog::load_or_refresh(&path, reg, "", &access, false, true)
            .await
            .unwrap();
        let e = cat.entries().next().expect("one entry");
        assert_eq!(e.repository_url, None, "legacy release-ref source is not a URL");
    }

    #[tokio::test]
    async fn build_reads_git_provenance_annotations() {
        // The `--git` opt-in stamps revision/created on the manifest; the
        // catalog read path surfaces both onto the entry.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("catalog.json");
        let inner = MemoryRegistry::new();
        let reg = "localhost:5000";
        let id = Identifier::new_registry("acme/traced".to_string(), reg.to_string());
        let mut manifest = skill_manifest("k", "d");
        manifest.annotations.insert(
            "org.opencontainers.image.revision".to_string(),
            "abc123def456-dirty".to_string(),
        );
        manifest.annotations.insert(
            "org.opencontainers.image.created".to_string(),
            "2026-06-29T12:00:00+00:00".to_string(),
        );
        let mdigest = inner.push_manifest(&id, &manifest).await.unwrap();
        inner.put_tag(&id, "latest", &mdigest).await.unwrap();
        let access: std::sync::Arc<dyn OciAccess> = std::sync::Arc::new(CatalogRegistry {
            inner,
            repos: vec!["acme/traced".to_string()],
            registry: reg.to_string(),
            blob_pulled: std::sync::Arc::new(Mutex::new(false)),
        });
        let cat = Catalog::load_or_refresh(&path, reg, "", &access, false, true)
            .await
            .unwrap();
        let e = cat.entries().next().expect("one entry");
        assert_eq!(e.revision.as_deref(), Some("abc123def456-dirty"));
        assert_eq!(e.created.as_deref(), Some("2026-06-29T12:00:00+00:00"));

        // A skill published without `--git` carries neither.
        assert_eq!(
            cat.entries().next().unwrap().registry,
            "localhost:5000",
            "sanity: entry built"
        );
    }

    #[test]
    fn v1_entry_without_repository_url_deserializes() {
        // Backward compat: a cache written before the field existed parses
        // with `None` (serde default), no version bump required.
        let json = r#"{"registry":"localhost:5000","repository":"acme/x","fetched_at":"2026-01-01T00:00:00Z"}"#;
        let e: CatalogEntry = serde_json::from_str(json).unwrap();
        assert_eq!(e.repository_url, None);
        // The same envelope predates `deprecated` too ⇒ defaults to None.
        assert_eq!(e.deprecated, None);
        // …and predates the git provenance fields ⇒ both default to None.
        assert_eq!(e.revision, None);
        assert_eq!(e.created, None);
    }

    #[tokio::test]
    async fn build_reads_deprecated_annotation() {
        // A `com.grimoire.deprecated` annotation on the representative tag's
        // manifest surfaces as the catalog entry's deprecation message.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("catalog.json");
        let inner = MemoryRegistry::new();
        let reg = "localhost:5000";
        let id = Identifier::new_registry("acme/old-skill".to_string(), reg.to_string());
        let mut manifest = skill_manifest("k", "d");
        manifest.annotations.insert(
            "com.grimoire.deprecated".to_string(),
            "use acme/new-skill instead".to_string(),
        );
        let mdigest = inner.push_manifest(&id, &manifest).await.unwrap();
        inner.put_tag(&id, "latest", &mdigest).await.unwrap();
        let access: std::sync::Arc<dyn OciAccess> = std::sync::Arc::new(CatalogRegistry {
            inner,
            repos: vec!["acme/old-skill".to_string()],
            registry: reg.to_string(),
            blob_pulled: std::sync::Arc::new(Mutex::new(false)),
        });
        let cat = Catalog::load_or_refresh(&path, reg, "", &access, false, true)
            .await
            .unwrap();
        let e = cat.entries().next().expect("one entry");
        assert_eq!(e.deprecated.as_deref(), Some("use acme/new-skill instead"));

        // A skill with no deprecation annotation stays None.
        let cat2 = {
            let inner = MemoryRegistry::new();
            let id = Identifier::new_registry("acme/fresh".to_string(), reg.to_string());
            let m = skill_manifest("k", "d");
            let d = inner.push_manifest(&id, &m).await.unwrap();
            inner.put_tag(&id, "latest", &d).await.unwrap();
            let access: std::sync::Arc<dyn OciAccess> = std::sync::Arc::new(CatalogRegistry {
                inner,
                repos: vec!["acme/fresh".to_string()],
                registry: reg.to_string(),
                blob_pulled: std::sync::Arc::new(Mutex::new(false)),
            });
            let dir2 = tempfile::tempdir().unwrap();
            Catalog::load_or_refresh(&dir2.path().join("c.json"), reg, "", &access, false, true)
                .await
                .unwrap()
        };
        assert_eq!(cat2.entries().next().unwrap().deprecated, None);
    }

    #[tokio::test]
    async fn fresh_cache_is_not_rebuilt() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("catalog.json");
        let reg: std::sync::Arc<dyn OciAccess> = std::sync::Arc::new(seed().await);
        // First build populates and persists.
        Catalog::load_or_refresh(&path, "localhost:5000", "", &reg, false, true)
            .await
            .unwrap();
        // A non-forced reload on a fresh cache returns it without touching
        // the (now-empty-catalog) registry double.
        let empty: std::sync::Arc<dyn OciAccess> = std::sync::Arc::new(MemoryRegistry::new());
        let cat = Catalog::load_or_refresh(&path, "localhost:5000", "", &empty, false, false)
            .await
            .unwrap();
        assert_eq!(cat.entries().count(), 1, "fresh cache served, not rebuilt");
    }

    #[tokio::test]
    async fn offline_serves_cached_and_never_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("catalog.json");
        let reg: std::sync::Arc<dyn OciAccess> = std::sync::Arc::new(seed().await);
        Catalog::load_or_refresh(&path, "localhost:5000", "", &reg, false, true)
            .await
            .unwrap();

        // Offline: a `MemoryRegistry` that would list nothing is never
        // consulted; the cached catalog is served.
        let empty: std::sync::Arc<dyn OciAccess> = std::sync::Arc::new(MemoryRegistry::new());
        let cat = Catalog::load_or_refresh(&path, "localhost:5000", "", &empty, true, false)
            .await
            .unwrap();
        assert_eq!(cat.entries().count(), 1);

        // Offline cold cache ⇒ empty catalog, still no error.
        let dir2 = tempfile::tempdir().unwrap();
        let cold = Catalog::load_or_refresh(&dir2.path().join("c.json"), "localhost:5000", "", &empty, true, false)
            .await
            .unwrap();
        assert_eq!(cold.entries().count(), 0);
    }

    #[tokio::test]
    async fn coordinated_rebuilds_when_uncontended() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("catalog").join("reg.json");
        let reg: std::sync::Arc<dyn OciAccess> = std::sync::Arc::new(seed().await);
        // No lock held ⇒ this process wins the refresh lock and rebuilds.
        // The parent `catalog/` dir is created by the coordinator itself.
        let cat = Catalog::load_or_refresh_coordinated(&path, "localhost:5000", "", &reg, false, true)
            .await
            .unwrap();
        assert_eq!(cat.entries().count(), 1, "uncontended refresh rebuilds");
        assert!(path.exists(), "rebuilt catalog is persisted");
    }

    #[tokio::test]
    async fn coordinated_serves_stale_when_refresh_lock_held() {
        // A peer process holds the refresh lock; the coordinator must serve
        // the existing (stale-by-scope) cache rather than walk the registry.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("catalog.json");
        let reg: std::sync::Arc<dyn OciAccess> = std::sync::Arc::new(seed().await);
        // Seed a cache built under the empty (browse) scope: 1 entry.
        Catalog::load_or_refresh(&path, "localhost:5000", "", &reg, false, true)
            .await
            .unwrap();

        // A peer holds the per-file refresh lock.
        let _peer = AdvisoryFileLock::try_acquire(&path).expect("peer acquires refresh lock");

        // Query a different scope so the fast path does not serve the cache;
        // an empty registry would rebuild to zero entries if the lock were
        // free. Because the lock is held, the coordinator serves the cached
        // catalog as-is (1 entry) without consulting the registry.
        let empty: std::sync::Arc<dyn OciAccess> = std::sync::Arc::new(MemoryRegistry::new());
        let cat = Catalog::load_or_refresh_coordinated(&path, "localhost:5000", "zzz", &empty, false, false)
            .await
            .unwrap();
        assert_eq!(
            cat.entries().count(),
            1,
            "contended refresh serves the stale cache, not a fresh empty walk"
        );
    }

    #[tokio::test]
    async fn coordinated_offline_serves_cached_without_locking() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("catalog.json");
        let reg: std::sync::Arc<dyn OciAccess> = std::sync::Arc::new(seed().await);
        Catalog::load_or_refresh(&path, "localhost:5000", "", &reg, false, true)
            .await
            .unwrap();
        let empty: std::sync::Arc<dyn OciAccess> = std::sync::Arc::new(MemoryRegistry::new());
        let cat = Catalog::load_or_refresh_coordinated(&path, "localhost:5000", "", &empty, true, false)
            .await
            .unwrap();
        assert_eq!(cat.entries().count(), 1, "offline serves cached");
    }

    /// An access whose catalog listing always fails — `build` aborts at
    /// `list_catalog`, so only that method is reached.
    struct FailingAccess;

    #[async_trait]
    impl OciAccess for FailingAccess {
        async fn resolve_digest(
            &self,
            _: &Identifier,
            _: crate::oci::access::Operation,
        ) -> Result<Option<crate::oci::Digest>, crate::oci::access::error::AccessError> {
            unreachable!()
        }
        async fn fetch_manifest(
            &self,
            _: &PinnedIdentifier,
        ) -> Result<Option<OciManifest>, crate::oci::access::error::AccessError> {
            unreachable!()
        }
        async fn fetch_blob(
            &self,
            _: &Identifier,
            _: &crate::oci::Digest,
        ) -> Result<Option<Vec<u8>>, crate::oci::access::error::AccessError> {
            unreachable!()
        }
        async fn list_tags(
            &self,
            _: &Identifier,
        ) -> Result<Option<Vec<String>>, crate::oci::access::error::AccessError> {
            unreachable!()
        }
        async fn list_catalog(&self, _: &str) -> Result<Vec<String>, crate::oci::access::error::AccessError> {
            Err(crate::oci::access::error::AccessError::without_identifier(
                crate::oci::access::error::AccessErrorKind::Registry(std::io::Error::other("simulated outage").into()),
            ))
        }
        async fn push_blob(
            &self,
            _: &Identifier,
            _: &[u8],
        ) -> Result<crate::oci::Digest, crate::oci::access::error::AccessError> {
            unreachable!()
        }
        async fn push_manifest(
            &self,
            _: &Identifier,
            _: &OciManifest,
        ) -> Result<crate::oci::Digest, crate::oci::access::error::AccessError> {
            unreachable!()
        }
        async fn put_tag(
            &self,
            _: &Identifier,
            _: &str,
            _: &crate::oci::Digest,
        ) -> Result<(), crate::oci::access::error::AccessError> {
            unreachable!()
        }
    }

    #[tokio::test]
    async fn coordinated_releases_lock_on_build_error() {
        // A forced rebuild whose registry walk fails (Phase 2 error path) must
        // still release the refresh lock — the spawn_blocking split disposes the
        // guard on the blocking pool on error, so a later acquire succeeds.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("catalog").join("reg.json");
        let failing: std::sync::Arc<dyn OciAccess> = std::sync::Arc::new(FailingAccess);
        let err = Catalog::load_or_refresh_coordinated(&path, "localhost:5000", "", &failing, false, true)
            .await
            .expect_err("a failing registry walk surfaces an error");
        assert!(
            matches!(err.kind, super::super::catalog_error::CatalogErrorKind::Access(_)),
            "a build failure must surface as a catalog Access error"
        );
        AdvisoryFileLock::try_acquire(&path).expect("lock must be released after a build error");
    }

    #[tokio::test]
    async fn coordinated_fast_path_serves_fresh_cache_without_locking() {
        // A fresh, same-scope cache is served by the fast path with no lock
        // taken: proven by holding the refresh lock with a peer and still being
        // served (the fast path never contends, so it cannot deadlock).
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("catalog.json");
        let reg: std::sync::Arc<dyn OciAccess> = std::sync::Arc::new(seed().await);
        Catalog::load_or_refresh_coordinated(&path, "localhost:5000", "", &reg, false, true)
            .await
            .unwrap();

        let _peer = AdvisoryFileLock::try_acquire(&path).expect("peer holds the refresh lock");
        let empty: std::sync::Arc<dyn OciAccess> = std::sync::Arc::new(MemoryRegistry::new());
        let cat = Catalog::load_or_refresh_coordinated(&path, "localhost:5000", "", &empty, false, false)
            .await
            .unwrap();
        assert_eq!(
            cat.entries().count(),
            1,
            "fresh same-scope cache served without the lock"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn coordinated_uncoordinated_rebuild_on_lock_fault() {
        // A lock I/O fault (here: a symlink planted at the cache path, which
        // `try_acquire` rejects) is not `Locked`, so the coordinator falls back
        // to an uncoordinated rebuild (`PreFlight::Rebuild(None)`) rather than
        // failing — the atomic write still replaces the path safely.
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("catalog.json");
        symlink(dir.path().join("nonexistent-target"), &path).unwrap();

        let reg: std::sync::Arc<dyn OciAccess> = std::sync::Arc::new(seed().await);
        let cat = Catalog::load_or_refresh_coordinated(&path, "localhost:5000", "", &reg, false, true)
            .await
            .expect("a lock fault degrades to an uncoordinated rebuild, not an error");
        assert_eq!(
            cat.entries().count(),
            1,
            "uncoordinated rebuild still builds the catalog"
        );
        assert!(
            !std::fs::symlink_metadata(&path).unwrap().file_type().is_symlink(),
            "the atomic write replaced the symlink with a real cache file"
        );
    }

    /// Seed a registry double advertising many repositories so the cap
    /// and name-prefilter are exercised.
    async fn seed_many(n: usize) -> CatalogRegistry {
        let inner = MemoryRegistry::new();
        let reg = "localhost:5000";
        let mut repos = Vec::new();
        for i in 0..n {
            let repo = format!("acme/skill-{i:04}");
            let id = Identifier::new_registry(repo.clone(), reg.to_string());
            let manifest = skill_manifest("kw", "desc");
            let d = inner.push_manifest(&id, &manifest).await.unwrap();
            inner.put_tag(&id, "latest", &d).await.unwrap();
            repos.push(repo);
        }
        CatalogRegistry {
            inner,
            repos,
            registry: reg.to_string(),
            blob_pulled: std::sync::Arc::new(Mutex::new(false)),
        }
    }

    #[tokio::test]
    async fn build_caps_repository_count() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("catalog.json");
        let reg: std::sync::Arc<dyn OciAccess> = std::sync::Arc::new(seed_many(MAX_CATALOG_REPOS + 25).await);
        let cat = Catalog::load_or_refresh(&path, "localhost:5000", "", &reg, false, true)
            .await
            .unwrap();
        assert_eq!(cat.entries().count(), MAX_CATALOG_REPOS, "build is capped");
        assert!(cat.truncated(), "a build that hit the cap reports truncation");
    }

    #[tokio::test]
    async fn build_under_cap_is_not_truncated() {
        // A candidate set that fits within the cap leaves the window
        // exhaustive, so a non-empty query can be read as complete.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("catalog.json");
        let reg: std::sync::Arc<dyn OciAccess> = std::sync::Arc::new(seed_many(MAX_CATALOG_REPOS - 1).await);
        let cat = Catalog::load_or_refresh(&path, "localhost:5000", "", &reg, false, true)
            .await
            .unwrap();
        assert_eq!(cat.entries().count(), MAX_CATALOG_REPOS - 1);
        assert!(!cat.truncated(), "an under-cap build is not truncated");
    }

    #[tokio::test]
    async fn build_exactly_at_cap_is_not_truncated() {
        // Boundary: exactly MAX_CATALOG_REPOS candidates fit — the probe
        // element (cap + 1) never materializes, so this is not truncation.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("catalog.json");
        let reg: std::sync::Arc<dyn OciAccess> = std::sync::Arc::new(seed_many(MAX_CATALOG_REPOS).await);
        let cat = Catalog::load_or_refresh(&path, "localhost:5000", "", &reg, false, true)
            .await
            .unwrap();
        assert_eq!(cat.entries().count(), MAX_CATALOG_REPOS);
        assert!(
            !cat.truncated(),
            "a build whose candidate count equals the cap is exhaustive, not truncated"
        );
    }

    #[tokio::test]
    async fn prefilter_scope_keeps_build_within_cap_untruncated() {
        // A name prefilter that selects a small candidate set must not flag
        // truncation even when the registry holds far more than the cap.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("catalog.json");
        let reg: std::sync::Arc<dyn OciAccess> = std::sync::Arc::new(seed_many(MAX_CATALOG_REPOS + 50).await);
        // Only `skill-0007` contains "0007".
        let cat = Catalog::load_or_refresh(&path, "localhost:5000", "0007", &reg, false, true)
            .await
            .unwrap();
        assert_eq!(cat.entries().count(), 1);
        assert!(!cat.truncated(), "a narrow prefilter is not truncated");
    }

    #[test]
    fn split_host_namespace_separates_host_and_namespace() {
        assert_eq!(split_host_namespace("ghcr.io/acme"), ("ghcr.io", Some("acme")));
        assert_eq!(split_host_namespace("localhost:5000"), ("localhost:5000", None));
        assert_eq!(
            split_host_namespace("localhost:5000/a/b"),
            ("localhost:5000", Some("a/b"))
        );
        assert_eq!(split_host_namespace("ghcr.io/"), ("ghcr.io/", None));
    }

    /// Seed a host with two namespaces; a namespaced configured registry
    /// must list the `_catalog` against the bare host and keep only its own
    /// namespace, with entries rooted at the host so `repo()` is consistent.
    async fn seed_namespaced() -> CatalogRegistry {
        let inner = MemoryRegistry::new();
        let host = "localhost:5000";
        for repo in ["acme/code-review", "other/foo"] {
            let id = Identifier::new_registry(repo.to_string(), host.to_string());
            let manifest = skill_manifest("kw", "desc");
            let d = inner.push_manifest(&id, &manifest).await.unwrap();
            inner.put_tag(&id, "latest", &d).await.unwrap();
        }
        CatalogRegistry {
            inner,
            repos: vec!["acme/code-review".to_string(), "other/foo".to_string()],
            registry: "localhost:5000/acme".to_string(),
            blob_pulled: std::sync::Arc::new(Mutex::new(false)),
        }
    }

    #[tokio::test]
    async fn namespaced_registry_scopes_to_its_namespace() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("catalog.json");
        let reg: std::sync::Arc<dyn OciAccess> = std::sync::Arc::new(seed_namespaced().await);
        let cat = Catalog::load_or_refresh(&path, "localhost:5000/acme", "", &reg, false, true)
            .await
            .unwrap();
        let repos: Vec<String> = cat.entries().map(CatalogEntry::repo).collect();
        // Only the configured namespace survives, and the entry is rooted
        // at the bare host (no doubled namespace).
        assert_eq!(repos, vec!["localhost:5000/acme/code-review".to_string()]);
        let e = cat.entries().next().unwrap();
        assert_eq!(e.registry, "localhost:5000");
        assert_eq!(e.repository, "acme/code-review");
    }

    #[tokio::test]
    async fn name_prefilter_scopes_the_build() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("catalog.json");
        let reg: std::sync::Arc<dyn OciAccess> = std::sync::Arc::new(seed_many(30).await);
        // Only `skill-0007` contains "0007".
        let cat = Catalog::load_or_refresh(&path, "localhost:5000", "0007", &reg, false, true)
            .await
            .unwrap();
        assert_eq!(cat.entries().count(), 1);
        assert_eq!(cat.entries().next().unwrap().repository, "acme/skill-0007");
    }

    #[tokio::test]
    async fn different_scope_forces_rebuild_online() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("catalog.json");
        let reg: std::sync::Arc<dyn OciAccess> = std::sync::Arc::new(seed_many(10).await);
        // Build under scope "0003".
        Catalog::load_or_refresh(&path, "localhost:5000", "0003", &reg, false, false)
            .await
            .unwrap();
        // A different scope on a still-fresh cache must rebuild (not serve
        // the narrow cached scope).
        let cat = Catalog::load_or_refresh(&path, "localhost:5000", "0005", &reg, false, false)
            .await
            .unwrap();
        assert_eq!(cat.entries().next().unwrap().repository, "acme/skill-0005");
    }

    #[test]
    fn entry_matches_query_case_insensitively() {
        let parse = SearchQuery::parse;
        let e = CatalogEntry {
            registry: "localhost:5000".to_string(),
            repository: "acme/code-review".to_string(),
            kind: Some("skill".to_string()),
            description: Some("Review code quality".to_string()),
            summary: Some("terse blurb".to_string()),
            keywords: vec!["lint".to_string()],
            repository_url: None,
            revision: None,
            created: None,
            deprecated: None,
            latest_tag: Some("latest".to_string()),
            version: None,
            fetched_at: ts(1),
        };
        assert!(e.matches(&parse("")), "empty query matches all");
        assert!(e.matches(&parse("REVIEW")), "repo path, case-insensitive");
        assert!(e.matches(&parse("quality")), "description substring");
        assert!(e.matches(&parse("BLURB")), "summary substring, case-insensitive");
        assert!(e.matches(&parse("lint")), "keyword");
        assert!(!e.matches(&parse("python")), "non-match");
        // Multi-term AND: both terms must match (repo + summary; repo + desc).
        assert!(e.matches(&parse("review blurb")), "multi-term AND over repo+summary");
        assert!(e.matches(&parse("review quality")), "multi-term AND over repo+desc");
        assert!(!e.matches(&parse("review python")), "multi-term miss ⇒ no match");
        // A bare kind keyword filters by kind, ANDed with text terms.
        assert!(e.matches(&parse("skill")), "kind keyword matches skill entry");
        assert!(e.matches(&parse("skill review")), "kind keyword AND text term");
        assert!(!e.matches(&parse("rule")), "kind keyword filters out non-rule");

        // A rule-kinded entry is filtered out by `skill` and in by `rule`.
        let r = CatalogEntry {
            kind: Some("rule".to_string()),
            ..e.clone()
        };
        assert!(r.matches(&parse("rule")), "rule keyword matches rule entry");
        assert!(!r.matches(&parse("skill")), "skill keyword filters out rule entry");
    }
}
