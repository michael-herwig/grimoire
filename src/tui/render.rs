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
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};

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
        ArtifactState::IntegrityMissing => ("✘", "integrity-missing", ColorKey::IntegrityMissing),
    }
}

/// Column widths (chars) — the projection pads/truncates to these so the
/// table aligns regardless of how long an identifier is.
const W_KIND: usize = 8;
const W_REPO: usize = 46;
const W_TAG: usize = 12;
/// Status column width — wide enough for the longest label
/// (`✘ integrity-missing`, 19 chars) so the header underline spans the
/// full column instead of stopping at `Status`.
const W_STATUS: usize = 19;
/// Total terminal columns the Catalog needs to show every fixed-width
/// column un-truncated: 2 (mark) + repo + 2 + kind + 2 + tag + 2 + status,
/// plus 2 block borders. Selection is shown by row highlight (no leading
/// symbol). Sized to exactly this side-by-side so Detail gets all slack.
const CATALOG_WIDTH: u16 = (2 + W_REPO + 2 + W_KIND + 2 + W_TAG + 2 + W_STATUS) as u16 + 2 /* borders */;

/// Truncate `s` to `width` *display chars* (ellipsis on overflow) then
/// left-pad to exactly `width`, so every cell is the same width and the
/// table never skews on a long repository path.
fn fit(s: &str, width: usize) -> String {
    let n = s.chars().count();
    if n > width {
        let keep: String = s.chars().take(width.saturating_sub(1)).collect();
        format!("{keep}…")
    } else {
        format!("{s:<width$}")
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

/// The modal version-picker overlay, projected for display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickerView {
    /// The popup title (e.g. `Versions — r/alpha`).
    pub title: String,
    /// Whether the tag list is still loading.
    pub loading: bool,
    /// The orderable tag list (highest version near the top).
    pub tags: Vec<String>,
    /// Selection index into `tags`.
    pub selected: usize,
    /// The row's currently-pinned tag, marked in the list when present.
    pub pinned: Option<String>,
}

/// A plain, ratatui-free description of the whole screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderModel {
    /// The title bar text.
    pub title: String,
    /// The search input line (the query, or a placeholder hint).
    pub search: String,
    /// Whether `search` is the grayed-out placeholder (no query yet, not
    /// editing) rather than a real query.
    pub search_placeholder: bool,
    /// The active scope label (`project` / `global`), shown in its own
    /// box beside the search; empty when no scope is resolvable.
    pub scope: String,
    /// The active scope's effective selected clients, pre-joined as
    /// `clients: a, b` for the legend line; empty when none are selected
    /// (the span is then omitted entirely).
    pub clients: String,
    /// Static column headers for the list.
    pub headers: [&'static str; 4],
    /// The visible (filtered) rows.
    pub rows: Vec<RenderRow>,
    /// The detail-pane text for the selected row (multi-line).
    pub detail: String,
    /// The bottom status line — transient only (loading, counts, batch
    /// results, marked-set actions). Empty when idle.
    pub status: String,
    /// The persistent compact keybinding summary (the widest tier),
    /// right-aligned on the legend line so `? help` is always visible (it
    /// no longer lives in `status`, which transient messages overwrite).
    pub hint: String,
    /// Hint variants widest → narrowest. [`draw`] picks the widest tier
    /// that fits the current terminal width, degrading down to `? help`
    /// on a very narrow terminal. Pure data — the width fit is mechanical.
    pub hint_tiers: Vec<String>,
    /// The one-line glyph legend (what each status symbol means).
    pub legend: String,
    /// Whether the detail pane is the focused element.
    pub detail_focused: bool,
    /// Whether the help overlay is showing.
    pub show_help: bool,
    /// The version-picker overlay, when [`Mode::VersionPick`].
    pub picker: Option<PickerView>,
}

/// Pick the widest hint tier whose text (plus a one-cell right margin)
/// fits in `avail` columns. Falls back to the narrowest tier (`? help`)
/// when even that does not fit, so help stays discoverable at any width.
fn fit_hint(tiers: &[String], avail: usize) -> String {
    tiers
        .iter()
        // `count + 1 <= avail` (text + one-cell right margin), simplified.
        .find(|h| h.chars().count() < avail)
        .or_else(|| tiers.last())
        .cloned()
        .unwrap_or_default()
}

/// Drop a leading `default_registry/` from a reference for display only
/// (the stored `repo` keeps the full reference for search and actions).
fn strip_default_registry<'a>(repo: &'a str, default_registry: Option<&str>) -> &'a str {
    if let Some(reg) = default_registry
        && let Some(rest) = repo.strip_prefix(reg)
        && let Some(rest) = rest.strip_prefix('/')
    {
        return rest;
    }
    repo
}

