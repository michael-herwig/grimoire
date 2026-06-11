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

use super::state::{Mode, TuiState};

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
    /// Rebuild the catalog from the registry.
    Refresh,
    /// Toggle the active scope (Global ⇄ Project) and recompute states.
    ToggleScope,
    /// Lazily fetch the tag list for `row` and feed it to the open picker.
    LoadVersions { row: usize },
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

/// Help-overlay keys: `q` quits, anything else dismisses back to the list.
fn handle_help(state: &mut TuiState, input: TuiInput) -> TuiAction {
    match input {
        TuiInput::Char('q') | TuiInput::Quit => TuiAction::Quit,
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
/// `↑`/`↓` are mode-sensitive: they move the selection in the list but
/// scroll the pane while the detail view is open (`j`/`k` alias the
/// scroll there; both stay literal query chars in search).
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
        TuiInput::Char('j') | TuiInput::Char('k') if state.mode == Mode::Detail => {
            state.scroll_detail(if input == TuiInput::Char('j') { 1 } else { -1 });
            TuiAction::None
        }
        TuiInput::Enter => {
            state.enter_detail();
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
        TuiInput::Char('i') | TuiInput::Install => batch(state, BatchOp::Install),
        TuiInput::Char('u') | TuiInput::Update => batch(state, BatchOp::Update),
        TuiInput::Char('d') | TuiInput::Delete => batch(state, BatchOp::Uninstall),
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
        TuiRow {
            kind: "skill".to_string(),
            repo: repo.to_string(),
            description: "d".to_string(),
            summary: String::new(),
            keywords: vec!["kw".to_string()],
            repository_url: None,
            latest_tag: "latest".to_string(),
            version: "1.0.0".to_string(),
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
    fn help_overlay_opens_and_any_key_closes() {
        let mut s = seeded();
        assert_eq!(handle(&mut s, TuiInput::Char('?')), TuiAction::None);
        assert_eq!(s.mode, Mode::Help);
        // Any non-quit key dismisses back to the list.
        assert_eq!(handle(&mut s, TuiInput::Down), TuiAction::None);
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

    #[test]
    fn enter_in_browse_always_opens_detail() {
        // With the tree removed there is no group branch — Enter always
        // opens the detail pane for the selected row.
        let mut s = seeded();
        s.move_selection(1);
        assert_eq!(handle(&mut s, TuiInput::Enter), TuiAction::None);
        assert_eq!(s.mode, Mode::Detail);
        assert_eq!(s.selected_row().unwrap().repo, "r/b");
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
        // Back in the list, arrows move the selection again.
        handle(&mut s, TuiInput::Esc);
        handle(&mut s, TuiInput::Down);
        assert_eq!(s.selected, 1);
        // j/k stay inert in list mode (no vim-navigation surprise).
        assert_eq!(handle(&mut s, TuiInput::Char('j')), TuiAction::None);
        assert_eq!(s.selected, 1);
        assert_eq!(s.detail_scroll, 0);
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

    #[test]
    fn t_is_inert_in_browse_and_literal_in_search() {
        let mut s = seeded();
        // `t` no longer toggles anything in list mode (tree removed).
        assert_eq!(handle(&mut s, TuiInput::Char('t')), TuiAction::None);
        assert_eq!(s.mode, Mode::List);
        // In search it is just a literal query char.
        handle(&mut s, TuiInput::Char('/'));
        assert_eq!(handle(&mut s, TuiInput::Char('t')), TuiAction::None);
        assert_eq!(s.query, "t");
    }
}
