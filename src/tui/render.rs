// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! The render projection.
//!
//! [`frame`] is a *pure* function turning a [`TuiState`] into a plain
//! [`RenderModel`] (a description of what to draw — no ratatui types, no
//! decisions). [`draw`] is the only ratatui-aware code: it lays the model
//! out into widgets with zero logic of its own. Splitting the projection
//! out keeps the decision surface (state/event) headlessly testable and
//! makes the ratatui code a trivial, decision-free sink.

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};

use super::state::{ArtifactState, Mode, TuiState};

/// A pure, ratatui-free color tag for a status cell. [`draw`] maps it to a
/// concrete ratatui [`Color`]; keeping it abstract preserves the headless
/// testability of [`frame`].
///
/// Closed internal enum — matches stay total, no `#[non_exhaustive]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorKey {
    /// Installed and intact.
    Installed,
    /// Not present in this scope.
    NotInstalled,
    /// A newer pin is locked than what is on disk.
    Outdated,
    /// On-disk content drifted from the recorded hash.
    Modified,
    /// Recorded but outputs missing/unreadable.
    IntegrityMissing,
}

/// Glyph + label + color for an [`ArtifactState`], as projected for
/// display. Plain Unicode (no font dependency).
fn status_view(state: ArtifactState) -> (&'static str, &'static str, ColorKey) {
    match state {
        ArtifactState::Installed => ("✓", "installed", ColorKey::Installed),
        ArtifactState::NotInstalled => ("·", "not-installed", ColorKey::NotInstalled),
        ArtifactState::Outdated => ("↑", "outdated", ColorKey::Outdated),
        ArtifactState::Modified => ("✱", "modified", ColorKey::Modified),
        ArtifactState::IntegrityMissing => ("⚠", "integrity-missing", ColorKey::IntegrityMissing),
    }
}

/// Glyph for an artifact kind (`skill` / `rule`, else a neutral dot).
fn kind_glyph(kind: &str) -> &'static str {
    match kind {
        "skill" => "◆",
        "rule" => "▸",
        _ => "•",
    }
}

/// One table row in the render model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderRow {
    /// The visible columns, already formatted (kind, repo, tag, status).
    pub columns: [String; 4],
    /// Whether this row is the current selection.
    pub selected: bool,
    /// Whether this row is marked for a batch action.
    pub marked: bool,
    /// The color the status cell should render in.
    pub status_color: ColorKey,
}

/// A plain, ratatui-free description of the whole screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderModel {
    /// The title bar text.
    pub title: String,
    /// The search input line (e.g. `Search: rust`).
    pub search: String,
    /// Static column headers for the list.
    pub headers: [&'static str; 4],
    /// The visible (filtered) rows.
    pub rows: Vec<RenderRow>,
    /// The detail-pane text for the selected row (multi-line).
    pub detail: String,
    /// The bottom status / hint line.
    pub status: String,
    /// The one-line glyph legend (what each status symbol means).
    pub legend: String,
    /// Whether the detail pane is the focused element.
    pub detail_focused: bool,
}