/// Build the visible cells for one catalog row. `repo_text` is the
/// Repo-column content (the full reference, default registry elided).
fn render_leaf(r: &super::state::TuiRow, repo_text: &str, selected: bool, marked: bool) -> RenderRow {
    let (glyph, label, color) = status_view(r.state);
    // A user-pinned version shows with a leading `*`; otherwise the
    // explicit highest version, falling back to the tag.
    let tag_cell = match &r.pinned_version {
        Some(p) => format!("*{p}"),
        None if !r.version.is_empty() => r.version.clone(),
        None => r.latest_tag.clone(),
    };
    RenderRow {
        columns: [
            fit(repo_text, W_REPO),
            fit(&r.kind, W_KIND),
            fit(&tag_cell, W_TAG),
            format!("{glyph} {label}"),
        ],
        selected,
        marked,
        status_color: color,
    }
}

/// Project `state` into a [`RenderModel`]. Pure — no I/O, no ratatui.
pub fn frame(state: &TuiState) -> RenderModel {
    let title = if state.offline {
        "Grimoire [offline]".to_string()
    } else {
        "Grimoire".to_string()
    };

    // Search shows the live query (cursor `_` while editing); when there
    // is no query and we are not editing, a grayed placeholder advertises
    // the `/` shortcut so the box never looks dead.
    let (search, search_placeholder) = if state.mode == Mode::Search {
        (format!("{}_", state.query), false)
    } else if state.query.is_empty() {
        ("type / to search".to_string(), true)
    } else {
        (state.query.clone(), false)
    };

    let rows: Vec<RenderRow> = state
        .filtered
        .iter()
        .enumerate()
        .filter_map(|(pos, &i)| state.rows.get(i).map(|r| (pos, i, r)))
        .map(|(pos, i, r)| {
            let shown = strip_default_registry(&r.repo, state.default_registry.as_deref());
            render_leaf(r, shown, pos == state.selected, state.is_row_marked(i))
        })
        .collect();

    let detail = match state.selected_row() {
        Some(r) => {
            let kw = if r.keywords.is_empty() {
                "-".to_string()
            } else {
                r.keywords.join(", ")
            };
            let version = if !r.version.is_empty() {
                r.version.as_str()
            } else if !r.latest_tag.is_empty() {
                r.latest_tag.as_str()
            } else {
                "-"
            };
            let pinned = match &r.pinned_version {
                Some(p) => format!("\npinned: {p}"),
                None => String::new(),
            };
            // Short blurb above the full description, only when present
            // (keeps the layout — and snapshot tests — unchanged for
            // entries without a summary).
            let summary = if r.summary.is_empty() {
                String::new()
            } else {
                format!("summary: {}\n\n", r.summary)
            };
            format!(
                "{}\n\n{}{}\n\nkeywords: {}\nversion: {}{}\nstatus: {}",
                r.repo,
                summary,
                if r.description.is_empty() { "-" } else { &r.description },
                kw,
                version,
                pinned,
                r.state
            )
        }
        None => "no selection".to_string(),
    };

    // Status is transient only — loading / counts / batch results, or the
    // marked-set action keys (contextual). The always-on key summary lives
    // in `hint` so a transient message can never hide `? help`.
    let status = if !state.status_line.is_empty() {
        state.status_line.clone()
    } else if state.loading {
        "loading catalog…".to_string()
    } else if state.marked.is_empty() {
        String::new()
    } else {
        format!(
            "{} marked — i install · u update · d delete · a all · c clear",
            state.marked.len()
        )
    };

    // Widest → narrowest. `draw` picks the widest that fits; the last
    // (`? help`) is the irreducible minimum so help is always discoverable.
    let hint_tiers = vec![
        "↑↓ move · space mark · i/u/d act · v versions · g scope · / search · ? help · q quit".to_string(),
        "↑↓ move · i/u/d act · v ver · g scope · / search · ? help · q quit".to_string(),
        "↑↓ · i/u/d · v · g · / · ? help · q".to_string(),
        "i/u/d v g / ? q".to_string(),
        "? help".to_string(),
    ];
    let hint = hint_tiers[0].clone();

    let picker = state.picker.as_ref().map(|p| {
        let repo = state.rows.get(p.row).map(|r| r.repo.clone()).unwrap_or_default();
        PickerView {
            title: format!("Versions — {repo}"),
            loading: p.loading,
            tags: p.tags.clone(),
            selected: p.selected,
            pinned: state.rows.get(p.row).and_then(|r| r.pinned_version.clone()),
        }
    });

    // Selected clients render as a quiet span on the legend line; empty
    // selection omits the span (no stray `clients:` label).
    let clients = if state.clients.is_empty() {
        String::new()
    } else {
        format!("clients: {}", state.clients.join(", "))
    };

    RenderModel {
        title,
        search,
        search_placeholder,
        scope: state.scope_label.clone(),
        clients,
        headers: ["Repo", "Kind", "Tag", "Status"],
        rows,
        detail,
        status,
        hint,
        hint_tiers,
        legend: "✓ installed   ↑ outdated   ✱ modified   ✘ integrity-missing   · not-installed".to_string(),
        detail_focused: state.mode == Mode::Detail,
        show_help: state.mode == Mode::Help,
        picker,
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
            Constraint::Min(3),    // content (list [+ detail])
            Constraint::Length(1), // legend
        ])
        .split(f.area());

    // Responsive content split: on a wide terminal the Detail pane sits to
    // the right of the Catalog (taller, more readable); on a narrow one it
    // falls back to a short band below it.
    // Side-by-side once there is room for the full Catalog plus a usable
    // Detail column; the Catalog takes exactly its natural width and
    // Detail absorbs all remaining space.
    const DETAIL_MIN_WIDTH: u16 = 30;
    let (list_area, detail_area) = if chunks[2].width >= CATALOG_WIDTH + DETAIL_MIN_WIDTH {
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(CATALOG_WIDTH), Constraint::Min(DETAIL_MIN_WIDTH)])
            .split(chunks[2]);
        (cols[0], cols[1])
    } else {
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(3), Constraint::Length(8)])
            .split(chunks[2]);
        (rows[0], rows[1])
    };
    let legend_chunk = chunks[3];

    let accent = Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD);

    // Title row: app title left, transient status right-aligned on the
    // same line (it used to own a dedicated bottom row).
    // Title centered across the whole line; transient status right-aligned
    // over the same row (short, so it never collides with the centered
    // title in practice).
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            model.title.clone(),
            Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD),
        )))
        .alignment(Alignment::Center),
        chunks[0],
    );
    f.render_widget(
        Paragraph::new(Span::styled(
            model.status.clone(),
            Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
        ))
        .alignment(Alignment::Right),
        chunks[0],
    );
    // Selected clients: a quiet, persistent span on the left of the title
    // row (the title is centered, so the left edge is free). Omitted
    // entirely when no clients are selected.
    if !model.clients.is_empty() {
        f.render_widget(
            Paragraph::new(Span::styled(
                model.clients.clone(),
                Style::default().fg(Color::DarkGray),
            ))
            .alignment(Alignment::Left),
            chunks[0],
        );
    }

    // Search row: scope-mode box on the left, query box on the right.
    let search_row = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(26), Constraint::Min(20)])
        .split(chunks[1]);

    // Scope box: just the active mode in caps. The toggle key lives in
    // the legend hint and the help overlay, not here.
    let (scope_text, scope_color) = match model.scope.as_str() {
        "project" => ("PROJECT MODE", Color::Green),
        "global" => ("GLOBAL MODE", Color::Magenta),
        _ => ("— no scope —", Color::DarkGray),
    };
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            scope_text,
            Style::default().fg(scope_color).add_modifier(Modifier::BOLD),
        )))
        .alignment(Alignment::Center)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(scope_color))
                .title(Span::styled("Scope", accent)),
        ),
        search_row[0],
    );

    let search_style = if model.search_placeholder {
        Style::default().fg(Color::DarkGray).add_modifier(Modifier::ITALIC)
    } else {
        Style::default().fg(Color::White)
    };
    f.render_widget(
        Paragraph::new(Span::styled(model.search.clone(), search_style)).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Blue))
                .title(Span::styled("Search", accent)),
        ),
        search_row[1],
    );

    let header = ListItem::new(Line::from(Span::styled(
        format!(
            "  {:<rw$}  {:<kw$}  {:<tw$}  {:<sw$}",
            model.headers[0],
            model.headers[1],
            model.headers[2],
            model.headers[3],
            rw = W_REPO,
            kw = W_KIND,
            tw = W_TAG,
            sw = W_STATUS,
        ),
        accent.add_modifier(Modifier::UNDERLINED),
    )));
    let mut items: Vec<ListItem> = vec![header];
    let mut selected_index: Option<usize> = None;
    for (idx, r) in model.rows.iter().enumerate() {
        if r.selected {
            selected_index = Some(idx + 1); // +1 for the header row
        }
        // Every cell is its own colored span; columns are already
        // fixed-width from `fit()` so the table never skews.
        let line = Line::from(vec![
            Span::styled(
                if r.marked { "▣ " } else { "  " }.to_string(),
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!("{}  ", r.columns[0]), Style::default().fg(Color::White)),
            Span::styled(
                format!("{}  ", r.columns[1]),
                Style::default().fg(Color::Blue).add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!("{}  ", r.columns[2]), Style::default().fg(Color::Yellow)),
            Span::styled(
                r.columns[3].clone(),
                Style::default()
                    .fg(color_for(r.status_color))
                    .add_modifier(Modifier::BOLD),
            ),
        ]);
        items.push(ListItem::new(line));
    }
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Blue))
                .title(Span::styled("Catalog", accent)),
        )
        .highlight_symbol("")
        .highlight_style(
            Style::default()
                .bg(Color::Indexed(236))
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        );
    let mut list_state = ListState::default();
    list_state.select(selected_index);
    f.render_stateful_widget(list, list_area, &mut list_state);

    let detail_block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(if model.detail_focused { Color::Cyan } else { Color::Blue }))
        .title(Span::styled("Detail", accent));
    f.render_widget(
        Paragraph::new(Span::styled(model.detail.clone(), Style::default().fg(Color::White)))
            .block(detail_block)
            .wrap(Wrap { trim: false }),
        detail_area,
    );

    // Legend left, persistent key summary right — same line, so `? help`
    // is always on screen. Width-responsive: pick the widest hint tier
    // that fits; if the glyph legend then has no room, drop it and give
    // the whole line to the hint (still degrading down to `? help`).
    let avail = legend_chunk.width as usize;
    let hint = fit_hint(&model.hint_tiers, avail);
    let hint_w = hint.chars().count() as u16;
    let legend_w = model.legend.chars().count();
    if legend_w + 2 + hint.chars().count() <= avail {
        let legend_row = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Min(10), Constraint::Length(hint_w + 1)])
            .split(legend_chunk);
        f.render_widget(Paragraph::new(legend_line()), legend_row[0]);
        f.render_widget(
            Paragraph::new(Span::styled(hint, Style::default().fg(Color::DarkGray))).alignment(Alignment::Right),
            legend_row[1],
        );
    } else {
        // Too narrow for the glyph legend — keep only the key hint.
        f.render_widget(
            Paragraph::new(Span::styled(hint, Style::default().fg(Color::DarkGray))).alignment(Alignment::Right),
            legend_chunk,
        );
    }

    if model.show_help {
        draw_help(f);
    }
    if let Some(p) = &model.picker {
        draw_picker(f, p);
    }
}

