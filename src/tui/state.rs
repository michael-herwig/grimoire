// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! The pure TUI screen model and its transitions.
//!
//! This module is deliberately free of ratatui, crossterm, and `std::io`
//! — every transition is a pure function over [`TuiState`] so the screen
//! logic is exhaustively unit-testable without a terminal. The render loop
//! ([`super::app`]) drives these transitions; [`super::render`] projects
//! the state for display.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::catalog::SearchQuery;

use super::bundle_members::{BundleMemberCache, BundleMemberKey};

/// The install state of a catalog repository relative to the active
/// scope, as shown in the TUI.
///
/// Richer than [`crate::install::status_badge::StatusBadge`] (which
/// `search`/`status` share): it splits "an install record exists but its
/// client outputs are gone or unreadable" out of `NotInstalled` into its
/// own [`ArtifactState::IntegrityMissing`] so the user can tell a
/// never-installed entry apart from a broken/tampered one. Precedence
/// otherwise mirrors `status.rs::derive_state`.
///
/// Closed internal enum — matches stay total, no `#[non_exhaustive]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactState {
    /// Not declared/locked/recorded in this scope.
    NotInstalled,
    /// Locked, recorded, every output present and content intact.
    Installed,
    /// A bundle member that is present and intact but **not** also declared
    /// standalone — it is installed only because a bundle provides it. Set on
    /// member nodes only (a catalog row never derives it); `Modified`,
    /// `Outdated`, and `IntegrityMissing` take precedence over it.
    ViaBundle,
    /// Locked + recorded, but the locked pin is ahead of the record.
    Outdated,
    /// Recorded, outputs present, but on-disk content drifted.
    Modified,
    /// An install record exists but one or more client outputs are
    /// missing or unreadable — the integrity record cannot be honored.
    IntegrityMissing,
}

impl std::fmt::Display for ArtifactState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::NotInstalled => "not-installed",
            Self::Installed => "installed",
            Self::ViaBundle => "via-bundle",
            Self::Outdated => "outdated",
            Self::Modified => "modified",
            Self::IntegrityMissing => "integrity-missing",
        })
    }
}

/// Whether the catalog browser renders a flat list or a grouped tree.
///
/// Closed internal enum — matches stay total, no `#[non_exhaustive]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ViewMode {
    /// Flat list view (default at startup unless overridden by config).
    #[default]
    Flat,
    /// Grouped collapsible tree view.
    Tree,
}

/// Which interaction mode the screen is in.
///
/// Closed internal enum — matches stay total, no `#[non_exhaustive]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Browsing the list; navigation keys move the selection.
    List,
    /// Editing the search box; character keys edit the query.
    Search,
    /// Viewing the selected row's detail pane.
    Detail,
    /// Viewing the keybinding help overlay.
    Help,
    /// Choosing a specific version for the selected row from a popup.
    VersionPick,
}

/// The modal version picker: the row it targets, the fetched tags, and the
/// in-popup selection. `tags` is empty while the lazy registry lookup is
/// still in flight (`loading`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionPicker {
    /// The `rows` index this picker pins a version onto.
    pub row: usize,
    /// Available tags, highest concrete version first; empty while loading.
    pub tags: Vec<String>,
    /// Selection index into `tags`.
    pub selected: usize,
    /// Whether the tag list is still being fetched from the registry.
    pub loading: bool,
}

/// One catalog row, projected from a [`crate::catalog::registry_catalog::CatalogEntry`]
/// plus the scope-derived install badge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TuiRow {
    /// `skill` / `rule`, or `-` when the manifest declared no kind.
    pub kind: String,
    /// Authoritative registry host + optional namespace (tree group key).
    ///
    /// Sourced from the catalog entry directly — never re-derived by splitting
    /// `repo` — so namespaced registries like `ghcr.io/acme` are handled
    /// correctly (D-TREE).
    pub registry: String,
    /// Repository path within the registry, segmented below the registry root
    /// by the tree builder (D-TREE).
    pub repository: String,
    /// Fully-qualified `registry/repository` reference.
    ///
    /// Kept alongside `registry`/`repository` for compatibility with the many
    /// production paths that key on this field (search filter, badge lookup,
    /// update-check identity). Always equals the [`Self::repo`] accessor; full
    /// removal (migrating every `row.repo` read to `row.repo()`) is a deferred
    /// follow-up.
    pub repo: String,
    /// Catalog description (empty string when absent).
    pub description: String,
    /// Catalog short summary (empty string when absent).
    pub summary: String,
    /// Catalog keywords.
    pub keywords: Vec<String>,
    /// HTTPS source-repository URL (already vetted by the catalog's
    /// `https://` read-back guard); target of the `o` open action.
    pub repository_url: Option<String>,
    /// Publisher's deprecation message when the artifact is deprecated;
    /// `None` otherwise. Drives the row marker + detail-pane highlight.
    pub deprecated: Option<String>,
    /// The representative tag (empty string when absent) — may be the
    /// moving `latest` pointer; used as the resolution fallback.
    pub latest_tag: String,
    /// The highest concrete version to display in the Tag column (falls
    /// back to `latest_tag` when no semver tag exists).
    pub version: String,
    /// A user-pinned version chosen via the picker; when set, install /
    /// update target this tag instead of the default resolution.
    pub pinned_version: Option<String>,
    /// The install status of this repository in the active scope.
    pub state: ArtifactState,
}

impl TuiRow {
    /// The fully-qualified `registry/repository` reference, derived from the
    /// authoritative [`Self::registry`] and [`Self::repository`] fields.
    ///
    /// Equals the stored `repo` field, which is retained for the many callers
    /// that key on it; collapsing the two into this accessor is a deferred
    /// follow-up.
    pub fn repo(&self) -> String {
        format!("{}/{}", self.registry, self.repository)
    }
}

/// Per-registry health summary, aggregated from the loaded
/// [`crate::catalog::catalog_service::CatalogResults`].
///
/// One seam for the status line: offline registries and truncated registries
/// are named so the user can tell which registries need attention.
///
/// C6 stub — aggregation logic is implement phase (T4).
#[derive(Debug, Clone, Default)]
pub struct RegistryHealth {
    /// Registry URLs that were served from offline / stale cache.
    pub offline: Vec<String>,
    /// Registry URLs whose browse window was truncated at the cap.
    pub truncated: Vec<String>,
}

/// The whole screen model.
#[derive(Debug, Clone)]
pub struct TuiState {
    /// Every catalog row (unfiltered).
    pub rows: Vec<TuiRow>,
    /// Indices into `rows` matching the current query, in row order.
    pub filtered: Vec<usize>,
    /// Selection index into the **active view's display list**: into
    /// `filtered` in [`ViewMode::Flat`], into the flattened tree
    /// ([`Self::flattened`], group headers interleaved) in
    /// [`ViewMode::Tree`]. Never an index into `rows`. Interpret it only via
    /// [`Self::selected_row_index`] / [`Self::flattened`]; relocate it across a
    /// view switch or row rebuild via [`Self::select_row`] (identity-preserving)
    /// so an un-marked action never targets the wrong artifact.
    pub selected: usize,
    /// Detail-pane vertical scroll offset (post-wrap rows). Clamped at
    /// both ends by [`Self::scroll_detail`]: 0 at the top, the content's
    /// post-wrap height minus the viewport at the bottom
    /// ([`super::detail::scroll_max`]).
    pub detail_scroll: u16,
    /// The terminal size `(width, height)` the app last observed — the
    /// input for the detail pane's scroll geometry. Kept current by the
    /// event loop (initial size + every resize event).
    pub term_size: (u16, u16),
    /// Help-overlay vertical scroll offset (rows). Reset to 0 when the `?`
    /// overlay opens and clamped by [`Self::scroll_help`] so the dialog can
    /// scroll when it does not fully fit the terminal height.
    pub help_scroll: u16,
    /// The live search query.
    pub query: String,
    /// Current interaction mode.
    pub mode: Mode,
    /// Whether a catalog load is in flight.
    pub loading: bool,
    /// Whether the catalog was served offline (cached / possibly stale).
    pub offline: bool,
    /// Whether the loaded catalog window was truncated at the browse cap
    /// (more repositories existed than were walked), so the row list /
    /// search results may be silently incomplete. Surfaced as a quiet hint.
    pub truncated: bool,
    /// A one-line status / hint shown at the bottom.
    pub status_line: String,
    /// Marked rows for batch actions, as indices into `rows` (stable
    /// across filter changes — a mark survives a query edit).
    pub marked: std::collections::BTreeSet<usize>,
    /// The active scope label (`project` / `global`), shown in the title.
    pub scope_label: String,
    /// The active version picker, when [`Mode::VersionPick`].
    pub picker: Option<VersionPicker>,
    /// The effective default registry; when a row's registry host equals
    /// it the registry prefix is elided from the displayed name (shorter
    /// names) while the stored `repo` keeps the full reference.
    pub default_registry: Option<String>,
    /// The active scope's effective selected client names (`claude`,
    /// `opencode`, …), surfaced in the status area. Pure display data — no
    /// effect on filtering or rows.
    pub clients: Vec<String>,
    /// Whether the catalog renders as a flat list or a grouped tree.
    pub view_mode: ViewMode,
    /// The set of tree-group keys that are collapsed (descendants hidden).
    pub collapsed: BTreeSet<String>,
    /// When true, insert a type-level group between the registry root and
    /// the path segments.
    pub group_by_type: bool,
    /// Characters (each a `String`) on which the repository path is split
    /// into nested groups. Defaults to `["/"]`.
    pub tree_separators: Vec<String>,
    /// The resolved registries in precedence order (F13). Threaded into
    /// [`super::tree::TreeBuildOptions`] so the tree's registry roots follow
    /// resolution precedence (not alphabetical order) and every resolved
    /// registry yields a root even with zero matching rows. Empty in
    /// single-registry / elided sessions.
    pub registry_order: Vec<String>,
    /// Ephemeral per-scope cache for bundle member nodes.
    ///
    /// Keyed by `(scope_label, bundle_repo)` so entries from one scope are
    /// never consulted under another. Lives **outside** `rows`/`filtered`/
    /// `marked` — virtual member rows never enter the index space.
    ///
    /// Lifecycle:
    /// - Cleared wholesale on `set_rows` (full catalog reload).
    /// - Pruned on `merge_catalog_rows` (entries whose `bundle_repo` no
    ///   longer appears in the fresh rows are dropped; survivors retained).
    /// - A `Failed` entry is never re-fetched on Expand (no retry storm).
    pub bundle_members: HashMap<BundleMemberKey, BundleMemberCache>,
    /// Explicit bundle-leaf expand state. A bundle leaf whose key is present
    /// here has been explicitly expanded (member rows are spliced in by
    /// `flatten_with_members`). Absent = collapsed (default state).
    ///
    /// This is ORTHOGONAL to `collapsed` (which gates GROUP descendants with
    /// the opposite default polarity — absent from `collapsed` = expanded).
    /// Two sets are needed because bundle leaves default-collapsed while groups
    /// default-expanded (D3a / GAP-1). Keys are `LeafNode.key` strings.
    ///
    /// Lifecycle mirrors `bundle_members` exactly (D3b):
    /// - Cleared wholesale on `set_rows`.
    /// - Pruned on `merge_catalog_rows` (same bundle-rows-only prune).
    /// - Cleared on scope toggle in `app.rs`.
    pub expanded_bundles: BTreeSet<String>,
    /// Per-registry health summary (C6).
    ///
    /// Aggregated from [`crate::catalog::catalog_service::CatalogResults`] on
    /// each catalog load, so the status line can name which registries are
    /// offline or truncated. Empty on the initial default state.
    ///
    /// C6 stub — aggregation wired in implement phase (T4).
    pub registry_health: RegistryHealth,
    /// Display labels for registries: maps registry URL → configured alias.
    ///
    /// When a `[[registries]]` entry has an `alias`, it is stored here so
    /// the flat list's Registry column and the tree registry-root rows can
    /// show the alias instead of the raw URL. Falls back to the URL when
    /// no alias was configured (see [`Self::registry_label`]).
    ///
    /// Populated on each successful catalog load via [`Self::set_registry_labels`].
    pub registry_labels: BTreeMap<String, String>,
}

impl Default for TuiState {
    fn default() -> Self {
        Self {
            rows: Vec::new(),
            filtered: Vec::new(),
            selected: 0,
            detail_scroll: 0,
            // A sane universal default until the app reports the real
            // size (before the first key event is ever processed).
            term_size: (80, 24),
            help_scroll: 0,
            query: String::new(),
            mode: Mode::List,
            loading: true,
            offline: false,
            truncated: false,
            status_line: String::new(),
            marked: std::collections::BTreeSet::new(),
            scope_label: String::new(),
            picker: None,
            default_registry: None,
            clients: Vec::new(),
            view_mode: ViewMode::Flat,
            collapsed: BTreeSet::new(),
            group_by_type: false,
            tree_separators: vec!["/".into()],
            registry_order: Vec::new(),
            bundle_members: HashMap::new(),
            expanded_bundles: BTreeSet::new(),
            registry_health: RegistryHealth::default(),
            registry_labels: BTreeMap::new(),
        }
    }
}

/// A resort- and reshape-stable handle to "what the cursor is on", captured
/// before a mutation (catalog refresh, view toggle, tree-option change) and
/// re-applied after. Leaves are identified by their stable `repo` string and
/// groups by their path-derived display key (both survive a `set_rows`
/// resort and a tree reshape), so an un-marked action never silently retargets
/// a different artifact or subtree.
enum SelectionAnchor {
    /// The selected leaf, by its stable `repo` string.
    Leaf(String),
    /// The selected group, by its display key, with the `repo` of its first
    /// descendant as a fallback target when the key does not survive a reshape.
    Group { key: String, first_repo: Option<String> },
    /// The selected virtual bundle-member row. Restores the cursor onto the
    /// parent bundle leaf after a reshape (members are display-only and do not
    /// survive a `set_rows` or flatten rebuild). `parent_bundle_repo` is the
    /// stable `registry/repository` of the bundle that owns this member.
    Member { parent_bundle_repo: String },
    /// Nothing actionable was selected.
    None,
}

/// Body line count of the `?` help overlay: the "Keybindings" header, a blank
/// separator, and one row per keybinding entry. Single source for help-scroll
/// clamping; the entry text lives in [`super::render::help_entries`] and
/// `render::tests::help_body_line_count_matches_state` guards it against drift.
pub(crate) const HELP_BODY_LINES: u16 = 18;

impl TuiState {
    /// A fresh state in the loading phase.
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace the catalog rows (a load completed). Rows are sorted once
    /// here — grouped by kind, then by case-insensitive leaf name — so the
    /// flat list always reads in a stable order and every downstream index
    /// (`filtered`, `marked`, app.rs batch targets) refers to the sorted
    /// order. This is the single choke point: marks are cleared below and
    /// app.rs derives per-row state by the `repo` string, never by a row
    /// index cached across a `set_rows` call, so sorting here is safe.
    pub fn set_rows(&mut self, mut rows: Vec<TuiRow>) {
        rows.sort_by(|a, b| {
            a.kind.cmp(&b.kind).then_with(|| {
                leaf_name(&a.repo)
                    .to_lowercase()
                    .cmp(&leaf_name(&b.repo).to_lowercase())
            })
        });
        self.rows = rows;
        self.loading = false;
        self.recompute_filter();
        self.selected = 0;
        self.detail_scroll = 0;
        // Row identities changed wholesale — stale marks would point at
        // unrelated rows.
        self.marked.clear();
        // Full reload invalidates all bundle-member cache entries: the set of
        // bundles, their repos, and their members may all have changed.
        self.bundle_members.clear();
        // Lifecycle (D3b): expanded_bundles mirrors bundle_members — clear
        // together so no stale expand state remains after a catalog reload.
        self.expanded_bundles.clear();
    }

