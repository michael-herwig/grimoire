// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! Ephemeral per-scope cache for bundle member nodes.
//!
//! This module is the single home for the types that describe the virtual
//! child rows a bundle leaf may expand into. The cache lives on
//! [`super::state::TuiState`] and is deliberately **outside** the
//! `rows`/`filtered`/`marked` index model — virtual member rows never
//! enter the index space; they are projection-only display artifacts.
//!
//! Cache key: `(scope_label, bundle_repo)` — scope-keyed so an entry
//! for one scope is never consulted under another.
//!
//! See `plan_tui_tree_view_phase2.md` D7 for the binding design decision.

use std::collections::HashSet;

use crate::oci::ArtifactKind;
use crate::oci::bundle::BundleMember;

use super::state::ArtifactState;

/// The scope-keyed cache key: `(scope_label, bundle_repo)`.
///
/// `scope_label` is the human label for the active scope (`"project"` /
/// `"global"`). `bundle_repo` is the `registry/repository` reference for
/// the bundle leaf row. Together they prevent cross-scope cache pollution
/// even when the same bundle repo appears in both scopes.
pub type BundleMemberKey = (String, String);

/// The state of the per-bundle member cache entry.
///
/// Closed internal enum — matches stay total, no `#[non_exhaustive]`.
/// Lifecycle:
/// - A bundle leaf is expanded with no entry → emit
///   `TuiAction::LoadBundleMembers`; insert `Loading` immediately.
/// - `BundleMembersMsg::Ready` received → replace with `Ready(members)`.
/// - `BundleMembersMsg::Failed` received → replace with `Failed(reason)`.
/// - Offline + no lock snapshot → insert `Offline` (no spawn).
/// - `set_rows` → clear entire cache.
/// - `merge_catalog_rows` → prune entries whose `bundle_repo` no longer
///   appears in the fresh rows.
#[derive(Debug, Clone)]
pub enum BundleMemberCache {
    /// A fetch is in flight; renders a `"loading…"` placeholder child row.
    Loading,
    /// Members successfully fetched (from the lock snapshot or the
    /// registry). Each `MemberNode` is a virtual child row.
    Ready(Vec<MemberNode>),
    /// The fetch failed; caches the sanitized reason string. No retry on
    /// subsequent Expand gestures (no retry storm on a failing registry).
    Failed(String),
    /// Offline mode and no lock snapshot for this bundle — no fetch
    /// attempted. Renders a single `"(offline — members unavailable)"`
    /// placeholder child row.
    Offline,
}

/// A single virtual member node, derived from a `BundleMember` at cache
/// build time and stored in [`BundleMemberCache::Ready`].
///
/// The `label` field holds the **raw** (untrusted) member name from the
/// registry or lock snapshot. It is sanitized at render time via
/// [`super::render::sanitize_member_label`] — never before display.
///
/// `member_repo` is `None` when `Identifier::parse(member.id)` fails;
/// the node still renders (TUI fails soft).
#[derive(Debug, Clone)]
pub struct MemberNode {
    /// Artifact kind of the member (`Skill`, `Rule`, `Agent`).
    /// Bundle members that are themselves bundles are rejected at expansion
    /// (`resolver.rs:367-372`) so `Bundle` should never appear, but the
    /// type carries the full set so render arms stay exhaustive.
    pub kind: ArtifactKind,
    /// Raw member name — **untrusted**; sanitize before terminal output.
    pub label: String,
    /// `registry/repository` reference derived from `Identifier::parse(id)`;
    /// `None` when the id fails to parse (fail-soft: node still renders
    /// without a deep-link target).
    pub member_repo: Option<String>,
    /// Install state, derived via `derive_artifact_state` against the
    /// active scope's lock + install state. `NotInstalled` when no lock
    /// entry exists.
    pub state: ArtifactState,
    /// `true` when `member_repo` matches some `rows[].repo` in the
    /// current catalog — the static related-highlight signal.
    /// Computed once at cache build; invalidated with the cache entry.
    pub related: bool,
}

/// Translate one raw [`BundleMember`] into a [`MemberNode`], fail-soft.
///
/// Returns `None` (with a `tracing::warn!`) when the member's `id` cannot
/// be parsed to extract a `registry/repository` string. In all other cases
/// a `MemberNode` is returned — the `member_repo` field is `None` when the
/// id is unparseable, but the node still renders without a deep-link target.
///
/// The `row_repos` set is the O(n) pre-built set of catalog leaf repos used
/// for the related-highlight signal (D2/P3.7). The `state` parameter is the
/// per-member install state already derived by the caller (lock-first path
/// derives it from the lock + install record; async-drain path supplies
/// `ArtifactState::NotInstalled` because the async fetch has no lock context).
///
/// # Boundary invariant
///
/// `member.name` (→ `label`) is stored **raw** per the two-boundary
/// invariant: sanitize at display time only (`sanitize_member_label`), never
/// here. This keeps the cache faithful to the registry/lock source.
pub fn member_node_from(member: &BundleMember, row_repos: &HashSet<&str>, state: ArtifactState) -> Option<MemberNode> {
    let member_repo = match crate::oci::Identifier::parse(&member.id) {
        Ok(id) => Some(format!("{}/{}", id.registry(), id.repository())),
        Err(e) => {
            // Fail-soft: drop member with unparseable id; warn for observability.
            tracing::warn!(
                id = %member.id,
                error = %e,
                "dropping bundle member with unparseable id (fail-soft)"
            );
            return None;
        }
    };
    // related = true when the member's repo also appears as a real catalog
    // leaf (static related-highlight, D2/P3.7).
    let related = member_repo.as_deref().is_some_and(|repo| row_repos.contains(repo));
    Some(MemberNode {
        kind: member.kind,
        label: member.name.clone(),
        member_repo,
        state,
        related,
    })
}