/// A centered version-picker popup: the tag list (highest version near the
/// top), the current pin marked, the selection highlighted.
fn draw_picker(f: &mut Frame, p: &PickerView) {
    let body: Vec<ListItem> = if p.loading {
        vec![ListItem::new(Line::from(Span::styled(
            "  loading versions…",
            Style::default().fg(Color::DarkGray).add_modifier(Modifier::ITALIC),
        )))]
    } else {
        p.tags
            .iter()
            .enumerate()
            .map(|(i, t)| {
                let is_pinned = p.pinned.as_deref() == Some(t.as_str());
                let mark = if is_pinned { "● " } else { "  " };
                let style = if i == p.selected {
                    Style::default()
                        .bg(Color::Indexed(236))
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD)
                } else if is_pinned {
                    Style::default().fg(Color::Green)
                } else {
                    Style::default().fg(Color::White)
                };
                ListItem::new(Line::from(Span::styled(format!("{mark}{t}"), style)))
            })
            .collect()
    };
    let area = centered_rect(50, 60, f.area());
    f.render_widget(Clear, area);
    f.render_widget(
        List::new(body).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan))
                .title(Span::styled(
                    format!(" {} ", p.title),
                    Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD),
                )),
        ),
        area,
    );
    // One-line footer hint inside the popup's bottom border region.
    let hint_area = Rect {
        x: area.x + 2,
        y: area.y + area.height.saturating_sub(1),
        width: area.width.saturating_sub(4),
        height: 1,
    };
    f.render_widget(
        Paragraph::new(Span::styled(
            "↑↓ select · enter pin · esc cancel",
            Style::default().fg(Color::DarkGray),
        )),
        hint_area,
    );
}

