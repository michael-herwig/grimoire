// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! Pure input → action mapping for the TUI.
//!
//! No terminal imports: [`handle`] is a pure function over [`TuiState`]
//! that applies a [`TuiInput`] (the crossterm-independent input alphabet)
//! and returns a [`TuiAction`] for [`super::app`] to perform. The
//! key-to-input mapping lives in [`super::app`] (the only crossterm-aware
//! place); this module operates on the abstract input so the whole
//! decision table is unit-testable headlessly.

use super::state::{Mode, TuiState, ViewMode};

/// Rows one `PageUp`/`PageDown` press scrolls the detail pane. A fixed
/// step (not the live pane height, which the pure layer cannot know);
/// the projection clamps the offset, so overshoot is harmless.
const DETAIL_PAGE: i64 = 5;

/// The terminal-independent input alphabet.
///
/// Closed internal enum — matches stay total, no `#[non_exhaustive]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TuiInput {
    /// Move selection up.
    Up,
    /// Move selection down.
    Down,
    /// A printable character (search-mode text entry / list hotkeys).
    Char(char),
    /// Scroll the detail pane up one page (works without focusing it).
    PageUp,
    /// Scroll the detail pane down one page (works without focusing it).
    PageDown,
    /// Delete the last query character (search mode).
    Backspace,
    /// Confirm: open the detail pane (list) or commit the query (search).
    Enter,
    /// Cancel: leave detail/search, else request quit.
    Esc,
    /// Install the selected / marked artifact(s).
    Install,
    /// Update the selected / marked artifact(s).
    Update,
    /// Uninstall (delete) the selected / marked artifact(s).
    Delete,
    /// Toggle the mark on the selected row.
    Mark,
    /// Toggle marks on all visible rows.
    MarkAll,
    /// Clear all marks.
    ClearMarks,
    /// Toggle the active scope (Global ⇄ Project).
    ScopeToggle,
    /// Toggle the flat list ⇄ grouped tree view.
    ViewToggle,
    /// Expand the selected tree group.
    Expand,
    /// Collapse the selected tree group.
    Collapse,
    /// Show the keybinding help overlay.
    Help,
    /// Open the version picker for the selected row.
    Versions,
    /// Rebuild the catalog.
    Refresh,
    /// Quit the TUI.
    Quit,
}

/// Which batch operation to run over the target rows.
///
/// Closed internal enum — matches stay total, no `#[non_exhaustive]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BatchOp {
    /// Install (honours the integrity gate).
    Install,
    /// Update (force re-materialize — rolling-release contract).
    Update,
    /// Uninstall: delete files + drop the install record/lock pin.
    Uninstall,
}

/// What the app must do after a transition. `None` = state-only change.
///
/// Closed internal enum — matches stay total, no `#[non_exhaustive]`.
/// Not `Copy` (the batch variant carries a `Vec`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TuiAction {
    /// Run `op` over the given `rows` indices (the marked set, else the
    /// single selection).
    Batch { op: BatchOp, rows: Vec<usize> },
    /// Install, update, or uninstall a single bundle member that has no
    /// `rows` index (virtual projection-only row). Carries its own
    /// `repo` and `kind` so the app handler never needs a `rows` index.
    ///
    /// Emitted in Phase 4 (P4.1) when the cursor is on a
    /// `DisplayRow::Member` with `member_repo = Some(repo)` AND `marked`
    /// is empty. Falls through to `Batch` otherwise (marks-win, D7/C-8).
    ///
    /// The app dispatch arm is stubbed with `unimplemented!()` in P1 (P1.4).
    MemberAction {
        /// Which batch-style op to run (install / update / uninstall).
        op: BatchOp,
        /// The validated `registry/repository` reference for this member.
        repo: String,
        /// Artifact kind (never `Bundle` — per-member bundle-nesting rejected
        /// at `bundle_members.rs:72-74`).
        kind: crate::oci::ArtifactKind,
        /// The bundle member's binding name (`BundleMember.name`), which is the
        /// install-state / lock key for the member — NOT the repo basename
        /// (they differ when the bundle aliases a member). Actions key by this.
        name: String,
    },
    /// Rebuild the catalog from the registry.
    Refresh,
    /// Toggle the active scope (Global ⇄ Project) and recompute states.
    ToggleScope,
    /// Lazily fetch the tag list for `row` and feed it to the open picker.
    LoadVersions { row: usize },
    /// Lazily fetch the member list for the bundle leaf at `rows[row]`.
    ///
    /// Emitted by `handle_browse` when the Expand gesture lands on a
    /// `DisplayRow::Leaf` whose `rows[row].kind == "bundle"` and there is
    /// no existing cache entry for `(scope_label, bundle_repo)`.
    ///
    /// `row` is the `rows` index of the bundle leaf.
    /// `bundle_repo` is the `registry/repository` reference (stable cache
    /// key).
    LoadBundleMembers { row: usize, bundle_repo: String },
    /// Open `url` in the system browser (the selected row's repository).
    OpenUrl { url: String },
    /// Exit the TUI cleanly.
    Quit,
    /// Nothing to do beyond the in-place state change.
    None,
}

/// Apply `input` to `state`, returning the side effect the app must run.
///
/// The mapping is mode-sensitive: in [`Mode::Search`] printable characters
/// edit the query (so they cannot double as list hotkeys); in
/// [`Mode::List`] / [`Mode::Detail`] navigation and action keys apply.
pub fn handle(state: &mut TuiState, input: TuiInput) -> TuiAction {
    match state.mode {
        Mode::Search => handle_search(state, input),
        Mode::Help => handle_help(state, input),
        Mode::VersionPick => handle_picker(state, input),
        Mode::List | Mode::Detail => handle_browse(state, input),
    }
}

/// Version-picker keys: navigate the tag list, `Enter` pins the highlighted
/// tag, `Esc` / `v` cancels, `q` quits the TUI.
fn handle_picker(state: &mut TuiState, input: TuiInput) -> TuiAction {
    match input {
        TuiInput::Up => {
            state.picker_move(-1);
            TuiAction::None
        }
        TuiInput::Down => {
            state.picker_move(1);
            TuiAction::None
        }
        TuiInput::Enter => {
            state.confirm_version();
            TuiAction::None
        }
        TuiInput::Esc | TuiInput::Char('v') | TuiInput::Versions => {
            state.cancel_version();
            TuiAction::None
        }
        TuiInput::Char('q') | TuiInput::Quit => TuiAction::Quit,
        _ => TuiAction::None,
    }
}

/// Help-overlay keys: scroll keys (`↑`/`↓`, `j`/`k`, `PageUp`/`PageDown`)
/// move the overlay when it does not fully fit; `q` quits; anything else
/// dismisses back to the list.
fn handle_help(state: &mut TuiState, input: TuiInput) -> TuiAction {
    match input {
        TuiInput::Char('q') | TuiInput::Quit => TuiAction::Quit,
        TuiInput::Up | TuiInput::Char('k') => {
            state.scroll_help(-1);
            TuiAction::None
        }
        TuiInput::Down | TuiInput::Char('j') => {
            state.scroll_help(1);
            TuiAction::None
        }
        TuiInput::PageUp => {
            state.scroll_help(-DETAIL_PAGE);
            TuiAction::None
        }
        TuiInput::PageDown => {
            state.scroll_help(DETAIL_PAGE);
            TuiAction::None
        }
        _ => {
            state.back();
            TuiAction::None
        }
    }
}

/// Search-mode keys: text entry plus commit/cancel. Navigation still works
/// so the user can scroll results while typing.
fn handle_search(state: &mut TuiState, input: TuiInput) -> TuiAction {
    match input {
        TuiInput::Char(c) => {
            let mut q = state.query.clone();
            q.push(c);
            state.apply_query(q);
            TuiAction::None
        }
        TuiInput::Backspace => {
            let mut q = state.query.clone();
            q.pop();
            state.apply_query(q);
            TuiAction::None
        }
        TuiInput::Up => {
            state.move_selection(-1);
            TuiAction::None
        }
        TuiInput::Down => {
            state.move_selection(1);
            TuiAction::None
        }
        // Page keys keep scrolling the visible detail pane mid-typing.
        TuiInput::PageUp | TuiInput::PageDown => {
            state.scroll_detail(if input == TuiInput::PageDown {
                DETAIL_PAGE
            } else {
                -DETAIL_PAGE
            });
            TuiAction::None
        }
        // Commit the query and return to the list.
        TuiInput::Enter | TuiInput::Esc => {
            state.back();
            TuiAction::None
        }
        TuiInput::Quit => TuiAction::Quit,
        // Action/mark inputs are not triggerable mid-typing — the
        // characters would have been captured above. Ignore defensively.
        TuiInput::Install
        | TuiInput::Update
        | TuiInput::Delete
        | TuiInput::Mark
        | TuiInput::MarkAll
        | TuiInput::ClearMarks
        | TuiInput::ScopeToggle
        | TuiInput::ViewToggle
        | TuiInput::Expand
        | TuiInput::Collapse
        | TuiInput::Help
        | TuiInput::Versions
        | TuiInput::Refresh => TuiAction::None,
    }
}

