// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! The pure TUI screen model and its transitions.
//!
//! This module is deliberately free of ratatui, crossterm, and `std::io`
//! — every transition is a pure function over [`TuiState`] so the screen
//! logic is exhaustively unit-testable without a terminal. The render loop
//! ([`super::app`]) drives these transitions; [`super::render`] projects
//! the state for display.

use crate::catalog::SearchQuery;

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
            Self::Outdated => "outdated",
            Self::Modified => "modified",
            Self::IntegrityMissing => "integrity-missing",
        })
    }
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
    /// `registry/repository` reference.
    pub repo: String,
    /// Catalog description (empty string when absent).
    pub description: String,
    /// Catalog short summary (empty string when absent).
    pub summary: String,
    /// Catalog keywords.
    pub keywords: Vec<String>,
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

/// The whole screen model.
#[derive(Debug, Clone)]
pub struct TuiState {
    /// Every catalog row (unfiltered).
    pub rows: Vec<TuiRow>,
    /// Indices into `rows` matching the current query, in row order.
    pub filtered: Vec<usize>,
    /// Selection index *into `filtered`* (not into `rows`).
    pub selected: usize,
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
}

impl Default for TuiState {
    fn default() -> Self {
        Self {
            rows: Vec::new(),
            filtered: Vec::new(),
            selected: 0,
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
        }
    }
}

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
        // Row identities changed wholesale — stale marks would point at
        // unrelated rows.
        self.marked.clear();
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
    /// - **Cursor**: the selection stays on the same `repo`; if that repo
    ///   vanished (or was filtered out) the selection clamps into range
    ///   rather than snapping to the top.
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
        let selected_repo = self.selected_row().map(|r| r.repo.clone());

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

        // The single kind-sort + filter choke point; clears marks and resets
        // selection, both of which we restore by repo key below.
        self.set_rows(fresh);

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

        // Re-position the cursor on the same repo. If it vanished or is
        // filtered out, `clamp_selection` keeps the selection in range rather
        // than snapping to the top.
        if let Some(repo) = selected_repo
            && let Some(rows_idx) = self.rows.iter().position(|r| r.repo == repo)
            && let Some(filtered_pos) = self.filtered.iter().position(|&i| i == rows_idx)
        {
            self.selected = filtered_pos;
        }
        self.clamp_selection();
    }

    /// Number of selectable rows (the filtered view).
    fn display_len(&self) -> usize {
        self.filtered.len()
    }

    /// The `rows` index of the current selection, if any.
    pub fn selected_row_index(&self) -> Option<usize> {
        self.filtered.get(self.selected).copied()
    }

    /// Whether the row at `rows` index `i` is marked.
    pub fn is_row_marked(&self, i: usize) -> bool {
        self.marked.contains(&i)
    }

    /// Toggle the mark on the current selection. No-op without a
    /// selectable target.
    pub fn toggle_mark_selected(&mut self) {
        if let Some(i) = self.selected_row_index()
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
    /// when non-empty, otherwise the single selected row. Always returned
    /// sorted and de-duplicated for deterministic, stable batch order.
    pub fn action_targets(&self) -> Vec<usize> {
        if !self.marked.is_empty() {
            return self.marked.iter().copied().collect();
        }
        // No marks: the single selection.
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
    /// selection so it stays in range.
    pub fn apply_query(&mut self, query: impl Into<String>) {
        self.query = query.into();
        self.recompute_filter();
        self.clamp_selection();
    }

    /// Move the selection by `delta` (saturating at both ends — never
    /// wraps, never out of range).
    pub fn move_selection(&mut self, delta: i64) {
        let len = self.display_len();
        if len == 0 {
            self.selected = 0;
            return;
        }
        let max = len as i64 - 1;
        let next = (self.selected as i64 + delta).clamp(0, max);
        self.selected = next as usize;
    }

    /// Set the effective default registry (elided from displayed names).
    pub fn set_default_registry(&mut self, registry: Option<String>) {
        self.default_registry = registry;
    }

    /// Enter the detail pane for the current selection. A no-op when there
    /// is no selectable row.
    pub fn enter_detail(&mut self) {
        if self.selected_row().is_some() {
            self.mode = Mode::Detail;
        }
    }

    /// Enter search-edit mode.
    pub fn enter_search(&mut self) {
        self.mode = Mode::Search;
    }

    /// Show the keybinding help overlay.
    pub fn enter_help(&mut self) {
        self.mode = Mode::Help;
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

    /// Clamp the selection into the current filtered range.
    fn clamp_selection(&mut self) {
        if self.filtered.is_empty() {
            self.selected = 0;
        } else if self.selected >= self.filtered.len() {
            self.selected = self.filtered.len() - 1;
        }
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
        TuiRow {
            kind: "skill".to_string(),
            repo: repo.to_string(),
            description: desc.to_string(),
            summary: String::new(),
            keywords: kw.iter().map(|s| s.to_string()).collect(),
            latest_tag: "latest".to_string(),
            version: "1.0.0".to_string(),
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
}