/// The status-glyph legend as colored spans (each glyph in its state
/// color), so the legend itself demonstrates the palette.
fn legend_line() -> Line<'static> {
    let pairs = [
        ("✓ installed", ColorKey::Installed),
        ("  ↑ outdated", ColorKey::Outdated),
        ("  ✱ modified", ColorKey::Modified),
        ("  ✘ integrity-missing", ColorKey::IntegrityMissing),
        ("  · not-installed", ColorKey::NotInstalled),
    ];
    Line::from(
        pairs
            .into_iter()
            .map(|(t, k)| Span::styled(t.to_string(), Style::default().fg(color_for(k))))
            .collect::<Vec<_>>(),
    )
}

/// A centered help overlay listing every keybinding.
fn draw_help(f: &mut Frame) {
    let rows = [
        ("↑ / ↓", "move selection"),
        ("space", "mark / unmark the row"),
        ("a / c", "mark all visible / clear marks"),
        ("i / u / d", "install / update / uninstall (marked set or selection)"),
        ("v", "pick a specific version for the selected row"),
        ("g", "toggle scope: project ⇄ global"),
        ("/", "search; type to filter, enter to commit"),
        ("enter", "open the detail pane"),
        ("r", "refresh the catalog from the registry"),
        ("? ", "this help (any key closes)"),
        ("q / esc", "quit"),
    ];
    let mut lines: Vec<Line> = vec![
        Line::from(Span::styled(
            "Keybindings",
            Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
    ];
    for (k, d) in rows {
        lines.push(Line::from(vec![
            Span::styled(
                format!("  {k:<10}"),
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            ),
            Span::styled(d.to_string(), Style::default().fg(Color::White)),
        ]));
    }
    let area = centered_rect(60, 50, f.area());
    f.render_widget(Clear, area);
    f.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan))
                .title(Span::styled(
                    " help ",
                    Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD),
                )),
        ),
        area,
    );
}