    /// Reconcile a freshly-refreshed catalog row set into the current screen
    /// model, **preserving the user's in-flight interaction** across a
    /// background refresh, then route through [`Self::set_rows`] so the
    /// kind-sort and the active filter stay consistent.
    ///
    /// A background catalog refresh arrives *while the user is browsing*: it
    /// re-derives every row's `state` from the on-disk lock + install record
    /// (which has not seen the live registry re-resolve) and may add or drop
    /// repositories. A naive replace would (a) erase a just-flipped
    /// `↑ Outdated`, (b) wipe the marked batch set, and (c) snap the cursor
    /// back to the top mid-scroll — a spec violation. This reconciler keys
    /// everything by the stable `repo` string so none of that happens:
    ///
    /// - **Live `↑` flag**: a live `Outdated` is re-applied **only** when the
    ///   fresh row is `Installed` (same precedence as
    ///   [`Self::mark_outdated_if_installed`] — a fresh `Modified` /
    ///   `IntegrityMissing` is stronger on-disk truth and wins).
    /// - **Picker pin**: the user's `pinned_version` is carried forward (it
    ///   is never part of the catalog).
    /// - **Marks**: the marked set is translated old-index → `repo` → new
    ///   index, so a mark survives the resort; marks on repos that vanished
    ///   from the fresh set drop (the row no longer exists to act on).
    /// - **Cursor**: the selection follows the same `repo` **only while it is
    ///   still visible** — present in the row set *and* in the active filter;
    ///   then it lands on that repo's new display position via
    ///   [`Self::select_row`] (view-aware: the flattened tree position in tree
    ///   mode, the `filtered` position in flat mode). If the repo vanished or
    ///   the active query now filters it out, the cursor resets to the top of
    ///   the view (index 0) — the reset `set_rows` already applied — and a
    ///   view-aware clamp only further adjusts in the degenerate empty-display
    ///   case (selection forced to 0). It never dangles.
    /// - **Filter**: the active query is untouched; `set_rows` recomputes
    ///   `filtered` against it.
    ///
    /// New repositories appear; vanished ones drop.
    pub fn merge_catalog_rows(&mut self, mut fresh: Vec<TuiRow>) {
        // Snapshot what the user is currently interacting with, keyed by the
        // stable repo string (indices are about to be invalidated by the
        // resort inside `set_rows`).
        let marked_repos: std::collections::HashSet<String> = self
            .marked
            .iter()
            .filter_map(|&i| self.rows.get(i).map(|r| r.repo.clone()))
            .collect();
        let selection = self.selection_anchor();

        for row in &mut fresh {
            let Some(existing) = self.rows.iter().find(|r| r.repo == row.repo) else {
                continue;
            };
            if existing.state == ArtifactState::Outdated && row.state == ArtifactState::Installed {
                row.state = ArtifactState::Outdated;
            }
            if row.pinned_version.is_none() {
                row.pinned_version = existing.pinned_version.clone();
            }
        }

        // Snapshot the bundle-member cache AND expanded_bundles before the
        // set_rows call erases them, so we can restore entries that survive the
        // merge (repos still present in the fresh catalog keep their cached
        // member list + expand state; repos that vanished are pruned).
        let saved_bundle_members = std::mem::take(&mut self.bundle_members);
        let saved_expanded_bundles = std::mem::take(&mut self.expanded_bundles);

        // The single kind-sort + filter choke point; clears marks and resets
        // selection, both of which we restore by repo key below.
        self.set_rows(fresh);

        // Re-populate bundle_members: restore entries whose bundle_repo still
        // exists in the post-merge row set as a bundle kind. Non-bundle repos
        // must not accidentally keep a stale bundle cache entry (W7: scope the
        // prune to bundle rows only so a skill/rule with the same repo name does
        // not prevent eviction of the bundle cache entry when the bundle vanishes).
        //
        // Lifecycle (D3b): expanded_bundles is pruned the same bundle-rows-only
        // way, using the same live_repos set so both are pruned in lockstep.
        if !saved_bundle_members.is_empty() || !saved_expanded_bundles.is_empty() {
            let live_repos: std::collections::HashSet<&str> = self
                .rows
                .iter()
                .filter(|r| r.kind == "bundle")
                .map(|r| r.repo.as_str())
                .collect();
            for (key, cache) in saved_bundle_members {
                if live_repos.contains(key.1.as_str()) {
                    self.bundle_members.insert(key, cache);
                }
            }
            // F3: expanded_bundles keys are now FULL bundle repo strings
            // (registry/repository), the same identity used by bundle_members.
            // The prune is a direct membership test: retain a key iff the full
            // repo is still present in the fresh live bundle-repo set.
            // This eliminates the brittle rsplit('/') leaf-name heuristic and
            // the P3 TODO comment.
            for key in saved_expanded_bundles {
                if live_repos.contains(key.as_str()) {
                    self.expanded_bundles.insert(key);
                }
            }
        }

        // Re-apply marks by repo: a mark survives the resort, and a mark on a
        // repo that vanished simply drops (no row to act on).
        if !marked_repos.is_empty() {
            self.marked = self
                .rows
                .iter()
                .enumerate()
                .filter(|(_, r)| marked_repos.contains(&r.repo))
                .map(|(i, _)| i)
                .collect();
        }

        // Re-position the cursor on the same artifact (leaf) or subtree (group)
        // by stable identity. In tree mode `selected` indexes the flattened
        // display list (group headers interleaved), so a flat position would
        // land on the wrong row — and a following un-marked install/update/
        // delete would act on the wrong artifact or a different group's whole
        // subtree. The anchor was captured before the `set_rows` resort.
        self.restore_selection(selection);
    }

    /// Number of selectable rows in the current view mode.
    fn display_len(&self) -> usize {
        match self.view_mode {
            ViewMode::Flat => self.filtered.len(),
            ViewMode::Tree => self.flattened().len(),
        }
    }

    /// Clamp the selection into the tree's visible range after a collapse/expand,
    /// using a `visible_len` already computed by the caller to avoid a redundant
    /// tree rebuild.
    fn clamp_tree_selection_to(&mut self, visible_len: usize) {
        if visible_len == 0 {
            self.selected = 0;
        } else if self.selected >= visible_len {
            self.selected = visible_len - 1;
        }
    }

    /// The `rows` index of the current selection, if any.
    ///
    /// In flat mode: the selected index into `filtered`.
    /// In tree mode: the `row` index of the selected leaf, or `None` when
    /// a group is selected (groups have no single row).
    pub fn selected_row_index(&self) -> Option<usize> {
        match self.view_mode {
            ViewMode::Flat => self.filtered.get(self.selected).copied(),
            ViewMode::Tree => {
                let flat = self.flattened();
                match flat.get(self.selected) {
                    Some(super::tree::DisplayRow::Leaf { row, .. }) => Some(*row),
                    // Groups have no single `rows` index — batch op targets all
                    // descendant rows (see `action_targets`).
                    Some(super::tree::DisplayRow::Group { .. }) => None,
                    // Virtual members carry no `rows` index (projection-only).
                    Some(super::tree::DisplayRow::Member { .. }) => None,
                    // Out-of-range or empty display list.
                    None => None,
                }
            }
        }
    }

    /// Whether the row at `rows` index `i` is marked.
    pub fn is_row_marked(&self, i: usize) -> bool {
        self.marked.contains(&i)
    }

    /// Toggle the mark on the current selection. No-op without a
    /// selectable target. In tree mode, marking a group materializes all
    /// its descendant leaf `rows` indices into `marked` (smart toggle:
    /// if all are marked, clear them; otherwise mark all).
    pub fn toggle_mark_selected(&mut self) {
        if self.view_mode == ViewMode::Tree && self.selected_is_group() {
            // Group mark: cascade into descendant leaf rows.
            let flat = self.flattened();
            let Some(display_row) = flat.get(self.selected) else {
                return;
            };
            let descendant_rows: Vec<usize> = match display_row {
                super::tree::DisplayRow::Group { rows, .. } => rows.clone(),
                super::tree::DisplayRow::Leaf { .. } => return,
                // Virtual members carry no `rows` index — marking is a no-op
                // (Phase 2 read-only; guard already returns early above via
                // `selected_is_group()` being false for Member, but the match
                // must be exhaustive per closed-enum discipline).
                super::tree::DisplayRow::Member { .. } => return,
            };
            if descendant_rows.is_empty() {
                return;
            }
            // Smart toggle: if ALL descendants are already marked, clear them;
            // otherwise mark all of them.
            let all_marked = descendant_rows.iter().all(|i| self.marked.contains(i));
            if all_marked {
                for i in &descendant_rows {
                    self.marked.remove(i);
                }
            } else {
                self.marked.extend(descendant_rows.iter().copied());
            }
        } else if let Some(i) = self.selected_row_index()
            && !self.marked.insert(i)
        {
            self.marked.remove(&i);
        }
    }

    /// Mark every currently-visible (filtered) row; if all visible rows
    /// are already marked, clear those instead (toggle-all).
    pub fn toggle_mark_all_filtered(&mut self) {
        let all_marked = !self.filtered.is_empty() && self.filtered.iter().all(|i| self.marked.contains(i));
        if all_marked {
            for i in &self.filtered {
                self.marked.remove(i);
            }
        } else {
            self.marked.extend(self.filtered.iter().copied());
        }
    }

    /// Clear all marks.
    pub fn clear_marks(&mut self) {
        self.marked.clear();
    }

    /// The `rows` indices a batch action should target: the marked set
    /// when non-empty, otherwise the single selected row. In tree mode with
    /// no marks, a group selection targets all its descendant leaf rows.
    /// Always returned sorted and de-duplicated for deterministic, stable
    /// batch order.
    ///
    /// # Contract C-5 (member-selection semantics)
    ///
    /// Explicit marks (a user multi-select) WIN regardless of the current
    /// cursor position — including when the cursor is on a virtual
    /// `DisplayRow::Member` row. This is consistent with how a
    /// `DisplayRow::Group` selection already behaves: marks always take
    /// precedence over the unmarked single-selection path.
    ///
    /// A `DisplayRow::Member` selection contributes NO target of its own:
    /// virtual members carry no `rows` index, so `selected_row_index()`
    /// returns `None` for a member row, and the fall-through to
    /// `selected_row_index().into_iter().collect()` yields an empty vec.
    /// This is the correct read-only behavior (contract C-5): a member is
    /// a display-only projection, not a first-class install target.
    ///
    /// Summary:
    /// - marks non-empty → return marks (regardless of cursor row type)
    /// - no marks + member selected → empty (read-only, no action target)
    /// - no marks + leaf selected → `[leaf_row_index]`
    /// - no marks + group selected → sorted descendant leaf row indices
    pub fn action_targets(&self) -> Vec<usize> {
        if !self.marked.is_empty() {
            return self.marked.iter().copied().collect();
        }
        // No marks: check if a group is selected in tree mode.
        if self.view_mode == ViewMode::Tree {
            let flat = self.flattened();
            if let Some(super::tree::DisplayRow::Group { rows, .. }) = flat.get(self.selected)
                && !rows.is_empty()
            {
                let mut sorted = rows.clone();
                sorted.sort_unstable();
                return sorted;
            }
        }
        // Fall back to the single selected row.
        self.selected_row_index().into_iter().collect()
    }

    /// Set the loading flag.
    pub fn set_loading(&mut self, loading: bool) {
        self.loading = loading;
    }

    /// Set the offline indicator.
    pub fn set_offline(&mut self, offline: bool) {
        self.offline = offline;
    }

    /// Set the catalog-truncated indicator (the browse window hit the cap,
    /// so the row list / search may be incomplete).
    pub fn set_truncated(&mut self, truncated: bool) {
        self.truncated = truncated;
    }

    /// Set the active-scope label shown in the title.
    pub fn set_scope_label(&mut self, label: impl Into<String>) {
        self.scope_label = label.into();
    }

    /// Set the active scope's effective selected client names (display only).
    pub fn set_clients(&mut self, clients: Vec<String>) {
        self.clients = clients;
    }

    /// Flip the row whose `repo` matches to [`ArtifactState::Outdated`], but
    /// **only** when it is currently [`ArtifactState::Installed`].
    ///
    /// This is the single merge primitive the background update-check feeds
    /// its registry-aware "a newer pin exists" result through. The guard is
    /// load-bearing: a background `↑` may *upgrade* a clean `Installed` row
    /// but must never *downgrade* a `Modified` / `IntegrityMissing` /
    /// `NotInstalled` row (those carry stronger on-disk truth the registry
    /// cannot override), and re-flipping an already-`Outdated` row is a
    /// no-op. Unknown `repo` is a silent no-op (the row may have been
    /// filtered or replaced by a catalog refresh between schedule and
    /// drain). Returns `true` when a flip actually happened.
    pub fn mark_outdated_if_installed(&mut self, repo: &str) -> bool {
        if let Some(row) = self.rows.iter_mut().find(|r| r.repo == repo)
            && row.state == ArtifactState::Installed
        {
            row.state = ArtifactState::Outdated;
            return true;
        }
        false
    }

    /// How many rows are currently in the [`ArtifactState::Outdated`] state —
    /// the tally the status-line breadcrumb reports ("N update(s)
    /// available"). Counts the full row set, not just the filtered view, so
    /// the number is stable across a search edit.
    pub fn outdated_count(&self) -> usize {
        self.rows.iter().filter(|r| r.state == ArtifactState::Outdated).count()
    }

    /// Replace the status line.
    pub fn set_status(&mut self, line: impl Into<String>) {
        self.status_line = line.into();
    }

    /// Apply a new query string and recompute the filter, clamping the
    /// selection so it stays in range. Resets the detail scroll — the
    /// selection may now point at a different row.
    pub fn apply_query(&mut self, query: impl Into<String>) {
        self.detail_scroll = 0;
        self.query = query.into();
        self.recompute_filter();
        // View-aware clamp: in tree mode `selected` indexes the flattened
        // display list, which is longer than `filtered` (group headers), so
        // clamping to `filtered.len()` would pull a valid tree cursor onto a
        // different visible row. `display_len()` is the active view's row count.
        let len = self.display_len();
        self.clamp_tree_selection_to(len);
    }

    /// Move the selection by `delta` (saturating at both ends — never
    /// wraps, never out of range). Resets the detail scroll — the pane now
    /// shows a different row.
    pub fn move_selection(&mut self, delta: i64) {
        self.detail_scroll = 0;
        let len = self.display_len();
        if len == 0 {
            self.selected = 0;
            return;
        }
        let max = len as i64 - 1;
        let next = (self.selected as i64 + delta).clamp(0, max);
        self.selected = next as usize;
    }

    /// Scroll the detail pane by `delta` rows, clamped at both ends: 0 at
    /// the top, and the content's post-wrap height minus the viewport at
    /// the bottom — scrolling stops when the last content row reaches the
    /// pane's bottom edge, mirroring the top saturation.
    pub fn scroll_detail(&mut self, delta: i64) {
        let lines = super::detail::detail_lines(self.selected_row());
        let max = super::detail::scroll_max(&lines, super::detail::viewport(self.term_size));
        let next = (i64::from(self.detail_scroll) + delta).clamp(0, i64::from(max));
        // `next` is in `[0, max]`, both u16-representable.
        self.detail_scroll = u16::try_from(next).unwrap_or(0);
    }

    /// Record the live terminal size (initial + every resize), re-clamping
    /// the detail scroll — a grown pane may shrink the scroll range.
    pub fn set_term_size(&mut self, size: (u16, u16)) {
        self.term_size = size;
        let lines = super::detail::detail_lines(self.selected_row());
        let max = super::detail::scroll_max(&lines, super::detail::viewport(size));
        self.detail_scroll = self.detail_scroll.min(max);
    }

    /// Set the effective default registry (elided from displayed names).
    pub fn set_default_registry(&mut self, registry: Option<String>) {
        self.default_registry = registry;
    }

    /// Set the resolved registries in precedence order (F13). Drives the
    /// tree's registry-root ordering and the empty-registry roots (D-EMPTY).
    pub fn set_registry_order(&mut self, order: Vec<String>) {
        self.registry_order = order;
    }

    /// Set the per-registry health summary (C6). Replaces the previous value
    /// wholesale; called from `reload_into` on each successful catalog load.
    pub fn set_registry_health(&mut self, health: RegistryHealth) {
        self.registry_health = health;
    }

    /// Store registry URL → display label mapping. Replaces the previous map
    /// wholesale; called from `apply_catalog_results` on each successful load.
    ///
    /// Each entry maps a registry URL to its configured alias (or to the URL
    /// itself when no alias was declared), so display code can call
    /// [`Self::registry_label`] without knowing whether an alias was set.
    pub fn set_registry_labels(&mut self, labels: BTreeMap<String, String>) {
        self.registry_labels = labels;
    }

    /// Return the display label for a registry URL.
    ///
    /// Returns the mapped alias when one was set via [`Self::set_registry_labels`],
    /// otherwise returns the URL unchanged. Callers can always use this as the
    /// display string without a separate alias-existence check.
    pub fn registry_label(&self, url: &str) -> String {
        self.registry_labels
            .get(url)
            .cloned()
            .unwrap_or_else(|| url.to_string())
    }

