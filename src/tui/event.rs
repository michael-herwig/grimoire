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
    /// Toggle the mark on the selected row.
    Mark,
    /// Toggle marks on all visible rows.
    MarkAll,
    /// Clear all marks.
    ClearMarks,
    /// Rebuild the catalog.
    Refresh,
    /// Quit the TUI.
    Quit,
}

/// What the app must do after a transition. `None` = state-only change.
///
/// Closed internal enum — matches stay total, no `#[non_exhaustive]`.
/// Not `Copy` (the batch variant carries a `Vec`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TuiAction {
    /// Install (`update=false`) or update (`update=true`) the given
    /// `rows` indices (the marked set, else the single selection).
    Batch { update: bool, rows: Vec<usize> },
    /// Rebuild the catalog from the registry.
    Refresh,
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
        Mode::List | Mode::Detail => handle_browse(state, input),
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
        | TuiInput::Mark
        | TuiInput::MarkAll
        | TuiInput::ClearMarks
        | TuiInput::Refresh => TuiAction::None,
    }
}

/// A batch action over the current targets (marked set, else selection).
/// `None` when there is nothing to act on.
fn batch(state: &TuiState, update: bool) -> TuiAction {
    let rows = state.action_targets();
    if rows.is_empty() {
        TuiAction::None
    } else {
        TuiAction::Batch { update, rows }
    }
}

/// List / detail keys: navigation, mode entry, and the artifact actions.
fn handle_browse(state: &mut TuiState, input: TuiInput) -> TuiAction {
    match input {
        TuiInput::Up => {
            state.move_selection(-1);
            TuiAction::None
        }
        TuiInput::Down => {
            state.move_selection(1);
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
        TuiInput::Char('i') | TuiInput::Install => batch(state, false),
        TuiInput::Char('u') | TuiInput::Update => batch(state, true),
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
            keywords: vec!["kw".to_string()],
            latest_tag: "latest".to_string(),
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
                update: false,
                rows: vec![1]
            }
        );
        assert_eq!(
            handle(&mut s, TuiInput::Install),
            TuiAction::Batch {
                update: false,
                rows: vec![1]
            }
        );
        assert_eq!(
            handle(&mut s, TuiInput::Update),
            TuiAction::Batch {
                update: true,
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
                update: false,
                rows: vec![0, 2]
            }
        );
        // Clear marks ⇒ falls back to the single selection (row 2).
        handle(&mut s, TuiInput::ClearMarks);
        assert_eq!(
            handle(&mut s, TuiInput::Update),
            TuiAction::Batch {
                update: true,
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
                update: false,
                rows: vec![0, 1, 2]
            }
        );
        handle(&mut s, TuiInput::MarkAll); // all marked ⇒ clears
        assert!(s.marked.is_empty());
    }

    #[test]
    fn install_without_selection_is_inert() {
        let mut s = TuiState::new();
        s.set_rows(vec![]);
        assert_eq!(handle(&mut s, TuiInput::Install), TuiAction::None);
        assert_eq!(handle(&mut s, TuiInput::Update), TuiAction::None);
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
        // 'i' in search mode is a literal character, NOT install.
        assert_eq!(handle(&mut s, TuiInput::Char('i')), TuiAction::None);
        assert_eq!(s.query, "i");
        // No repo contains 'i' here ⇒ empty filter.
        assert!(s.filtered.is_empty());
        // Backspace clears it.
        handle(&mut s, TuiInput::Backspace);
        assert_eq!(s.query, "");
        assert_eq!(s.filtered.len(), 3);
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
}