/// A `pct_x` × `pct_y` percent rectangle centered in `area`.
fn centered_rect(pct_x: u16, pct_y: u16, area: Rect) -> Rect {
    let vert = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - pct_y) / 2),
            Constraint::Percentage(pct_y),
            Constraint::Percentage((100 - pct_y) / 2),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - pct_x) / 2),
            Constraint::Percentage(pct_x),
            Constraint::Percentage((100 - pct_x) / 2),
        ])
        .split(vert[1])[1]
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
            summary: String::new(),
            keywords: vec!["rust".to_string(), "lint".to_string()],
            latest_tag: "latest".to_string(),
            version: "2.1.0".to_string(),
            pinned_version: None,
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
        assert_eq!(m.title, "Grimoire");
        assert_eq!(m.search, "type / to search");
        assert!(m.search_placeholder);
        assert_eq!(m.scope, "");
        assert_eq!(m.headers, ["Repo", "Kind", "Tag", "Status"]);
        assert_eq!(m.rows.len(), 2);
        // Columns are fixed-width (padded/truncated by `fit`) so the
        // table aligns; status keeps its glyph+label verbatim. Repo is
        // the first column, kind second.
        assert_eq!(m.rows[0].columns[0], fit("r/alpha", W_REPO));
        assert_eq!(m.rows[0].columns[1], fit("skill", W_KIND));
        // The Tag column shows the explicit version, not `latest`.
        assert_eq!(m.rows[0].columns[2], fit("2.1.0", W_TAG));
        assert_eq!(m.rows[0].columns[3], "✓ installed");
        assert_eq!(m.rows[0].columns[0].chars().count(), W_REPO);
        assert_eq!(m.rows[0].status_color, ColorKey::Installed);
        assert_eq!(m.rows[1].status_color, ColorKey::NotInstalled);
        assert!(m.rows[0].selected, "first row selected by default");
        assert!(!m.rows[1].selected);
        assert!(m.detail.contains("r/alpha"));
        assert!(m.detail.contains("keywords: rust, lint"));
        assert!(m.detail.contains("version: 2.1.0"));
        assert!(m.detail.contains("status: installed"));
        assert!(!m.detail_focused);
        // Idle: status is empty; the key summary lives in `hint`.
        assert_eq!(m.status, "");
        assert!(m.hint.contains("quit"));
        assert!(m.hint.contains("? help"));
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
                "✘",
                "integrity-missing",
                ColorKey::IntegrityMissing,
            ),
        ] {
            assert_eq!(status_view(st), (glyph, label, color));
        }
    }

    #[test]
    fn fit_pads_short_and_ellipsizes_long() {
        assert_eq!(fit("abc", 6), "abc   ");
        assert_eq!(fit("abc", 3), "abc");
        // Over-long: last char becomes the ellipsis, exact width kept.
        let long = "registry.example.com/very/long/repository/path";
        let out = fit(long, 10);
        assert_eq!(out.chars().count(), 10);
        assert!(out.ends_with('…'));
    }

    #[test]
    fn help_mode_sets_show_help() {
        let mut s = TuiState::new();
        s.set_rows(vec![row("r/a", ArtifactState::Installed)]);
        assert!(!frame(&s).show_help);
        s.enter_help();
        assert!(frame(&s).show_help);
    }

    #[test]
    fn frame_marks_offline_and_loading() {
        let mut s = TuiState::new();
        s.set_offline(true);
        assert!(s.loading);
        let m = frame(&s);
        assert_eq!(m.title, "Grimoire [offline]");
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
        assert_eq!(m.search, "al_");
        assert!(!m.search_placeholder);
        s.back();
        s.enter_detail();
        let m2 = frame(&s);
        assert!(m2.detail_focused);
    }

    #[test]
    fn frame_projects_scope_label_and_persistent_hint() {
        let mut s = TuiState::new();
        s.set_rows(vec![row("r/a", ArtifactState::Installed)]);
        s.set_scope_label("project");
        let m = frame(&s);
        assert_eq!(m.scope, "project");
        // The key summary is always present even with an empty status.
        assert!(m.status.is_empty());
        assert!(m.hint.contains("g scope"));
    }

    #[test]
    fn fit_hint_degrades_to_minimum() {
        let m = frame(&{
            let mut s = TuiState::new();
            s.set_rows(vec![row("r/a", ArtifactState::Installed)]);
            s
        });
        let t = &m.hint_tiers;
        assert!(t.len() >= 2);
        assert_eq!(t.last().unwrap(), "? help");
        // Wide terminal ⇒ the full (widest) tier.
        assert_eq!(fit_hint(t, 200), t[0]);
        // Zero width ⇒ still the minimum, never empty.
        assert_eq!(fit_hint(t, 0), "? help");
        // A mid width picks a middle tier (narrower than full, fits).
        let mid = fit_hint(t, 40);
        assert!(mid.chars().count() < 40);
        assert!(mid.contains("? help"));
    }

    #[test]
    fn frame_projects_picker_and_pinned_tag() {
        let mut s = TuiState::new();
        s.set_rows(vec![row("r/alpha", ArtifactState::Installed)]);
        // No picker when not in version-pick mode.
        assert!(frame(&s).picker.is_none());
        s.open_version_pick();
        s.set_picker_tags(vec!["latest".to_string(), "2.1.0".to_string()]);
        let m = frame(&s);
        let p = m.picker.expect("picker projected in version-pick mode");
        assert!(p.title.contains("r/alpha"));
        assert!(!p.loading);
        assert_eq!(p.tags, vec!["latest".to_string(), "2.1.0".to_string()]);
        // Pin the second tag; the Tag column shows it with a `*` marker.
        s.picker_move(1);
        s.confirm_version();
        let m2 = frame(&s);
        assert_eq!(m2.rows[0].columns[2], fit("*2.1.0", W_TAG));
        assert!(m2.detail.contains("pinned: 2.1.0"));
    }

    #[test]
    fn frame_status_line_overrides_hint() {
        let mut s = TuiState::new();
        s.set_rows(vec![row("r/a", ArtifactState::Installed)]);
        s.set_status("installed r/a");
        assert_eq!(frame(&s).status, "installed r/a");
    }

    #[test]
    fn frame_projects_selected_clients_and_omits_when_empty() {
        let mut s = TuiState::new();
        s.set_rows(vec![row("r/a", ArtifactState::Installed)]);
        // No clients selected ⇒ the span is omitted (empty string).
        assert_eq!(frame(&s).clients, "");
        s.set_clients(vec!["claude".to_string(), "opencode".to_string()]);
        assert_eq!(frame(&s).clients, "clients: claude, opencode");
    }

    #[test]
    fn flat_view_strips_default_registry_prefix() {
        let mut s = TuiState::new();
        s.set_default_registry(Some("localhost:5000".to_string()));
        s.set_rows(vec![
            row("localhost:5000/acme/tool", ArtifactState::Installed),
            row("ghcr.io/other/tool", ArtifactState::Installed),
        ]);
        let m = frame(&s);
        // Default registry dropped for display…
        assert_eq!(m.rows[0].columns[0], fit("acme/tool", W_REPO));
        // …but a non-default registry keeps its host.
        assert_eq!(m.rows[1].columns[0], fit("ghcr.io/other/tool", W_REPO));
    }

    #[test]
    fn frame_flat_view_keeps_full_ref() {
        let mut s = TuiState::new();
        s.set_rows(vec![row("reg/acme/tool", ArtifactState::Installed)]);
        let m = frame(&s);
        assert_eq!(m.rows.len(), 1);
        assert_eq!(
            m.rows[0].columns[0],
            fit("reg/acme/tool", W_REPO),
            "flat keeps the full ref"
        );
    }
}