    /// Whether more than one registry is currently in scope.
    ///
    /// Used to gate the flat list's Registry column: with a single registry
    /// every row belongs to the same origin, so the column is redundant.
    /// With multiple registries the column identifies which registry each
    /// artifact came from.
    pub fn is_multi_registry(&self) -> bool {
        self.registry_order.len() > 1
    }

    /// Seed the view mode from a typed [`crate::config::declaration::DefaultView`]
    /// config value. `None` keeps the default (Flat).
    pub fn set_view_mode_from_config(&mut self, default_view: Option<crate::config::declaration::DefaultView>) {
        match default_view {
            Some(crate::config::declaration::DefaultView::Tree) => self.view_mode = ViewMode::Tree,
            Some(crate::config::declaration::DefaultView::Flat) | None => {}
        }
    }

    /// Seed the tree build options from resolved config values.
    pub fn set_tree_options(&mut self, group_by_type: bool, tree_separators: Vec<String>) {
        // The grouping / separator change reshapes the flattened tree, so the
        // pre-change `selected` (a flattened display index in tree mode) would
        // point at a different row — including a different *group* whose
        // un-marked batch action would hit an unrelated subtree. Preserve the
        // cursor by stable leaf/group identity across the reshape.
        let anchor = self.selection_anchor();
        self.group_by_type = group_by_type;
        self.tree_separators = if tree_separators.is_empty() {
            vec!["/".into()]
        } else {
            tree_separators
        };
        self.restore_selection(anchor);
    }

    /// Toggle between [`ViewMode::Flat`] and [`ViewMode::Tree`]. Ephemeral —
    /// never written back to config.
    ///
    /// Flat and tree views index `selected` in different coordinate spaces
    /// (`filtered` vs the flattened display list), so the selection is
    /// preserved by stable leaf/group identity: the selected artifact (or
    /// group) is captured before the flip and re-located in the new view.
    /// Without this, a single (un-marked) install/update/delete after a
    /// toggle could act on a different artifact than the one under the cursor.
    pub fn toggle_view_mode(&mut self) {
        let anchor = self.selection_anchor();
        self.view_mode = match self.view_mode {
            ViewMode::Flat => ViewMode::Tree,
            ViewMode::Tree => ViewMode::Flat,
        };
        self.restore_selection(anchor);
    }

    /// Move the selection cursor onto the display position of `rows` index
    /// `row` in the current view. When the row is not visible (e.g. inside a
    /// collapsed group), clamp into the visible range instead.
    fn select_row(&mut self, row: usize) {
        let pos = match self.view_mode {
            ViewMode::Flat => self.filtered.iter().position(|&i| i == row),
            ViewMode::Tree => self
                .flattened()
                .iter()
                .position(|dr| matches!(dr, super::tree::DisplayRow::Leaf { row: r, .. } if *r == row)),
        };
        match pos {
            Some(p) => self.selected = p,
            None => {
                let len = self.display_len();
                self.clamp_tree_selection_to(len);
            }
        }
    }

    /// Capture a stable handle to the current selection (leaf `repo` or group
    /// key) before a mutation that rebuilds rows or reshapes the tree. Paired
    /// with [`Self::restore_selection`].
    fn selection_anchor(&self) -> SelectionAnchor {
        if self.view_mode == ViewMode::Tree {
            let flat = self.flattened();
            match flat.get(self.selected) {
                Some(super::tree::DisplayRow::Leaf { row, .. }) => self
                    .rows
                    .get(*row)
                    .map(|r| SelectionAnchor::Leaf(r.repo.clone()))
                    .unwrap_or(SelectionAnchor::None),
                Some(super::tree::DisplayRow::Group { key, rows, .. }) => SelectionAnchor::Group {
                    key: key.clone(),
                    first_repo: rows
                        .iter()
                        .min()
                        .and_then(|&i| self.rows.get(i))
                        .map(|r| r.repo.clone()),
                },
                // A virtual member row: anchor on the parent bundle leaf so
                // restore_selection lands the cursor on the bundle after a reshape.
                // Members are display-only and do not survive a rows rebuild.
                Some(super::tree::DisplayRow::Member { parent_bundle_repo, .. }) => SelectionAnchor::Member {
                    parent_bundle_repo: parent_bundle_repo.clone(),
                },
                None => SelectionAnchor::None,
            }
        } else {
            match self.filtered.get(self.selected).and_then(|&i| self.rows.get(i)) {
                Some(r) => SelectionAnchor::Leaf(r.repo.clone()),
                None => SelectionAnchor::None,
            }
        }
    }

    /// Re-apply a [`SelectionAnchor`] after a rows rebuild / tree reshape,
    /// preserving the cursor's artifact (leaf) or subtree (group) identity. A
    /// group whose key did not survive a reshape falls back to a descendant
    /// leaf — never a different group — so an un-marked batch action cannot hit
    /// an unrelated subtree. A vanished target clamps view-aware into range.
    fn restore_selection(&mut self, anchor: SelectionAnchor) {
        match anchor {
            SelectionAnchor::Leaf(repo) => self.select_repo_or_clamp(&repo),
            // A virtual member does not survive a reshape — fall back to the
            // parent bundle leaf. P3 may refine this to re-find the member
            // position within the re-flattened tree once member rows are
            // re-spliced after the flatten.
            // TODO(P3): re-find the member's position in the re-flattened tree
            //           so the cursor lands on the member rather than its bundle.
            SelectionAnchor::Member { parent_bundle_repo } => {
                self.select_repo_or_clamp(&parent_bundle_repo);
            }
            SelectionAnchor::Group { key, first_repo } => {
                // Prefer re-finding the same group by its path-derived key
                // (stable across a resort / refresh with unchanged options).
                if self.view_mode == ViewMode::Tree
                    && let Some(pos) = self
                        .flattened()
                        .iter()
                        .position(|dr| matches!(dr, super::tree::DisplayRow::Group { key: k, .. } if *k == key))
                {
                    self.selected = pos;
                    return;
                }
                // The group key did not survive the reshape (e.g. group_by_type
                // changed) or we are now in flat mode: fall back to a descendant
                // leaf so the cursor stays inside the original subtree.
                match first_repo {
                    Some(repo) => self.select_repo_or_clamp(&repo),
                    None => {
                        let len = self.display_len();
                        self.clamp_tree_selection_to(len);
                    }
                }
            }
            SelectionAnchor::None => {
                let len = self.display_len();
                self.clamp_tree_selection_to(len);
            }
        }
    }

    /// Relocate the cursor onto `repo` when it is still present and visible,
    /// else clamp view-aware into range.
    fn select_repo_or_clamp(&mut self, repo: &str) {
        if let Some(rows_idx) = self.rows.iter().position(|r| r.repo == repo)
            && self.filtered.contains(&rows_idx)
        {
            self.select_row(rows_idx);
        } else {
            let len = self.display_len();
            self.clamp_tree_selection_to(len);
        }
    }

    /// Return `true` when the current selection in tree mode points at a
    /// [`super::tree::DisplayRow::Group`] node.
    pub fn selected_is_group(&self) -> bool {
        if self.view_mode != ViewMode::Tree {
            return false;
        }
        let flat = self.flattened();
        matches!(flat.get(self.selected), Some(super::tree::DisplayRow::Group { .. }))
    }

    /// Flatten the tree over the current filtered rows and return the
    /// visible display rows (tree mode only; returns empty in flat mode —
    /// callers must branch on [`Self::view_mode`]).
    ///
    /// When a search query is active (`!self.query.is_empty()`), collapsed
    /// groups are treated as expanded so that matching descendants are never
    /// hidden behind a collapsed ancestor. The `collapsed` set is preserved
    /// and takes effect again when the query clears.
    pub fn flattened(&self) -> Vec<super::tree::DisplayRow> {
        let opts = super::tree::TreeBuildOptions {
            default_registry: self.default_registry.clone(),
            group_by_type: self.group_by_type,
            separators: self.tree_separators.clone(),
            registry_order: self.registry_order.clone(),
        };
        let tree = super::tree::build(&self.rows, &self.filtered, &opts);
        // While a query is active, ignore the collapsed set: the tree prunes
        // to matching leaves only, so any collapsed ancestor would silently
        // hide a visible match. The collapsed state is preserved and restored
        // when the query clears.
        //
        // An owned empty set is used so the two branches agree on the type
        // (both `&BTreeSet<String>`) without a heap alloc per call —
        // the `if` evaluates once and the reference lives for the call.
        let empty = BTreeSet::new();
        let effective_collapsed: &BTreeSet<String> = if self.query.is_empty() { &self.collapsed } else { &empty };
        super::tree::flatten_with_members(
            &tree,
            effective_collapsed,
            &self.expanded_bundles,
            &self.bundle_members,
            &self.scope_label,
            &self.rows,
        )
    }

    /// Expand the selected group (remove it from the collapsed set). A
    /// no-op when not in tree mode, or when the selected row is a leaf.
    pub fn expand_selected(&mut self) {
        if self.view_mode != ViewMode::Tree {
            return;
        }
        let flat = self.flattened();
        if let Some(super::tree::DisplayRow::Group { key, .. }) = flat.get(self.selected) {
            self.collapsed.remove(key);
            // The flat list is stale after removing from collapsed; recompute length.
            let new_len = self.flattened().len();
            self.clamp_tree_selection_to(new_len);
        }
    }

    /// Collapse the selected group (add it to the collapsed set). A no-op
    /// when not in tree mode, or when the selected row is a leaf.
    pub fn collapse_selected(&mut self) {
        if self.view_mode != ViewMode::Tree {
            return;
        }
        let flat = self.flattened();
        if let Some(super::tree::DisplayRow::Group { key, .. }) = flat.get(self.selected) {
            let key = key.clone();
            self.collapsed.insert(key);
            // After collapsing, re-flatten to get the new visible length.
            let new_len = self.flattened().len();
            self.clamp_tree_selection_to(new_len);
        }
    }

    /// Insert `bundle_repo` into `expanded_bundles` (mark this bundle leaf as
    /// expanded). No-op if already present. Does NOT affect `bundle_members`
    /// cache or emit actions — callers handle those.
    pub fn expand_bundle_leaf(&mut self, bundle_repo: String) {
        self.expanded_bundles.insert(bundle_repo);
    }

    /// Remove `bundle_repo` from `expanded_bundles` (collapse this bundle
    /// leaf) and re-clamp the selection so it cannot point past the
    /// newly-shortened flat list.
    pub fn collapse_bundle_leaf(&mut self, bundle_repo: &str) {
        self.expanded_bundles.remove(bundle_repo);
        let new_len = self.flattened().len();
        self.clamp_tree_selection_to(new_len);
    }

    /// Handle the `←` (Collapse) key with ARIA/tree-widget standard behavior:
    ///
    /// - On an expanded group: collapse it (same as [`Self::collapse_selected`]).
    /// - On an already-collapsed group OR on a leaf: move the selection to the
    ///   nearest ancestor group by scanning `flattened()` backward from the
    ///   current position for the last row whose depth is strictly less than the
    ///   current row's depth.
    ///
    /// A no-op when not in tree mode or there is no visible ancestor.
    pub fn collapse_or_jump_to_parent(&mut self) {
        if self.view_mode != ViewMode::Tree {
            return;
        }
        let flat = self.flattened();
        let Some(selected_row) = flat.get(self.selected) else {
            return;
        };

        // Extract the current depth and whether the row is a collapsed group.
        let (current_depth, is_collapsed_group) = match selected_row {
            super::tree::DisplayRow::Group { depth, collapsed, .. } => (*depth, *collapsed),
            super::tree::DisplayRow::Leaf { depth, .. } => (*depth, false),
            // A virtual member: depth scan upward to the parent bundle leaf.
            // `is_collapsed_group = false` so the else-branch triggers the
            // ancestor scan (the member is neither a group nor collapsed).
            super::tree::DisplayRow::Member { depth, .. } => (*depth, false),
        };

        if !is_collapsed_group && matches!(selected_row, super::tree::DisplayRow::Group { .. }) {
            // Expanded group: collapse it (standard ARIA behavior).
            if let super::tree::DisplayRow::Group { key, .. } = selected_row {
                let key = key.clone();
                self.collapsed.insert(key);
            }
            let new_len = self.flattened().len();
            self.clamp_tree_selection_to(new_len);
        } else {
            // Already-collapsed group or leaf: jump to the nearest ancestor.
            // Scan backward from the row before the current selection.
            if current_depth == 0 {
                // Already at the root — no parent to jump to.
                return;
            }
            let parent_pos = flat[..self.selected].iter().rposition(|row| {
                let d = match row {
                    super::tree::DisplayRow::Group { depth, .. } => *depth,
                    super::tree::DisplayRow::Leaf { depth, .. } => *depth,
                    super::tree::DisplayRow::Member { depth, .. } => *depth,
                };
                d < current_depth
            });
            if let Some(pos) = parent_pos {
                self.selected = pos;
            }
        }
    }

    /// Toggle collapse of the selected group (expand if collapsed, collapse
    /// if expanded). A no-op on leaves.
    pub fn toggle_collapse_selected(&mut self) {
        if self.view_mode != ViewMode::Tree {
            return;
        }
        let flat = self.flattened();
        if let Some(super::tree::DisplayRow::Group { key, collapsed, .. }) = flat.get(self.selected) {
            if *collapsed {
                self.collapsed.remove(key);
            } else {
                let key = key.clone();
                self.collapsed.insert(key);
            }
            // After toggling collapse state, recompute visible length.
            let new_len = self.flattened().len();
            self.clamp_tree_selection_to(new_len);
        }
    }

    /// Enter the detail pane for the current selection, starting at the
    /// top. A no-op when there is no selectable row.
    pub fn enter_detail(&mut self) {
        if self.selected_row().is_some() {
            self.detail_scroll = 0;
            self.mode = Mode::Detail;
        }
    }

    /// Enter the detail pane when the cursor is on a `DisplayRow::Member`.
    ///
    /// `enter_detail` cannot be reused here because `selected_row()` returns
    /// `None` for member rows (members are virtual — they have no backing
    /// `TuiRow` index). This helper checks the flattened tree directly.
    pub fn enter_member_detail(&mut self) {
        if self.view_mode != crate::tui::state::ViewMode::Tree {
            return;
        }
        let flat = self.flattened();
        if matches!(
            flat.get(self.selected),
            Some(crate::tui::tree::DisplayRow::Member { .. })
        ) {
            self.detail_scroll = 0;
            self.mode = Mode::Detail;
        }
    }

    /// Enter search-edit mode.
    pub fn enter_search(&mut self) {
        self.mode = Mode::Search;
    }

    /// Show the keybinding help overlay (reset to the top).
    pub fn enter_help(&mut self) {
        self.help_scroll = 0;
        self.mode = Mode::Help;
    }

    /// Scroll the help overlay by `delta` rows, clamped to its scroll range.
    /// The overlay is content-sized then capped to the terminal height; rows
    /// beyond the inner viewport scroll. A no-op when the whole overlay fits.
    pub fn scroll_help(&mut self, delta: i64) {
        // Inner viewport = box height (body + 2 borders, capped to the
        // screen) minus the 2 borders.
        let box_h = HELP_BODY_LINES.saturating_add(2).min(self.term_size.1.max(1));
        let viewport = box_h.saturating_sub(2);
        let max = HELP_BODY_LINES.saturating_sub(viewport);
        let next = (i64::from(self.help_scroll) + delta).clamp(0, i64::from(max));
        self.help_scroll = u16::try_from(next).unwrap_or(0);
    }

    /// Leave detail / search / help and return to the list.
    pub fn back(&mut self) {
        self.mode = Mode::List;
    }

    /// Open the version picker for the current selection. Returns the
    /// `rows` index whose tags the app must lazily fetch, or `None` when
    /// there is no selectable row (then it is a no-op).
    pub fn open_version_pick(&mut self) -> Option<usize> {
        let i = self.selected_row_index()?;
        self.mode = Mode::VersionPick;
        self.picker = Some(VersionPicker {
            row: i,
            tags: Vec::new(),
            selected: 0,
            loading: true,
        });
        Some(i)
    }

    /// Populate the open picker with fetched `tags` (highest version
    /// first). The selection lands on the row's currently-pinned version
    /// if present, else the top. No-op when no picker is open.
    pub fn set_picker_tags(&mut self, tags: Vec<String>) {
        let Some(p) = self.picker.as_mut() else {
            return;
        };
        let pinned = self.rows.get(p.row).and_then(|r| r.pinned_version.clone());
        p.selected = pinned.and_then(|v| tags.iter().position(|t| *t == v)).unwrap_or(0);
        p.tags = tags;
        p.loading = false;
    }