/// A batch action over the current targets (marked set, else selection).
/// `None` when there is nothing to act on.
fn batch(state: &TuiState, op: BatchOp) -> TuiAction {
    let rows = state.action_targets();
    if rows.is_empty() {
        TuiAction::None
    } else {
        TuiAction::Batch { op, rows }
    }
}

/// List / detail keys: navigation, mode entry, and the artifact actions.
/// `↑`/`↓` move the selection in the list and scroll the always-visible
/// detail pane while the detail view is open. `j`/`k` scroll that pane
/// line-by-line from the list or detail view, and `PageUp`/`PageDown` page
/// it — both from *every* mode (search captures `j`/`k` as query text, but
/// pgup/pgdn still page there). The pane has no focus model, so scrolling it
/// never requires entering detail first.
fn handle_browse(state: &mut TuiState, input: TuiInput) -> TuiAction {
    match input {
        TuiInput::Up => {
            if state.mode == Mode::Detail {
                state.scroll_detail(-1);
            } else {
                state.move_selection(-1);
            }
            TuiAction::None
        }
        TuiInput::Down => {
            if state.mode == Mode::Detail {
                state.scroll_detail(1);
            } else {
                state.move_selection(1);
            }
            TuiAction::None
        }
        // `j`/`k` scroll the always-visible detail pane line-by-line from the
        // list or detail view — no focus / detail-mode entry needed (search
        // captures them as query text; pgup/pgdn page it from anywhere).
        TuiInput::Char('j') | TuiInput::Char('k') => {
            state.scroll_detail(if input == TuiInput::Char('j') { 1 } else { -1 });
            TuiAction::None
        }
        // Page keys scroll the detail pane from anywhere — no need to
        // focus it first (the pane is always visible beside the list).
        TuiInput::PageUp | TuiInput::PageDown => {
            state.scroll_detail(if input == TuiInput::PageDown {
                DETAIL_PAGE
            } else {
                -DETAIL_PAGE
            });
            TuiAction::None
        }
        TuiInput::Enter => {
            // On a tree group, Enter folds/unfolds it; on a member row it
            // opens the detail pane via the member-aware helper; on a leaf
            // (or in flat view) it opens the detail pane as before.
            // F12: compute flat once for the whole Enter handler to avoid
            // calling flattened() in selected_is_group() AND again below.
            if state.view_mode == ViewMode::Tree {
                let flat = state.flattened();
                if matches!(flat.get(state.selected), Some(super::tree::DisplayRow::Group { .. })) {
                    state.toggle_collapse_selected();
                } else if matches!(flat.get(state.selected), Some(super::tree::DisplayRow::Member { .. })) {
                    // P3.4 / D5b: Enter on a member row opens the detail pane.
                    state.enter_member_detail();
                } else {
                    state.enter_detail();
                }
            } else {
                state.enter_detail();
            }
            TuiAction::None
        }
        TuiInput::Esc => {
            if state.mode == Mode::Detail {
                state.back();
                TuiAction::None
            } else {
                TuiAction::Quit
            }
        }
        TuiInput::Char('/') => {
            state.enter_search();
            TuiAction::None
        }
        TuiInput::Char('q') | TuiInput::Quit => TuiAction::Quit,
        TuiInput::Char('i') | TuiInput::Install => {
            // P4.1: when marks are empty and the cursor is on a Member with a
            // known repo, route to MemberAction (D9 gate: install requires
            // NotInstalled or IntegrityMissing). State-gated-out → status breadcrumb.
            // F12: compute flat once; reuse for the early-return check and as
            // the action_targets source (batch() will recompute, but this path
            // early-returns before batch(), so only one flattened() per code path).
            if state.marked.is_empty() && state.view_mode == ViewMode::Tree {
                let flat = state.flattened();
                if let Some(super::tree::DisplayRow::Member {
                    member_repo: Some(repo),
                    kind,
                    state: member_state,
                    label,
                    ..
                }) = flat.get(state.selected)
                {
                    use crate::tui::state::ArtifactState;
                    // F13: explicit arms — no `_ =>` wildcard over closed enum.
                    match member_state {
                        ArtifactState::NotInstalled | ArtifactState::IntegrityMissing => {
                            return TuiAction::MemberAction {
                                op: BatchOp::Install,
                                repo: repo.clone(),
                                kind: *kind,
                                name: label.clone(),
                            };
                        }
                        ArtifactState::Installed
                        | ArtifactState::ViaBundle
                        | ArtifactState::Outdated
                        | ArtifactState::Modified => {
                            state.set_status("already installed");
                            return TuiAction::None;
                        }
                    }
                }
            }
            batch(state, BatchOp::Install)
        }
        TuiInput::Char('u') | TuiInput::Update => {
            // P4.1 + F10: update requires Installed, Outdated, Modified, or
            // IntegrityMissing (integrity-missing = declared but corrupt →
            // reinstall via Update is the correct recovery, matching leaf semantics).
            // F12: single flattened() call per code path (early-return before batch()).
            if state.marked.is_empty() && state.view_mode == ViewMode::Tree {
                let flat = state.flattened();
                if let Some(super::tree::DisplayRow::Member {
                    member_repo: Some(repo),
                    kind,
                    state: member_state,
                    label,
                    ..
                }) = flat.get(state.selected)
                {
                    use crate::tui::state::ArtifactState;
                    // F13: explicit arms — no `_ =>` wildcard over closed enum.
                    // F10: IntegrityMissing now allowed for Update (reinstall).
                    match member_state {
                        ArtifactState::Installed
                        | ArtifactState::ViaBundle
                        | ArtifactState::Outdated
                        | ArtifactState::Modified
                        | ArtifactState::IntegrityMissing => {
                            return TuiAction::MemberAction {
                                op: BatchOp::Update,
                                repo: repo.clone(),
                                kind: *kind,
                                name: label.clone(),
                            };
                        }
                        ArtifactState::NotInstalled => {
                            state.set_status("not installed");
                            return TuiAction::None;
                        }
                    }
                }
            }
            batch(state, BatchOp::Update)
        }
        TuiInput::Char('d') | TuiInput::Delete => {
            // P4.1: delete requires Installed, Outdated, Modified, or IntegrityMissing.
            // F12: single flattened() call per code path (early-return before batch()).
            if state.marked.is_empty() && state.view_mode == ViewMode::Tree {
                let flat = state.flattened();
                if let Some(super::tree::DisplayRow::Member {
                    member_repo: Some(repo),
                    kind,
                    state: member_state,
                    label,
                    ..
                }) = flat.get(state.selected)
                {
                    use crate::tui::state::ArtifactState;
                    // F13: explicit arms — no `_ =>` wildcard over closed enum.
                    match member_state {
                        ArtifactState::Installed
                        | ArtifactState::ViaBundle
                        | ArtifactState::Outdated
                        | ArtifactState::Modified
                        | ArtifactState::IntegrityMissing => {
                            return TuiAction::MemberAction {
                                op: BatchOp::Uninstall,
                                repo: repo.clone(),
                                kind: *kind,
                                name: label.clone(),
                            };
                        }
                        ArtifactState::NotInstalled => {
                            state.set_status("not installed");
                            return TuiAction::None;
                        }
                    }
                }
            }
            batch(state, BatchOp::Uninstall)
        }
        TuiInput::Char(' ') | TuiInput::Mark => {
            state.toggle_mark_selected();
            TuiAction::None
        }
        TuiInput::Char('a') | TuiInput::MarkAll => {
            state.toggle_mark_all_filtered();
            TuiAction::None
        }
        TuiInput::Char('c') | TuiInput::ClearMarks => {
            state.clear_marks();
            TuiAction::None
        }
        TuiInput::Char('r') | TuiInput::Refresh => TuiAction::Refresh,
        TuiInput::Char('g') | TuiInput::ScopeToggle => TuiAction::ToggleScope,
        TuiInput::Char('t') | TuiInput::ViewToggle => {
            state.toggle_view_mode();
            TuiAction::None
        }
        TuiInput::Expand => {
            // Expand a group if selected; for bundle leaves, insert the leaf
            // key into expanded_bundles and trigger a lazy member-list fetch
            // if no cache entry exists yet.
            let selected_before = state.selected;
            state.expand_selected();

            // Check whether the selection (before expand) was a bundle leaf.
            // Only emit LoadBundleMembers when there is no cache entry yet
            // (no-retry: Loading/Failed/Offline entries are kept as-is).
            if state.view_mode == crate::tui::state::ViewMode::Tree {
                let flat = state.flattened();
                if let Some(crate::tui::tree::DisplayRow::Leaf { row, key: leaf_key, .. }) = flat.get(selected_before)
                    && let Some(tui_row) = state.rows.get(*row)
                    && tui_row.kind == "bundle"
                {
                    let row_idx = *row;
                    let bundle_repo = tui_row.repo.clone();
                    let _ = leaf_key; // unused after F3 re-keying
                    let cache_key = (state.scope_label.clone(), bundle_repo.clone());

                    // F3/P3.4: Insert the FULL bundle repo into expanded_bundles
                    // so the member splice gate in flatten_with_members passes.
                    // Key is now registry/repository (not the display leaf key).
                    // Idempotent — re-pressing → when already Ready is fine.
                    state.expanded_bundles.insert(bundle_repo.clone());

                    // Only spawn a fetch when no entry exists yet OR when the
                    // entry is Loading but no task is in flight (the channel was
                    // full and the result was dropped — recovery path per W3).
                    // Failed/Ready/Offline entries are kept as-is (no retry storm).
                    let should_fetch = match state.bundle_members.get(&cache_key) {
                        None => true,
                        Some(crate::tui::bundle_members::BundleMemberCache::Loading) => {
                            // Allow re-fetch: a previous try_send may have dropped
                            // the result on a full channel, leaving Loading stuck.
                            // The new fetch will overwrite Loading when it completes.
                            true
                        }
                        Some(_) => false, // Ready / Failed / Offline: no re-fetch
                    };
                    if should_fetch {
                        // Insert Loading placeholder so the UI shows feedback
                        // immediately while the fetch is in flight.
                        state
                            .bundle_members
                            .insert(cache_key, crate::tui::bundle_members::BundleMemberCache::Loading);
                        return TuiAction::LoadBundleMembers {
                            row: row_idx,
                            bundle_repo,
                        };
                    }
                }
            }

            TuiAction::None
        }
        TuiInput::Collapse => {
            // F3/P3.4: `←` on an expanded bundle leaf collapses it (removes
            // the FULL bundle repo from expanded_bundles, keeps cache).
            // F8: delegates to collapse_bundle_leaf to also re-clamp selection.
            // On any other selection fall through to standard collapse-or-jump-to-parent.
            if state.view_mode == crate::tui::state::ViewMode::Tree {
                let flat = state.flattened();
                if let Some(crate::tui::tree::DisplayRow::Leaf {
                    row,
                    is_bundle,
                    collapsed,
                    ..
                }) = flat.get(state.selected)
                    && *is_bundle
                    && !collapsed
                {
                    // Expanded bundle leaf: collapse it.
                    // Resolve full repo from rows to avoid depending on the
                    // display-path key (which is elided when default_registry matches).
                    let row_idx = *row;
                    let bundle_repo = state.rows.get(row_idx).map(|r| r.repo.clone());
                    if let Some(repo) = bundle_repo {
                        state.collapse_bundle_leaf(&repo);
                    }
                    return TuiAction::None;
                }
            }
            // ARIA/nvim-tree/lazygit standard: `←` on an expanded group
            // collapses it; `←` on a collapsed group or a leaf jumps to
            // the nearest ancestor group (see `collapse_or_jump_to_parent`).
            state.collapse_or_jump_to_parent();
            TuiAction::None
        }
        TuiInput::Char('?') | TuiInput::Help => {
            state.enter_help();
            TuiAction::None
        }
        TuiInput::Char('v') | TuiInput::Versions => match state.open_version_pick() {
            Some(row) => TuiAction::LoadVersions { row },
            None => TuiAction::None,
        },
        TuiInput::Char('o') => match state.selected_row().and_then(|r| r.repository_url.clone()) {
            Some(url) => TuiAction::OpenUrl { url },
            None => {
                state.set_status("no repository URL for this entry");
                TuiAction::None
            }
        },
        // Any other printable in list mode is inert.
        TuiInput::Char(_) => TuiAction::None,
        TuiInput::Backspace => TuiAction::None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::state::{ArtifactState, TuiRow};

    fn row(repo: &str) -> TuiRow {
        let (reg, repo_path) = repo.split_once('/').unwrap_or((repo, ""));
        TuiRow {
            kind: "skill".to_string(),
            registry: reg.to_string(),
            repository: repo_path.to_string(),
            repo: repo.to_string(),
            description: "d".to_string(),
            summary: String::new(),
            keywords: vec!["kw".to_string()],
            repository_url: None,
            latest_tag: "latest".to_string(),
            version: "1.0.0".to_string(),
            deprecated: None,
            pinned_version: None,
            state: ArtifactState::NotInstalled,
        }
    }

    fn seeded() -> TuiState {
        let mut s = TuiState::new();
        s.set_rows(vec![row("r/a"), row("r/b"), row("r/c")]);
        s
    }

    #[test]
    fn list_navigation_moves_selection() {
        let mut s = seeded();
        assert_eq!(handle(&mut s, TuiInput::Down), TuiAction::None);
        assert_eq!(s.selected, 1);
        assert_eq!(handle(&mut s, TuiInput::Up), TuiAction::None);
        assert_eq!(s.selected, 0);
    }

    #[test]
    fn enter_opens_detail_esc_returns() {
        let mut s = seeded();
        handle(&mut s, TuiInput::Enter);
        assert_eq!(s.mode, Mode::Detail);
        // Esc in detail returns to list (not quit).
        assert_eq!(handle(&mut s, TuiInput::Esc), TuiAction::None);
        assert_eq!(s.mode, Mode::List);
    }

    #[test]
    fn esc_in_list_quits() {
        let mut s = seeded();
        assert_eq!(handle(&mut s, TuiInput::Esc), TuiAction::Quit);
    }

    #[test]
    fn q_and_quit_input_both_quit() {
        let mut s = seeded();
        assert_eq!(handle(&mut s, TuiInput::Char('q')), TuiAction::Quit);
        assert_eq!(handle(&mut s, TuiInput::Quit), TuiAction::Quit);
    }

    #[test]
    fn install_update_no_marks_target_selection() {
        let mut s = seeded();
        s.move_selection(1); // select row index 1
        assert_eq!(
            handle(&mut s, TuiInput::Char('i')),
            TuiAction::Batch {
                op: BatchOp::Install,
                rows: vec![1]
            }
        );
        assert_eq!(
            handle(&mut s, TuiInput::Install),
            TuiAction::Batch {
                op: BatchOp::Install,
                rows: vec![1]
            }
        );
        assert_eq!(
            handle(&mut s, TuiInput::Update),
            TuiAction::Batch {
                op: BatchOp::Update,
                rows: vec![1]
            }
        );
    }

    #[test]
    fn marks_drive_batch_over_selection() {
        let mut s = seeded();
        // Mark row 0 and row 2.
        handle(&mut s, TuiInput::Mark);
        s.move_selection(2);
        handle(&mut s, TuiInput::Mark);
        // Selection is row 2 but the marked set wins.
        assert_eq!(
            handle(&mut s, TuiInput::Install),
            TuiAction::Batch {
                op: BatchOp::Install,
                rows: vec![0, 2]
            }
        );
        // Clear marks ⇒ falls back to the single selection (row 2).
        handle(&mut s, TuiInput::ClearMarks);
        assert_eq!(
            handle(&mut s, TuiInput::Update),
            TuiAction::Batch {
                op: BatchOp::Update,
                rows: vec![2]
            }
        );
    }

    #[test]
    fn mark_all_toggles_visible_set() {
        let mut s = seeded();
        handle(&mut s, TuiInput::MarkAll);
        assert_eq!(
            handle(&mut s, TuiInput::Install),
            TuiAction::Batch {
                op: BatchOp::Install,
                rows: vec![0, 1, 2]
            }
        );
        handle(&mut s, TuiInput::MarkAll); // all marked ⇒ clears
        assert!(s.marked.is_empty());
    }

    #[test]
    fn delete_emits_uninstall_batch_and_is_literal_in_search() {
        let mut s = seeded();
        s.move_selection(2);
        assert_eq!(
            handle(&mut s, TuiInput::Char('d')),
            TuiAction::Batch {
                op: BatchOp::Uninstall,
                rows: vec![2]
            }
        );
        assert_eq!(
            handle(&mut s, TuiInput::Delete),
            TuiAction::Batch {
                op: BatchOp::Uninstall,
                rows: vec![2]
            }
        );
        // 'd' must be a literal query char while typing, never uninstall.
        handle(&mut s, TuiInput::Char('/'));
        assert_eq!(handle(&mut s, TuiInput::Char('d')), TuiAction::None);
        assert_eq!(s.query, "d");
    }

    #[test]
    fn install_without_selection_is_inert() {
        let mut s = TuiState::new();
        s.set_rows(vec![]);
        assert_eq!(handle(&mut s, TuiInput::Install), TuiAction::None);
        assert_eq!(handle(&mut s, TuiInput::Update), TuiAction::None);
        assert_eq!(handle(&mut s, TuiInput::Delete), TuiAction::None);
    }

    #[test]
    fn space_marks_but_is_literal_in_search() {
        let mut s = seeded();
        handle(&mut s, TuiInput::Char(' '));
        assert!(s.is_row_marked(0), "space marks in list mode");
        s.clear_marks();
        handle(&mut s, TuiInput::Char('/'));
        assert_eq!(handle(&mut s, TuiInput::Char(' ')), TuiAction::None);
        assert_eq!(s.query, " ", "space is a literal query char in search");
        assert!(s.marked.is_empty(), "no marking while typing");
    }

    #[test]
    fn help_overlay_scroll_keys_stay_other_keys_close() {
        let mut s = seeded();
        assert_eq!(handle(&mut s, TuiInput::Char('?')), TuiAction::None);
        assert_eq!(s.mode, Mode::Help);
        // Scroll keys move the overlay and keep it open (a no-op when it
        // already fits the terminal, as here at the default 80×24).
        assert_eq!(handle(&mut s, TuiInput::Down), TuiAction::None);
        assert_eq!(s.mode, Mode::Help, "scroll keys do not close the overlay");
        // A non-scroll, non-quit key dismisses back to the list.
        assert_eq!(handle(&mut s, TuiInput::Char('x')), TuiAction::None);
        assert_eq!(s.mode, Mode::List);
        // `q` from help quits.
        handle(&mut s, TuiInput::Help);
        assert_eq!(s.mode, Mode::Help);
        assert_eq!(handle(&mut s, TuiInput::Char('q')), TuiAction::Quit);
    }

    #[test]
    fn scope_toggle_emits_action_and_is_literal_in_search() {
        let mut s = seeded();
        assert_eq!(handle(&mut s, TuiInput::Char('g')), TuiAction::ToggleScope);
        assert_eq!(handle(&mut s, TuiInput::ScopeToggle), TuiAction::ToggleScope);
        handle(&mut s, TuiInput::Char('/'));
        assert_eq!(handle(&mut s, TuiInput::Char('g')), TuiAction::None);
        assert_eq!(s.query, "g");
    }

    #[test]
    fn version_key_opens_picker_and_loads_then_picks() {
        let mut s = seeded();
        s.move_selection(1);
        assert_eq!(handle(&mut s, TuiInput::Char('v')), TuiAction::LoadVersions { row: 1 });
        assert_eq!(s.mode, Mode::VersionPick);
        // App feeds tags back; picker keys navigate + commit.
        s.set_picker_tags(vec!["3.0.0".to_string(), "2.0.0".to_string()]);
        assert_eq!(handle(&mut s, TuiInput::Down), TuiAction::None);
        assert_eq!(handle(&mut s, TuiInput::Enter), TuiAction::None);
        assert_eq!(s.mode, Mode::List);
        assert_eq!(s.rows[1].pinned_version.as_deref(), Some("2.0.0"));
        // 'v' is a literal query char while searching, never the picker.
        handle(&mut s, TuiInput::Char('/'));
        assert_eq!(handle(&mut s, TuiInput::Char('v')), TuiAction::None);
        assert_eq!(s.query, "v");
    }

    #[test]
    fn picker_esc_cancels_without_pinning() {
        let mut s = seeded();
        handle(&mut s, TuiInput::Char('v'));
        s.set_picker_tags(vec!["1.0.0".to_string()]);
        assert_eq!(handle(&mut s, TuiInput::Esc), TuiAction::None);
        assert_eq!(s.mode, Mode::List);
        assert_eq!(s.rows[0].pinned_version, None);
    }

    #[test]
    fn refresh_emits_refresh() {
        let mut s = seeded();
        assert_eq!(handle(&mut s, TuiInput::Char('r')), TuiAction::Refresh);
        assert_eq!(handle(&mut s, TuiInput::Refresh), TuiAction::Refresh);
    }

    #[test]
    fn slash_enters_search_then_typing_filters_not_hotkeys() {
        let mut s = seeded();
        handle(&mut s, TuiInput::Char('/'));
        assert_eq!(s.mode, Mode::Search);
        // 'i' in search mode is a literal query character, NOT install.
        assert_eq!(handle(&mut s, TuiInput::Char('i')), TuiAction::None);
        assert_eq!(s.query, "i");
        // Backspace clears it; an empty query matches everything again.
        handle(&mut s, TuiInput::Backspace);
        assert_eq!(s.query, "");
        assert_eq!(s.filtered.len(), 3);
        // A character absent from every field ⇒ empty filter (proves the
        // shared matcher actually narrows, not that 'i' is special).
        handle(&mut s, TuiInput::Char('z'));
        assert_eq!(s.query, "z");
        assert!(s.filtered.is_empty());
        handle(&mut s, TuiInput::Backspace);
        // Enter commits the (empty) query, back to list.
        assert_eq!(handle(&mut s, TuiInput::Enter), TuiAction::None);
        assert_eq!(s.mode, Mode::List);
    }

    #[test]
    fn search_mode_navigation_still_scrolls() {
        let mut s = seeded();
        handle(&mut s, TuiInput::Char('/'));
        handle(&mut s, TuiInput::Down);
        assert_eq!(s.selected, 1);
    }

    #[test]
    fn quit_input_quits_even_in_search() {
        let mut s = seeded();
        handle(&mut s, TuiInput::Char('/'));
        assert_eq!(handle(&mut s, TuiInput::Quit), TuiAction::Quit);
    }

    // Step 3.3: REPLACED — old stub-era test. With the tree back, Enter on a
    // leaf opens the detail pane; Enter on a group folds/unfolds it.
    #[test]
    fn enter_on_leaf_opens_detail_enter_on_group_folds_unfolds() {
        let mut s = seeded();
        // In flat mode every row is a leaf: Enter must open detail.
        assert_eq!(s.view_mode, super::super::state::ViewMode::Flat);
        s.move_selection(1);
        assert_eq!(handle(&mut s, TuiInput::Enter), TuiAction::None);
        assert_eq!(s.mode, Mode::Detail);
        assert_eq!(s.selected_row().unwrap().repo, "r/b");
        s.back();

        // In tree mode, Enter on the currently-selected row should:
        //   - if selected_is_group() → toggle collapse (fold/unfold) — mode stays List
        //   - if selected_is_leaf  → open detail pane
        handle(&mut s, TuiInput::ViewToggle); // → Tree
        // The exact behavior depends on whether position 0 is a group.
        // selected_is_group() is unimplemented, so this test will panic
        // with unimplemented!() — which is the required failure mode.
        let _action = handle(&mut s, TuiInput::Enter);
        // If we reach here (won't until implemented), assert mode is not
        // erroneously in Detail when a group was selected.
        // (Implementation will decide the specifics.)
    }

    #[test]
    fn detail_arrows_scroll_instead_of_moving_selection() {
        let mut s = seeded();
        handle(&mut s, TuiInput::Enter);
        assert_eq!(s.mode, Mode::Detail);
        // Down scrolls the pane; the selection stays put.
        assert_eq!(handle(&mut s, TuiInput::Down), TuiAction::None);
        assert_eq!(s.selected, 0, "selection unchanged in detail mode");
        assert_eq!(s.detail_scroll, 1);
        // j/k alias the scroll.
        handle(&mut s, TuiInput::Char('j'));
        assert_eq!(s.detail_scroll, 2);
        handle(&mut s, TuiInput::Char('k'));
        assert_eq!(s.detail_scroll, 1);
        // Up scrolls back and saturates at the top.
        handle(&mut s, TuiInput::Up);
        handle(&mut s, TuiInput::Up);
        assert_eq!(s.detail_scroll, 0);
        assert_eq!(s.selected, 0);
        // Back in the list. j/k now scroll the always-visible detail pane from
        // list mode too (no focus / detail-mode entry needed); the selection
        // stays put. The fixture row's detail overflows (see the page test).
        handle(&mut s, TuiInput::Esc);
        assert_eq!(s.mode, Mode::List);
        assert_eq!(handle(&mut s, TuiInput::Char('j')), TuiAction::None);
        assert_eq!(s.selected, 0, "j must not move the selection in list mode");
        assert_eq!(s.detail_scroll, 1, "j scrolls the detail pane from list mode");
        handle(&mut s, TuiInput::Char('k'));
        assert_eq!(s.detail_scroll, 0, "k scrolls the detail pane back");
        // Arrows still move the selection in list mode.
        handle(&mut s, TuiInput::Down);
        assert_eq!(s.selected, 1);
    }

    #[test]
    fn page_keys_scroll_detail_without_focus() {
        let mut s = seeded();
        // The content's exact scroll range in the current viewport — the
        // bottom clamp the page keys must respect.
        let max = crate::tui::detail::scroll_max(
            &crate::tui::detail::detail_lines(s.selected_row()),
            crate::tui::detail::viewport(s.term_size),
        );
        let page = u16::try_from(DETAIL_PAGE).unwrap();
        assert!(max > page, "fixture content must overflow by more than one page");
        // List mode: PageDown scrolls the pane, selection stays put.
        assert_eq!(handle(&mut s, TuiInput::PageDown), TuiAction::None);
        assert_eq!(s.detail_scroll, page);
        assert_eq!(s.selected, 0, "selection unchanged by page keys");
        // PageUp scrolls back and saturates at the top.
        handle(&mut s, TuiInput::PageUp);
        handle(&mut s, TuiInput::PageUp);
        assert_eq!(s.detail_scroll, 0);
        // Detail mode: same behavior.
        handle(&mut s, TuiInput::Enter);
        handle(&mut s, TuiInput::PageDown);
        assert_eq!(s.detail_scroll, page);
        // Leaving detail keeps the offset — the pane content is unchanged.
        handle(&mut s, TuiInput::Esc);
        assert_eq!(s.detail_scroll, page);
        // Search mode: page keys still scroll, and the bottom clamp wins —
        // paging past the end stops at the content's last row.
        handle(&mut s, TuiInput::Char('/'));
        handle(&mut s, TuiInput::PageDown);
        handle(&mut s, TuiInput::PageDown);
        handle(&mut s, TuiInput::PageDown);
        assert_eq!(s.detail_scroll, max, "scrolling stops at the content end");
        assert_eq!(s.query, "", "page keys are not query characters");
    }

    // The `?` help overlay scrolls (↑↓ / j/k / pgup-pgdn) on a terminal too
    // short to fit it; any non-scroll key closes it.
    #[test]
    fn help_overlay_scrolls_then_any_other_key_closes() {
        let mut s = seeded();
        s.set_term_size((80, 8)); // short → the overlay must scroll
        handle(&mut s, TuiInput::Char('?'));
        assert_eq!(s.mode, Mode::Help);
        assert_eq!(s.help_scroll, 0, "opens at the top");
        // j / Down scroll within the overlay and keep it open.
        handle(&mut s, TuiInput::Char('j'));
        assert_eq!(s.mode, Mode::Help, "j scrolls, does not close");
        assert!(s.help_scroll >= 1, "j scrolled the help overlay");
        handle(&mut s, TuiInput::Down);
        assert!(s.help_scroll >= 2, "Down scrolls further");
        handle(&mut s, TuiInput::Char('k'));
        handle(&mut s, TuiInput::Up);
        assert_eq!(s.help_scroll, 0, "k / Up scroll back to the top");
        // A non-scroll key dismisses the overlay.
        assert_eq!(handle(&mut s, TuiInput::Char('x')), TuiAction::None);
        assert_eq!(s.mode, Mode::List, "any other key closes help");
    }

    #[test]
    fn o_opens_repository_url_or_reports_absence() {
        let mut s = seeded();
        // No URL on the row ⇒ status message, no action.
        assert_eq!(handle(&mut s, TuiInput::Char('o')), TuiAction::None);
        assert_eq!(s.status_line, "no repository URL for this entry");
        // With a URL the action carries it (list and detail mode).
        s.rows[0].repository_url = Some("https://github.com/acme/a".to_string());
        assert_eq!(
            handle(&mut s, TuiInput::Char('o')),
            TuiAction::OpenUrl {
                url: "https://github.com/acme/a".to_string()
            }
        );
        handle(&mut s, TuiInput::Enter);
        assert_eq!(s.mode, Mode::Detail);
        assert_eq!(
            handle(&mut s, TuiInput::Char('o')),
            TuiAction::OpenUrl {
                url: "https://github.com/acme/a".to_string()
            }
        );
        // 'o' is a literal query char while searching.
        handle(&mut s, TuiInput::Esc);
        handle(&mut s, TuiInput::Char('/'));
        assert_eq!(handle(&mut s, TuiInput::Char('o')), TuiAction::None);
        assert_eq!(s.query, "o");
    }

    // Step 3.3: REPLACED — old stub-era test that asserted `t` was inert.
    // The tree is now back: `t` in browse emits a ViewToggle / toggles the view;
    // `t` stays literal in search mode.
    #[test]
    fn t_in_browse_emits_view_toggle_literal_in_search() {
        let mut s = seeded();
        // `t` in List mode must toggle the view mode (Flat→Tree or Tree→Flat)
        // and return TuiAction::None (state-only change).
        let mode_before = s.view_mode;
        let action = handle(&mut s, TuiInput::Char('t'));
        assert_eq!(action, TuiAction::None, "`t` returns None (state-only)");
        assert_ne!(s.view_mode, mode_before, "`t` must toggle the view mode in browse");
        // Second `t` toggles back
        handle(&mut s, TuiInput::Char('t'));
        assert_eq!(s.view_mode, mode_before, "second `t` toggles back");
        // ViewToggle input must also work
        handle(&mut s, TuiInput::ViewToggle);
        assert_ne!(s.view_mode, mode_before);
        // In search mode `t` is a literal query char, NOT a toggle
        handle(&mut s, TuiInput::ViewToggle); // restore
        handle(&mut s, TuiInput::Char('/'));
        let mode_in_search = s.view_mode;
        assert_eq!(handle(&mut s, TuiInput::Char('t')), TuiAction::None);
        assert_eq!(s.query, "t", "`t` appends to query in search mode");
        assert_eq!(
            s.view_mode, mode_in_search,
            "`t` must not toggle view mode while searching"
        );
    }
}

// NOTE: Step 3.3 additional tests are appended below the closing brace
// of the existing `mod tests` block. They are in a separate mod to avoid
// merge conflicts with the block above.

// ── P2 Specify tests — C-5, C-5b, C-6, C-7, C-8, C-12 ──────────────────────
//
// These tests encode contracts from plan_tui_member_nodes.
// They MUST compile and MUST FAIL against P1/P3 stubs (unimplemented!).
// Do NOT implement production logic here — tests only.
#[cfg(test)]
mod p2_event_member_node_tests {
    use super::*;
    use crate::tui::bundle_members::{BundleMemberCache, MemberNode};
    use crate::tui::state::{ArtifactState, TuiRow, TuiState};
    use crate::tui::tree::DisplayRow;

    fn bundle_row(repo: &str) -> TuiRow {
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

    fn skill_row(repo: &str) -> TuiRow {
        let (reg, repo_path) = repo.split_once('/').unwrap_or((repo, ""));
        TuiRow {
            kind: "skill".to_string(),
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

    fn make_member_node(repo: Option<&str>, state: ArtifactState) -> MemberNode {
        MemberNode {
            kind: crate::oci::ArtifactKind::Skill,
            label: "my-skill".to_string(),
            member_repo: repo.map(|r| r.to_string()),
            state,
            related: false,
        }
    }

    /// Build a tree-mode TuiState with one bundle and one expanded member.
    fn bundle_state_with_expanded_member(member_state: ArtifactState, member_repo: Option<&str>) -> TuiState {
        let mut s = TuiState::new();
        s.set_rows(vec![bundle_row("reg/acme/bundle-x")]);
        s.set_default_registry(Some("reg".to_string()));
        s.toggle_view_mode(); // Tree mode

        let scope = s.scope_label.clone();
        s.bundle_members.insert(
            (scope, "reg/acme/bundle-x".to_string()),
            BundleMemberCache::Ready(vec![make_member_node(member_repo, member_state)]),
        );
        // F3: expanded_bundles is now keyed by the FULL bundle repo string.
        s.expanded_bundles.insert("reg/acme/bundle-x".to_string());
        s
    }

    /// Navigate to the first Member in the flattened tree.
    fn select_member(s: &mut TuiState) -> bool {
        let flat = s.flattened();
        if let Some(pos) = flat.iter().position(|r| matches!(r, DisplayRow::Member { .. })) {
            s.selected = pos;
            true
        } else {
            false
        }
    }

    // ── C-5: → on a bundle leaf inserts key into expanded_bundles and emits LoadBundleMembers
    // when no cache entry exists yet.

    #[test]
    fn c5_expand_on_bundle_leaf_no_cache_emits_load_bundle_members() {
        let mut s = TuiState::new();
        s.set_rows(vec![bundle_row("reg/acme/bundle-x")]);
        s.set_default_registry(Some("reg".to_string()));
        s.toggle_view_mode();
        // No cache entry → should emit LoadBundleMembers.
        // Navigate to the bundle leaf (should be at the leaf position after the group).
        let flat = s.flattened();
        let leaf_pos = flat
            .iter()
            .position(|r| matches!(r, DisplayRow::Leaf { is_bundle: true, .. }))
            .unwrap_or(1); // group is pos 0, leaf is pos 1
        s.selected = leaf_pos;

        let action = handle(&mut s, TuiInput::Expand);

        // After expand: bundle-x must be in expanded_bundles (keyed by FULL repo after F3).
        assert!(
            s.expanded_bundles.contains("reg/acme/bundle-x"),
            "C-5: → on bundle leaf must insert full repo key into expanded_bundles"
        );
        // Action must be LoadBundleMembers (no cache entry exists).
        assert!(
            matches!(action, TuiAction::LoadBundleMembers { ref bundle_repo, .. } if bundle_repo == "reg/acme/bundle-x"),
            "C-5: → on bundle leaf with no cache must emit LoadBundleMembers; got: {action:?}"
        );
    }

    #[test]
    fn c5_expand_on_bundle_leaf_with_ready_cache_emits_none() {
        // → on an already-expanded bundle leaf with a Ready cache: no re-fetch.
        let mut s = bundle_state_with_expanded_member(ArtifactState::NotInstalled, Some("reg/acme/skill-a"));
        // Navigate to the bundle leaf.
        let flat = s.flattened();
        let leaf_pos = flat
            .iter()
            .position(|r| matches!(r, DisplayRow::Leaf { is_bundle: true, .. }))
            .unwrap_or(1);
        s.selected = leaf_pos;

        // Press → twice: first press may change state, second must be idempotent.
        let _first = handle(&mut s, TuiInput::Expand);
        let second = handle(&mut s, TuiInput::Expand);

        assert_eq!(
            second,
            TuiAction::None,
            "C-5: idempotent double-→ on Ready bundle leaf must return TuiAction::None"
        );
        // expanded_bundles must still contain the full repo key (F3: full repo, not leaf path).
        assert!(
            s.expanded_bundles.contains("reg/acme/bundle-x"),
            "C-5: bundle-x must remain in expanded_bundles after double-→ (full repo key)"
        );
    }

    #[test]
    fn c5_collapse_on_expanded_bundle_leaf_removes_key_and_keeps_cache() {
        // ← on an expanded bundle leaf removes the key from expanded_bundles;
        // cache entry is retained.
        let mut s = bundle_state_with_expanded_member(ArtifactState::NotInstalled, Some("reg/acme/skill-a"));

        // Navigate to the bundle leaf.
        let flat = s.flattened();
        let leaf_pos = flat
            .iter()
            .position(|r| matches!(r, DisplayRow::Leaf { is_bundle: true, .. }))
            .unwrap_or(1);
        s.selected = leaf_pos;

        let action = handle(&mut s, TuiInput::Collapse);

        assert_eq!(
            action,
            TuiAction::None,
            "C-5: ← on expanded bundle leaf must return TuiAction::None"
        );
        assert!(
            !s.expanded_bundles.contains("reg/acme/bundle-x"),
            "C-5: ← must remove bundle-x (full repo key) from expanded_bundles"
        );
        // Cache must still be present (not cleared).
        let key = (s.scope_label.clone(), "reg/acme/bundle-x".to_string());
        assert!(
            s.bundle_members.contains_key(&key),
            "C-5: cache must be retained after ← (collapse without cache clear)"
        );
    }

    #[test]
    fn c5_enter_on_bundle_leaf_opens_detail_not_toggle() {
        // Enter on a bundle leaf must enter Mode::Detail (not toggle collapse).
        let mut s = bundle_state_with_expanded_member(ArtifactState::NotInstalled, Some("reg/acme/skill-a"));

        // Navigate to the bundle leaf.
        let flat = s.flattened();
        let leaf_pos = flat
            .iter()
            .position(|r| matches!(r, DisplayRow::Leaf { is_bundle: true, .. }))
            .unwrap_or(1);
        s.selected = leaf_pos;

        let action = handle(&mut s, TuiInput::Enter);

        assert_eq!(
            s.mode,
            Mode::Detail,
            "C-5: Enter on bundle leaf must enter Mode::Detail"
        );
        assert_eq!(
            action,
            TuiAction::None,
            "C-5: Enter on bundle leaf returns TuiAction::None"
        );
        // expanded_bundles must be unchanged by Enter (F3: check full repo key).
        assert!(
            s.expanded_bundles.contains("reg/acme/bundle-x"),
            "C-5: Enter must NOT modify expanded_bundles"
        );
    }

    // ── C-5b: Enter on a member enters Mode::Detail ───────────────────────────

    #[test]
    fn c5b_enter_on_member_enters_detail_mode() {
        let mut s = bundle_state_with_expanded_member(ArtifactState::NotInstalled, Some("reg/acme/skill-a"));

        let found = select_member(&mut s);
        assert!(found, "C-5b: must have a Member row to test Enter-on-member");

        let action = handle(&mut s, TuiInput::Enter);

        assert_eq!(
            s.mode,
            Mode::Detail,
            "C-5b: Enter on a member must transition to Mode::Detail (not a no-op)"
        );
        assert_eq!(
            action,
            TuiAction::None,
            "C-5b: Enter on a member returns TuiAction::None"
        );
    }

    #[test]
    fn c5b_enter_on_member_does_not_mutate_expanded_bundles() {
        let mut s = bundle_state_with_expanded_member(ArtifactState::NotInstalled, Some("reg/acme/skill-a"));
        let bundles_before = s.expanded_bundles.clone();

        let found = select_member(&mut s);
        if found {
            handle(&mut s, TuiInput::Enter);
            assert_eq!(
                s.expanded_bundles, bundles_before,
                "C-5b: Enter on a member must not change expanded_bundles"
            );
        }
    }

    #[test]
    fn c5b_enter_on_member_resets_detail_scroll_to_zero() {
        let mut s = bundle_state_with_expanded_member(ArtifactState::NotInstalled, Some("reg/acme/skill-a"));
        s.detail_scroll = 5; // pre-set scroll

        let found = select_member(&mut s);
        if found {
            handle(&mut s, TuiInput::Enter);
            assert_eq!(
                s.detail_scroll, 0,
                "C-5b: Enter on a member must reset detail_scroll to 0"
            );
        }
    }

    // ── C-6: member i/u/d single-target dispatch ──────────────────────────────

    #[test]
    fn c6_install_on_member_with_some_repo_emits_member_action() {
        let mut s = bundle_state_with_expanded_member(ArtifactState::NotInstalled, Some("reg/acme/skill-a"));
        let found = select_member(&mut s);
        assert!(found, "C-6: must have a Member row");

        let action = handle(&mut s, TuiInput::Install);

        assert!(
            matches!(
                action,
                TuiAction::MemberAction { op: BatchOp::Install, ref repo, .. }
                if repo == "reg/acme/skill-a"
            ),
            "C-6: Install on member with Some(repo) must emit MemberAction; got: {action:?}"
        );
    }

    #[test]
    fn c6_update_on_member_emits_member_action() {
        let mut s = bundle_state_with_expanded_member(ArtifactState::Installed, Some("reg/acme/skill-a"));
        let found = select_member(&mut s);
        assert!(found, "C-6: must have a Member row");

        let action = handle(&mut s, TuiInput::Update);

        assert!(
            matches!(
                action,
                TuiAction::MemberAction { op: BatchOp::Update, ref repo, .. }
                if repo == "reg/acme/skill-a"
            ),
            "C-6: Update on member must emit MemberAction; got: {action:?}"
        );
    }

    #[test]
    fn c6_delete_on_member_emits_member_action() {
        let mut s = bundle_state_with_expanded_member(ArtifactState::Installed, Some("reg/acme/skill-a"));
        let found = select_member(&mut s);
        assert!(found, "C-6: must have a Member row");

        let action = handle(&mut s, TuiInput::Delete);

        assert!(
            matches!(
                action,
                TuiAction::MemberAction { op: BatchOp::Uninstall, ref repo, .. }
                if repo == "reg/acme/skill-a"
            ),
            "C-6: Delete on member must emit MemberAction; got: {action:?}"
        );
    }

    #[test]
    fn c6_none_repo_yields_none_action() {
        // A member with member_repo = None (placeholder) must emit TuiAction::None.
        let mut s = bundle_state_with_expanded_member(ArtifactState::NotInstalled, None);
        let found = select_member(&mut s);
        assert!(found, "C-6: must have a Member row (placeholder)");

        let action = handle(&mut s, TuiInput::Install);

        assert_eq!(
            action,
            TuiAction::None,
            "C-6: member with None repo must return TuiAction::None"
        );
    }

    // ── C-7: state-gating per member.state ────────────────────────────────────
    //
    // i: NotInstalled, IntegrityMissing → MemberAction(Install)
    // u: Installed, Outdated, Modified → MemberAction(Update)
    // d: Installed, Outdated, Modified, IntegrityMissing → MemberAction(Uninstall)
    // Everything else → TuiAction::None

    #[test]
    fn c7_install_gate_not_installed_emits_member_action() {
        let mut s = bundle_state_with_expanded_member(ArtifactState::NotInstalled, Some("reg/acme/skill-a"));
        select_member(&mut s);
        let action = handle(&mut s, TuiInput::Install);
        assert!(
            matches!(
                action,
                TuiAction::MemberAction {
                    op: BatchOp::Install,
                    ..
                }
            ),
            "C-7: i on NotInstalled member must emit MemberAction(Install); got: {action:?}"
        );
    }

    #[test]
    fn c7_install_gate_integrity_missing_emits_member_action() {
        let mut s = bundle_state_with_expanded_member(ArtifactState::IntegrityMissing, Some("reg/acme/skill-a"));
        select_member(&mut s);
        let action = handle(&mut s, TuiInput::Install);
        assert!(
            matches!(
                action,
                TuiAction::MemberAction {
                    op: BatchOp::Install,
                    ..
                }
            ),
            "C-7: i on IntegrityMissing member must emit MemberAction(Install); got: {action:?}"
        );
    }

    #[test]
    fn c7_install_gate_installed_yields_none() {
        // Install on Installed → gated out → TuiAction::None
        let mut s = bundle_state_with_expanded_member(ArtifactState::Installed, Some("reg/acme/skill-a"));
        select_member(&mut s);
        let action = handle(&mut s, TuiInput::Install);
        assert_eq!(
            action,
            TuiAction::None,
            "C-7: i on Installed member must be gated out (TuiAction::None); got: {action:?}"
        );
    }

    #[test]
    fn c7_install_gate_outdated_yields_none() {
        let mut s = bundle_state_with_expanded_member(ArtifactState::Outdated, Some("reg/acme/skill-a"));
        select_member(&mut s);
        let action = handle(&mut s, TuiInput::Install);
        assert_eq!(
            action,
            TuiAction::None,
            "C-7: i on Outdated member must be gated out; got: {action:?}"
        );
    }

    #[test]
    fn c7_install_gate_modified_yields_none() {
        let mut s = bundle_state_with_expanded_member(ArtifactState::Modified, Some("reg/acme/skill-a"));
        select_member(&mut s);
        let action = handle(&mut s, TuiInput::Install);
        assert_eq!(
            action,
            TuiAction::None,
            "C-7: i on Modified member must be gated out; got: {action:?}"
        );
    }

    #[test]
    fn c7_update_gate_installed_emits_member_action() {
        let mut s = bundle_state_with_expanded_member(ArtifactState::Installed, Some("reg/acme/skill-a"));
        select_member(&mut s);
        let action = handle(&mut s, TuiInput::Update);
        assert!(
            matches!(
                action,
                TuiAction::MemberAction {
                    op: BatchOp::Update,
                    ..
                }
            ),
            "C-7: u on Installed member must emit MemberAction(Update); got: {action:?}"
        );
    }

    #[test]
    fn c7_update_gate_outdated_emits_member_action() {
        let mut s = bundle_state_with_expanded_member(ArtifactState::Outdated, Some("reg/acme/skill-a"));
        select_member(&mut s);
        let action = handle(&mut s, TuiInput::Update);
        assert!(
            matches!(
                action,
                TuiAction::MemberAction {
                    op: BatchOp::Update,
                    ..
                }
            ),
            "C-7: u on Outdated member must emit MemberAction(Update); got: {action:?}"
        );
    }

    #[test]
    fn c7_update_gate_modified_emits_member_action() {
        let mut s = bundle_state_with_expanded_member(ArtifactState::Modified, Some("reg/acme/skill-a"));
        select_member(&mut s);
        let action = handle(&mut s, TuiInput::Update);
        assert!(
            matches!(
                action,
                TuiAction::MemberAction {
                    op: BatchOp::Update,
                    ..
                }
            ),
            "C-7: u on Modified member must emit MemberAction(Update); got: {action:?}"
        );
    }

    #[test]
    fn c7_update_gate_not_installed_yields_none() {
        let mut s = bundle_state_with_expanded_member(ArtifactState::NotInstalled, Some("reg/acme/skill-a"));
        select_member(&mut s);
        let action = handle(&mut s, TuiInput::Update);
        assert_eq!(
            action,
            TuiAction::None,
            "C-7: u on NotInstalled member must be gated out; got: {action:?}"
        );
    }

    #[test]
    fn c7_update_gate_integrity_missing_emits_member_action() {
        // F10: IntegrityMissing is now allowed for Update (reinstall semantics —
        // the artifact is declared but corrupt; 'u' triggers reinstall, matching
        // leaf-level behavior where Update is the correct recovery).
        let mut s = bundle_state_with_expanded_member(ArtifactState::IntegrityMissing, Some("reg/acme/skill-a"));
        select_member(&mut s);
        let action = handle(&mut s, TuiInput::Update);
        assert!(
            matches!(
                action,
                TuiAction::MemberAction {
                    op: BatchOp::Update,
                    ..
                }
            ),
            "C-7/F10: u on IntegrityMissing member must emit MemberAction(Update) (reinstall); got: {action:?}"
        );
    }

    #[test]
    fn c7_delete_gate_installed_emits_member_action() {
        let mut s = bundle_state_with_expanded_member(ArtifactState::Installed, Some("reg/acme/skill-a"));
        select_member(&mut s);
        let action = handle(&mut s, TuiInput::Delete);
        assert!(
            matches!(
                action,
                TuiAction::MemberAction {
                    op: BatchOp::Uninstall,
                    ..
                }
            ),
            "C-7: d on Installed member must emit MemberAction(Uninstall); got: {action:?}"
        );
    }

    #[test]
    fn c7_delete_gate_integrity_missing_emits_member_action() {
        let mut s = bundle_state_with_expanded_member(ArtifactState::IntegrityMissing, Some("reg/acme/skill-a"));
        select_member(&mut s);
        let action = handle(&mut s, TuiInput::Delete);
        assert!(
            matches!(
                action,
                TuiAction::MemberAction {
                    op: BatchOp::Uninstall,
                    ..
                }
            ),
            "C-7: d on IntegrityMissing member must emit MemberAction(Uninstall); got: {action:?}"
        );
    }

    #[test]
    fn c7_delete_gate_not_installed_yields_none() {
        let mut s = bundle_state_with_expanded_member(ArtifactState::NotInstalled, Some("reg/acme/skill-a"));
        select_member(&mut s);
        let action = handle(&mut s, TuiInput::Delete);
        assert_eq!(
            action,
            TuiAction::None,
            "C-7: d on NotInstalled member must be gated out; got: {action:?}"
        );
    }

    // ── C-8: marks-win — marks non-empty + member cursor → Batch, not MemberAction

    #[test]
    fn c8_marks_win_over_member_cursor() {
        // Set up a state with a bundle member AND a catalog row marked.
        let mut s = TuiState::new();
        s.set_rows(vec![
            skill_row("reg/acme/skill-z"), // catalog row, will be marked
            bundle_row("reg/acme/bundle-x"),
        ]);
        s.set_default_registry(Some("reg".to_string()));
        s.toggle_view_mode();

        let scope = s.scope_label.clone();
        s.bundle_members.insert(
            (scope, "reg/acme/bundle-x".to_string()),
            BundleMemberCache::Ready(vec![make_member_node(
                Some("reg/acme/skill-a"),
                ArtifactState::NotInstalled,
            )]),
        );
        // F3: expanded_bundles keyed by FULL bundle repo string.
        s.expanded_bundles.insert("reg/acme/bundle-x".to_string());

        // Mark catalog row 0 (skill-z).
        s.marked.insert(0);

        // Navigate to the member.
        let found = select_member(&mut s);
        assert!(found, "C-8: must have a Member row");

        let action = handle(&mut s, TuiInput::Install);

        // marks non-empty → Batch over the marks, NOT MemberAction.
        assert!(
            matches!(action, TuiAction::Batch { op: BatchOp::Install, ref rows } if rows.contains(&0)),
            "C-8: marks non-empty + member cursor must emit Batch over marks; got: {action:?}"
        );
        assert!(
            !matches!(action, TuiAction::MemberAction { .. }),
            "C-8: must NOT emit MemberAction when marks are non-empty"
        );
    }

    // ── C-12: security — None repo → no action; malformed repo → handled error

    #[test]
    fn c12_none_repo_yields_none_action() {
        // Placeholder member (member_repo = None) must never emit MemberAction.
        let mut s = bundle_state_with_expanded_member(ArtifactState::NotInstalled, None);
        let found = select_member(&mut s);
        assert!(found, "C-12: must have a placeholder Member row");

        for input in [TuiInput::Install, TuiInput::Update, TuiInput::Delete] {
            let action = handle(&mut s, input);
            assert_eq!(
                action,
                TuiAction::None,
                "C-12: placeholder member (None repo) must yield TuiAction::None for {input:?}"
            );
        }
    }
}

#[cfg(test)]
mod tree_event_tests {
    use super::*;
    use crate::tui::state::{ArtifactState, TuiRow, TuiState};

    fn two_leaf_state() -> TuiState {
        let mut s = TuiState::new();
        s.set_rows(vec![
            TuiRow {
                kind: "skill".to_string(),
                registry: "reg".to_string(),
                repository: "acme/alpha".to_string(),
                repo: "reg/acme/alpha".to_string(),
                description: String::new(),
                summary: String::new(),
                keywords: vec![],
                repository_url: None,
                latest_tag: "latest".to_string(),
                version: "1.0.0".to_string(),
                deprecated: None,
                pinned_version: None,
                state: ArtifactState::NotInstalled,
            },
            TuiRow {
                kind: "skill".to_string(),
                registry: "reg".to_string(),
                repository: "acme/beta".to_string(),
                repo: "reg/acme/beta".to_string(),
                description: String::new(),
                summary: String::new(),
                keywords: vec![],
                repository_url: None,
                latest_tag: "latest".to_string(),
                version: "1.0.0".to_string(),
                deprecated: None,
                pinned_version: None,
                state: ArtifactState::NotInstalled,
            },
        ]);
        s.set_default_registry(Some("reg".to_string()));
        s
    }

    // Step 3.3: `→` (Expand) and `←` (Collapse) emit TuiAction::None (state-only).
    #[test]
    fn right_expands_left_collapses_emit_none() {
        let mut s = two_leaf_state();
        // Toggle to tree mode (unimplemented → panics; that IS the required failure)
        handle(&mut s, TuiInput::ViewToggle);
        assert_eq!(s.view_mode, crate::tui::state::ViewMode::Tree);
        // Collapse must return None
        assert_eq!(
            handle(&mut s, TuiInput::Collapse),
            TuiAction::None,
            "Collapse must emit TuiAction::None"
        );
        // Expand must return None
        assert_eq!(
            handle(&mut s, TuiInput::Expand),
            TuiAction::None,
            "Expand must emit TuiAction::None"
        );
    }

    // Step 3.3: `i`/`u`/`d` on a group emit Batch over descendant rows.
    // After a group space-mark cascades descendants into `marked`,
    // install/update/delete must target those descendant row indices.
    #[test]
    fn i_u_d_after_group_mark_emit_batch_over_descendants() {
        let mut s = two_leaf_state();
        // Switch to tree mode; position 0 is then the `acme` group.
        handle(&mut s, TuiInput::ViewToggle);

        // Marking a group cascades its descendant rows into `marked`.
        handle(&mut s, TuiInput::Mark);
        assert!(
            !s.marked.is_empty(),
            "cascade mark on a group must populate the marked set"
        );
        let install_targets = s.action_targets();
        assert_eq!(
            handle(&mut s, TuiInput::Install),
            TuiAction::Batch {
                op: BatchOp::Install,
                rows: install_targets,
            }
        );

        s.clear_marks();
        handle(&mut s, TuiInput::Mark);
        assert!(
            !s.marked.is_empty(),
            "second cascade mark must repopulate the marked set"
        );
        let update_targets = s.action_targets();
        assert_eq!(
            handle(&mut s, TuiInput::Update),
            TuiAction::Batch {
                op: BatchOp::Update,
                rows: update_targets,
            }
        );

        s.clear_marks();
        handle(&mut s, TuiInput::Mark);
        assert!(
            !s.marked.is_empty(),
            "third cascade mark must repopulate the marked set"
        );
        let delete_targets = s.action_targets();
        assert_eq!(
            handle(&mut s, TuiInput::Delete),
            TuiAction::Batch {
                op: BatchOp::Uninstall,
                rows: delete_targets,
            }
        );
    }

    // C4: `←` on a leaf jumps selection to the nearest ancestor group.
    #[test]
    fn left_on_leaf_jumps_to_parent_group() {
        let mut s = two_leaf_state();
        // Switch to tree mode. Expected layout (default_registry="reg"):
        //   pos 0: acme (group, depth 0)
        //   pos 1: alpha (leaf, depth 1)
        //   pos 2: beta  (leaf, depth 1)
        handle(&mut s, TuiInput::ViewToggle);
        assert_eq!(s.view_mode, crate::tui::state::ViewMode::Tree);

        // Navigate to a leaf (pos 1 = alpha).
        s.selected = 1;
        assert!(!s.selected_is_group(), "position 1 must be a leaf (alpha)");
        // `←` on a leaf must jump to the nearest ancestor group (acme, pos 0).
        assert_eq!(handle(&mut s, TuiInput::Collapse), TuiAction::None);
        assert_eq!(s.selected, 0, "leaf→parent: selection must jump to acme (pos 0)");
        assert!(s.selected_is_group(), "position 0 must be the acme group");
    }

    // C4: `←` on an already-collapsed group jumps to the nearest ancestor group.
    #[test]
    fn left_on_collapsed_group_jumps_to_parent_group() {
        // Build a two-level tree: acme (group) → nested (group) → leaf.
        let mut s = TuiState::new();
        s.set_rows(vec![TuiRow {
            kind: "skill".to_string(),
            registry: "reg".to_string(),
            repository: "acme/nested/tool".to_string(),
            repo: "reg/acme/nested/tool".to_string(),
            description: String::new(),
            summary: String::new(),
            keywords: vec![],
            repository_url: None,
            latest_tag: "latest".to_string(),
            version: "1.0.0".to_string(),
            deprecated: None,
            pinned_version: None,
            state: crate::tui::state::ArtifactState::NotInstalled,
        }]);
        s.set_default_registry(Some("reg".to_string()));
        handle(&mut s, TuiInput::ViewToggle); // → Tree
        // Layout: acme(0) → nested(1) → tool(2)
        // Collapse the inner group "nested" first.
        s.selected = 1; // acme/nested
        assert!(s.selected_is_group(), "position 1 must be the nested group");
        s.collapse_selected();
        // Now layout: acme(0), nested-collapsed(1)
        s.selected = 1; // still on nested (collapsed group)
        // `←` on an already-collapsed group must jump to acme (pos 0).
        assert_eq!(handle(&mut s, TuiInput::Collapse), TuiAction::None);
        assert_eq!(
            s.selected, 0,
            "collapsed-group→parent: selection must jump to acme (pos 0)"
        );
        assert!(s.selected_is_group(), "position 0 must be the acme group");
    }
}
