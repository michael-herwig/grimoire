// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! The pure TUI screen model and its transitions.
//!
//! This module is deliberately free of ratatui, crossterm, and `std::io`
//! — every transition is a pure function over [`TuiState`] so the screen
//! logic is exhaustively unit-testable without a terminal. The render loop
//! ([`super::app`]) drives these transitions; [`super::render`] projects
//! the state for display.

/// The install state of a catalog repository relative to the active
/// scope, as shown in the TUI.
///
/// Richer than [`crate::install::status_badge::StatusBadge`] (which
/// `search`/`status` share): it splits "an install record exists but its
/// editor outputs are gone or unreadable" out of `NotInstalled` into its
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
    /// An install record exists but one or more editor outputs are
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
    /// Catalog keywords.
    pub keywords: Vec<String>,
    /// The representative tag (empty string when absent).
    pub latest_tag: String,
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
    /// A one-line status / hint shown at the bottom.
    pub status_line: String,
    /// Marked rows for batch actions, as indices into `rows` (stable
    /// across filter changes — a mark survives a query edit).
    pub marked: std::collections::BTreeSet<usize>,
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
            status_line: String::new(),
            marked: std::collections::BTreeSet::new(),
        }
    }
}

impl TuiState {
    /// A fresh state in the loading phase.
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace the catalog rows (a load completed). The filter is
    /// recomputed against the current query and the selection is clamped.
    pub fn set_rows(&mut self, rows: Vec<TuiRow>) {
        self.rows = rows;
        self.loading = false;
        self.recompute_filter();
        self.selected = 0;
        // Row identities changed wholesale — stale marks would point at
        // unrelated rows.
        self.marked.clear();
    }

    /// The `rows` index of the current selection, if any.
    pub fn selected_row_index(&self) -> Option<usize> {
        self.filtered.get(self.selected).copied()
    }

    /// Whether the row at `rows` index `i` is marked.
    pub fn is_row_marked(&self, i: usize) -> bool {
        self.marked.contains(&i)
    }

    /// Toggle the mark on the currently-selected row. No-op without a
    /// selectable row.
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
        if self.marked.is_empty() {
            self.selected_row_index().into_iter().collect()
        } else {
            self.marked.iter().copied().collect()
        }
    }

    /// Set the loading flag.
    pub fn set_loading(&mut self, loading: bool) {
        self.loading = loading;
    }

    /// Set the offline indicator.
    pub fn set_offline(&mut self, offline: bool) {
        self.offline = offline;
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
        if self.filtered.is_empty() {
            self.selected = 0;
            return;
        }
        let max = self.filtered.len() as i64 - 1;
        let next = (self.selected as i64 + delta).clamp(0, max);
        self.selected = next as usize;
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

    /// Leave detail / search and return to the list.
    pub fn back(&mut self) {
        self.mode = Mode::List;
    }

    /// The currently selected row, if any.
    pub fn selected_row(&self) -> Option<&TuiRow> {
        self.filtered.get(self.selected).and_then(|&i| self.rows.get(i))
    }

    /// Recompute `filtered` from `rows` against the current query
    /// (case-insensitive substring over repo / description / keywords).
    fn recompute_filter(&mut self) {
        let q = self.query.to_lowercase();
        self.filtered = self
            .rows
            .iter()
            .enumerate()
            .filter(|(_, r)| {
                q.is_empty()
                    || r.repo.to_lowercase().contains(&q)
                    || r.description.to_lowercase().contains(&q)
                    || r.keywords.iter().any(|k| k.to_lowercase().contains(&q))
            })
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

#[cfg(test)]
mod tests {
    use super::*;

    fn row(repo: &str, desc: &str, kw: &[&str], state: ArtifactState) -> TuiRow {
        TuiRow {
            kind: "skill".to_string(),
            repo: repo.to_string(),
            description: desc.to_string(),
            keywords: kw.iter().map(|s| s.to_string()).collect(),
            latest_tag: "latest".to_string(),
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
}