    /// Move the picker selection by `delta`, saturating within the tag
    /// list. No-op when no picker is open or it is still loading.
    pub fn picker_move(&mut self, delta: i64) {
        if let Some(p) = self.picker.as_mut()
            && !p.tags.is_empty()
        {
            let max = p.tags.len() as i64 - 1;
            p.selected = (p.selected as i64 + delta).clamp(0, max) as usize;
        }
    }

    /// Commit the picked tag as the target row's `pinned_version` and
    /// return to the list. No-op (just closes) when the list is empty.
    pub fn confirm_version(&mut self) {
        if let Some(p) = self.picker.take()
            && let (Some(tag), Some(row)) = (p.tags.get(p.selected), self.rows.get_mut(p.row))
        {
            row.pinned_version = Some(tag.clone());
        }
        self.mode = Mode::List;
    }

    /// Close the picker without changing the pin.
    pub fn cancel_version(&mut self) {
        self.picker = None;
        self.mode = Mode::List;
    }

    /// The currently selected row, if any.
    pub fn selected_row(&self) -> Option<&TuiRow> {
        self.selected_row_index().and_then(|i| self.rows.get(i))
    }

    /// Recompute `filtered` from `rows` against the current query using the
    /// shared [`SearchQuery`] matcher, so the TUI search bar and `grim
    /// search` apply identical semantics: whitespace-split AND-of-terms over
    /// kind / repo / summary / description / keywords, plus bare kind
    /// keywords (`skill`/`rule`/`bundle` ± plural) that filter by kind. The
    /// query is parsed once, then every row is matched against it.
    fn recompute_filter(&mut self) {
        let query = SearchQuery::parse(&self.query);
        self.filtered = self
            .rows
            .iter()
            .enumerate()
            .filter(|(_, r)| query.matches_fields(Some(&r.kind), &r.repo, &r.summary, &r.description, &r.keywords))
            .map(|(i, _)| i)
            .collect();
    }
}

