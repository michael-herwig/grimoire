// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! The shared catalog seam every front-end calls.
//!
//! `grim search` and the `grim mcp` server's `grim_search` tool browse the
//! same catalog over the same registry set through this seam: they annotate
//! each repository with the same install badge and apply the same query
//! filter. This module does that **once**: [`load_catalog`] loads (or
//! coordinately refreshes) every configured registry in parallel, filters
//! with the shared [`SearchQuery`] matcher, derives the [`StatusBadge`] for
//! every surviving row, and returns the result grouped by registry.
//! Front-ends shape the presentation (a flat table or a JSON payload) from
//! one source of truth. The TUI's migration onto this seam — and its
//! collapsible registry-tree projection — is a deferred follow-up; it still
//! browses a single registry directly via
//! [`Catalog::load_or_refresh_coordinated`].
//!
//! The catalog is built per registry under the empty (browse) scope and
//! filtered in memory — a build-time repository-name prefilter would drop
//! entries whose only match is in the summary / description / keywords
//! (those annotations are never fetched for filtered-out repos). This keeps
//! every front-end's result set identical.

use std::sync::Arc;

use crate::catalog::registry_catalog::Catalog;
use crate::catalog::search_match::SearchQuery;
use crate::config::ResolvedRegistry;
use crate::install::client_target::ClientTarget;
use crate::install::install_state::InstallState;
use crate::install::path_anchor::AnchorRoots;
use crate::install::status_badge::{StatusBadge, derive_badge};
use crate::lock::grimoire_lock::GrimoireLock;
use crate::oci::access::OciAccess;
use crate::store::paths::GrimPaths;

/// Scope inputs for badge derivation, resolved once by the caller and shared
/// across every row of every group.
pub struct BadgeContext<'a> {
    /// The scope's lock, if one exists.
    pub lock: Option<&'a GrimoireLock>,
    /// The scope's install state.
    pub state: &'a InstallState,
    /// The scope's resolved anchor roots.
    pub roots: &'a AnchorRoots,
    /// The currently-active client set for the scope (vendor dir present — see
    /// [`crate::install::target::detect_clients`]). A record's per-client
    /// outputs are reconciled against this so a client removed since install
    /// does not badge the repository as broken.
    pub active: &'a [ClientTarget],
}

/// One repository row: catalog metadata plus the derived install badge.
/// Everything any front-end needs from a catalog entry, computed once.
#[derive(Debug, Clone)]
pub struct CatalogRow {
    /// `skill` / `rule` / `agent` / `bundle`, or `None` when the manifest
    /// declared no kind.
    pub kind: Option<String>,
    /// The registry host the repository lives on.
    pub registry: String,
    /// The repository path within the registry.
    pub repository: String,
    /// The short catalog summary, if any.
    pub summary: Option<String>,
    /// The catalog description, if any.
    pub description: Option<String>,
    /// The catalog keywords.
    pub keywords: Vec<String>,
    /// The HTTPS source-repository URL, if any.
    pub repository_url: Option<String>,
    /// The publishing commit revision (`--git` opt-in), if any.
    pub revision: Option<String>,
    /// The publishing commit date (RFC3339, `--git` opt-in), if any.
    pub created: Option<String>,
    /// The publisher's deprecation message when the artifact is deprecated;
    /// `None` otherwise. Drives the search / TUI deprecation highlight.
    pub deprecated: Option<String>,
    /// The representative tag the metadata was read from.
    pub latest_tag: Option<String>,
    /// The highest concrete semver tag, if any.
    pub version: Option<String>,
    /// How this repository relates to the current scope.
    pub badge: StatusBadge,
}

impl CatalogRow {
    /// The fully-qualified `registry/repository` reference.
    pub fn repo(&self) -> String {
        format!("{}/{}", self.registry, self.repository)
    }
}

/// One registry's slice of the result set — the TUI tree's root node.
#[derive(Debug, Clone)]
pub struct CatalogGroup {
    /// The registry host (and optional namespace).
    pub registry: String,
    /// The configured alias for this registry, if any.
    pub alias: Option<String>,
    /// Whether this registry's browse window hit the repository cap.
    pub truncated: bool,
    /// RFC3339 timestamp of this registry's catalog build.
    pub built_at: String,
    /// Whether this group lacks freshly-built network data this call: `true`
    /// when the browse ran in `--offline` mode, or the registry was
    /// unavailable (a transport failure degrades the group to empty). A stale
    /// catalog served because a peer held the refresh lock is *not* currently
    /// distinguished here — no front-end consumes that finer signal yet
    /// (YAGNI); thread it through [`Catalog::load_or_refresh_coordinated`]'s
    /// return when one does.
    pub served_offline: bool,
    /// The matching rows, already filtered and badged, sorted by repository.
    pub rows: Vec<CatalogRow>,
}