/// Project `state` into a [`RenderModel`]. Pure — no I/O, no ratatui.
pub fn frame(state: &TuiState) -> RenderModel {
    let title = if state.offline {
        "grim — catalog [offline]".to_string()
    } else {
        "grim — catalog".to_string()
    };

    let search = match state.mode {
        Mode::Search => format!("Search: {}_", state.query),
        _ => format!("Search: {}", state.query),
    };

    let rows: Vec<RenderRow> = state
        .filtered
        .iter()
        .enumerate()
        .filter_map(|(pos, &i)| state.rows.get(i).map(|r| (pos, i, r)))
        .map(|(pos, i, r)| {
            let (glyph, label, color) = status_view(r.state);
            RenderRow {
                columns: [
                    format!("{} {}", kind_glyph(&r.kind), r.kind),
                    r.repo.clone(),
                    r.latest_tag.clone(),
                    format!("{glyph} {label}"),
                ],
                selected: pos == state.selected,
                marked: state.is_row_marked(i),
                status_color: color,
            }
        })
        .collect();

    let detail = match state.selected_row() {
        Some(r) => {
            let kw = if r.keywords.is_empty() {
                "-".to_string()
            } else {
                r.keywords.join(", ")
            };
            format!(
                "{}\n\n{}\n\nkeywords: {}\ntag: {}\nstatus: {}",
                r.repo,
                if r.description.is_empty() { "-" } else { &r.description },
                kw,
                if r.latest_tag.is_empty() { "-" } else { &r.latest_tag },
                r.state
            )
        }
        None => "no selection".to_string(),
    };

    let status = if !state.status_line.is_empty() {
        state.status_line.clone()
    } else if state.loading {
        "loading catalog…".to_string()
    } else if state.marked.is_empty() {
        "↑/↓ move  space mark  / search  i install  u update  r refresh  q quit".to_string()
    } else {
        format!(
            "{} marked  i install  u update  a all  c clear  q quit",
            state.marked.len()
        )
    };

    RenderModel {
        title,
        search,
        headers: ["Kind", "Repo", "Tag", "Status"],
        rows,
        detail,
        status,
        legend: "✓ installed   ↑ outdated   ✱ modified   ⚠ integrity-missing   · not-installed".to_string(),
        detail_focused: state.mode == Mode::Detail,
    }
}

/// Draw `model` into the frame. The *only* ratatui-specific code; it makes
/// no decisions — every choice was already made in [`frame`].
pub fn draw(f: &mut Frame, model: &RenderModel) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // title
            Constraint::Length(3), // search box
            Constraint::Min(3),    // list
            Constraint::Length(8), // detail pane
            Constraint::Length(1), // legend
            Constraint::Length(1), // status
        ])
        .split(f.area());

    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            model.title.clone(),
            Style::default().add_modifier(Modifier::BOLD),
        ))),
        chunks[0],
    );

    f.render_widget(
        Paragraph::new(model.search.clone()).block(Block::default().borders(Borders::ALL)),
        chunks[1],
    );

    let header = ListItem::new(Line::from(Span::styled(
        format!(
            "   {:<8}  {:<40}  {:<10}  {}",
            model.headers[0], model.headers[1], model.headers[2], model.headers[3]
        ),
        Style::default().add_modifier(Modifier::BOLD),
    )));
    let mut items: Vec<ListItem> = vec![header];
    let mut selected_index: Option<usize> = None;
    for (idx, r) in model.rows.iter().enumerate() {
        if r.selected {
            selected_index = Some(idx + 1); // +1 for the header row
        }
        // A leading mark cell, then a plain prefix; the status cell
        // carries its own state color so a glance reads the catalog
        // without entering detail.
        let mark = if r.marked { " ▣ " } else { "   " };
        let line = Line::from(vec![
            Span::styled(
                mark.to_string(),
                Style::default().fg(if r.marked { Color::Cyan } else { Color::Reset }),
            ),
            Span::raw(format!(
                "{:<8}  {:<40}  {:<10}  ",
                r.columns[0], r.columns[1], r.columns[2]
            )),
            Span::styled(r.columns[3].clone(), Style::default().fg(color_for(r.status_color))),
        ]);
        items.push(ListItem::new(line));
    }
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title("Catalog"))
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    let mut list_state = ListState::default();
    list_state.select(selected_index);
    f.render_stateful_widget(list, chunks[2], &mut list_state);

    let detail_block = Block::default().borders(Borders::ALL).title("Detail");
    let detail_block = if model.detail_focused {
        detail_block.border_style(Style::default().add_modifier(Modifier::BOLD))
    } else {
        detail_block
    };
    f.render_widget(Paragraph::new(model.detail.clone()).block(detail_block), chunks[3]);

    f.render_widget(
        Paragraph::new(Span::styled(
            model.legend.clone(),
            Style::default().add_modifier(Modifier::DIM),
        )),
        chunks[4],
    );

    f.render_widget(Paragraph::new(model.status.clone()), chunks[5]);
}