/// The trailing name segment of a `registry/repository` reference — the
/// sort key for the flat list within a kind group. Falls back to the whole
/// string when there is no `/`.
fn leaf_name(repo: &str) -> &str {
    repo.rsplit('/').next().unwrap_or(repo)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(repo: &str, desc: &str, kw: &[&str], state: ArtifactState) -> TuiRow {
        let (reg, repo_path) = repo.split_once('/').unwrap_or((repo, ""));
        TuiRow {
            kind: "skill".to_string(),
            registry: reg.to_string(),
            repository: repo_path.to_string(),
            repo: repo.to_string(),
            description: desc.to_string(),
            summary: String::new(),
            keywords: kw.iter().map(|s| s.to_string()).collect(),
            repository_url: None,
            latest_tag: "latest".to_string(),
            version: "1.0.0".to_string(),
            deprecated: None,
            pinned_version: None,
            state,
        }
    }

    fn seeded() -> TuiState {
        let mut s = TuiState::new();
        s.set_rows(vec![
            row("r/alpha", "first thing", &["rust"], ArtifactState::Installed),
            row("r/beta", "second thing", &["python"], ArtifactState::NotInstalled),
            row("r/gamma", "third thing", &["rust", "lint"], ArtifactState::Outdated),
        ]);
        s
    }

    #[test]
    fn marks_toggle_and_action_targets() {
        let mut s = seeded();
        // No marks ⇒ target is the selected row's index.
        assert_eq!(s.action_targets(), vec![0]);
        s.move_selection(1);
        assert_eq!(s.action_targets(), vec![1]);
        // Mark beta (sel=1) and gamma (sel=2).
        s.toggle_mark_selected();
        s.move_selection(1);
        s.toggle_mark_selected();
        assert!(s.is_row_marked(1) && s.is_row_marked(2));
        assert_eq!(s.action_targets(), vec![1, 2]);
        // Toggling off removes it.
        s.toggle_mark_selected();
        assert!(!s.is_row_marked(2));
        assert_eq!(s.action_targets(), vec![1]);
    }

    #[test]
    fn marks_survive_filter_change_and_clear_on_reload() {
        let mut s = seeded();
        s.toggle_mark_selected(); // mark row 0 (alpha)
        s.apply_query("beta"); // alpha filtered out
        assert!(s.is_row_marked(0), "mark keyed by row index, survives filter");
        s.clear_marks();
        assert!(s.marked.is_empty());
        s.toggle_mark_selected();
        s.set_rows(vec![row("r/x", "d", &[], ArtifactState::NotInstalled)]);
        assert!(s.marked.is_empty(), "reload drops stale marks");
    }

    #[test]
    fn toggle_mark_all_filtered_is_toggle() {
        let mut s = seeded();
        s.apply_query("rust"); // alpha + gamma visible
        s.toggle_mark_all_filtered();
        assert_eq!(s.action_targets(), vec![0, 2]);
        s.toggle_mark_all_filtered(); // all marked ⇒ clears them
        assert!(s.marked.is_empty());
    }

    #[test]
    fn set_clients_round_trips() {
        let mut s = TuiState::new();
        assert!(s.clients.is_empty(), "default is empty");
        s.set_clients(vec!["claude".to_string(), "opencode".to_string()]);
        assert_eq!(s.clients, vec!["claude".to_string(), "opencode".to_string()]);
    }

    #[test]
    fn set_truncated_round_trips() {
        let mut s = TuiState::new();
        assert!(!s.truncated, "default is not truncated");
        s.set_truncated(true);
        assert!(s.truncated);
        s.set_truncated(false);
        assert!(!s.truncated, "setter is pure — flips both ways");
    }

    #[test]
    fn artifact_state_display_is_kebab() {
        assert_eq!(ArtifactState::NotInstalled.to_string(), "not-installed");
        assert_eq!(ArtifactState::Installed.to_string(), "installed");
        assert_eq!(ArtifactState::Outdated.to_string(), "outdated");
        assert_eq!(ArtifactState::Modified.to_string(), "modified");
        assert_eq!(ArtifactState::IntegrityMissing.to_string(), "integrity-missing");
    }

    #[test]
    fn set_rows_clears_loading_and_resets_selection() {
        let s = seeded();
        assert!(!s.loading);
        assert_eq!(s.filtered.len(), 3);
        assert_eq!(s.selected, 0);
    }

    #[test]
    fn query_filters_rows_and_clamps_selection() {
        let mut s = seeded();
        s.move_selection(2); // select gamma (index 2)
        assert_eq!(s.selected, 2);
        s.apply_query("rust");
        // alpha + gamma match; selection clamped to last (index 1).
        assert_eq!(s.filtered.len(), 2);
        assert_eq!(s.selected, 1);
        assert_eq!(s.selected_row().unwrap().repo, "r/gamma");
    }

    #[test]
    fn empty_query_matches_all() {
        let mut s = seeded();
        s.apply_query("zzz");
        assert!(s.filtered.is_empty());
        assert!(s.selected_row().is_none());
        s.apply_query("");
        assert_eq!(s.filtered.len(), 3);
    }

    #[test]
    fn selection_saturates_at_bounds() {
        let mut s = seeded();
        s.move_selection(-5);
        assert_eq!(s.selected, 0);
        s.move_selection(99);
        assert_eq!(s.selected, 2);
        s.move_selection(99);
        assert_eq!(s.selected, 2, "never out of range");
    }

    #[test]
    fn selection_on_empty_filter_is_zero_and_safe() {
        let mut s = TuiState::new();
        s.set_rows(vec![]);
        s.move_selection(3);
        assert_eq!(s.selected, 0);
        assert!(s.selected_row().is_none());
    }

    #[test]
    fn mode_transitions_enter_and_back() {
        let mut s = seeded();
        assert_eq!(s.mode, Mode::List);
        s.enter_search();
        assert_eq!(s.mode, Mode::Search);
        s.back();
        assert_eq!(s.mode, Mode::List);
        s.enter_detail();
        assert_eq!(s.mode, Mode::Detail);
        s.back();
        assert_eq!(s.mode, Mode::List);
    }

    #[test]
    fn version_picker_pins_selected_tag() {
        let mut s = seeded();
        // Open on the selected row (index 0), then load tags.
        assert_eq!(s.open_version_pick(), Some(0));
        assert_eq!(s.mode, Mode::VersionPick);
        assert!(s.picker.as_ref().unwrap().loading);
        s.set_picker_tags(vec!["2.0.0".to_string(), "1.0.0".to_string()]);
        let p = s.picker.as_ref().unwrap();
        assert!(!p.loading);
        assert_eq!(p.selected, 0, "no prior pin ⇒ top of the list");
        s.picker_move(1);
        s.picker_move(5); // saturates
        s.confirm_version();
        assert_eq!(s.mode, Mode::List);
        assert!(s.picker.is_none());
        assert_eq!(s.rows[0].pinned_version.as_deref(), Some("1.0.0"));
        // Reopening positions the selection on the existing pin.
        s.open_version_pick();
        s.set_picker_tags(vec!["2.0.0".to_string(), "1.0.0".to_string()]);
        assert_eq!(s.picker.as_ref().unwrap().selected, 1);
    }

    #[test]
    fn version_picker_cancel_keeps_pin_unchanged() {
        let mut s = seeded();
        s.open_version_pick();
        s.set_picker_tags(vec!["9.9.9".to_string()]);
        s.cancel_version();
        assert_eq!(s.mode, Mode::List);
        assert!(s.picker.is_none());
        assert_eq!(s.rows[0].pinned_version, None);
    }

    #[test]
    fn open_version_pick_is_noop_without_selection() {
        let mut s = TuiState::new();
        s.set_rows(vec![]);
        assert_eq!(s.open_version_pick(), None);
        assert_eq!(s.mode, Mode::List);
    }

    #[test]
    fn enter_detail_is_noop_without_selection() {
        let mut s = TuiState::new();
        s.set_rows(vec![]);
        s.enter_detail();
        assert_eq!(s.mode, Mode::List, "no row ⇒ stays in list");
    }

    #[test]
    fn keyword_match_is_case_insensitive() {
        let mut s = seeded();
        s.apply_query("LINT");
        assert_eq!(s.filtered.len(), 1);
        assert_eq!(s.selected_row().unwrap().repo, "r/gamma");
    }

    #[test]
    fn summary_match_is_case_insensitive() {
        let mut s = TuiState::new();
        let mut r = row("r/delta", "plain description", &[], ArtifactState::NotInstalled);
        r.summary = "Concise Blurb".to_string();
        s.set_rows(vec![r]);
        s.apply_query("blurb");
        assert_eq!(s.filtered.len(), 1);
        assert_eq!(s.selected_row().unwrap().repo, "r/delta");
    }

    #[test]
    fn set_rows_sorts_by_kind_then_case_insensitive_name() {
        let mut s = TuiState::new();
        // Shuffled input across two kinds and mixed case.
        let mut z_rule = row("r/Zeta", "d", &[], ArtifactState::NotInstalled);
        z_rule.kind = "rule".to_string();
        let mut a_rule = row("r/alpha", "d", &[], ArtifactState::NotInstalled);
        a_rule.kind = "rule".to_string();
        let z_skill = row("r/zulu", "d", &[], ArtifactState::NotInstalled); // skill
        let a_skill = row("r/Bravo", "d", &[], ArtifactState::NotInstalled); // skill
        s.set_rows(vec![z_skill, z_rule, a_skill, a_rule]);
        // Grouped by kind (rule < skill), then case-insensitive leaf name.
        let order: Vec<(&str, &str)> = s.rows.iter().map(|r| (r.kind.as_str(), r.repo.as_str())).collect();
        assert_eq!(
            order,
            vec![
                ("rule", "r/alpha"),
                ("rule", "r/Zeta"),
                ("skill", "r/Bravo"),
                ("skill", "r/zulu"),
            ]
        );
    }

    #[test]
    fn filter_is_multi_term_and_via_shared_matcher() {
        let mut s = TuiState::new();
        s.set_rows(vec![
            row("acme/rust-style", "d", &["lint"], ArtifactState::NotInstalled),
            row("acme/python", "d", &["lint"], ArtifactState::NotInstalled),
        ]);
        // Both terms must hit (one in repo, one in keywords) — AND, not OR.
        s.apply_query("rust lint");
        assert_eq!(s.filtered.len(), 1);
        assert_eq!(s.selected_row().unwrap().repo, "acme/rust-style");
        // A single term that only one row carries still matches just that row.
        s.apply_query("python");
        assert_eq!(s.filtered.len(), 1);
        assert_eq!(s.selected_row().unwrap().repo, "acme/python");
    }

    #[test]
    fn mark_outdated_if_installed_flips_only_installed() {
        let mut s = TuiState::new();
        s.set_rows(vec![
            row("r/installed", "d", &[], ArtifactState::Installed),
            row("r/modified", "d", &[], ArtifactState::Modified),
            row("r/integrity", "d", &[], ArtifactState::IntegrityMissing),
            row("r/notinstalled", "d", &[], ArtifactState::NotInstalled),
            row("r/already", "d", &[], ArtifactState::Outdated),
        ]);

        // Installed flips to Outdated, and the call reports the flip.
        assert!(s.mark_outdated_if_installed("r/installed"));
        assert_eq!(
            s.rows.iter().find(|r| r.repo == "r/installed").unwrap().state,
            ArtifactState::Outdated
        );

        // Stronger on-disk states are never downgraded.
        for (repo, expected) in [
            ("r/modified", ArtifactState::Modified),
            ("r/integrity", ArtifactState::IntegrityMissing),
            ("r/notinstalled", ArtifactState::NotInstalled),
        ] {
            assert!(!s.mark_outdated_if_installed(repo), "{repo} must not flip");
            assert_eq!(s.rows.iter().find(|r| r.repo == repo).unwrap().state, expected);
        }

        // Re-flipping an already-Outdated row is a no-op (not Installed).
        assert!(!s.mark_outdated_if_installed("r/already"));
        assert_eq!(
            s.rows.iter().find(|r| r.repo == "r/already").unwrap().state,
            ArtifactState::Outdated
        );

        // An unknown repo is a silent no-op.
        assert!(!s.mark_outdated_if_installed("r/ghost"));
    }

    #[test]
    fn merge_catalog_rows_preserves_live_outdated_and_resorts() {
        let mut s = TuiState::new();
        s.set_rows(vec![
            row("acme/alpha", "old", &[], ArtifactState::Installed),
            row("acme/beta", "old", &[], ArtifactState::Modified),
        ]);
        // A live per-row check flipped alpha to Outdated.
        s.mark_outdated_if_installed("acme/alpha");
        assert_eq!(
            s.rows.iter().find(|r| r.repo == "acme/alpha").unwrap().state,
            ArtifactState::Outdated
        );

        // A background catalog refresh arrives: alpha re-derives Installed
        // (the on-disk lock has not advanced), beta re-derives Modified, and
        // a new gamma appears. Input order is shuffled to prove re-sorting.
        let fresh = vec![
            row("acme/gamma", "new", &[], ArtifactState::NotInstalled),
            row("acme/beta", "new", &[], ArtifactState::Modified),
            row("acme/alpha", "new", &[], ArtifactState::Installed),
        ];
        s.merge_catalog_rows(fresh);

        // Sort + filter stay consistent: three rows, sorted by leaf name.
        assert_eq!(s.filtered.len(), 3);
        let order: Vec<&str> = s.rows.iter().map(|r| r.repo.as_str()).collect();
        assert_eq!(order, vec!["acme/alpha", "acme/beta", "acme/gamma"]);

        // The live ↑ survived the refresh (alpha was Installed in the fresh
        // set, so the carried-over Outdated re-applies).
        assert_eq!(
            s.rows.iter().find(|r| r.repo == "acme/alpha").unwrap().state,
            ArtifactState::Outdated
        );
        // The fresh description replaced the old one.
        assert_eq!(
            s.rows.iter().find(|r| r.repo == "acme/alpha").unwrap().description,
            "new"
        );
        // A fresh Modified is stronger on-disk truth — never downgraded by a
        // stale live flag (beta had no live ↑ anyway, stays Modified).
        assert_eq!(
            s.rows.iter().find(|r| r.repo == "acme/beta").unwrap().state,
            ArtifactState::Modified
        );
    }

    #[test]
    fn merge_catalog_rows_does_not_relift_outdated_onto_modified() {
        let mut s = TuiState::new();
        s.set_rows(vec![row("acme/alpha", "d", &[], ArtifactState::Installed)]);
        s.mark_outdated_if_installed("acme/alpha"); // now Outdated (live ↑)

        // The refresh re-derives alpha as Modified (the file drifted on
        // disk). The stale live ↑ must NOT override the stronger Modified.
        s.merge_catalog_rows(vec![row("acme/alpha", "d", &[], ArtifactState::Modified)]);
        assert_eq!(
            s.rows[0].state,
            ArtifactState::Modified,
            "Modified wins over a stale live ↑"
        );
    }

    #[test]
    fn merge_catalog_rows_preserves_marks_by_repo() {
        let mut s = TuiState::new();
        s.set_rows(vec![
            row("acme/alpha", "old", &[], ArtifactState::Installed),
            row("acme/beta", "old", &[], ArtifactState::Installed),
            row("acme/gamma", "old", &[], ArtifactState::Installed),
        ]);
        // Mark alpha (idx 0) and gamma (idx 2).
        s.toggle_mark_selected();
        s.move_selection(2);
        s.toggle_mark_selected();
        assert_eq!(s.action_targets(), vec![0, 2]);

        // A background refresh arrives reordered, drops beta, adds delta.
        let fresh = vec![
            row("acme/delta", "new", &[], ArtifactState::Installed),
            row("acme/gamma", "new", &[], ArtifactState::Installed),
            row("acme/alpha", "new", &[], ArtifactState::Installed),
        ];
        s.merge_catalog_rows(fresh);

        // Rows re-sorted by leaf name: alpha, delta, gamma.
        let order: Vec<&str> = s.rows.iter().map(|r| r.repo.as_str()).collect();
        assert_eq!(order, vec!["acme/alpha", "acme/delta", "acme/gamma"]);
        // Marks follow alpha (now idx 0) and gamma (now idx 2) by repo key.
        assert!(s.is_row_marked(0), "alpha mark survived the resort");
        assert!(s.is_row_marked(2), "gamma mark survived the resort");
        assert!(!s.is_row_marked(1), "delta is unmarked");
        assert_eq!(s.action_targets(), vec![0, 2]);
    }

    #[test]
    fn merge_catalog_rows_drops_marks_for_vanished_repos() {
        let mut s = TuiState::new();
        s.set_rows(vec![
            row("acme/alpha", "d", &[], ArtifactState::Installed),
            row("acme/beta", "d", &[], ArtifactState::Installed),
        ]);
        s.move_selection(1);
        s.toggle_mark_selected(); // mark beta
        assert_eq!(s.action_targets(), vec![1]);

        // beta vanishes from the fresh set; its mark must drop.
        s.merge_catalog_rows(vec![row("acme/alpha", "d", &[], ArtifactState::Installed)]);
        assert!(s.marked.is_empty(), "a mark on a vanished repo drops");
    }

    #[test]
    fn merge_catalog_rows_keeps_cursor_on_same_repo() {
        let mut s = TuiState::new();
        s.set_rows(vec![
            row("acme/alpha", "d", &[], ArtifactState::Installed),
            row("acme/beta", "d", &[], ArtifactState::Installed),
            row("acme/gamma", "d", &[], ArtifactState::Installed),
        ]);
        s.move_selection(2); // cursor on gamma
        assert_eq!(s.selected_row().unwrap().repo, "acme/gamma");

        // A refresh adds 'aaa' which sorts to the very top, shifting indices.
        let fresh = vec![
            row("acme/gamma", "d", &[], ArtifactState::Installed),
            row("acme/aaa", "d", &[], ArtifactState::Installed),
            row("acme/beta", "d", &[], ArtifactState::Installed),
            row("acme/alpha", "d", &[], ArtifactState::Installed),
        ];
        s.merge_catalog_rows(fresh);

        // Despite the new top row shifting every index, the cursor stays on
        // gamma rather than snapping to index 0.
        assert_eq!(
            s.selected_row().unwrap().repo,
            "acme/gamma",
            "cursor follows the repo, not the index"
        );
    }

    #[test]
    fn merge_catalog_rows_clamps_cursor_when_selected_repo_vanishes() {
        let mut s = TuiState::new();
        s.set_rows(vec![
            row("acme/alpha", "d", &[], ArtifactState::Installed),
            row("acme/beta", "d", &[], ArtifactState::Installed),
            row("acme/gamma", "d", &[], ArtifactState::Installed),
        ]);
        s.move_selection(2); // cursor on gamma (last row)
        assert_eq!(s.selected_row().unwrap().repo, "acme/gamma");

        // gamma vanishes; the cursor must clamp into range, never dangle.
        s.merge_catalog_rows(vec![
            row("acme/alpha", "d", &[], ArtifactState::Installed),
            row("acme/beta", "d", &[], ArtifactState::Installed),
        ]);
        assert_eq!(s.filtered.len(), 2);
        assert!(s.selected < s.filtered.len(), "selection stays in range");
        assert!(s.selected_row().is_some(), "a valid row is always selected");
    }

    #[test]
    fn merge_catalog_rows_keeps_cursor_under_active_filter() {
        let mut s = TuiState::new();
        s.set_rows(vec![
            row("acme/rust-a", "d", &["rust"], ArtifactState::Installed),
            row("acme/py-b", "d", &["python"], ArtifactState::Installed),
            row("acme/rust-c", "d", &["rust"], ArtifactState::Installed),
        ]);
        s.apply_query("rust"); // rust-a (idx 0) + rust-c (idx 2) visible
        s.move_selection(1); // cursor on rust-c within the filtered view
        assert_eq!(s.selected_row().unwrap().repo, "acme/rust-c");

        // A refresh reorders and adds a row; the filter must stay applied and
        // the cursor stay on rust-c.
        let fresh = vec![
            row("acme/rust-c", "d", &["rust"], ArtifactState::Installed),
            row("acme/py-b", "d", &["python"], ArtifactState::Installed),
            row("acme/rust-a", "d", &["rust"], ArtifactState::Installed),
            row("acme/rust-z", "d", &["rust"], ArtifactState::Installed),
        ];
        s.merge_catalog_rows(fresh);

        // Filter still narrows to the three rust rows.
        assert_eq!(s.filtered.len(), 3, "the active query still filters");
        assert!(
            s.filtered
                .iter()
                .all(|&i| s.rows[i].keywords.contains(&"rust".to_string())),
            "only rust rows are visible"
        );
        assert_eq!(
            s.selected_row().unwrap().repo,
            "acme/rust-c",
            "cursor stays on the same repo under an active filter"
        );
    }

    #[test]
    fn outdated_count_tallies_full_row_set() {
        let mut s = TuiState::new();
        s.set_rows(vec![
            row("r/a", "d", &[], ArtifactState::Installed),
            row("r/b", "d", &[], ArtifactState::Installed),
            row("r/c", "d", &[], ArtifactState::Outdated),
        ]);
        assert_eq!(s.outdated_count(), 1, "one row starts Outdated");
        s.mark_outdated_if_installed("r/a");
        s.mark_outdated_if_installed("r/b");
        assert_eq!(s.outdated_count(), 3, "both Installed rows flipped");
        // The tally counts the full row set even when a search hides some.
        s.apply_query("r/a");
        assert_eq!(s.filtered.len(), 1, "filter hides two rows");
        assert_eq!(s.outdated_count(), 3, "tally is over all rows, not the filter");
    }

    #[test]
    fn filter_bare_kind_keyword_filters_by_kind() {
        let mut s = TuiState::new();
        let skill = row("acme/code-review", "d", &[], ArtifactState::NotInstalled);
        let mut rule = row("acme/rust-style", "d", &[], ArtifactState::NotInstalled);
        rule.kind = "rule".to_string();
        s.set_rows(vec![skill, rule]);
        // A bare kind keyword filters by kind, not as a literal text term.
        s.apply_query("rule");
        assert_eq!(s.filtered.len(), 1);
        assert_eq!(s.selected_row().unwrap().kind, "rule");
        // Kind keyword AND a text term: kind==skill and `review` in the repo.
        s.apply_query("skill review");
        assert_eq!(s.filtered.len(), 1);
        assert_eq!(s.selected_row().unwrap().repo, "acme/code-review");
    }

    #[test]
    fn detail_scroll_saturates_at_zero_and_resets_on_context_change() {
        let mut s = seeded();
        // Saturates at the top.
        s.scroll_detail(-5);
        assert_eq!(s.detail_scroll, 0);
        s.scroll_detail(3);
        assert_eq!(s.detail_scroll, 3);
        // Selection move ⇒ different row ⇒ reset.
        s.move_selection(1);
        assert_eq!(s.detail_scroll, 0);
        // Entering detail starts at the top.
        s.scroll_detail(2);
        s.enter_detail();
        assert_eq!(s.detail_scroll, 0);
        // A query edit may change the selected row ⇒ reset.
        s.scroll_detail(2);
        s.apply_query("alpha");
        assert_eq!(s.detail_scroll, 0);
        // A wholesale row replacement resets too.
        s.scroll_detail(2);
        s.set_rows(vec![row("r/zeta", "d", &[], ArtifactState::NotInstalled)]);
        assert_eq!(s.detail_scroll, 0);
    }

    #[test]
    fn detail_scroll_clamps_at_content_end_and_reclamps_on_resize() {
        let mut s = seeded();
        let max = super::super::detail::scroll_max(
            super::super::detail::detail_lines(s.selected_row()).as_slice(),
            super::super::detail::viewport(s.term_size),
        );
        assert!(max > 0, "fixture content must overflow the default viewport");
        // Scrolling far past the end stops exactly at the content bottom.
        s.scroll_detail(500);
        assert_eq!(s.detail_scroll, max);
        // Growing the terminal shrinks the range — the offset re-clamps.
        s.set_term_size((400, 200));
        assert_eq!(s.detail_scroll, 0, "content fits a huge pane: no scroll range left");
    }

    #[test]
    fn scroll_help_clamps_to_range() {
        let mut s = TuiState::new();
        s.set_term_size((80, 8)); // short → the help overlay must scroll
        s.scroll_help(-1);
        assert_eq!(s.help_scroll, 0, "cannot scroll above the top");
        s.scroll_help(100);
        let bottom = s.help_scroll;
        assert!(bottom > 0, "a short terminal leaves a scroll range");
        s.scroll_help(100);
        assert_eq!(s.help_scroll, bottom, "clamps at the bottom");
        // A terminal tall enough to fit the whole overlay → no scroll range.
        s.set_term_size((80, 50));
        s.scroll_help(1);
        assert_eq!(s.help_scroll, 0, "no scroll when the overlay fully fits");
    }

    // ── Step 3.2: tree-aware state spec tests ─────────────────────────────────

    fn tree_row(repo: &str, kind: &str, state: ArtifactState) -> TuiRow {
        let (reg, repo_path) = repo.split_once('/').unwrap_or((repo, ""));
        TuiRow {
            kind: kind.to_string(),
            registry: reg.to_string(),
            repository: repo_path.to_string(),
            repo: repo.to_string(),
            description: String::new(),
            summary: String::new(),
            keywords: vec![],
            repository_url: None,
            latest_tag: "latest".to_string(),
            version: "1.0.0".to_string(),
            deprecated: None,
            pinned_version: None,
            state,
        }
    }

    fn tree_seeded() -> TuiState {
        let mut s = TuiState::new();
        // Two rows under the same registry → one group
        s.set_rows(vec![
            tree_row("reg/acme/alpha", "skill", ArtifactState::Installed),
            tree_row("reg/acme/beta", "skill", ArtifactState::NotInstalled),
        ]);
        s.set_default_registry(Some("reg".to_string()));
        s
    }

    // `toggle_view_mode` flips Flat ⇄ Tree.
    #[test]
    fn toggle_view_mode_flips_flat_and_tree() {
        let mut s = tree_seeded();
        assert_eq!(s.view_mode, ViewMode::Flat, "default is Flat");
        s.toggle_view_mode();
        assert_eq!(s.view_mode, ViewMode::Tree);
        s.toggle_view_mode();
        assert_eq!(s.view_mode, ViewMode::Flat);
    }

    // Marks survive the flat ⇄ tree toggle (indices into `rows` unchanged).
    #[test]
    fn marks_survive_view_mode_toggle() {
        let mut s = tree_seeded();
        // Mark row 0 in flat mode
        s.toggle_mark_selected();
        assert!(s.is_row_marked(0), "row 0 marked in flat mode");
        // Toggle to tree
        s.toggle_view_mode();
        assert_eq!(s.view_mode, ViewMode::Tree);
        assert!(s.is_row_marked(0), "mark must survive the flat→tree toggle");
        // Toggle back to flat
        s.toggle_view_mode();
        assert_eq!(s.view_mode, ViewMode::Flat);
        assert!(s.is_row_marked(0), "mark must survive the tree→flat toggle");
    }

    // `selected_is_group()` returns true on a group line, false on a leaf.
    #[test]
    fn selected_is_group_returns_true_for_group_false_for_leaf() {
        let mut s = tree_seeded();
        s.toggle_view_mode(); // → Tree
        // tree_seeded() produces: "acme" group at position 0,
        // leaves "alpha" and "beta" at positions 1 and 2.
        s.selected = 0;
        assert!(
            s.selected_is_group(),
            "position 0 (acme group header) must return true from selected_is_group()"
        );
        // Navigate to a leaf position.
        s.selected = 1;
        assert!(
            !s.selected_is_group(),
            "position 1 (a leaf) must return false from selected_is_group()"
        );
    }

    // Codex-H regression: toggling view mode must keep the SAME artifact
    // selected. Flat and tree index `selected` in different coordinate
    // spaces, so a naive index reuse could retarget to a neighbor and make a
    // single (un-marked) i/u/d act on the wrong artifact.
    #[test]
    fn view_toggle_preserves_selected_artifact() {
        let mut s = tree_seeded();
        // Flat: filtered = [0 (alpha), 1 (beta)]; select beta (rows index 1).
        s.selected = 1;
        assert_eq!(s.selected_row_index(), Some(1), "beta selected in flat mode");
        // Flat → Tree: beta is a leaf at a different display position, but the
        // selected artifact must remain beta (rows index 1), not a neighbor.
        s.toggle_view_mode();
        assert_eq!(s.view_mode, ViewMode::Tree);
        assert_eq!(
            s.selected_row_index(),
            Some(1),
            "view toggle must keep beta (rows index 1) selected, not retarget to alpha"
        );
        // Tree → Flat round-trips back to beta.
        s.toggle_view_mode();
        assert_eq!(s.view_mode, ViewMode::Flat);
        assert_eq!(s.selected_row_index(), Some(1), "round-trip keeps beta selected");
    }

    // `selected_is_group()` returns false in flat mode (the early-return fast path).
    #[test]
    fn selected_is_group_returns_false_in_flat_mode() {
        let s = tree_seeded();
        // Default is Flat mode — selected_is_group() must return false
        // without consulting the tree (fast path).
        assert_eq!(s.view_mode, ViewMode::Flat);
        assert!(
            !s.selected_is_group(),
            "selected_is_group() must be false in flat mode (early-return fast path)"
        );
    }

    // `action_targets()` with no marks and a group selected returns the group's
    // sorted descendant leaf row indices (distinct from the cascade-mark test
    // which marks first then calls action_targets on the marked set).
    #[test]
    fn action_targets_no_marks_group_selection_returns_descendant_indices() {
        let mut s = tree_seeded();
        s.toggle_view_mode(); // → Tree
        s.selected = 0;
        assert!(s.selected_is_group(), "position 0 must be a group");
        assert!(s.marked.is_empty(), "no marks before calling action_targets");
        // action_targets() with no marks on a group → descendant leaf row indices.
        let targets = s.action_targets();
        assert!(
            !targets.is_empty(),
            "action_targets must return the group's descendant rows when no marks"
        );
        // The two rows in tree_seeded() are at indices 0 and 1 in `rows`.
        // Both must be in targets.
        assert!(
            targets.contains(&0),
            "row 0 (alpha) must be in action_targets; got: {targets:?}"
        );
        assert!(
            targets.contains(&1),
            "row 1 (beta) must be in action_targets; got: {targets:?}"
        );
        // Targets must be sorted.
        let mut sorted = targets.clone();
        sorted.sort_unstable();
        assert_eq!(targets, sorted, "action_targets must return sorted indices");
    }

    // expand/collapse/toggle clamp the selection to the new visible length.
    #[test]
    fn expand_collapse_clamp_selection() {
        let mut s = tree_seeded();
        s.toggle_view_mode(); // → Tree
        // Collapse the group (selection goes to position 0 = the group header)
        s.collapse_selected();
        // Now the tree has only 1 visible row (collapsed group header);
        // selection must be within range
        let flat = s.flattened();
        assert!(
            s.selected < flat.len().max(1),
            "selection must be in range after collapse"
        );
        // Expand restores descendants; selection still valid
        s.expand_selected();
        let flat_expanded = s.flattened();
        assert!(!flat_expanded.is_empty());
        assert!(
            s.selected < flat_expanded.len(),
            "selection must be in range after expand"
        );
    }

    // Regression (swarm-review B1 / Codex): in tree mode a background catalog
    // refresh must keep the cursor on the SAME artifact by identity, not snap
    // it to a flat `filtered` position. `selected` is a flattened display
    // index in tree mode (group headers interleaved), so assigning the flat
    // position would land the cursor on a different row — and a following
    // un-marked install/update/delete would hit the wrong artifact.
    #[test]
    fn merge_catalog_rows_preserves_tree_cursor_identity_beyond_filtered_len() {
        let mut s = tree_seeded();
        s.toggle_view_mode(); // → Tree
        // flattened = [acme(group, 0), alpha(leaf, 1), beta(leaf, 2)].
        // Select beta — display index 2, which is BEYOND filtered.len()-1 (=1).
        s.selected = 2;
        assert_eq!(
            s.selected_row().map(|r| r.repo.as_str()),
            Some("reg/acme/beta"),
            "precondition: beta selected at flattened index 2"
        );
        // A background refresh delivers the same repos (possibly re-sorted).
        s.merge_catalog_rows(vec![
            tree_row("reg/acme/alpha", "skill", ArtifactState::Installed),
            tree_row("reg/acme/beta", "skill", ArtifactState::NotInstalled),
        ]);
        // The cursor must still be on beta — NOT clamped down to filtered.len()-1
        // (which is alpha, the pre-fix bug).
        assert_eq!(
            s.selected_row().map(|r| r.repo.as_str()),
            Some("reg/acme/beta"),
            "tree cursor identity must survive a catalog refresh"
        );
    }

    // Regression (swarm-review RC-3): changing structural tree options (e.g. a
    // scope toggle bringing a different `[options.tui]`) reshapes the flattened
    // tree, so the cursor must be relocated by stable identity rather than left
    // at a now-stale flattened index.
    #[test]
    fn set_tree_options_preserves_selection_identity() {
        let mut s = tree_seeded();
        s.toggle_view_mode(); // → Tree
        s.selected = 2; // beta leaf
        assert_eq!(s.selected_row().map(|r| r.repo.as_str()), Some("reg/acme/beta"));
        // Enable group_by_type: shape becomes [skill(0), acme(1), alpha(2), beta(3)].
        s.set_tree_options(true, vec!["/".to_string()]);
        assert_eq!(
            s.selected_row().map(|r| r.repo.as_str()),
            Some("reg/acme/beta"),
            "cursor identity must survive a tree-options reshape"
        );
    }

    // A view toggle with a GROUP selected (no single row) must land the cursor
    // on a valid flat row, never dangle (swarm-review test-coverage gap).
    #[test]
    fn toggle_view_mode_with_group_selected_lands_on_valid_row() {
        let mut s = tree_seeded();
        s.toggle_view_mode(); // → Tree
        s.selected = 0; // the acme group header
        assert!(s.selected_is_group(), "precondition: a group is selected");
        s.toggle_view_mode(); // → Flat
        assert!(s.selected < s.filtered.len(), "selection must be in flat range");
        assert!(
            s.selected_row().is_some(),
            "cursor must land on a valid row, not dangle"
        );
    }

    // Tree-mode navigation/action over an empty filtered set must be safe
    // (swarm-review test-coverage gap — the flat analog already exists).
    #[test]
    fn tree_mode_empty_filter_navigation_is_safe() {
        let mut s = tree_seeded();
        s.toggle_view_mode(); // → Tree
        s.apply_query("zzz-no-such-match");
        assert!(s.flattened().is_empty(), "no rows match → empty flattened tree");
        assert!(!s.selected_is_group(), "no group is selected over an empty tree");
        assert!(s.action_targets().is_empty(), "no targets over an empty tree");
        s.move_selection(3);
        assert_eq!(s.selected, 0, "move over an empty tree keeps selection at 0");
    }

    // `move_selection` in tree mode saturates at the flattened (display) length,
    // not the flat `filtered` length, and re-saturates after a collapse shrinks
    // the visible set (swarm-review test-coverage gap).
    #[test]
    fn move_selection_tree_saturates_at_flattened_len() {
        let mut s = tree_seeded();
        s.toggle_view_mode(); // → Tree; flattened len = 3
        s.move_selection(100);
        assert_eq!(s.selected, s.flattened().len() - 1, "saturate at last display row");
        assert_eq!(s.selected, 2, "flattened len is 3 (1 group + 2 leaves)");
        // Collapse the group → only the group header remains visible.
        s.selected = 0;
        s.collapse_selected();
        s.move_selection(100);
        assert_eq!(
            s.selected,
            s.flattened().len() - 1,
            "re-saturate to the smaller display"
        );
    }

    // Two registries → two top-level groups, so a selection reset to index 0
    // would land on the WRONG group (detectable).
    fn two_group_tree() -> TuiState {
        let mut s = TuiState::new();
        s.set_rows(vec![
            tree_row("reg/acme/x", "skill", ArtifactState::Installed),
            tree_row("reg/zeta/y", "skill", ArtifactState::NotInstalled),
        ]);
        s.set_default_registry(Some("reg".to_string()));
        s.toggle_view_mode(); // → Tree; flattened = [acme(0), x(1), zeta(2), y(3)]
        s
    }

    // Regression (swarm-review round-2 / Codex): a background refresh with a
    // GROUP selected must keep the cursor on that group's subtree, not reset to
    // a different group — else an un-marked batch action hits the wrong subtree.
    #[test]
    fn merge_catalog_rows_preserves_tree_group_selection_identity() {
        let mut s = two_group_tree();
        s.selected = 2; // the zeta group (NOT the first group)
        assert!(s.selected_is_group(), "precondition: a group is selected");
        let y_idx = s.rows.iter().position(|r| r.repo == "reg/zeta/y").unwrap();
        assert_eq!(s.action_targets(), vec![y_idx], "precondition: zeta group targets y");
        // Background refresh delivers the same repos.
        s.merge_catalog_rows(vec![
            tree_row("reg/acme/x", "skill", ArtifactState::Installed),
            tree_row("reg/zeta/y", "skill", ArtifactState::NotInstalled),
        ]);
        let y_after = s.rows.iter().position(|r| r.repo == "reg/zeta/y").unwrap();
        assert_eq!(
            s.action_targets(),
            vec![y_after],
            "group selection must stay on the zeta subtree across a refresh, not reset to acme"
        );
    }

    // Regression (swarm-review round-2 / Codex): a tree-options reshape (e.g. a
    // scope toggle enabling group_by_type) with a GROUP selected must keep the
    // cursor within the original subtree (the same group re-found, or one of its
    // descendant leaves) — never a different group.
    #[test]
    fn set_tree_options_group_selection_stays_in_subtree() {
        let mut s = two_group_tree();
        s.selected = 2; // the zeta group
        assert!(s.selected_is_group(), "precondition: a group is selected");
        let y_idx = s.rows.iter().position(|r| r.repo == "reg/zeta/y").unwrap();
        // Enable group_by_type → the tree reshapes (a type level is inserted).
        s.set_tree_options(true, vec!["/".to_string()]);
        let targets = s.action_targets();
        assert!(!targets.is_empty(), "selection must stay actionable after a reshape");
        assert!(
            targets.iter().all(|&t| t == y_idx),
            "targets must stay within the original zeta subtree, got {targets:?}"
        );
    }

    // tree reflects the active filter (filtered subset → pruned tree).
    #[test]
    fn tree_reflects_active_filter() {
        let mut s = TuiState::new();
        s.set_rows(vec![
            tree_row("reg/acme/alpha", "skill", ArtifactState::Installed),
            tree_row("reg/acme/beta", "skill", ArtifactState::NotInstalled),
            tree_row("reg/other/gamma", "skill", ArtifactState::Installed),
        ]);
        s.set_default_registry(Some("reg".to_string()));
        s.toggle_view_mode(); // → Tree
        // Apply a query that matches only "alpha"
        s.apply_query("alpha");
        let flat = s.flattened();
        // beta and gamma must not appear in the flattened tree
        let labels: Vec<String> = flat
            .iter()
            .map(|d| match d {
                super::super::tree::DisplayRow::Group { label, .. } => label.clone(),
                super::super::tree::DisplayRow::Leaf { label, .. } => label.clone(),
                super::super::tree::DisplayRow::Member { label, .. } => label.clone(),
            })
            .collect();
        assert!(
            !labels.iter().any(|l| l == "beta" || l == "gamma"),
            "filtered-out rows must not appear in the tree; labels: {labels:?}"
        );
        assert!(
            labels.iter().any(|l| l == "alpha" || l == "acme"),
            "matching leaf or its ancestor must appear; labels: {labels:?}"
        );
    }

    // Collapsing a group, then applying + clearing a filter must leave the group
    // key in `collapsed` and the group rendering as collapsed.
    #[test]
    fn collapse_state_stable_across_filter_rebuilds() {
        let mut s = TuiState::new();
        s.set_rows(vec![
            tree_row("reg/acme/alpha", "skill", ArtifactState::Installed),
            tree_row("reg/acme/beta", "skill", ArtifactState::NotInstalled),
            tree_row("reg/acme/gamma", "skill", ArtifactState::Installed),
        ]);
        s.set_default_registry(Some("reg".to_string()));
        s.toggle_view_mode(); // → Tree

        // Collapse the "acme" group (position 0).
        s.selected = 0;
        assert!(s.selected_is_group(), "position 0 must be the acme group");
        s.collapse_selected();
        let acme_key = "acme".to_string();
        assert!(
            s.collapsed.contains(&acme_key),
            "acme must be in collapsed set after collapse_selected()"
        );

        // Apply a filter that matches only some leaves.
        s.apply_query("alpha");
        // The collapsed key must still be in the set.
        assert!(
            s.collapsed.contains(&acme_key),
            "acme collapsed key must survive a filter application"
        );

        // Clear the filter.
        s.apply_query("");
        assert!(
            s.collapsed.contains(&acme_key),
            "acme collapsed key must survive clearing the filter"
        );

        // The group must render as collapsed in the flat list.
        let flat = s.flattened();
        let group_display = flat
            .iter()
            .find(|d| matches!(d, super::super::tree::DisplayRow::Group { key, .. } if key == &acme_key));
        assert!(group_display.is_some(), "acme group must be in the flattened tree");
        match group_display.unwrap() {
            super::super::tree::DisplayRow::Group { collapsed, .. } => {
                assert!(*collapsed, "acme group must render as collapsed");
            }
            _ => unreachable!(),
        }
    }

    // Parent-cascade: toggling mark on a group materializes all descendant
    // leaf `rows` indices into `marked`.
    #[test]
    fn group_mark_cascades_to_descendant_leaf_rows() {
        let mut s = tree_seeded();
        s.toggle_view_mode(); // → Tree
        // Select position 0 which should be the "acme" group header
        s.selected = 0;
        assert!(
            s.selected_is_group(),
            "position 0 must be a group in tree_seeded(); tree shape changed unexpectedly"
        );
        assert!(s.marked.is_empty(), "no marks before toggle");
        s.toggle_mark_selected();
        // Both descendant leaf rows (0 and 1) must now be marked
        assert!(!s.marked.is_empty(), "group toggle must materialize descendant marks");
        assert!(
            s.marked.contains(&0) && s.marked.contains(&1),
            "both descendant rows (0 and 1) must be marked; marked: {:?}",
            s.marked
        );
    }

    // Smart toggle: if all descendants are marked, a second group toggle clears them.
    #[test]
    fn group_mark_smart_toggle_all_marked_clears() {
        let mut s = tree_seeded();
        s.toggle_view_mode(); // → Tree
        s.selected = 0;
        assert!(
            s.selected_is_group(),
            "position 0 must be a group in tree_seeded(); tree shape changed unexpectedly"
        );
        // First toggle: marks all
        s.toggle_mark_selected();
        let first_marked = s.marked.clone();
        assert!(!first_marked.is_empty(), "first toggle must mark descendants");
        // Second toggle: all already marked → clears them
        s.toggle_mark_selected();
        assert!(
            s.marked.is_empty(),
            "second toggle when all descendants marked must clear all; marked: {:?}",
            s.marked
        );
    }

    // `action_targets()` returns the marked set unchanged (descendant leaf
    // row indices) after a group cascade mark.
    #[test]
    fn action_targets_returns_cascade_marked_descendants() {
        let mut s = tree_seeded();
        s.toggle_view_mode(); // → Tree
        s.selected = 0;
        assert!(
            s.selected_is_group(),
            "position 0 must be a group in tree_seeded(); tree shape changed unexpectedly"
        );
        s.toggle_mark_selected();
        let targets = s.action_targets();
        assert!(!targets.is_empty(), "action_targets must return cascade-marked rows");
        // Targets must only contain valid row indices
        for &t in &targets {
            assert!(t < s.rows.len(), "target index {t} out of bounds");
        }
    }

    // C3: A query must not hide matching descendants behind a collapsed group.
    // Collapse a group, set a query matching a descendant, assert the leaf appears.
    // When the query clears, the group returns to its collapsed state.
    #[test]
    fn query_exposes_matches_behind_collapsed_ancestor() {
        let mut s = TuiState::new();
        s.set_rows(vec![
            tree_row("reg/acme/alpha", "skill", ArtifactState::Installed),
            tree_row("reg/acme/beta", "skill", ArtifactState::NotInstalled),
        ]);
        s.set_default_registry(Some("reg".to_string()));
        s.toggle_view_mode(); // → Tree

        // Collapse the "acme" group.
        s.selected = 0;
        assert!(s.selected_is_group(), "position 0 must be the acme group");
        s.collapse_selected();
        assert!(s.collapsed.contains("acme"), "acme must be collapsed");

        // With no query: only the collapsed group header is visible.
        let flat_collapsed = s.flattened();
        assert_eq!(flat_collapsed.len(), 1, "collapsed group hides its 2 children");
        assert!(
            matches!(&flat_collapsed[0], super::super::tree::DisplayRow::Group { .. }),
            "only the group header is visible when collapsed"
        );

        // Set a query matching the "beta" descendant.
        s.apply_query("beta");
        let flat_query = s.flattened();
        // The "beta" leaf must appear — the collapsed state must not hide it.
        let leaf_labels: Vec<String> = flat_query
            .iter()
            .filter_map(|d| match d {
                super::super::tree::DisplayRow::Leaf { label, .. } => Some(label.clone()),
                _ => None,
            })
            .collect();
        assert!(
            leaf_labels.contains(&"beta".to_string()),
            "beta must be visible in flattened() while query is active, even though its group is collapsed; got: {flat_query:?}"
        );

        // Clear the query: the group returns to its collapsed state.
        s.apply_query("");
        assert!(
            s.collapsed.contains("acme"),
            "acme must still be in collapsed set after query cleared"
        );
        let flat_after = s.flattened();
        assert_eq!(
            flat_after.len(),
            1,
            "after clearing the query, the group collapses again; got: {flat_after:?}"
        );
        assert!(
            matches!(
                &flat_after[0],
                super::super::tree::DisplayRow::Group { collapsed: true, .. }
            ),
            "group header must render as collapsed after query cleared"
        );
    }

    // Gap (a): set_view_mode_from_config routes correctly.
    //   Some(Tree)  → ViewMode::Tree
    //   None        → ViewMode::Flat (unchanged)
    //   Some(Flat)  → ViewMode::Flat (unchanged)
    #[test]
    fn set_view_mode_from_config_tree_overrides_default() {
        use crate::config::declaration::DefaultView;
        let mut s = TuiState::new();
        assert_eq!(s.view_mode, ViewMode::Flat, "default view mode must be Flat");

        s.set_view_mode_from_config(Some(DefaultView::Tree));
        assert_eq!(s.view_mode, ViewMode::Tree, "Some(Tree) must set Tree view mode");

        // Reset and verify None leaves Flat.
        let mut s2 = TuiState::new();
        s2.set_view_mode_from_config(None);
        assert_eq!(s2.view_mode, ViewMode::Flat, "None must leave view mode as Flat");

        // Some(Flat) also leaves it Flat.
        let mut s3 = TuiState::new();
        s3.set_view_mode_from_config(Some(DefaultView::Flat));
        assert_eq!(s3.view_mode, ViewMode::Flat, "Some(Flat) must leave view mode as Flat");
    }

    // Gap (b): set_tree_options normalizes empty separator list to ["/"].
    #[test]
    fn set_tree_options_normalizes_empty_separators_to_slash() {
        let mut s = TuiState::new();

        // Empty vec → normalized to ["/"].
        s.set_tree_options(true, vec![]);
        assert_eq!(
            s.tree_separators,
            vec!["/"],
            "empty tree_separators must normalize to [\"/\"]"
        );

        // Non-empty vec passes through unchanged.
        s.set_tree_options(false, vec![".".to_string(), "/".to_string()]);
        assert_eq!(
            s.tree_separators,
            vec![".", "/"],
            "non-empty tree_separators must be stored as-is"
        );
    }

    // ── C-3 Cache lifecycle ────────────────────────────────────────────────────
    //
    // These tests exercise the `bundle_members` cache on `TuiState`:
    // clear-on-set_rows and prune-on-merge_catalog_rows (both implemented).

    fn make_ready_cache_entry() -> BundleMemberCache {
        BundleMemberCache::Ready(vec![super::super::bundle_members::MemberNode {
            kind: crate::oci::ArtifactKind::Skill,
            label: "my-skill".to_string(),
            member_repo: Some("reg/acme/my-skill".to_string()),
            state: ArtifactState::Installed,
            related: false,
        }])
    }

    #[test]
    fn cache_is_empty_after_set_rows() {
        // C-3: clear-on-set_rows — after set_rows(...), bundle_members must be empty.
        let mut s = TuiState::new();
        // Pre-populate the cache with an entry.
        s.bundle_members.insert(
            ("project".to_string(), "reg/acme/bundle".to_string()),
            make_ready_cache_entry(),
        );
        assert!(!s.bundle_members.is_empty(), "precondition: cache is non-empty");

        // A call to set_rows should wipe the cache.
        s.set_rows(vec![tree_row("reg/acme/bundle", "bundle", ArtifactState::Installed)]);
        assert!(
            s.bundle_members.is_empty(),
            "C-3: bundle_members must be empty after set_rows; got {:?} entries",
            s.bundle_members.len()
        );
    }

    #[test]
    fn cache_is_empty_after_set_rows_with_no_prior_entries() {
        // C-3: clear-on-set_rows is a no-op when already empty (idempotent, no panic).
        let mut s = TuiState::new();
        assert!(s.bundle_members.is_empty());
        s.set_rows(vec![]);
        assert!(s.bundle_members.is_empty());
    }

    #[test]
    fn cache_prune_on_merge_drops_vanished_bundle_repo() {
        // C-3: prune-on-merge_catalog_rows — entries whose bundle_repo no longer
        // appears in the fresh rows are dropped; survivors are retained.
        let mut s = TuiState::new();
        s.set_rows(vec![
            tree_row("reg/acme/bundle-a", "bundle", ArtifactState::Installed),
            tree_row("reg/acme/bundle-b", "bundle", ArtifactState::Installed),
        ]);
        // Populate cache for both bundles.
        s.bundle_members.insert(
            ("project".to_string(), "reg/acme/bundle-a".to_string()),
            make_ready_cache_entry(),
        );
        s.bundle_members.insert(
            ("project".to_string(), "reg/acme/bundle-b".to_string()),
            make_ready_cache_entry(),
        );
        assert_eq!(s.bundle_members.len(), 2, "precondition: two cache entries");

        // A fresh catalog that drops bundle-b but keeps bundle-a.
        s.merge_catalog_rows(vec![tree_row("reg/acme/bundle-a", "bundle", ArtifactState::Installed)]);

        // The entry for bundle-b must be pruned; bundle-a survives.
        let key_b = ("project".to_string(), "reg/acme/bundle-b".to_string());
        let key_a = ("project".to_string(), "reg/acme/bundle-a".to_string());
        assert!(
            !s.bundle_members.contains_key(&key_b),
            "C-3: bundle-b entry must be pruned when the repo vanishes from fresh rows"
        );
        assert!(
            s.bundle_members.contains_key(&key_a),
            "C-3: bundle-a entry must survive when the repo is still in fresh rows"
        );
    }

    #[test]
    fn cache_scope_isolation_different_scopes_independent() {
        // C-3: scope-keyed — an entry under (scope_a, repo) is never confused
        // with (scope_b, repo). The two entries coexist independently.
        let mut s = TuiState::new();
        let repo = "reg/acme/bundle".to_string();
        let key_project = ("project".to_string(), repo.clone());
        let key_global = ("global".to_string(), repo.clone());

        s.bundle_members.insert(key_project.clone(), BundleMemberCache::Loading);
        s.bundle_members.insert(key_global.clone(), BundleMemberCache::Offline);

        // Reading under project scope must see Loading, not Offline.
        assert!(
            matches!(s.bundle_members.get(&key_project), Some(BundleMemberCache::Loading)),
            "C-3: project scope entry must be Loading"
        );
        // Reading under global scope must see Offline, not Loading.
        assert!(
            matches!(s.bundle_members.get(&key_global), Some(BundleMemberCache::Offline)),
            "C-3: global scope entry must be Offline"
        );
        // They must not alias each other.
        assert_ne!(
            std::mem::discriminant(s.bundle_members.get(&key_project).unwrap()),
            std::mem::discriminant(s.bundle_members.get(&key_global).unwrap()),
            "C-3: project and global scope entries must be independent (different enum variants)"
        );
    }

    #[test]
    fn cache_failed_entry_persists_no_new_ready_overwrites() {
        // C-3 no-retry: a Failed entry is NOT re-fetched on subsequent Expand.
        // We test the decision-predicate side: if there is already a Failed entry
        // in the cache, the app should NOT spawn a new fetch (the check is
        // "no entry at all → spawn"). We verify that a Failed entry stays Failed
        // after no explicit mutation (the P3 spawn-gate must check for absence,
        // not just for non-Ready).
        let mut s = TuiState::new();
        let key = ("project".to_string(), "reg/acme/bundle".to_string());
        s.bundle_members
            .insert(key.clone(), BundleMemberCache::Failed("503 error".to_string()));

        // The entry must still be Failed — nothing changed it.
        assert!(
            matches!(s.bundle_members.get(&key), Some(BundleMemberCache::Failed(_))),
            "C-3: Failed entry must remain Failed without an explicit mutation"
        );

        // Simulate what the spawn-gate predicate should check:
        // spawn is triggered only when `!s.bundle_members.contains_key(key)`.
        // With a Failed entry present, contains_key is true → no spawn.
        assert!(
            s.bundle_members.contains_key(&key),
            "C-3 no-retry: Failed entry makes contains_key true — spawn must be skipped"
        );
    }

    // ── C-5 DisplayRow::Member selection / action behavior ────────────────────
    //
    // To drive these tests we need a flattened tree that includes Member rows.
    // `flatten_with_members` is unimplemented (P3), so we directly push a Member
    // variant into a synthetic flattened list and inject it into the state via
    // the `selected` index. However, `selected` indexes `flattened()` which calls
    // the stub. We test the parts we can:
    //
    // - `selected_is_group()` — verifiable because the match is exhaustive and
    //   the `Member` arm is NOT Group (already wired in the stub arm).
    // - `selected_row_index()` — returns None for non-Leaf, already wired.
    // - `action_targets()` — returns [] when selected_row_index is None and no marks.
    // - `toggle_mark_selected()` — calls selected_row_index which returns None;
    //   the guard prevents any mark from being inserted.
    // - `collapse_or_jump_to_parent()` — the Member depth arm is already wired.
    //
    // For tests that require a Member to actually appear in the flattened list
    // (C-5 full selection), we note them as "integration-only" until P3.

    #[test]
    fn selected_row_index_returns_none_for_non_leaf_in_tree_mode() {
        // C-5 partial: when no Leaf is selected, selected_row_index() → None.
        // In tree mode with a group selected (position 0), None is returned.
        let mut s = tree_seeded();
        s.toggle_view_mode(); // → Tree
        s.selected = 0; // the acme group header
        assert!(s.selected_is_group(), "precondition: group selected");
        assert_eq!(
            s.selected_row_index(),
            None,
            "C-5: selected_row_index() must be None when a group is selected"
        );
    }

    #[test]
    fn action_targets_empty_when_no_marks_and_no_leaf_selected() {
        // C-5 partial: action_targets() → [] when no marks and a group is
        // selected (but the group has no descendant rows — forced via an empty
        // tree). This exercises the "fall back to selected_row_index" branch
        // for a group with rows, but we need the no-marks + group path to return
        // the descendant rows (tested above). Here we test the truly empty case.
        let mut s = TuiState::new();
        s.set_rows(vec![]);
        s.toggle_view_mode();
        // No rows → empty flat tree; selected_row_index() is None; no marks.
        assert_eq!(
            s.action_targets(),
            Vec::<usize>::new(),
            "C-5: action_targets() must be [] when there is no selection and no marks"
        );
    }

    #[test]
    fn toggle_mark_selected_noop_when_selected_row_index_is_none() {
        // C-5: toggle_mark_selected() on a group (selected_row_index → None) must
        // cascade to descendants (already tested in group_mark_cascades_to_*).
        // Here we test that toggle_mark_selected() when in tree mode and on a
        // LEAF does NOT crash or produce invalid indices.
        let mut s = tree_seeded();
        s.toggle_view_mode(); // → Tree; [group(0), alpha(1), beta(2)]
        s.selected = 1; // alpha leaf
        let before_marks = s.marked.clone();
        s.toggle_mark_selected(); // mark alpha
        assert_ne!(s.marked, before_marks, "C-5: toggle_mark on a leaf must insert a mark");
        s.toggle_mark_selected(); // unmark alpha
        assert_eq!(
            s.marked, before_marks,
            "C-5: second toggle on a leaf must remove the mark"
        );
    }

    #[test]
    fn selected_is_group_returns_false_for_leaf() {
        // C-5: selected_is_group() → false for a Leaf (not a Member,
        // but proves the non-Member, non-Group arm).
        let mut s = tree_seeded();
        s.toggle_view_mode(); // → Tree; [group(0), alpha(1), beta(2)]
        s.selected = 1; // alpha leaf
        assert!(
            !s.selected_is_group(),
            "C-5: selected_is_group() must be false for a leaf"
        );
    }

    #[test]
    fn collapse_or_jump_to_parent_from_leaf_moves_to_ancestor() {
        // C-5 partial: collapse_or_jump_to_parent() on a leaf (depth > 0)
        // must jump to the nearest ancestor group.
        let mut s = tree_seeded();
        s.toggle_view_mode(); // → Tree; [acme(0, depth=0), alpha(1, depth=1), beta(2, depth=1)]
        s.selected = 1; // alpha leaf, depth=1
        s.collapse_or_jump_to_parent();
        // The nearest ancestor is position 0 (acme group, depth=0).
        assert_eq!(
            s.selected, 0,
            "C-5: collapse_or_jump_to_parent() from a leaf must move to the ancestor group"
        );
    }

    // ── C-5 Member action-target regression tests ─────────────────────────────
    //
    // These tests pin the decision: a `DisplayRow::Member` contributes NO
    // action target of its own, but explicit marks always win regardless of
    // cursor position (consistent with how group selection + marks works).
    //
    // We construct a state that produces real `DisplayRow::Member` rows by
    // seeding a bundle row + populating the `bundle_members` cache, then
    // switching to tree mode and navigating to the member position.

    /// Build a `TuiState` in tree mode with a bundle row whose cache is
    /// `Ready` with one member. The flattened display is:
    ///   0: acme (group)
    ///   1: bundle-x (leaf, kind=bundle)
    ///   2: skill-a (Member, virtual)
    /// `scope_label` is set to `"project"` to match the cache key.
    fn member_state() -> TuiState {
        let mut s = TuiState::new();
        let bundle_row = TuiRow {
            kind: "bundle".to_string(),
            registry: "reg".to_string(),
            repository: "acme/bundle-x".to_string(),
            repo: "reg/acme/bundle-x".to_string(),
            description: String::new(),
            summary: String::new(),
            keywords: vec![],
            repository_url: None,
            latest_tag: "latest".to_string(),
            version: "1.0.0".to_string(),
            deprecated: None,
            pinned_version: None,
            state: ArtifactState::Installed,
        };
        s.set_rows(vec![bundle_row]);
        s.set_default_registry(Some("reg".to_string()));
        s.set_scope_label("project");
        // Populate the bundle_members cache with one Ready member.
        s.bundle_members.insert(
            ("project".to_string(), "reg/acme/bundle-x".to_string()),
            BundleMemberCache::Ready(vec![super::super::bundle_members::MemberNode {
                kind: crate::oci::ArtifactKind::Skill,
                label: "skill-a".to_string(),
                member_repo: Some("reg/acme/skill-a".to_string()),
                state: ArtifactState::Installed,
                related: false,
            }]),
        );
        // F3: expanded_bundles keyed by full bundle repo (not display leaf path).
        s.expanded_bundles.insert("reg/acme/bundle-x".to_string());
        s.toggle_view_mode(); // → Tree
        s
    }

    // Regression C-5(a): marks set + cursor on Member → action_targets returns
    // exactly the marked rows, NOT empty and NOT any member/OOB index.
    #[test]
    fn action_targets_marks_win_over_member_selection() {
        let mut s = member_state();
        // Navigate to the member position.
        let flat = s.flattened();
        let member_pos = flat
            .iter()
            .position(|dr| matches!(dr, super::super::tree::DisplayRow::Member { .. }))
            .expect("member_state must produce a Member row");
        s.selected = member_pos;
        assert!(
            matches!(
                s.flattened().get(s.selected),
                Some(super::super::tree::DisplayRow::Member { .. })
            ),
            "precondition: cursor must be on a Member row"
        );
        // Mark the bundle leaf (rows index 0) explicitly.
        s.marked.insert(0);
        // With marks, action_targets must return the marked rows regardless of
        // the member cursor — marks always win (contract C-5).
        let targets = s.action_targets();
        assert_eq!(
            targets,
            vec![0],
            "C-5(a): marks must win over member selection; got {targets:?}"
        );
    }

    // Regression C-5(b): no marks + cursor on Member → action_targets is empty
    // (read-only; member carries no rows index).
    #[test]
    fn action_targets_empty_with_member_selected_and_no_marks() {
        let mut s = member_state();
        let flat = s.flattened();
        let member_pos = flat
            .iter()
            .position(|dr| matches!(dr, super::super::tree::DisplayRow::Member { .. }))
            .expect("member_state must produce a Member row");
        s.selected = member_pos;
        assert!(s.marked.is_empty(), "precondition: no marks");
        let targets = s.action_targets();
        assert!(
            targets.is_empty(),
            "C-5(b): action_targets must be empty when a Member is selected with no marks; got {targets:?}"
        );
    }

    // Regression C-5(c): toggle_mark_selected on a Member is a no-op — the
    // marked set must remain unchanged (members have no rows index to mark).
    #[test]
    fn toggle_mark_on_member_is_noop() {
        let mut s = member_state();
        let flat = s.flattened();
        let member_pos = flat
            .iter()
            .position(|dr| matches!(dr, super::super::tree::DisplayRow::Member { .. }))
            .expect("member_state must produce a Member row");
        s.selected = member_pos;
        let marks_before = s.marked.clone();
        s.toggle_mark_selected();
        assert_eq!(
            s.marked, marks_before,
            "C-5(c): toggle_mark_selected on a Member must not change the marked set; \
             before={marks_before:?}, after={:?}",
            s.marked
        );
    }

    // Gap (c): empty tree — flatten(build([], [], &opts), &collapsed) is empty.
    #[test]
    fn empty_rows_produce_empty_flat_tree() {
        use crate::tui::tree::{TreeBuildOptions, build, flatten};
        use std::collections::BTreeSet;

        let opts = TreeBuildOptions {
            default_registry: None,
            group_by_type: false,
            separators: vec!["/".to_string()],
            registry_order: Vec::new(),
        };
        let tree = build(&[], &[], &opts);
        let collapsed: BTreeSet<String> = BTreeSet::new();
        let flat = flatten(&tree, &collapsed, &BTreeSet::new(), &[]);
        assert!(
            flat.is_empty(),
            "flatten of an empty build must return an empty vec; got: {flat:?}"
        );
    }

    /// A two-registry tree-view `TuiState`: `reg-a/skill-a` and `reg-b/skill-b`,
    /// no elision, registry roots ordered `[reg-a, reg-b]` (F13).
    fn two_registry_tree_state() -> TuiState {
        let mut s = TuiState::new();
        s.set_rows(vec![
            row("reg-a/skill-a", "from reg-a", &[], ArtifactState::NotInstalled),
            row("reg-b/skill-b", "from reg-b", &[], ArtifactState::NotInstalled),
        ]);
        s.set_registry_order(vec!["reg-a".to_string(), "reg-b".to_string()]);
        s.toggle_view_mode();
        assert_eq!(s.view_mode, ViewMode::Tree);
        s
    }

    // AC F11: marking BOTH registry roots cascades into the leaves of BOTH
    // registries — `marked` and `action_targets()` cover every descendant.
    #[test]
    fn mark_cascade_across_two_registry_roots_targets_both_registries() {
        let mut s = two_registry_tree_state();
        // Locate the two registry-root group positions in the flattened display.
        let flat = s.flattened();
        let root_positions: Vec<usize> = flat
            .iter()
            .enumerate()
            .filter_map(|(i, dr)| match dr {
                crate::tui::tree::DisplayRow::Group { key, depth: 0, .. } if key == "reg-a" || key == "reg-b" => {
                    Some(i)
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            root_positions.len(),
            2,
            "both registry roots must be visible group rows; flat={flat:?}"
        );

        // Mark each registry root in turn (group mark cascades to descendants).
        for pos in root_positions {
            s.selected = pos;
            assert!(s.selected_is_group(), "position {pos} must be a registry-root group");
            s.toggle_mark_selected();
        }

        // The two leaf rows are rows[0] (reg-a/skill-a) and rows[1] (reg-b/skill-b).
        let reg_a_leaf = s.rows.iter().position(|r| r.repo == "reg-a/skill-a").unwrap();
        let reg_b_leaf = s.rows.iter().position(|r| r.repo == "reg-b/skill-b").unwrap();
        assert!(
            s.marked.contains(&reg_a_leaf) && s.marked.contains(&reg_b_leaf),
            "marking both roots must mark leaves from BOTH registries; marked={:?}",
            s.marked
        );
        let targets = s.action_targets();
        assert!(
            targets.contains(&reg_a_leaf) && targets.contains(&reg_b_leaf),
            "action_targets() must include leaves from BOTH registries; targets={targets:?}"
        );
    }

    // AC F12: a query that matches only reg-a's leaf keeps reg-a's root visible
    // as an ancestor of the match; reg-b surfaces NO matching leaf (it appears
    // only as an empty D-EMPTY root — "collapsed" with a 0/0 rollup, never
    // hiding a match). Cross-registry filtering shows exactly the one match.
    #[test]
    fn search_keeps_matching_registry_root_as_ancestor_drops_non_matching_leaves() {
        let mut s = two_registry_tree_state();
        // Match only the reg-a leaf by its unique repository segment.
        s.apply_query("skill-a");
        let flat = s.flattened();
        let group_keys: Vec<&str> = flat
            .iter()
            .filter_map(|dr| match dr {
                crate::tui::tree::DisplayRow::Group { key, .. } => Some(key.as_str()),
                _ => None,
            })
            .collect();
        assert!(
            group_keys.contains(&"reg-a"),
            "matching registry root 'reg-a' must remain as an ancestor; groups={group_keys:?}"
        );
        // The only leaf shown is reg-a's match — reg-b's skill-b is filtered out,
        // so reg-b surfaces no matching descendant (it is empty / collapsed).
        let leaf_labels: Vec<&str> = flat
            .iter()
            .filter_map(|dr| match dr {
                crate::tui::tree::DisplayRow::Leaf { label, .. } => Some(label.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            leaf_labels,
            vec!["skill-a"],
            "only reg-a's matching leaf is shown; got {leaf_labels:?}"
        );
        // If reg-b's empty root is present (D-EMPTY), it must carry no descendants.
        if let Some(crate::tui::tree::DisplayRow::Group { rows, .. }) = flat
            .iter()
            .find(|dr| matches!(dr, crate::tui::tree::DisplayRow::Group { key, .. } if key == "reg-b"))
        {
            assert!(
                rows.is_empty(),
                "non-matching reg-b root must surface no matching descendants"
            );
        }
    }
}

// ── P2 Specify tests — C-2c / C-9 contracts ──────────────────────────────────
//
// These tests encode contracts from plan_tui_member_nodes:
//   C-2c: expanded_bundles lifecycle mirrors bundle_members
//   C-9:  index model is untouched after a member dispatch
//
// They MUST compile. C-2c lifecycle tests MAY already pass (lifecycle plumbing
// landed in P1). C-9 tests pin the isolation invariants.
#[cfg(test)]
mod p2_state_member_node_tests {
    use super::*;
    use crate::tui::bundle_members::{BundleMemberCache, MemberNode};

    fn bundle_tui_row(repo: &str) -> TuiRow {
        let (reg, repo_path) = repo.split_once('/').unwrap_or((repo, ""));
        TuiRow {
            kind: "bundle".to_string(),
            registry: reg.to_string(),
            repository: repo_path.to_string(),
            repo: repo.to_string(),
            description: String::new(),
            summary: String::new(),
            keywords: vec![],
            repository_url: None,
            latest_tag: "latest".to_string(),
            version: "1.0.0".to_string(),
            deprecated: None,
            pinned_version: None,
            state: ArtifactState::NotInstalled,
        }
    }

    fn make_ready_member() -> MemberNode {
        MemberNode {
            kind: crate::oci::ArtifactKind::Skill,
            label: "skill-a".to_string(),
            member_repo: Some("reg/acme/skill-a".to_string()),
            state: ArtifactState::NotInstalled,
            related: false,
        }
    }

    // ── C-2c lifecycle: set_rows clears expanded_bundles ──────────────────────

    #[test]
    fn c2c_expanded_bundles_cleared_on_set_rows() {
        let mut s = TuiState::new();
        // Seed expanded_bundles with a bundle key.
        s.expanded_bundles.insert("reg/acme/bundle-x".to_string()); // F3: full repo key
        assert!(
            !s.expanded_bundles.is_empty(),
            "precondition: expanded_bundles non-empty"
        );

        // Also seed a Ready cache entry.
        s.bundle_members.insert(
            ("project".to_string(), "reg/acme/bundle-x".to_string()),
            BundleMemberCache::Ready(vec![make_ready_member()]),
        );

        // set_rows must clear both.
        s.set_rows(vec![bundle_tui_row("reg/acme/bundle-x")]);

        assert!(
            s.expanded_bundles.is_empty(),
            "C-2c: set_rows must clear expanded_bundles; got {:?}",
            s.expanded_bundles
        );
        assert!(
            s.bundle_members.is_empty(),
            "C-2c: set_rows must clear bundle_members; got {} entries",
            s.bundle_members.len()
        );
    }

    // ── C-2c lifecycle: merge_catalog_rows prunes expanded_bundles ────────────
    //
    // Mirror the bundle_members prune test: a key whose bundle_repo survives the
    // merge is retained; a key whose repo vanishes is pruned.

    #[test]
    fn c2c_expanded_bundles_prune_on_merge_drops_vanished_repo() {
        let mut s = TuiState::new();
        s.set_rows(vec![
            bundle_tui_row("reg/acme/bundle-a"),
            bundle_tui_row("reg/acme/bundle-b"),
        ]);
        // F3: expanded_bundles keyed by FULL bundle repo string.
        s.expanded_bundles.insert("reg/acme/bundle-a".to_string());
        s.expanded_bundles.insert("reg/acme/bundle-b".to_string());
        // Also seed bundle_members so the prune path runs.
        s.bundle_members.insert(
            ("project".to_string(), "reg/acme/bundle-a".to_string()),
            BundleMemberCache::Ready(vec![make_ready_member()]),
        );
        s.bundle_members.insert(
            ("project".to_string(), "reg/acme/bundle-b".to_string()),
            BundleMemberCache::Ready(vec![make_ready_member()]),
        );

        // Fresh catalog: bundle-b vanishes.
        s.merge_catalog_rows(vec![bundle_tui_row("reg/acme/bundle-a")]);

        assert!(
            s.expanded_bundles.contains("reg/acme/bundle-a"),
            "C-2c: bundle-a key must survive merge when its repo is still live"
        );
        assert!(
            !s.expanded_bundles.contains("reg/acme/bundle-b"),
            "C-2c: bundle-b key must be pruned when its repo vanishes from fresh rows"
        );
    }

    // ── C-9: index model untouched for a member selection ─────────────────────
    //
    // selected_row_index() == None when cursor is on a Member.
    // action_targets() == [] when on a Member with no marks.
    // toggle_mark_selected() is a no-op on a Member.
    // rows/filtered/marked are unchanged.
    //
    // To get a Member into the flattened tree we need a Ready bundle-members
    // cache AND the bundle key in expanded_bundles. The flattened() call
    // depends on tree mode, so we set up a tree-mode state with a bundle row
    // and a Ready cache entry, then navigate to the member position.

    #[test]
    fn c9_selected_row_index_none_for_member() {
        let mut s = TuiState::new();
        s.set_rows(vec![bundle_tui_row("reg/acme/bundle-x")]);
        s.set_default_registry(Some("reg".to_string()));
        s.toggle_view_mode(); // Tree mode

        // Seed the cache and expand state so a Member row appears.
        s.bundle_members.insert(
            (s.scope_label.clone(), "reg/acme/bundle-x".to_string()),
            BundleMemberCache::Ready(vec![make_ready_member()]),
        );
        s.expanded_bundles.insert("reg/acme/bundle-x".to_string()); // F3: full repo key

        // Find the member position in the flattened output.
        let flat = s.flattened();
        let member_pos = flat
            .iter()
            .position(|r| matches!(r, crate::tui::tree::DisplayRow::Member { .. }));

        assert!(
            member_pos.is_some(),
            "C-9: must have a Member in the flat output for this test"
        );
        s.selected = member_pos.unwrap();

        assert_eq!(
            s.selected_row_index(),
            None,
            "C-9: selected_row_index() must be None when cursor is on a Member"
        );
    }

    #[test]
    fn c9_action_targets_empty_for_member_with_no_marks() {
        let mut s = TuiState::new();
        s.set_rows(vec![bundle_tui_row("reg/acme/bundle-x")]);
        s.set_default_registry(Some("reg".to_string()));
        s.toggle_view_mode();

        s.bundle_members.insert(
            (s.scope_label.clone(), "reg/acme/bundle-x".to_string()),
            BundleMemberCache::Ready(vec![make_ready_member()]),
        );
        s.expanded_bundles.insert("reg/acme/bundle-x".to_string()); // F3: full repo key

        let flat = s.flattened();
        let member_pos = flat
            .iter()
            .position(|r| matches!(r, crate::tui::tree::DisplayRow::Member { .. }));

        if let Some(pos) = member_pos {
            s.selected = pos;
            assert_eq!(
                s.action_targets(),
                Vec::<usize>::new(),
                "C-9: action_targets() must be empty for a Member with no marks"
            );
        }
    }

    #[test]
    fn c9_toggle_mark_selected_noop_on_member() {
        let mut s = TuiState::new();
        s.set_rows(vec![bundle_tui_row("reg/acme/bundle-x")]);
        s.set_default_registry(Some("reg".to_string()));
        s.toggle_view_mode();

        s.bundle_members.insert(
            (s.scope_label.clone(), "reg/acme/bundle-x".to_string()),
            BundleMemberCache::Ready(vec![make_ready_member()]),
        );
        s.expanded_bundles.insert("reg/acme/bundle-x".to_string()); // F3: full repo key

        let flat = s.flattened();
        let member_pos = flat
            .iter()
            .position(|r| matches!(r, crate::tui::tree::DisplayRow::Member { .. }));

        if let Some(pos) = member_pos {
            s.selected = pos;
            let marks_before = s.marked.clone();
            s.toggle_mark_selected();
            assert_eq!(
                s.marked, marks_before,
                "C-9: toggle_mark_selected on a Member must be a no-op; marks unchanged"
            );
        }
    }

    // ── F3 regression: two bundles sharing the same final path component ───────

    /// F3 regression: two bundles whose repos share the same final path
    /// component (e.g., "reg-a/acme/bundle" and "reg-b/acme/bundle") must
    /// have independent expanded_bundles entries. Removing one from
    /// expanded_bundles must not affect the other.
    ///
    /// This proves the old `rsplit('/')` heuristic (which compared only the
    /// leaf component) is gone — the new full-repo key is unambiguous.
    #[test]
    fn f3_two_bundles_same_leaf_name_independent_state() {
        let mut s = TuiState::new();
        s.set_rows(vec![
            bundle_tui_row("reg-a/acme/bundle"),
            bundle_tui_row("reg-b/acme/bundle"),
        ]);
        s.set_default_registry(None);

        // Expand both bundles (using full repo keys per F3).
        s.expanded_bundles.insert("reg-a/acme/bundle".to_string());
        s.expanded_bundles.insert("reg-b/acme/bundle".to_string());

        // Collapse only the first one via collapse_bundle_leaf.
        s.collapse_bundle_leaf("reg-a/acme/bundle");

        // The second bundle must still be in expanded_bundles.
        assert!(
            !s.expanded_bundles.contains("reg-a/acme/bundle"),
            "F3: reg-a/acme/bundle must be collapsed (removed from expanded_bundles)"
        );
        assert!(
            s.expanded_bundles.contains("reg-b/acme/bundle"),
            "F3: reg-b/acme/bundle must remain expanded (independent key)"
        );
    }

    /// F3 regression: merge_catalog_rows prune retains the exact bundle that
    /// survived and drops the one that disappeared. With the old rsplit('/')
    /// heuristic, two bundles with the same final component could keep each
    /// other alive after one was removed.
    #[test]
    fn f3_merge_catalog_rows_prunes_removed_bundle_only() {
        let mut s = TuiState::new();
        // Start with both bundles and their expanded state.
        s.set_rows(vec![
            bundle_tui_row("reg-a/acme/bundle"),
            bundle_tui_row("reg-b/acme/bundle"),
        ]);
        s.expanded_bundles.insert("reg-a/acme/bundle".to_string());
        s.expanded_bundles.insert("reg-b/acme/bundle".to_string());

        // Simulate a catalog refresh that drops reg-b/acme/bundle.
        let fresh = vec![bundle_tui_row("reg-a/acme/bundle")];
        s.merge_catalog_rows(fresh);

        assert!(
            s.expanded_bundles.contains("reg-a/acme/bundle"),
            "F3: reg-a/acme/bundle must survive the prune"
        );
        assert!(
            !s.expanded_bundles.contains("reg-b/acme/bundle"),
            "F3: reg-b/acme/bundle must be pruned (not in fresh rows)"
        );
    }

    // --- registry_label / set_registry_labels / is_multi_registry ---

    // registry_label returns mapped alias when present.
    #[test]
    fn registry_label_returns_alias_when_mapped() {
        let mut s = TuiState::new();
        let mut labels = BTreeMap::new();
        labels.insert("ghcr.io/acme".to_string(), "acme (ghcr.io/acme)".to_string());
        s.set_registry_labels(labels);
        assert_eq!(
            s.registry_label("ghcr.io/acme"),
            "acme (ghcr.io/acme)",
            "registry_label must return the mapped alias"
        );
    }

    // registry_label falls back to the URL when no alias is configured.
    #[test]
    fn registry_label_falls_back_to_url_when_no_alias() {
        let s = TuiState::new(); // empty labels
        assert_eq!(
            s.registry_label("ghcr.io/other"),
            "ghcr.io/other",
            "registry_label must fall back to the URL itself when not mapped"
        );
    }

    // is_multi_registry is false when registry_order has one entry.
    #[test]
    fn is_multi_registry_false_for_single_registry() {
        let mut s = TuiState::new();
        s.set_registry_order(vec!["ghcr.io/acme".into()]);
        assert!(
            !s.is_multi_registry(),
            "is_multi_registry must be false for single-registry order"
        );
    }

    // is_multi_registry is true when registry_order has two or more entries.
    #[test]
    fn is_multi_registry_true_for_two_registries() {
        let mut s = TuiState::new();
        s.set_registry_order(vec!["ghcr.io/acme".into(), "ghcr.io/other".into()]);
        assert!(
            s.is_multi_registry(),
            "is_multi_registry must be true for two-registry order"
        );
    }

    // is_multi_registry is false when registry_order is empty.
    #[test]
    fn is_multi_registry_false_for_empty_order() {
        let s = TuiState::new(); // empty order
        assert!(
            !s.is_multi_registry(),
            "is_multi_registry must be false when registry_order is empty"
        );
    }
}