/// The full, registry-grouped result of a catalog browse/search.
#[derive(Debug, Clone)]
pub struct CatalogResults {
    /// One group per configured registry, in resolution order.
    pub groups: Vec<CatalogGroup>,
}

impl CatalogResults {
    /// Whether any registry's browse window was truncated at the cap.
    pub fn any_truncated(&self) -> bool {
        self.groups.iter().any(|g| g.truncated)
    }

    /// Flatten every group's rows into one list in registry **declaration
    /// order** — the resolution precedence carried by [`Self::groups`] — with
    /// each group already sorted by repository. The default registry's
    /// artifacts come first, then each subsequent registry's, so `grim search`'s
    /// flat table matches the TUI tree's F13 precedence order rather than a
    /// global alphabetical merge (which would interleave registries and, for
    /// equal-prefix hosts, order non-deterministically by repository name).
    pub fn into_flat_rows(self) -> Vec<CatalogRow> {
        self.groups.into_iter().flat_map(|g| g.rows).collect()
    }
}

/// Load (or coordinately refresh) every configured registry's catalog in
/// parallel, filter by `query`, badge every surviving row, and return the
/// result grouped by registry.
///
/// A single registry's transport failure degrades **that group** to empty
/// (logged, marked `served_offline`) rather than failing the whole browse —
/// the other registries still return. The per-registry refresh is
/// coordinated across processes (advisory lock, serve-stale-on-contention)
/// so a long-lived MCP server and ad-hoc CLI/TUI runs sharing one
/// `$GRIM_HOME` never stampede the network.
///
/// # Errors
///
/// Currently infallible per registry (failures degrade to an empty group);
/// the `Result` is retained for forward compatibility with hard failures
/// that should abort the whole browse.
pub async fn load_catalog(
    paths: &GrimPaths,
    registries: &[ResolvedRegistry],
    query: &str,
    access: &Arc<dyn OciAccess>,
    badges: &BadgeContext<'_>,
    offline: bool,
    force: bool,
) -> Result<CatalogResults, crate::catalog::catalog_error::CatalogError> {
    let parsed = SearchQuery::parse(query);

    // Fan out one coordinated, per-registry refresh on a JoinSet. Each task
    // owns its inputs ('static); the borrowed `badges` stays on this task and
    // is applied after the joins (badge derivation is synchronous).
    let mut set: tokio::task::JoinSet<(usize, Option<Catalog>)> = tokio::task::JoinSet::new();
    for (idx, reg) in registries.iter().enumerate() {
        let path = paths.catalog_file_for(&reg.url);
        let registry = reg.url.clone();
        let kind = reg.kind;
        let git_dir = paths.index_git_dir_for(&reg.url);
        let access = Arc::clone(access);
        set.spawn(async move {
            // Browse scope (empty query) — the in-memory filter below applies
            // the real query so summary/description/keyword-only matches are
            // not dropped at build time. An index source lists from the
            // package index; a registry source walks `_catalog`.
            let result = if kind.is_index() {
                Catalog::load_or_refresh_index_coordinated(&path, &registry, kind, "", &git_dir, offline, force).await
            } else {
                Catalog::load_or_refresh_coordinated(&path, &registry, "", &access, offline, force).await
            };
            match result {
                Ok(catalog) => (idx, Some(catalog)),
                Err(e) => {
                    tracing::warn!("catalog for source '{registry}' unavailable: {e}");
                    (idx, None)
                }
            }
        });
    }

    // Collect into a BTreeMap keyed by input index: deterministic group order
    // regardless of completion order (quality-rust JoinSet rule) with no
    // separate sort. A task that panicked is logged and its registry degrades
    // to an absent (empty) group below rather than vanishing silently.
    let mut by_index: std::collections::BTreeMap<usize, Option<Catalog>> = std::collections::BTreeMap::new();
    while let Some(joined) = set.join_next().await {
        match joined {
            Ok((idx, catalog)) => {
                by_index.insert(idx, catalog);
            }
            Err(e) => tracing::error!("catalog refresh task failed to join: {e}"),
        }
    }

    let mut groups = Vec::with_capacity(registries.len());
    for (idx, reg) in registries.iter().enumerate() {
        let catalog = by_index.remove(&idx).flatten();
        let group = match catalog {
            Some(catalog) => {
                let rows: Vec<CatalogRow> = catalog
                    .entries()
                    .filter(|e| e.matches(&parsed))
                    .map(|e| CatalogRow {
                        kind: e.kind.clone(),
                        registry: e.registry.clone(),
                        repository: e.repository.clone(),
                        summary: e.summary.clone(),
                        description: e.description.clone(),
                        keywords: e.keywords.clone(),
                        repository_url: e.repository_url.clone(),
                        revision: e.revision.clone(),
                        created: e.created.clone(),
                        deprecated: e.deprecated.clone(),
                        latest_tag: e.latest_tag.clone(),
                        version: e.version.clone(),
                        badge: derive_badge(
                            &e.registry,
                            &e.repository,
                            badges.lock,
                            badges.state,
                            badges.roots,
                            badges.active,
                        ),
                    })
                    .collect();
                CatalogGroup {
                    registry: reg.url.clone(),
                    alias: reg.alias.clone(),
                    truncated: catalog.truncated(),
                    built_at: catalog.built_at().to_string(),
                    served_offline: offline,
                    rows,
                }
            }
            None => CatalogGroup {
                registry: reg.url.clone(),
                alias: reg.alias.clone(),
                truncated: false,
                built_at: String::new(),
                served_offline: true,
                rows: Vec::new(),
            },
        };
        groups.push(group);
    }

    Ok(CatalogResults { groups })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use async_trait::async_trait;

    use super::*;
    use crate::context::Context;
    use crate::oci::access::Operation;
    use crate::oci::access::error::{AccessError, AccessErrorKind};
    use crate::oci::manifest::OciManifest;
    use crate::oci::{Digest, Identifier, PinnedIdentifier};

    /// An access whose catalog listing always fails — drives the per-registry
    /// degrade-to-empty-group path. Only `list_catalog` is reached (a build
    /// aborts there), so the rest is `unreachable!` rather than stubbed.
    struct FailingAccess;

    #[async_trait]
    impl OciAccess for FailingAccess {
        async fn resolve_digest(&self, _: &Identifier, _: Operation) -> Result<Option<Digest>, AccessError> {
            unreachable!("not reached once list_catalog fails")
        }
        async fn fetch_manifest(&self, _: &PinnedIdentifier) -> Result<Option<OciManifest>, AccessError> {
            unreachable!()
        }
        async fn fetch_blob(&self, _: &Identifier, _: &Digest) -> Result<Option<Vec<u8>>, AccessError> {
            unreachable!()
        }
        async fn list_tags(&self, _: &Identifier) -> Result<Option<Vec<String>>, AccessError> {
            unreachable!()
        }
        async fn list_catalog(&self, _: &str) -> Result<Vec<String>, AccessError> {
            Err(AccessError::without_identifier(AccessErrorKind::Registry(
                std::io::Error::other("simulated registry outage").into(),
            )))
        }
        async fn push_blob(&self, _: &Identifier, _: &[u8]) -> Result<Digest, AccessError> {
            unreachable!()
        }
        async fn push_manifest(&self, _: &Identifier, _: &OciManifest) -> Result<Digest, AccessError> {
            unreachable!()
        }
        async fn put_tag(&self, _: &Identifier, _: &str, _: &Digest) -> Result<(), AccessError> {
            unreachable!()
        }
    }

    #[tokio::test]
    async fn per_registry_failure_degrades_to_empty_group_in_input_order() {
        // A registry whose walk fails must degrade *that* group to empty
        // (flagged served_offline) without failing the whole browse, and the
        // groups must stay in resolution order regardless of join order.
        let tmp = tempfile::tempdir().unwrap();
        let ctx = Context::hermetic(tmp.path().to_path_buf());
        let paths = GrimPaths::new(tmp.path().to_path_buf());
        let state = InstallState::empty(tmp.path());
        let roots = AnchorRoots::resolve(PathBuf::new(), &ctx);
        let badges = BadgeContext {
            lock: None,
            state: &state,
            roots: &roots,
            active: &ClientTarget::ALL,
        };

        let registries = vec![
            ResolvedRegistry {
                url: "registry.one/ns".to_string(),
                alias: Some("one".to_string()),
                is_default: true,
                kind: crate::config::registry_resolve::SourceKind::Registry,
            },
            ResolvedRegistry {
                url: "registry.two".to_string(),
                alias: None,
                is_default: false,
                kind: crate::config::registry_resolve::SourceKind::Registry,
            },
        ];
        let access: Arc<dyn OciAccess> = Arc::new(FailingAccess);

        let results = load_catalog(&paths, &registries, "", &access, &badges, false, true)
            .await
            .expect("a per-registry failure never fails the whole browse");

        assert_eq!(results.groups.len(), 2, "one group per registry");
        assert_eq!(results.groups[0].registry, "registry.one/ns");
        assert_eq!(results.groups[0].alias.as_deref(), Some("one"));
        assert_eq!(results.groups[1].registry, "registry.two");
        for g in &results.groups {
            assert!(g.rows.is_empty(), "a failed registry yields no rows");
            assert!(g.served_offline, "a failed registry is flagged served_offline");
        }
        assert!(!results.any_truncated());
    }
}