/// Map a pure [`ColorKey`] to a concrete ratatui [`Color`]. Named ANSI
/// colors only (not 256/RGB) so they remain legible on any terminal
/// theme; the glyph stays the primary signal regardless of palette.
fn color_for(key: ColorKey) -> Color {
    match key {
        ColorKey::Installed => Color::Green,
        ColorKey::NotInstalled => Color::DarkGray,
        ColorKey::Outdated => Color::Yellow,
        ColorKey::Modified => Color::Red,
        ColorKey::IntegrityMissing => Color::Magenta,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::state::{ArtifactState, TuiRow};

    fn row(repo: &str, state: ArtifactState) -> TuiRow {
        TuiRow {
            kind: "skill".to_string(),
            repo: repo.to_string(),
            description: "review code".to_string(),
            keywords: vec!["rust".to_string(), "lint".to_string()],
            latest_tag: "latest".to_string(),
            state,
        }
    }

    #[test]
    fn frame_projects_known_state_snapshot() {
        let mut s = TuiState::new();
        s.set_rows(vec![
            row("r/alpha", ArtifactState::Installed),
            row("r/beta", ArtifactState::NotInstalled),
        ]);
        let m = frame(&s);
        assert_eq!(m.title, "grim — catalog");
        assert_eq!(m.search, "Search: ");
        assert_eq!(m.headers, ["Kind", "Repo", "Tag", "Status"]);
        assert_eq!(m.rows.len(), 2);
        assert_eq!(
            m.rows[0].columns,
            [
                "◆ skill".to_string(),
                "r/alpha".to_string(),
                "latest".to_string(),
                "✓ installed".to_string()
            ]
        );
        assert_eq!(m.rows[0].status_color, ColorKey::Installed);
        assert_eq!(m.rows[1].status_color, ColorKey::NotInstalled);
        assert!(m.rows[0].selected, "first row selected by default");
        assert!(!m.rows[1].selected);
        assert!(m.detail.contains("r/alpha"));
        assert!(m.detail.contains("keywords: rust, lint"));
        assert!(m.detail.contains("status: installed"));
        assert!(!m.detail_focused);
        assert!(m.status.contains("quit"));
        assert!(m.legend.contains("integrity-missing"));
    }

    #[test]
    fn status_view_maps_every_state() {
        for (st, glyph, label, color) in [
            (ArtifactState::Installed, "✓", "installed", ColorKey::Installed),
            (
                ArtifactState::NotInstalled,
                "·",
                "not-installed",
                ColorKey::NotInstalled,
            ),
            (ArtifactState::Outdated, "↑", "outdated", ColorKey::Outdated),
            (ArtifactState::Modified, "✱", "modified", ColorKey::Modified),
            (
                ArtifactState::IntegrityMissing,
                "⚠",
                "integrity-missing",
                ColorKey::IntegrityMissing,
            ),
        ] {
            assert_eq!(status_view(st), (glyph, label, color));
        }
        assert_eq!(kind_glyph("skill"), "◆");
        assert_eq!(kind_glyph("rule"), "▸");
        assert_eq!(kind_glyph("-"), "•");
    }

    #[test]
    fn frame_marks_offline_and_loading() {
        let mut s = TuiState::new();
        s.set_offline(true);
        assert!(s.loading);
        let m = frame(&s);
        assert_eq!(m.title, "grim — catalog [offline]");
        assert_eq!(m.status, "loading catalog…");
        assert!(m.rows.is_empty());
        assert_eq!(m.detail, "no selection");
    }

    #[test]
    fn frame_search_mode_shows_cursor_and_focus() {
        let mut s = TuiState::new();
        s.set_rows(vec![row("r/alpha", ArtifactState::Installed)]);
        s.enter_search();
        s.apply_query("al");
        let m = frame(&s);
        assert_eq!(m.search, "Search: al_");
        s.back();
        s.enter_detail();
        let m2 = frame(&s);
        assert!(m2.detail_focused);
    }

    #[test]
    fn frame_status_line_overrides_hint() {
        let mut s = TuiState::new();
        s.set_rows(vec![row("r/a", ArtifactState::Installed)]);
        s.set_status("installed r/a");
        assert_eq!(frame(&s).status, "installed r/a");
    }
}
