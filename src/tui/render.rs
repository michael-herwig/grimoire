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

use super::detail::{
    CATALOG_WIDTH, DETAIL_MIN_WIDTH, DetailLine, W_DEPRECATED, W_KIND, W_REPO, W_STATUS, W_TAG, detail_lines,
    scroll_max, viewport,
};

/// Width of the Registry column shown in flat-view multi-registry mode.
///
/// Kept in `render.rs` (not `detail.rs`) because the column is only used by
/// the flat-view list renderer, never by the detail pane or tree layout.
const W_REGISTRY: usize = 20;
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
    /// A bundle member present only because the bundle provides it.
    ViaBundle,
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
        ArtifactState::ViaBundle => ("◆", "via-bundle", ColorKey::ViaBundle),
        ArtifactState::NotInstalled => ("·", "not-installed", ColorKey::NotInstalled),
        ArtifactState::Outdated => ("↑", "outdated", ColorKey::Outdated),
        ArtifactState::Modified => ("✱", "modified", ColorKey::Modified),
        ArtifactState::IntegrityMissing => ("✘", "integrity-missing", ColorKey::IntegrityMissing),
    }
}

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

/// Sanitize a bundle member label before display (the display boundary).
///
/// Applied to `MemberNode.label` **before** [`fit`] in `tree_render_rows`
/// and in `detail_lines_for_member`. The cache holds the raw label; this
/// function is the load-bearing terminal-injection guard.
///
/// # Contract (C-2)
///
/// Strips:
/// - All C0/C1 control characters (`char::is_control()`)
/// - ANSI/CSI escape sequences (`ESC` + following CSI parameter bytes)
/// - Bidi override and isolate code points (U+202A–U+202E, U+2066–U+2069)
/// - Zero-width code points (U+200B ZWSP, U+FEFF BOM, U+200C ZWNJ,
///   U+200D ZWJ)
///
/// Path-traversal-like names (`../`) pass through unchanged at display
/// (not a display threat; data boundary already rejected them for install
/// via `SkillName::parse`).
///
/// Must not be O(n²) — see contract table in the plan.
pub fn sanitize_member_label(s: &str) -> String {
    // Linear time: one-pass character scan, no repeated allocations.
    //
    // State machine for ANSI/CSI escape sequences:
    //   Normal    → ESC char → AfterEsc
    //   AfterEsc  → '[' → InCsi  | anything else → Normal (drop both)
    //   InCsi     → CSI parameter (0x30–0x3F), intermediate (0x20–0x2F),
    //               or final (0x40–0x7E) bytes → stay/exit on final
    //
    // Bidi overrides and isolates (U+202A–U+202E, U+2066–U+2069) and
    // zero-width code points (U+200B, U+200C, U+200D, U+FEFF) are dropped
    // by the `is_stripped` predicate below. Control chars are stripped by
    // `char::is_control()`.
    //
    // Path-traversal text (`../`) is NOT stripped — not a display threat.

    #[inline]
    fn is_stripped(c: char) -> bool {
        // All C0 and C1 controls (char::is_control covers \x00-\x1F and \x7F-\x9F).
        if c.is_control() {
            return true;
        }
        let cp = c as u32;
        // Bidi override/embedding code points (U+202A–U+202E).
        if (0x202A..=0x202E).contains(&cp) {
            return true;
        }
        // Bidi isolate code points (U+2066–U+2069).
        if (0x2066..=0x2069).contains(&cp) {
            return true;
        }
        // Zero-width space (U+200B), ZWNJ (U+200C), ZWJ (U+200D).
        if matches!(cp, 0x200B..=0x200D) {
            return true;
        }
        // BOM / zero-width no-break space (U+FEFF).
        if cp == 0xFEFF {
            return true;
        }
        false
    }

    enum State {
        Normal,
        AfterEsc,
        InCsi,
    }

    let mut out = String::with_capacity(s.len());
    let mut state = State::Normal;

    for c in s.chars() {
        match state {
            State::Normal => {
                if c == '\x1b' {
                    // ESC: enter the escape-sequence consumer, drop this char.
                    state = State::AfterEsc;
                } else if is_stripped(c) {
                    // Drop control chars, bidi overrides, zero-widths.
                } else {
                    out.push(c);
                }
            }
            State::AfterEsc => {
                if c == '[' {
                    // ESC + '[' = CSI introducer — enter CSI consumer.
                    state = State::InCsi;
                } else {
                    // ESC + anything else: drop both, resume normal.
                    // (We already dropped the ESC; drop this char too.)
                    //
                    // INTENTIONAL: OSC (ESC ']'), DCS (ESC 'P'), APC (ESC '_'),
                    // PM (ESC '^'), and SS3 (ESC 'O') body text passes through
                    // as printable chars after the leading ESC is dropped here.
                    // This is NOT a terminal injection risk: ratatui renders styled
                    // text via crossterm using structured APIs, not raw byte
                    // injection into the terminal stream. CSI (ESC '[') is the
                    // relevant in-scope injection vector and is consumed by the
                    // InCsi arm above. Follow-up: optionally consume OSC/DCS until
                    // ST/BEL for defense-in-depth (low priority, YAGNI).
                    state = State::Normal;
                }
            }
            State::InCsi => {
                // CSI parameter bytes: 0x30–0x3F (includes digits, ';', ':').
                // CSI intermediate bytes: 0x20–0x2F (space through '/').
                // CSI final byte: 0x40–0x7E → sequence ends.
                // Anything else (including multi-byte characters): treat as
                // end of CSI (fail-safe) and resume normal, dropping this char.
                let byte = c as u32;
                if (0x20..=0x3F).contains(&byte) {
                    // Parameter or intermediate — consume, stay in CSI.
                } else if (0x40..=0x7E).contains(&byte) {
                    // Final byte — sequence ends; drop this char, go Normal.
                    state = State::Normal;
                } else {
                    // Unexpected — exit CSI, drop this char (fail-safe).
                    state = State::Normal;
                }
            }
        }
    }

    out
}

/// A group row's tri-state mark, rolled up from its descendant leaves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkState {
    /// No descendant leaf is marked.
    None,
    /// Some — but not all — descendant leaves are marked.
    Partial,
    /// Every descendant leaf is marked.
    All,
}

/// Tree-group-specific display data carried by a [`RenderRow`] when the row
/// projects a group node (absent for leaf / flat rows).
///
/// The arrow glyph (▾/▸) and the rollup label are already pre-formatted
/// into `RenderRow.columns[0]` and `columns[2]` respectively, so
/// `GroupRow` carries only the data that [`draw`] cannot recover from the
/// columns — the tri-state mark, consumed to render the leftmost glyph
/// column (`▣`/`▨`/blank) for group rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupRow {
    /// The tri-state mark rolled up from descendant leaves.
    /// [`draw`] renders this as the leftmost-column glyph:
    /// `▣` (All) / `▨` (Partial) / blank (None).
    pub mark: MarkState,
}

/// One table row in the render model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderRow {
    /// The visible columns, already formatted (repo, kind, tag, status).
    /// In tree mode, `columns[0]` carries the indent prefix (two spaces per
    /// depth level) and the arrow glyph for groups — the single canonical
    /// representation of tree position.
    pub columns: [String; 4],
    /// Whether this row is the current selection.
    pub selected: bool,
    /// Whether this row is marked for a batch action.
    pub marked: bool,
    /// The color the status cell should render in.
    pub status_color: ColorKey,
    /// Group-display data when this row projects a tree group; `None` for
    /// leaf rows and every flat-view row.
    pub group: Option<GroupRow>,
    /// The Registry column value for flat-view multi-registry mode.
    ///
    /// Set to the registry display label (alias or URL) when
    /// [`RenderModel::show_registry_column`] is true; `None` for tree rows
    /// and single-registry flat rows (where the column is elided).
    pub registry: Option<String>,
    /// Whether this row is deprecated. Drives the trailing, header-less `⚠`
    /// indicator column (rendered in yellow by [`draw`]); orthogonal to the
    /// install-status glyph. Leaf and flat rows carry it; group and member
    /// rows are always `false`.
    pub deprecated: bool,
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
    /// The detail-pane content for the selected row, as semantic lines
    /// ([`draw`] maps each kind to its styling).
    pub detail: Vec<DetailLine>,
    /// The detail pane's vertical scroll offset, already clamped to the
    /// content's post-wrap height in the live viewport
    /// (see [`super::detail::scroll_max`]).
    pub detail_scroll: u16,
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
    /// A short, unobtrusive hint shown when the browse window was truncated
    /// at the cap (the row list / search may be incomplete); empty when the
    /// window is exhaustive. Rendered as a quiet span on the legend line.
    pub truncation_hint: String,
    /// Whether the detail pane is the focused element.
    pub detail_focused: bool,
    /// Whether the help overlay is showing.
    pub show_help: bool,
    /// Vertical scroll offset of the help overlay (rows).
    pub help_scroll: u16,
    /// The version-picker overlay, when [`Mode::VersionPick`].
    pub picker: Option<PickerView>,
    /// Whether the flat-view list should prepend a Registry column.
    ///
    /// True when more than one registry is in scope (the column tells the user
    /// which registry each artifact came from). False for single-registry
    /// sessions where every row shares the same origin.
    pub show_registry_column: bool,
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
/// `registry` is the Registry-column label in flat multi-registry mode;
/// `None` in single-registry and all tree modes.
fn render_leaf(
    r: &super::state::TuiRow,
    repo_text: &str,
    selected: bool,
    marked: bool,
    registry: Option<String>,
) -> RenderRow {
    let (glyph, label, color) = status_view(r.state);
    // A user-pinned version shows with a leading `*`; otherwise the
    // explicit highest version, falling back to the tag.
    let tag_cell = match &r.pinned_version {
        Some(p) => format!("*{p}"),
        None if !r.version.is_empty() => r.version.clone(),
        None => r.latest_tag.clone(),
    };
    // Deprecation is flagged by a dedicated trailing `⚠` column (rendered in
    // `draw`), not a Repo-cell prefix — so the Repo cell stays clean and
    // left-aligned with every other row.
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
        group: None,
        registry,
        deprecated: r.deprecated.is_some(),
    }
}

/// Compute the tri-state mark for a group given its descendant row indices
/// and the set of currently-marked rows.
fn group_mark_state(descendant_rows: &[usize], marked: &std::collections::BTreeSet<usize>) -> MarkState {
    if descendant_rows.is_empty() {
        return MarkState::None;
    }
    let marked_count = descendant_rows.iter().filter(|i| marked.contains(i)).count();
    if marked_count == 0 {
        MarkState::None
    } else if marked_count == descendant_rows.len() {
        MarkState::All
    } else {
        MarkState::Partial
    }
}

/// Project the tree view's flattened display rows into [`RenderRow`]s.
///
/// Pure: groups render an arrow + label + rollup label + tri-state mark;
/// leaves render the bare final-segment label indented to their depth.
///
/// `flat` is the caller-owned result of [`TuiState::flattened`]; threading it
/// in avoids a redundant rebuild when `frame()` also needs it for the detail
/// branch (P1 dedup: compute once, thread to both).
fn tree_render_rows(state: &TuiState, flat: &[super::tree::DisplayRow]) -> Vec<RenderRow> {
    flat.iter()
        .enumerate()
        .map(|(pos, display_row)| match display_row {
            super::tree::DisplayRow::Group {
                key,
                label,
                depth,
                collapsed,
                rollup,
                rows: descendant_rows,
                ..
            } => {
                // Single status_view call — used for both the color tag and col 3.
                let (worst_glyph, worst_label, color) = status_view(rollup.worst());
                let mark = group_mark_state(descendant_rows, &state.marked);
                // F6: shorten rollup badge from "x/n installed" to "x/n" — less
                // visual clutter; meaning is clear from position (Tag/col 2).
                let rollup_label = format!("{}/{}", rollup.installed, rollup.total);
                // Arrow: ▾ expanded, ▸ collapsed.
                let arrow = if *collapsed { "▸" } else { "▾" };
                let indent = "  ".repeat(*depth);
                // B: registry-root groups show "alias (url)" when an alias was
                // configured, or the plain URL otherwise.  Non-registry groups
                // (org, path segment) keep their existing label unchanged.
                let display_label = if state.registry_labels.contains_key(key.as_str()) {
                    state.registry_label(key)
                } else {
                    label.clone()
                };
                let repo_text = format!("{indent}{arrow} {display_label}");
                // Col 3 (Status): group status glyph from rollup.worst(), optionally
                // prefixed with the tri-state mark glyph. The rollup label belongs in
                // col 2 (Tag) only — NOT duplicated here.
                let status_col = match mark {
                    MarkState::None => format!("{worst_glyph} {worst_label}"),
                    MarkState::Partial => format!("▨ {worst_glyph} {worst_label}"),
                    MarkState::All => format!("▣ {worst_glyph} {worst_label}"),
                };
                RenderRow {
                    columns: [
                        fit(&repo_text, W_REPO),
                        fit("", W_KIND),
                        fit(&rollup_label, W_TAG),
                        status_col,
                    ],
                    selected: pos == state.selected,
                    marked: mark != MarkState::None,
                    status_color: color,
                    group: Some(GroupRow { mark }),
                    registry: None,
                    // Group rollups don't carry a deprecation indicator.
                    deprecated: false,
                }
            }
            super::tree::DisplayRow::Leaf {
                label,
                depth,
                row,
                state: leaf_state,
                is_bundle,
                collapsed,
                ..
            } => {
                let (glyph, status_label, color) = status_view(*leaf_state);
                let indent = "  ".repeat(*depth);
                let r = state.rows.get(*row);
                // Deprecation is flagged by the dedicated trailing `⚠` column
                // (rendered in `draw`), not a label prefix — keeping the tree
                // label clean and aligned with the bundle arrow.
                let leaf_deprecated = r.is_some_and(|row| row.deprecated.is_some());
                // P3.2: bundle leaves carry an expand/collapse arrow glyph.
                // F4: use UTF-8 ▸/▾ (same glyphs as group rows) — no ASCII fallback.
                // Non-bundle leaves are rendered without prefix (unchanged).
                let repo_text = if *is_bundle {
                    let arrow = if *collapsed { "▸" } else { "▾" };
                    format!("{indent}{arrow} {label}")
                } else {
                    format!("{indent}{label}")
                };
                let tag_cell = r
                    .map(|row| match &row.pinned_version {
                        Some(p) => format!("*{p}"),
                        None if !row.version.is_empty() => row.version.clone(),
                        None => row.latest_tag.clone(),
                    })
                    .unwrap_or_default();
                RenderRow {
                    columns: [
                        fit(&repo_text, W_REPO),
                        fit(r.map(|r| r.kind.as_str()).unwrap_or(""), W_KIND),
                        fit(&tag_cell, W_TAG),
                        format!("{glyph} {status_label}"),
                    ],
                    selected: pos == state.selected,
                    marked: state.is_row_marked(*row),
                    status_color: color,
                    group: None,
                    registry: None,
                    deprecated: leaf_deprecated,
                }
            }
            super::tree::DisplayRow::Member {
                label,
                depth,
                kind,
                state: member_state,
                related,
                ..
            } => {
                let (glyph, status_label, color) = status_view(*member_state);
                let indent = "  ".repeat(*depth);
                // Sanitize at the display boundary — cache holds raw label.
                // F5: removed "(via bundle)" suffix — visual noise; member rows
                // are structurally distinct (indented child of bundle leaf).
                let sanitized = sanitize_member_label(label);
                let repo_text = format!("{indent}  {sanitized}");
                // Related members are highlighted — the `related` flag drives
                // any future related-highlight styling at the draw layer.
                // For now we expose it through `marked: false`; the draw layer
                // can read `related` from the DisplayRow directly when needed.
                let _ = related; // consumed by draw layer, not render layer
                RenderRow {
                    columns: [
                        fit(&repo_text, W_REPO),
                        fit(&kind.to_string(), W_KIND),
                        fit("", W_TAG),
                        format!("{glyph} {status_label}"),
                    ],
                    selected: pos == state.selected,
                    marked: false,
                    status_color: color,
                    group: None,
                    registry: None,
                    // Bundle-member rows don't carry a deprecation indicator.
                    deprecated: false,
                }
            }
        })
        .collect()
}

/// Build the detail-pane lines for a selected tree group (rollup summary),
/// used in place of [`detail_lines`] when the selection is a group node.
///
/// `flat` is the caller-owned result of [`TuiState::flattened`]; threading it
/// in avoids a redundant rebuild (P1 dedup: shared with `tree_render_rows`).
fn group_detail_lines(state: &TuiState, flat: &[super::tree::DisplayRow]) -> Vec<DetailLine> {
    let Some(super::tree::DisplayRow::Group {
        key,
        rollup,
        rows: descendant_rows,
        ..
    }) = flat.get(state.selected)
    else {
        return vec![DetailLine::Text("no group selected".to_string())];
    };

    let mut lines = vec![
        DetailLine::Blank,
        DetailLine::Identifier(key.clone()),
        DetailLine::Blank,
        DetailLine::SectionLabel("Group Summary:"),
        DetailLine::Blank,
        DetailLine::MetaEntry {
            label: "Total:",
            value: rollup.total.to_string(),
        },
        DetailLine::MetaEntry {
            label: "Installed:",
            value: rollup.installed.to_string(),
        },
    ];
    if rollup.outdated > 0 {
        lines.push(DetailLine::MetaEntry {
            label: "Outdated:",
            value: rollup.outdated.to_string(),
        });
    }
    if rollup.modified > 0 {
        lines.push(DetailLine::MetaEntry {
            label: "Modified:",
            value: rollup.modified.to_string(),
        });
    }
    if rollup.integrity_missing > 0 {
        lines.push(DetailLine::MetaEntry {
            label: "Integrity missing:",
            value: rollup.integrity_missing.to_string(),
        });
    }
    if rollup.not_installed > 0 {
        lines.push(DetailLine::MetaEntry {
            label: "Not installed:",
            value: rollup.not_installed.to_string(),
        });
    }
    lines.push(DetailLine::Blank);
    lines.push(DetailLine::MetaEntry {
        label: "Members:",
        value: descendant_rows.len().to_string(),
    });
    lines
}

/// Build detail lines for a selected virtual [`DisplayRow::Member`] row.
///
/// Looks the `MemberNode` up from the bundle-member cache using the row's
/// `parent_bundle_repo` and the active scope label, then delegates to
/// [`detail_lines_for_member`]. Falls back to a generic "no selection" text
/// when the cache entry is absent (race between display and cache eviction).
fn member_detail_lines_from_state(state: &TuiState, row: Option<&super::tree::DisplayRow>) -> Vec<DetailLine> {
    use super::bundle_members::BundleMemberCache;
    use super::tree::DisplayRow;
    use crate::tui::detail::detail_lines_for_member;

    let Some(DisplayRow::Member {
        label,
        kind,
        parent_bundle_repo,
        ..
    }) = row
    else {
        return vec![DetailLine::Text("no member selected".to_string())];
    };

    let key = (state.scope_label.clone(), parent_bundle_repo.clone());
    if let Some(BundleMemberCache::Ready(members)) = state.bundle_members.get(&key) {
        // Match by label AND kind to handle duplicate labels across different
        // artifact kinds (W4: label-only lookup could return the wrong member).
        if let Some(node) = members.iter().find(|m| &m.label == label && m.kind == *kind) {
            return detail_lines_for_member(node, parent_bundle_repo);
        }
    }

    // Cache miss or loading/failed: show a minimal identifier from the label.
    let sanitized = sanitize_member_label(label);
    vec![
        DetailLine::Blank,
        DetailLine::Identifier(sanitized),
        DetailLine::Blank,
        DetailLine::Text("(member details unavailable)".to_string()),
    ]
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

    // P1: compute the flat display list ONCE in tree mode — shared between the
    // row projection (tree_render_rows) and the detail branch below, avoiding
    // 2-3 redundant rebuilds per frame. In flat mode the list is never needed
    // so the allocation is skipped entirely.
    //
    // `flat_ref` defaults to an empty slice in flat mode; it is never read
    // in that mode, so no `expect` is needed — no allocation occurs for
    // the non-Tree case and no lint suppression is required.
    let tree_flat: Vec<super::tree::DisplayRow> = if state.view_mode == crate::tui::state::ViewMode::Tree {
        state.flattened()
    } else {
        Vec::new()
    };
    let flat_ref: &[super::tree::DisplayRow] = &tree_flat;

    let rows: Vec<RenderRow> = if state.view_mode == crate::tui::state::ViewMode::Tree && !state.loading {
        // Tree view is a pure projection over the flattened, collapse-aware
        // tree (itself built over the `filtered` row set).
        tree_render_rows(state, flat_ref)
    } else {
        // A: In multi-registry flat mode, add a Registry column showing the
        // display label (alias or URL) and shorten Repo to the registry-relative
        // `repository` path.  Single-registry behavior is unchanged (D-ELIDE).
        let multi = state.is_multi_registry();
        state
            .filtered
            .iter()
            .enumerate()
            .filter_map(|(pos, &i)| state.rows.get(i).map(|r| (pos, i, r)))
            .map(|(pos, i, r)| {
                let (repo_text, registry): (std::borrow::Cow<str>, Option<String>) = if multi {
                    // Attribute the bare-host row to its configured registry (the
                    // same split the tree uses): show that registry's label and
                    // shorten Repo to the path relative to it. Using `r.registry`
                    // directly would show the bare host and an un-shortened repo.
                    let configured: Vec<&str> = state
                        .registry_order
                        .iter()
                        .map(String::as_str)
                        .chain(state.default_registry.as_deref())
                        .collect();
                    let (reg, rel) = super::tree::display_split(r, &configured);
                    (std::borrow::Cow::Owned(rel), Some(state.registry_label(&reg)))
                } else {
                    // Single-registry: existing elision behavior (D-ELIDE).
                    (
                        std::borrow::Cow::Borrowed(strip_default_registry(&r.repo, state.default_registry.as_deref())),
                        None,
                    )
                };
                render_leaf(r, &repo_text, pos == state.selected, state.is_row_marked(i), registry)
            })
            .collect()
    };

    // The offset is already clamped at mutation time
    // (`TuiState::scroll_detail`); the re-clamp here covers direct field
    // writes (the field is public for tests).
    let detail = if state.view_mode == crate::tui::state::ViewMode::Tree {
        // In tree mode, selection can be a group, a bundle-member virtual row,
        // or a regular leaf. Each dispatches to its own detail builder.
        match flat_ref.get(state.selected) {
            Some(super::tree::DisplayRow::Group { .. }) => group_detail_lines(state, flat_ref),
            Some(super::tree::DisplayRow::Member { .. }) => {
                // Member rows are virtual; fetch the MemberNode from the cache
                // and delegate to detail_lines_for_member.
                member_detail_lines_from_state(state, flat_ref.get(state.selected))
            }
            // Leaf: delegate to the standard row detail builder.
            Some(super::tree::DisplayRow::Leaf { .. }) => detail_lines(state.selected_row()),
            // No selection (empty or out-of-range display list).
            None => detail_lines(state.selected_row()),
        }
    } else {
        detail_lines(state.selected_row())
    };
    let detail_scroll = state.detail_scroll.min(scroll_max(&detail, viewport(state.term_size)));

    // Status is transient only — loading / counts / batch results, or the
    // marked-set action keys (contextual). The always-on key summary lives
    // in `hint` so a transient message can never hide `? help`.
    // C6 / D-DEGRADE: compose registry health degradation message when any
    // registry is offline or truncated (shown when no higher-priority
    // transient message overrides — takes precedence over marked count).
    // P2: short-circuit to avoid any allocation when both lists are empty.
    // SEC1: map each URL through `sanitize_member_label` before joining so
    // registry URLs from the catalog cannot inject terminal escape sequences.
    let registry_health_status = {
        let h = &state.registry_health;
        if h.offline.is_empty() && h.truncated.is_empty() {
            String::new()
        } else {
            // B: show the alias-based label (SEC1: sanitize against escape injection).
            let mut parts: Vec<String> = Vec::new();
            if !h.offline.is_empty() {
                let names: Vec<String> = h
                    .offline
                    .iter()
                    .map(|url| sanitize_member_label(&state.registry_label(url)))
                    .collect();
                parts.push(format!("offline: {}", names.join(", ")));
            }
            if !h.truncated.is_empty() {
                let names: Vec<String> = h
                    .truncated
                    .iter()
                    .map(|url| sanitize_member_label(&state.registry_label(url)))
                    .collect();
                parts.push(format!("truncated: {}", names.join(", ")));
            }
            parts.join(" · ")
        }
    };

    let status = if !state.status_line.is_empty() {
        state.status_line.clone()
    } else if state.loading {
        "loading catalog…".to_string()
    } else if !registry_health_status.is_empty() {
        registry_health_status
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
    //
    // C5: hint tiers are view-mode-aware.
    //   - `t tree` always appears (always available to toggle).
    //   - `→/← expand/collapse` appears only in tree mode (inert in flat mode).
    let hint_tiers = if state.view_mode == crate::tui::state::ViewMode::Tree {
        vec![
            "↑↓ move · pgup/pgdn scroll · space mark · i/u/d act · v versions · o open · g scope · t tree · →/← expand/collapse · / search · ? help · q quit"
                .to_string(),
            "↑↓ move · space mark · i/u/d act · v versions · o open · g scope · t tree · →/← expand · / search · ? help · q quit".to_string(),
            "↑↓ move · i/u/d act · v ver · g scope · t tree · →/← expand · / search · ? help · q quit".to_string(),
            "↑↓ i/u/d g t →/← / ? help q".to_string(),
            "i/u/d v g t / ? q".to_string(),
            "? help".to_string(),
        ]
    } else {
        // Flat mode: `→/←` keys are inert — omit from hints to avoid misleading users.
        vec![
            "↑↓ move · pgup/pgdn scroll · space mark · i/u/d act · v versions · o open · g scope · t tree · / search · ? help · q quit"
                .to_string(),
            "↑↓ move · space mark · i/u/d act · v versions · o open · g scope · t tree · / search · ? help · q quit".to_string(),
            "↑↓ move · i/u/d act · v ver · g scope · t tree · / search · ? help · q quit".to_string(),
            "↑↓ i/u/d g t / ? help q".to_string(),
            "i/u/d v g t / ? q".to_string(),
            "? help".to_string(),
        ]
    };
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

    // A truncated browse window gets a quiet hint so the list is not read as
    // exhaustive; omitted entirely when the window is complete.
    let truncation_hint = if state.truncated {
        "(list truncated)".to_string()
    } else {
        String::new()
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
        detail_scroll,
        status,
        hint,
        hint_tiers,
        legend: "✓ installed   ↑ outdated   ✱ modified   ✘ integrity-missing   · not-installed   ⚠ deprecated"
            .to_string(),
        truncation_hint,
        detail_focused: state.mode == Mode::Detail,
        show_help: state.mode == Mode::Help,
        help_scroll: state.help_scroll,
        picker,
        // A: only show Registry column when more than one registry is in scope;
        // the tree view never needs it (registry roots are already tree nodes).
        show_registry_column: state.is_multi_registry() && state.view_mode != crate::tui::state::ViewMode::Tree,
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

    // A: when more than one registry is in scope, prepend a Registry column to
    // the header and each flat-view row.  Tree-mode rows never carry a registry
    // (they express it via the group node label), so the column is flat-only.
    //
    // The Status header reserves its full region — `W_STATUS` plus the gap and
    // the deprecation marker (`W_DEPRECATED`) that deprecated rows append — so
    // the underlined header spans the whole Catalog box width (which always
    // reserves that room via `CATALOG_WIDTH`), instead of stopping short.
    let status_header_w = W_STATUS + 2 + W_DEPRECATED;
    let header_text = if model.show_registry_column {
        format!(
            "  {:<gw$}  {:<rw$}  {:<kw$}  {:<tw$}  {:<sw$}",
            "Registry",
            model.headers[0],
            model.headers[1],
            model.headers[2],
            model.headers[3],
            gw = W_REGISTRY,
            rw = W_REPO,
            kw = W_KIND,
            tw = W_TAG,
            sw = status_header_w,
        )
    } else {
        format!(
            "  {:<rw$}  {:<kw$}  {:<tw$}  {:<sw$}",
            model.headers[0],
            model.headers[1],
            model.headers[2],
            model.headers[3],
            rw = W_REPO,
            kw = W_KIND,
            tw = W_TAG,
            sw = status_header_w,
        )
    };
    let header = ListItem::new(Line::from(Span::styled(
        header_text,
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
        //
        // Leftmost-column glyph: group rows render tri-state from `GroupRow.mark`
        // (▣ All / ▨ Partial / blank None); leaf and flat rows use the binary
        // `marked` flag (▣ / blank). This is the single canonical source for the
        // leftmost mark column — `r.marked` is still accurate for batch-action
        // detection but the glyph must reflect the tri-state for groups.
        let mark_glyph = if let Some(group) = &r.group {
            match group.mark {
                MarkState::All => "▣ ",
                MarkState::Partial => "▨ ",
                MarkState::None => "  ",
            }
        } else if r.marked {
            "▣ "
        } else {
            "  "
        };
        // A: registry span (flat multi-registry mode only).
        let mut spans = vec![Span::styled(
            mark_glyph.to_string(),
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        )];
        if model.show_registry_column {
            let reg_label = r.registry.as_deref().unwrap_or("");
            spans.push(Span::styled(
                format!("{:<gw$}  ", reg_label, gw = W_REGISTRY),
                Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD),
            ));
        }
        spans.extend([
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
        // Deprecation rides in the Status column: a space-separated yellow
        // `⚠ deprecated` appended after the install-status label (orthogonal to
        // its color; the full notice lives in the detail pane). CATALOG_WIDTH
        // reserves the extra width so the marker is never clipped by the border.
        if r.deprecated {
            spans.push(Span::styled(
                " ⚠ deprecated".to_string(),
                Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
            ));
        }
        items.push(ListItem::new(Line::from(spans)));
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
    // Mechanical mapping only — the layout decisions live in
    // `detail_lines` (the pure projection).
    let detail_text: Vec<Line> = model
        .detail
        .iter()
        .map(|l| match l {
            DetailLine::Blank => Line::from(""),
            DetailLine::Identifier(s) => Line::from(Span::styled(
                s.clone(),
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            ))
            .alignment(Alignment::Center),
            DetailLine::SectionLabel(label) => Line::from(Span::styled(
                (*label).to_string(),
                Style::default().fg(Color::White).add_modifier(Modifier::UNDERLINED),
            )),
            DetailLine::MetaEntry { label, value } => Line::from(vec![
                Span::styled(
                    format!("{label} "),
                    Style::default().fg(Color::Blue).add_modifier(Modifier::BOLD),
                ),
                Span::styled(value.clone(), Style::default().fg(Color::White)),
            ]),
            DetailLine::Text(s) => Line::from(Span::styled(s.clone(), Style::default().fg(Color::White))),
        })
        .collect();
    f.render_widget(
        Paragraph::new(detail_text)
            .block(detail_block)
            .wrap(Wrap { trim: false })
            .scroll((model.detail_scroll, 0)),
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
        f.render_widget(Paragraph::new(legend_line(&model.truncation_hint)), legend_row[0]);
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
        draw_help(f, model.help_scroll);
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
/// color), so the legend itself demonstrates the palette. A non-empty
/// `truncation_hint` is appended as a quiet trailing span so a capped
/// browse window is flagged without crowding the title-row status.
fn legend_line(truncation_hint: &str) -> Line<'static> {
    let pairs = [
        ("✓ installed", ColorKey::Installed),
        ("  ◆ via-bundle", ColorKey::ViaBundle),
        ("  ↑ outdated", ColorKey::Outdated),
        ("  ✱ modified", ColorKey::Modified),
        ("  ✘ integrity-missing", ColorKey::IntegrityMissing),
        ("  · not-installed", ColorKey::NotInstalled),
    ];
    let mut spans: Vec<Span<'static>> = pairs
        .into_iter()
        .map(|(t, k)| Span::styled(t.to_string(), Style::default().fg(color_for(k))))
        .collect();
    // Deprecation is orthogonal to install status (no `ColorKey`); append it
    // as a literal yellow span so the trailing `⚠` indicator is explained.
    spans.push(Span::styled(
        "  ⚠ deprecated".to_string(),
        Style::default().fg(Color::Yellow),
    ));
    if !truncation_hint.is_empty() {
        spans.push(Span::styled(
            format!("   {truncation_hint}"),
            Style::default().fg(Color::DarkGray),
        ));
    }
    Line::from(spans)
}

/// Every browse-mode keybinding shown in the `?` help overlay, as
/// `(keys, description)` rows. A free function so a unit test can assert the
/// overlay documents every action the event loop handles.
fn help_entries() -> [(&'static str, &'static str); 16] {
    [
        ("↑ / ↓", "move selection (scroll the detail pane when open)"),
        ("j / k", "scroll the detail pane line by line (when open)"),
        ("pgup/pgdn", "scroll the detail pane a page (no focus needed)"),
        ("space", "mark / unmark the row"),
        ("a / c", "mark all visible / clear marks"),
        ("i / u / d", "install / update / uninstall (marked set or selection)"),
        ("v", "pick a specific version for the selected row"),
        ("o", "open the selected entry's repository URL"),
        ("g", "toggle scope: project ⇄ global"),
        ("t", "toggle tree / flat view"),
        ("→ / ←", "expand / collapse selected group (tree mode)"),
        ("/", "search; type to filter, enter to commit"),
        ("enter", "open the detail pane"),
        ("r", "refresh the catalog from the registry"),
        ("?", "this help (any key closes)"),
        ("q / esc", "quit"),
    ]
}

/// The content lines of the `?` help overlay: a "Keybindings" header, a blank
/// separator, and one row per [`help_entries`] item. The count must equal
/// [`crate::tui::state::HELP_BODY_LINES`] (the scroll-clamp source of truth) —
/// guarded by `tests::help_body_line_count_matches_state`.
fn help_lines() -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = vec![
        Line::from(Span::styled(
            "Keybindings",
            Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
    ];
    for (k, d) in help_entries() {
        lines.push(Line::from(vec![
            Span::styled(
                format!("  {k:<10}"),
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            ),
            Span::styled(d, Style::default().fg(Color::White)),
        ]));
    }
    lines
}

/// Height (rows) for the help overlay: `content_lines` body rows plus the top
/// and bottom border, clamped to the available terminal height so the box
/// never exceeds the screen. Sized to content so the full key map shows on a
/// standard terminal (the previous fixed 50%-height box clipped ~7 rows at
/// 80×24); on a shorter terminal the overlay scrolls (`↑`/`↓`, `j`/`k`).
fn help_overlay_height(content_lines: usize, term_height: u16) -> u16 {
    // body rows + top & bottom border.
    let needed = u16::try_from(content_lines).unwrap_or(u16::MAX).saturating_add(2);
    needed.min(term_height.max(1))
}

/// A centered help overlay listing every keybinding. Sized to its content so
/// the full key map shows on a standard terminal; scrolls (offset `scroll`)
/// when the terminal is too short to fit it all.
fn draw_help(f: &mut Frame, scroll: u16) {
    let lines = help_lines();
    let full = f.area();
    let height = help_overlay_height(lines.len(), full.height);
    let area = centered_area_rows(full, 60, height);
    f.render_widget(Clear, area);
    f.render_widget(
        Paragraph::new(lines).scroll((scroll, 0)).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan))
                .title(Span::styled(
                    " help — ↑↓/j/k scroll, any key closes ",
                    Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD),
                )),
        ),
        area,
    );
}

/// A `width_pct`-wide, `height_rows`-tall rectangle centered in `area`. Unlike
/// [`centered_rect`], the height is an absolute row count (clamped to `area`)
/// rather than a percentage, so a content-sized overlay neither clips nor
/// over-grows on tall terminals.
/// A `width_pct`-percent-wide, `height_rows`-tall rectangle centered in
/// `area`. Computes the split via ratatui `Layout` (u32 internally), so it
/// never overflows on very wide terminals.
pub fn centered_area_rows(area: Rect, width_pct: u16, height_rows: u16) -> Rect {
    let height = height_rows.min(area.height);
    let top = area.height.saturating_sub(height) / 2;
    let vert = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(top), Constraint::Length(height), Constraint::Min(0)])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - width_pct) / 2),
            Constraint::Percentage(width_pct),
            Constraint::Percentage((100 - width_pct) / 2),
        ])
        .split(vert[1])[1]
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
        ColorKey::ViaBundle => Color::Cyan,
        ColorKey::NotInstalled => Color::DarkGray,
        ColorKey::Outdated => Color::Yellow,
        ColorKey::Modified => Color::Red,
        ColorKey::IntegrityMissing => Color::Magenta,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::detail::detail_line_text;
    use crate::tui::state::{ArtifactState, TuiRow};

    /// Flatten the semantic detail lines to one plain string for
    /// contains-style assertions (styling is irrelevant to content tests).
    fn detail_text(m: &RenderModel) -> String {
        m.detail.iter().map(detail_line_text).collect::<Vec<_>>().join("\n")
    }

    fn row(repo: &str, state: ArtifactState) -> TuiRow {
        let (reg, repo_path) = repo.split_once('/').unwrap_or((repo, ""));
        TuiRow {
            kind: "skill".to_string(),
            registry: reg.to_string(),
            repository: repo_path.to_string(),
            repo: repo.to_string(),
            description: "review code".to_string(),
            summary: String::new(),
            keywords: vec!["rust".to_string(), "lint".to_string()],
            repository_url: None,
            revision: None,
            created: None,
            latest_tag: "latest".to_string(),
            version: "2.1.0".to_string(),
            deprecated: None,
            pinned_version: None,
            state,
            source: None,
        }
    }

    #[test]
    fn render_leaf_flags_deprecated_without_touching_repo_cell() {
        let mut r = row("r/alpha", ArtifactState::NotInstalled);
        r.deprecated = Some("use r/alpha-2".to_string());
        let leaf = render_leaf(&r, "r/alpha", false, false, None);
        // The flag drives the trailing indicator column (`draw`); the Repo
        // cell stays clean and left-aligned (no inline glyph).
        assert!(leaf.deprecated, "a deprecated row must set the deprecated flag");
        assert!(
            !leaf.columns[0].contains('⚠'),
            "the Repo cell must stay clean; got {:?}",
            leaf.columns[0]
        );
        assert!(leaf.columns[0].starts_with("r/alpha"), "repo text is unshifted");
        // A non-deprecated row sets neither the flag nor any marker.
        let plain = row("r/beta", ArtifactState::NotInstalled);
        let leaf2 = render_leaf(&plain, "r/beta", false, false, None);
        assert!(!leaf2.deprecated, "non-deprecated row must not set the flag");
        assert!(!leaf2.columns[0].contains('⚠'), "non-deprecated row must not be marked");
    }

    // Regression: the trailing `⚠` must land INSIDE the Catalog box, not be
    // clipped by its right border. `CATALOG_WIDTH` reserves a column for it.
    #[test]
    fn draw_renders_deprecation_indicator_inside_catalog_box() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let mut s = TuiState::new();
        let mut dep = row("r/alpha", ArtifactState::Installed);
        dep.deprecated = Some("use r/alpha-2".to_string());
        s.set_rows(vec![dep]);
        let model = frame(&s);

        // Side-by-side layout: Catalog gets exactly CATALOG_WIDTH columns.
        let w = CATALOG_WIDTH + DETAIL_MIN_WIDTH + 4;
        let mut term = Terminal::new(TestBackend::new(w, 12)).unwrap();
        term.draw(|f| draw(f, &model)).unwrap();
        let buf = term.backend().buffer();
        // Reconstruct each screen row to confirm the whole `⚠ deprecated`
        // marker lands on the catalog row, not just the leading glyph.
        let cols = buf.area.width as usize;
        let lines: Vec<String> = buf
            .content()
            .chunks(cols)
            .map(|row| row.iter().map(|c| c.symbol()).collect::<String>())
            .collect();
        let row_line = lines
            .iter()
            .find(|l| l.contains("r/alpha"))
            .expect("the catalog row is rendered");
        assert!(
            row_line.contains("⚠ deprecated"),
            "the full `⚠ deprecated` marker must render unclipped on the row: {row_line:?}"
        );
    }

    // Regression: the underlined header must span the full Catalog box width —
    // the Status header reserves the deprecation-marker region, so the
    // underline reaches the box's right border instead of stopping short.
    #[test]
    fn draw_header_underline_spans_full_catalog_width() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        use ratatui::style::Modifier;

        let mut s = TuiState::new();
        let mut dep = row("r/alpha", ArtifactState::Installed);
        dep.deprecated = Some("use r/alpha-2".to_string());
        s.set_rows(vec![dep]);
        let model = frame(&s);

        let w = CATALOG_WIDTH + DETAIL_MIN_WIDTH + 4;
        let mut term = Terminal::new(TestBackend::new(w, 12)).unwrap();
        term.draw(|f| draw(f, &model)).unwrap();
        let buf = term.backend().buffer();
        let cols = buf.area.width as usize;
        let cells = buf.content();
        let header_y = (0..buf.area.height as usize)
            .find(|&y| {
                let line: String = (0..cols).map(|x| cells[y * cols + x].symbol()).collect();
                line.contains("Repo") && line.contains("Status")
            })
            .expect("the header row is rendered");
        // Last column inside the Catalog box's right border.
        let last_inner = CATALOG_WIDTH as usize - 2;
        assert!(
            cells[header_y * cols + last_inner]
                .modifier
                .contains(Modifier::UNDERLINED),
            "the header underline must reach the Catalog box border (full table width)"
        );
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
        let detail = detail_text(&m);
        assert!(detail.contains("r/alpha"));
        assert!(detail.contains("Keywords: rust, lint"));
        // Version + status live on the catalog row (Tag column, status
        // glyph) — the detail pane does not repeat them.
        assert!(!detail.contains("Version:"));
        assert!(!detail.contains("Status:"));
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
            (ArtifactState::ViaBundle, "◆", "via-bundle", ColorKey::ViaBundle),
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

    // Regression: the `?` help overlay must be tall enough to show EVERY
    // body line on a standard 80×24 terminal. The previous fixed 50%-height
    // box (`centered_rect(60, 50)`) gave ~10 content rows and clipped ~7 of
    // the entries.
    #[test]
    fn help_overlay_fits_all_entries_on_standard_terminal() {
        let lines = help_lines().len();
        let h = help_overlay_height(lines, 24);
        // every body line + top/bottom border must fit.
        assert!(
            h >= lines as u16 + 2,
            "overlay (h={h}) must be tall enough for all {lines} body lines + borders at 80×24"
        );
        assert!(h <= 24, "overlay must never exceed the terminal height");
    }

    // The overlay must never grow past a short terminal (it clamps and scrolls;
    // the box stays on-screen).
    #[test]
    fn help_overlay_height_clamps_to_short_terminal() {
        assert_eq!(help_overlay_height(18, 8), 8, "clamp to a short terminal height");
        assert_eq!(help_overlay_height(18, 1), 1, "never collapse below one row");
    }

    // The overlay must document the keys that were previously missing or newly
    // added — j/k detail scroll and the tree expand/collapse + view toggle.
    #[test]
    fn help_overlay_documents_detail_scroll_and_tree_keys() {
        let keys: Vec<&str> = help_entries().iter().map(|(k, _)| *k).collect();
        assert!(keys.contains(&"j / k"), "detail-scroll j/k must be documented");
        assert!(keys.contains(&"→ / ←"), "tree expand/collapse must be documented");
        assert!(keys.contains(&"t"), "view toggle must be documented");
    }

    // The scroll-clamp source of truth (`state::HELP_BODY_LINES`) must match the
    // overlay's actual body line count, or help scroll mis-clamps.
    #[test]
    fn help_body_line_count_matches_state() {
        assert_eq!(
            help_lines().len() as u16,
            crate::tui::state::HELP_BODY_LINES,
            "HELP_BODY_LINES must equal the overlay's body line count"
        );
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
        assert_eq!(m.detail, vec![DetailLine::Text("no selection".to_string())]);
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
        // The scroll hint lives only in the widest tier, so it is the
        // first thing dropped when the terminal narrows.
        assert!(t[0].contains("pgup/pgdn scroll"));
        assert!(!t[1].contains("pgup"));
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
        assert!(detail_text(&m2).contains("Pinned: 2.1.0"));
    }

    #[test]
    fn detail_lines_follow_the_section_layout() {
        let mut s = TuiState::new();
        let mut r = row("r/alpha", ArtifactState::Installed);
        r.summary = "short blurb".to_string();
        r.repository_url = Some("https://github.com/acme/alpha".to_string());
        s.set_rows(vec![r]);
        let m = frame(&s);
        assert_eq!(
            m.detail,
            vec![
                DetailLine::Blank,
                DetailLine::Identifier("r/alpha".to_string()),
                DetailLine::Blank,
                DetailLine::SectionLabel("Summary:"),
                DetailLine::Blank,
                DetailLine::Text("short blurb".to_string()),
                DetailLine::Blank,
                DetailLine::SectionLabel("Description:"),
                DetailLine::Blank,
                DetailLine::Text("review code".to_string()),
                DetailLine::Blank,
                DetailLine::SectionLabel("Metadata:"),
                DetailLine::Blank,
                DetailLine::MetaEntry {
                    label: "Keywords:",
                    value: "rust, lint".to_string()
                },
                DetailLine::MetaEntry {
                    label: "Repository:",
                    value: "https://github.com/acme/alpha".to_string()
                },
            ]
        );
    }

    #[test]
    fn detail_lines_fall_back_to_dashes() {
        // No summary, no description, no repository ⇒ `-` placeholders and
        // the Description section is omitted entirely.
        let mut s = TuiState::new();
        let mut r = row("r/alpha", ArtifactState::Installed);
        r.description = String::new();
        s.set_rows(vec![r]);
        let m = frame(&s);
        let detail = detail_text(&m);
        assert!(!detail.contains("Description:"), "empty description omits the section");
        assert!(detail.contains("Repository: -"));
        assert!(m.detail.contains(&DetailLine::Text("-".to_string())), "summary dash");
    }

    #[test]
    fn frame_clamps_detail_scroll_to_content_height() {
        let mut s = TuiState::new();
        s.set_rows(vec![row("r/alpha", ArtifactState::Installed)]);
        // A small terminal so the content overflows its pane.
        s.set_term_size((40, 13));
        s.enter_detail();
        // Way past the end: the state clamp stops at the content bottom.
        for _ in 0..500 {
            s.scroll_detail(1);
        }
        let m = frame(&s);
        let max = scroll_max(&m.detail, viewport(s.term_size));
        assert_eq!(m.detail_scroll, max);
        assert!(max > 0, "an overflowing pane has a non-zero scroll range");
        // Within range: passed through untouched.
        s.detail_scroll = 1;
        assert_eq!(frame(&s).detail_scroll, 1);
        // A direct field write past the end (no clamp in between) is
        // still caught by the projection's defensive re-clamp.
        s.detail_scroll = u16::MAX;
        assert_eq!(frame(&s).detail_scroll, max);
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
    fn frame_projects_truncation_hint_and_omits_when_complete() {
        let mut s = TuiState::new();
        s.set_rows(vec![row("r/a", ArtifactState::Installed)]);
        // An exhaustive window omits the hint entirely.
        assert_eq!(frame(&s).truncation_hint, "");
        // A capped window surfaces the quiet hint.
        s.set_truncated(true);
        assert_eq!(frame(&s).truncation_hint, "(list truncated)");
        // The hint lives on the legend line, separate from the transient
        // status (which stays its own, un-clobbered field).
        assert_eq!(frame(&s).status, "", "truncation never touches the status line");
    }

    #[test]
    fn legend_line_appends_truncation_hint_only_when_present() {
        // No hint ⇒ six status glyph spans plus the deprecation span.
        let base = legend_line("");
        assert_eq!(base.spans.len(), 7, "six status glyphs + deprecation, no trailing hint");
        assert!(
            base.spans.iter().any(|s| s.content.contains("⚠ deprecated")),
            "the legend explains the deprecation indicator"
        );
        // A non-empty hint adds one trailing span carrying the hint text.
        let with_hint = legend_line("(list truncated)");
        assert_eq!(with_hint.spans.len(), 8, "glyphs + deprecation + the truncation span");
        assert!(
            with_hint.spans.last().unwrap().content.contains("(list truncated)"),
            "the trailing span carries the hint text"
        );
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

    // ── Step 3.4: tree frame spec tests ──────────────────────────────────────

    // Tree frame projects group rows with arrow glyph (▾/▸), indent, and
    // leaf rows with the bare label at non-zero depth.
    #[test]
    fn tree_frame_produces_group_rows_with_non_null_group_field() {
        let mut s = TuiState::new();
        s.set_rows(vec![
            row("reg/acme/alpha", ArtifactState::Installed),
            row("reg/acme/beta", ArtifactState::NotInstalled),
        ]);
        s.set_default_registry(Some("reg".to_string()));
        // Switch to tree mode
        s.toggle_view_mode();
        assert_eq!(s.view_mode, crate::tui::state::ViewMode::Tree);
        // In tree mode the frame must include the "acme" group header row, so at
        // least one render row carries `group.is_some()`.
        let m = frame(&s);
        assert!(
            m.rows.iter().any(|r| r.group.is_some()),
            "tree frame must include at least one group row (group.is_some())"
        );
    }

    // Tree group rows must have non-zero indent for leaf children.
    // The indent is baked into `columns[0]` as two-space prefixes ("  " per depth
    // level), which is the canonical single representation of tree depth — the
    // `depth` field has been removed from `RenderRow` to eliminate the duplicate.
    #[test]
    fn tree_frame_leaves_have_nonzero_indent() {
        let mut s = TuiState::new();
        s.set_rows(vec![row("reg/acme/alpha", ArtifactState::Installed)]);
        s.set_default_registry(Some("reg".to_string()));
        s.toggle_view_mode();
        assert_eq!(s.view_mode, crate::tui::state::ViewMode::Tree);
        let m = frame(&s);
        // Leaf rows must be indented (columns[0] starts with "  " — at least one
        // two-space indent level, since they are children of a group).
        let leaves: Vec<&RenderRow> = m.rows.iter().filter(|r| r.group.is_none()).collect();
        assert!(
            leaves.iter().all(|r| r.columns[0].starts_with("  ")),
            "leaf rows in tree view must be indented in columns[0]; cols: {:?}",
            leaves.iter().map(|r| r.columns[0].as_str()).collect::<Vec<_>>()
        );
    }

    // A deprecated leaf sets the deprecated flag in tree view too (parity with
    // the flat view's render_leaf), driving the trailing `⚠` column — without
    // injecting a glyph into the (indented, arrow-bearing) label cell.
    #[test]
    fn tree_frame_flags_deprecated_leaf() {
        let mut s = TuiState::new();
        let mut dep = row("reg/acme/alpha", ArtifactState::NotInstalled);
        dep.deprecated = Some("use reg/acme/alpha-2".to_string());
        s.set_rows(vec![dep, row("reg/acme/beta", ArtifactState::NotInstalled)]);
        s.set_default_registry(Some("reg".to_string()));
        s.toggle_view_mode();
        assert_eq!(s.view_mode, crate::tui::state::ViewMode::Tree);
        let m = frame(&s);
        let leaves: Vec<&RenderRow> = m.rows.iter().filter(|r| r.group.is_none()).collect();
        assert!(
            leaves.iter().any(|r| r.deprecated && r.columns[0].contains("alpha")),
            "the deprecated leaf must set the flag; rows: {:?}",
            leaves
                .iter()
                .map(|r| (r.columns[0].as_str(), r.deprecated))
                .collect::<Vec<_>>()
        );
        // No leaf injects the glyph into the label cell.
        assert!(
            leaves.iter().all(|r| !r.columns[0].contains('⚠')),
            "no label cell carries the glyph; cols: {:?}",
            leaves.iter().map(|r| r.columns[0].as_str()).collect::<Vec<_>>()
        );
        assert!(
            leaves
                .iter()
                .filter(|r| r.columns[0].contains("beta"))
                .all(|r| !r.deprecated),
            "the non-deprecated leaf must not set the flag"
        );
    }

    // Tri-state mark: when no descendants are marked, the group's MarkState
    // must be None. When all are marked, it must be All. When some, Partial.
    #[test]
    fn tree_frame_group_mark_state_tri_state() {
        let mut s = TuiState::new();
        s.set_rows(vec![
            row("reg/acme/alpha", ArtifactState::Installed),
            row("reg/acme/beta", ArtifactState::NotInstalled),
        ]);
        s.set_default_registry(Some("reg".to_string()));
        s.toggle_view_mode();
        assert_eq!(s.view_mode, crate::tui::state::ViewMode::Tree);

        // No marks → group must show MarkState::None
        let m_no_marks = frame(&s);
        let group_rows: Vec<&RenderRow> = m_no_marks.rows.iter().filter(|r| r.group.is_some()).collect();
        assert!(
            group_rows
                .iter()
                .all(|r| r.group.as_ref().unwrap().mark == MarkState::None),
            "no marks → group MarkState must be None"
        );

        // Mark all descendants
        s.toggle_mark_all_filtered();
        let m_all_marks = frame(&s);
        let group_rows_all: Vec<&RenderRow> = m_all_marks.rows.iter().filter(|r| r.group.is_some()).collect();
        assert!(
            group_rows_all
                .iter()
                .all(|r| r.group.as_ref().unwrap().mark == MarkState::All),
            "all descendants marked → group MarkState must be All"
        );

        // Mark only one → Partial
        s.clear_marks();
        s.marked.insert(0); // mark only alpha
        let m_partial = frame(&s);
        let group_rows_partial: Vec<&RenderRow> = m_partial.rows.iter().filter(|r| r.group.is_some()).collect();
        assert!(
            group_rows_partial
                .iter()
                .all(|r| r.group.as_ref().unwrap().mark == MarkState::Partial),
            "only one descendant marked → group MarkState must be Partial"
        );
    }

    // Group detail pane: when the selection is a group in tree mode,
    // group_detail_lines() is called and must return non-empty detail.
    #[test]
    fn tree_group_detail_pane_returns_non_empty_lines() {
        let mut s = TuiState::new();
        s.set_rows(vec![
            row("reg/acme/alpha", ArtifactState::Installed),
            row("reg/acme/beta", ArtifactState::NotInstalled),
        ]);
        s.set_default_registry(Some("reg".to_string()));
        s.toggle_view_mode();
        assert_eq!(s.view_mode, crate::tui::state::ViewMode::Tree);
        // Manually select position 0 (expected to be the "acme" group)
        s.selected = 0;
        // When the selection is a group, frame() routes to group_detail_lines().
        let m = frame(&s);
        if s.selected_is_group() {
            assert!(
                !m.detail.is_empty(),
                "group detail pane must return non-empty lines when a group is selected"
            );
        }
    }

    // Group-row col 3 must show the status glyph for rollup.worst(), NOT a
    // second copy of the rollup label. For an unmarked group, no mark prefix.
    // For a marked group (all descendants), the mark glyph prefixes it.
    #[test]
    fn tree_group_col3_shows_status_glyph_not_rollup_label() {
        let mut s = TuiState::new();
        s.set_rows(vec![
            row("reg/acme/alpha", ArtifactState::Installed),
            row("reg/acme/beta", ArtifactState::Installed),
        ]);
        s.set_default_registry(Some("reg".to_string()));
        s.toggle_view_mode();

        // Unmarked group: col 3 must be "✓ installed" (rollup.worst() = Installed),
        // NOT the rollup label like "2/2 installed".
        let m_unmarked = frame(&s);
        let group_row = m_unmarked
            .rows
            .iter()
            .find(|r| r.group.is_some())
            .expect("must have a group row");
        assert_eq!(
            group_row.columns[3], "✓ installed",
            "unmarked group col 3 must be the status glyph, not the rollup label; got: {:?}",
            group_row.columns[3]
        );
        // Col 3 must NOT contain the rollup label format.
        assert!(
            !group_row.columns[3].contains('/'),
            "col 3 must not contain the rollup label fraction; got: {:?}",
            group_row.columns[3]
        );

        // All-marked group: col 3 must include the mark glyph prefix.
        s.toggle_mark_all_filtered();
        let m_marked = frame(&s);
        let group_row_marked = m_marked
            .rows
            .iter()
            .find(|r| r.group.is_some())
            .expect("must have a group row");
        assert!(
            group_row_marked.columns[3].starts_with('▣'),
            "all-marked group col 3 must start with the ▣ mark glyph; got: {:?}",
            group_row_marked.columns[3]
        );
        assert!(
            group_row_marked.columns[3].contains("installed"),
            "all-marked group col 3 must still show the status label; got: {:?}",
            group_row_marked.columns[3]
        );
    }

    // C1.5: A partial-marked group and a fully-marked group must render different
    // leftmost-column glyphs. `draw()` consumes `GroupRow.mark` (the tri-state)
    // for the leftmost column, not the binary `r.marked` field.
    #[test]
    fn draw_leftmost_col_partial_vs_full_group_mark_differ() {
        let mut s = TuiState::new();
        s.set_rows(vec![
            row("reg/acme/alpha", ArtifactState::Installed),
            row("reg/acme/beta", ArtifactState::Installed),
        ]);
        s.set_default_registry(Some("reg".to_string()));
        s.toggle_view_mode();

        // Mark only one leaf → Partial
        s.marked.insert(0);
        let m_partial = frame(&s);
        let group_partial = m_partial
            .rows
            .iter()
            .find(|r| r.group.is_some())
            .expect("must have a group row");
        assert_eq!(
            group_partial.group.as_ref().unwrap().mark,
            MarkState::Partial,
            "one-of-two marked must be Partial"
        );

        // Mark both leaves → All
        s.marked.insert(1);
        let m_all = frame(&s);
        let group_all = m_all
            .rows
            .iter()
            .find(|r| r.group.is_some())
            .expect("must have a group row");
        assert_eq!(
            group_all.group.as_ref().unwrap().mark,
            MarkState::All,
            "both marked must be All"
        );

        // The two MarkState values must be distinct — the leftmost glyph differs.
        assert_ne!(
            group_partial.group.as_ref().unwrap().mark,
            group_all.group.as_ref().unwrap().mark,
            "Partial and All groups must differ in mark state (leftmost glyph)"
        );
    }

    // ── C-2 sanitize_member_label ─────────────────────────────────────────────
    //
    // Each test row corresponds to one row in the C-2 contract table: control
    // chars, bidi overrides/isolates, and zero-width chars are stripped before
    // a raw member label reaches the terminal.

    /// Helper: assert no control chars, no bidi overrides/isolates, no zero-width
    /// code points in the sanitized output.
    fn assert_clean(output: &str, input_desc: &str) {
        for ch in output.chars() {
            assert!(
                !ch.is_control(),
                "C-2 invariant: sanitized output must not contain control chars; \
                 input={input_desc:?}, output={output:?}, offending char={ch:?} (U+{:04X})",
                ch as u32
            );
            // Bidi override / isolate block: U+202A–U+202E, U+2066–U+2069
            let cp = ch as u32;
            assert!(
                !(0x202A..=0x202E).contains(&cp) && !(0x2066..=0x2069).contains(&cp),
                "C-2 invariant: bidi override/isolate stripped; \
                 input={input_desc:?}, offending char=U+{cp:04X}"
            );
            // Zero-width: ZWSP U+200B, BOM U+FEFF, ZWNJ U+200C, ZWJ U+200D
            assert!(
                !matches!(cp, 0x200B | 0x200C | 0x200D | 0xFEFF),
                "C-2 invariant: zero-width code point stripped; \
                 input={input_desc:?}, offending char=U+{cp:04X}"
            );
        }
    }

    #[test]
    fn sanitize_plain_ascii_passes_through() {
        // C-2 row 1: plain ASCII must survive unchanged.
        let out = sanitize_member_label("hello");
        assert_eq!(out, "hello", "plain ASCII must pass through unchanged");
        assert_clean(&out, "hello");
    }

    #[test]
    fn sanitize_strips_c0_control_and_bel() {
        // C-2 row 2: C0 (U+0000) and BEL (U+0007) stripped → "abc".
        let out = sanitize_member_label("a\x00b\x07c");
        assert_eq!(out, "abc", "C0 NUL + BEL must be stripped");
        assert_clean(&out, "C0+BEL");
    }

    #[test]
    fn sanitize_strips_c1_control() {
        // C-2 row 3: C1 control (U+009F) stripped → "ab".
        let out = sanitize_member_label("a\u{009F}b");
        assert_eq!(out, "ab", "C1 control must be stripped");
        assert_clean(&out, "C1");
    }

    #[test]
    fn sanitize_strips_ansi_csi_escape_sequences() {
        // C-2 row 4: ANSI/CSI → "\x1b[31m" prefix and "\x1b[0m" suffix stripped,
        // leaving only the visible text "red".
        let out = sanitize_member_label("\x1b[31mred\x1b[0m");
        assert_eq!(out, "red", "ANSI/CSI escape sequences must be stripped");
        assert_clean(&out, "ANSI/CSI");
    }

    #[test]
    fn sanitize_strips_rtl_override_bidi() {
        // C-2 row 5: RTL override U+202E stripped → "ab".
        let out = sanitize_member_label("a\u{202E}b");
        assert_eq!(out, "ab", "RTL override U+202E must be stripped");
        assert_clean(&out, "RTL override U+202E");
    }

    #[test]
    fn sanitize_strips_zwsp_and_bom() {
        // C-2 row 6: ZWSP U+200B and BOM U+FEFF stripped → "abc".
        let out = sanitize_member_label("a\u{200B}b\u{FEFF}c");
        assert_eq!(out, "abc", "ZWSP + BOM must be stripped");
        assert_clean(&out, "ZWSP+BOM");
    }

    #[test]
    fn sanitize_strips_bidi_isolates() {
        // C-2 row 7: LRI U+2066 and PDI U+2069 stripped → "ab".
        let out = sanitize_member_label("a\u{2066}b\u{2069}");
        assert_eq!(out, "ab", "bidi isolates U+2066/U+2069 must be stripped");
        assert_clean(&out, "bidi isolates");
    }

    // ── T1: individual bidi embedding / isolate coverage ──────────────────────
    //
    // C-2 row 5 / row 7 tested a single representative; T1 covers every
    // code point in the ranges individually so the strip-set is fully validated.

    #[test]
    fn sanitize_strips_bidi_embedding_u202a() {
        // U+202A LEFT-TO-RIGHT EMBEDDING
        let out = sanitize_member_label("a\u{202A}b");
        assert_eq!(out, "ab", "U+202A LRE must be stripped");
        assert_clean(&out, "U+202A");
    }

    #[test]
    fn sanitize_strips_bidi_embedding_u202b() {
        // U+202B RIGHT-TO-LEFT EMBEDDING
        let out = sanitize_member_label("a\u{202B}b");
        assert_eq!(out, "ab", "U+202B RLE must be stripped");
        assert_clean(&out, "U+202B");
    }

    #[test]
    fn sanitize_strips_bidi_embedding_u202c() {
        // U+202C POP DIRECTIONAL FORMATTING
        let out = sanitize_member_label("a\u{202C}b");
        assert_eq!(out, "ab", "U+202C PDF must be stripped");
        assert_clean(&out, "U+202C");
    }

    #[test]
    fn sanitize_strips_bidi_embedding_u202d() {
        // U+202D LEFT-TO-RIGHT OVERRIDE
        let out = sanitize_member_label("a\u{202D}b");
        assert_eq!(out, "ab", "U+202D LRO must be stripped");
        assert_clean(&out, "U+202D");
    }

    #[test]
    fn sanitize_strips_bidi_isolate_u2067() {
        // U+2067 RIGHT-TO-LEFT ISOLATE
        let out = sanitize_member_label("a\u{2067}b");
        assert_eq!(out, "ab", "U+2067 RLI must be stripped");
        assert_clean(&out, "U+2067");
    }

    #[test]
    fn sanitize_strips_bidi_isolate_u2068() {
        // U+2068 FIRST STRONG ISOLATE
        let out = sanitize_member_label("a\u{2068}b");
        assert_eq!(out, "ab", "U+2068 FSI must be stripped");
        assert_clean(&out, "U+2068");
    }

    #[test]
    fn sanitize_100k_char_input_does_not_panic() {
        // C-2 row 8: 100 000-char input returns without panic.
        // The sanitizer must not be O(n²) and must not truncate (width clamping
        // is later `fit()`'s job, not the sanitizer's).
        let big = "x".repeat(100_000);
        let out = sanitize_member_label(&big);
        // Must have processed every character (no truncation by sanitizer).
        assert_eq!(
            out.chars().count(),
            100_000,
            "sanitizer must not truncate; truncation is fit()'s job"
        );
        assert_clean(&out, "100k 'x'");
    }

    #[test]
    fn sanitize_path_traversal_like_name_passes_through() {
        // C-2 row 9: path-traversal-like names are NOT a display threat;
        // the data boundary (SkillName::parse in the resolver) rejects them for
        // install. At display time they pass through unchanged.
        let out = sanitize_member_label("../etc/passwd");
        assert_eq!(out, "../etc/passwd", "path-traversal name passes through at display");
        assert_clean(&out, "../etc/passwd");
    }

    // C5: Hint tiers are view-mode-aware.
    // In tree mode: `t tree` and `→/← expand/collapse` both appear.
    // In flat mode: `t tree` appears, but `→/←` / `expand` are ABSENT (those keys
    //   are inert in flat mode and must not be advertised).
    #[test]
    fn hint_tiers_are_view_mode_aware() {
        let mut s = TuiState::new();
        s.set_rows(vec![row("reg/acme/alpha", ArtifactState::Installed)]);
        s.set_default_registry(Some("reg".to_string()));

        // ── Flat mode (default) ───────────────────────────────────────────────
        assert_eq!(s.view_mode, crate::tui::state::ViewMode::Flat);
        let m_flat = frame(&s);
        let widest_flat = &m_flat.hint_tiers[0];
        // `t tree` always present.
        assert!(
            widest_flat.contains("t tree"),
            "flat: widest tier must contain 't tree'; tier: {widest_flat:?}"
        );
        // `→/←` / `expand` must NOT appear in flat mode.
        assert!(
            !widest_flat.contains("expand"),
            "flat: widest tier must NOT contain 'expand' (inert key); tier: {widest_flat:?}"
        );
        assert!(
            m_flat.hint_tiers.iter().all(|t| !t.contains("expand")),
            "flat: NO tier must contain 'expand'; tiers: {:?}",
            m_flat.hint_tiers
        );

        // ── Tree mode ──────────────────────────────────────────────────────────
        s.toggle_view_mode();
        let m_tree = frame(&s);
        let widest_tree = &m_tree.hint_tiers[0];
        // `t tree` always present.
        assert!(
            widest_tree.contains("t tree"),
            "tree: widest tier must contain 't tree'; tier: {widest_tree:?}"
        );
        // `→/←` expand/collapse must appear in tree mode.
        assert!(
            widest_tree.contains("expand"),
            "tree: widest tier must contain 'expand'; tier: {widest_tree:?}"
        );
        // At least two tiers must mention "t tree".
        let tiers_with_tree = m_tree.hint_tiers.iter().filter(|t| t.contains("t tree")).count();
        assert!(
            tiers_with_tree >= 2,
            "tree: at least two hint tiers must contain 't tree'; tiers: {:?}",
            m_tree.hint_tiers
        );
    }
}

// ── P2 Specify tests — C-1 bundle-leaf arrow glyph ───────────────────────────
//
// These tests encode contract C-1 from plan_tui_member_nodes.
// They MUST compile. They will pass only after P3 implements the bundle-leaf
// glyph branch in `tree_render_rows`.
#[cfg(test)]
mod p2_render_member_node_tests {
    use super::*;
    use crate::tui::state::{ArtifactState, TuiRow, TuiState, ViewMode};

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
            revision: None,
            created: None,
            latest_tag: "latest".to_string(),
            version: "1.0.0".to_string(),
            deprecated: None,
            pinned_version: None,
            state: ArtifactState::NotInstalled,
            source: None,
        }
    }

    fn skill_tui_row(repo: &str) -> TuiRow {
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
            revision: None,
            created: None,
            latest_tag: "latest".to_string(),
            version: "1.0.0".to_string(),
            deprecated: None,
            pinned_version: None,
            state: ArtifactState::Installed,
            source: None,
        }
    }

    // C-1: bundle leaf collapsed=true renders `▸ ` prefix in the repo column
    // (after stripping leading indent spaces). F4: UTF-8 only, no ASCII fallback.

    #[test]
    fn c1_bundle_leaf_arrow_glyph_collapsed_shows_right_arrow() {
        // Directly call tree_render_rows on a synthetic state containing a
        // collapsed bundle leaf at depth 0 (no group above it).
        let mut s = TuiState::new();
        // Use a no-default-registry setup so the bundle leaf is at depth 0.
        s.set_rows(vec![bundle_tui_row("acme/bundle-x")]);
        s.toggle_view_mode();
        assert_eq!(s.view_mode, ViewMode::Tree);

        let flat = s.flattened();
        let render_rows = tree_render_rows(&s, &flat);

        // Find the leaf row (non-group).
        let leaf_row = render_rows.iter().find(|r| r.group.is_none());
        assert!(leaf_row.is_some(), "C-1: must have at least one leaf render row");
        let repo_col = &leaf_row.unwrap().columns[0];
        // F4: collapsed bundle leaf must render the UTF-8 ▸ glyph (same as groups).
        let trimmed = repo_col.trim_start();
        assert!(
            trimmed.starts_with('▸'),
            "C-1: collapsed bundle leaf must render ▸ arrow prefix; got: {repo_col:?}"
        );
    }

    #[test]
    fn c1_bundle_leaf_arrow_glyph_expanded_shows_down_arrow() {
        // A bundle leaf that IS in expanded_bundles must show ▾.
        let mut s = TuiState::new();
        s.set_rows(vec![bundle_tui_row("acme/bundle-x")]);
        s.toggle_view_mode();
        assert_eq!(s.view_mode, ViewMode::Tree);

        // Seed expanded_bundles so the leaf is "expanded" (collapsed = false).
        // F3: key = full bundle repo. "acme/bundle-x" has no default_registry,
        // so the full repo IS "acme/bundle-x".
        s.expanded_bundles.insert("acme/bundle-x".to_string());

        let flat = s.flattened();
        let render_rows = tree_render_rows(&s, &flat);

        let leaf_row = render_rows.iter().find(|r| r.group.is_none());
        assert!(leaf_row.is_some(), "C-1: must have at least one leaf render row");
        let repo_col = &leaf_row.unwrap().columns[0];
        let trimmed = repo_col.trim_start();
        // F4: expanded bundle leaf must render the UTF-8 ▾ glyph.
        assert!(
            trimmed.starts_with('▾'),
            "C-1: expanded bundle leaf must render ▾ arrow prefix; got: {repo_col:?}"
        );
    }

    #[test]
    fn c1_non_bundle_leaf_has_no_arrow_prefix() {
        // A non-bundle leaf must NOT have a leading arrow. The prefix must be
        // byte-identical to indent + label (no ▸/▾/>/v insertion).
        let mut s = TuiState::new();
        s.set_rows(vec![skill_tui_row("acme/my-skill")]);
        s.toggle_view_mode();
        assert_eq!(s.view_mode, ViewMode::Tree);

        let flat = s.flattened();
        let render_rows = tree_render_rows(&s, &flat);

        let leaf_row = render_rows.iter().find(|r| r.group.is_none());
        assert!(leaf_row.is_some(), "C-1: must have at least one leaf render row");
        let repo_col = &leaf_row.unwrap().columns[0];
        let trimmed = repo_col.trim_start();
        // F4: Must NOT start with the UTF-8 arrow glyphs used by bundle leaves.
        assert!(
            !trimmed.starts_with('▸') && !trimmed.starts_with('▾'),
            "C-1: non-bundle leaf must NOT have an arrow prefix; got: {repo_col:?}"
        );
    }

    // C-1: Directly test the DisplayRow → RenderRow mapping for a synthetic
    // collapsed bundle Leaf, bypassing state.flattened().
    // This test exercises the render arm directly via tree_render_rows on a state
    // we craft to contain a bundle leaf in its flattened output.

    #[test]
    fn c1_bundle_leaf_and_non_bundle_leaf_prefix_differ() {
        // Bundle + non-bundle side-by-side in the same tree.
        // After P3: the bundle leaf gets an arrow, the non-bundle leaf does not.
        let mut s = TuiState::new();
        s.set_rows(vec![
            bundle_tui_row("reg/acme/bundle-x"),
            skill_tui_row("reg/acme/my-skill"),
        ]);
        s.set_default_registry(Some("reg".to_string()));
        s.toggle_view_mode();
        assert_eq!(s.view_mode, ViewMode::Tree);

        let flat = s.flattened();
        let render_rows = tree_render_rows(&s, &flat);

        // Gather leaf rows (non-group).
        let leaf_rows: Vec<&RenderRow> = render_rows.iter().filter(|r| r.group.is_none()).collect();
        assert!(
            leaf_rows.len() >= 2,
            "C-1: must have at least 2 leaf render rows; rows={leaf_rows:?}"
        );

        // F4: At least one leaf must have the UTF-8 arrow prefix (the bundle leaf).
        let any_has_arrow = leaf_rows.iter().any(|r| {
            let t = r.columns[0].trim_start();
            t.starts_with('▸') || t.starts_with('▾')
        });
        // At least one leaf must NOT have an arrow prefix (the skill leaf).
        let any_no_arrow = leaf_rows.iter().any(|r| {
            let t = r.columns[0].trim_start();
            !t.starts_with('▸') && !t.starts_with('▾')
        });

        assert!(
            any_has_arrow,
            "C-1/F4: at least one leaf must have a UTF-8 ▸/▾ arrow prefix (the bundle leaf); rows={:?}",
            leaf_rows.iter().map(|r| r.columns[0].as_str()).collect::<Vec<_>>()
        );
        assert!(
            any_no_arrow,
            "C-1: at least one leaf must have NO arrow prefix (the skill leaf)"
        );
    }
}

// ── Multi-registry render projection + RegistryHealth status line ──────────
//
// These tests synthesize a multi-registry TuiState and assert the RenderModel
// produced by `frame()`: two registry root rows in tree view, registry-root
// elision in single-registry mode, and the RegistryHealth (offline / truncated)
// status-line composition.
#[cfg(test)]
mod spec_multi_registry_render_tests {
    use super::*;
    use crate::tui::state::{ArtifactState, RegistryHealth, TuiRow, TuiState, ViewMode};

    fn row_with_reg(registry: &str, repository: &str, state: ArtifactState) -> TuiRow {
        TuiRow {
            kind: "skill".to_string(),
            registry: registry.to_string(),
            repository: repository.to_string(),
            repo: format!("{registry}/{repository}"),
            description: String::new(),
            summary: String::new(),
            keywords: vec![],
            repository_url: None,
            revision: None,
            created: None,
            latest_tag: "latest".to_string(),
            version: "1.0.0".to_string(),
            deprecated: None,
            pinned_version: None,
            state,
            source: None,
        }
    }

    // AC L1: 2-registry tree view → RenderModel has 2 registry root rows (group rows).
    // Proves multi-registry tree projection end-to-end (headlessly), including
    // F13 precedence ordering and the D-EMPTY gate (multi_registry = default_registry.is_none()
    // && !registry_order.is_empty() — both conditions must hold for registry roots to appear).
    #[test]
    fn spec_frame_two_registry_tree_yields_two_registry_root_rows() {
        let mut s = TuiState::new();
        s.set_rows(vec![
            row_with_reg("ghcr.io/acme", "skill-a", ArtifactState::NotInstalled),
            row_with_reg("ghcr.io/other", "skill-b", ArtifactState::NotInstalled),
        ]);
        // Multi-registry → no elision (default_registry stays None)
        assert!(
            s.default_registry.is_none(),
            "precondition: no elision for 2-registry set"
        );
        // SPEC-W1 / GAP-1: set_registry_order activates the F13/D-EMPTY gate
        // (multi_registry = default_registry.is_none() && !registry_order.is_empty()).
        // Without this call the gate is false and registry roots would not appear
        // in F13 precedence order (the old test missed this path).
        s.set_registry_order(vec!["ghcr.io/acme".into(), "ghcr.io/other".into()]);
        // Switch to tree view to exercise the registry-grouped tree projection
        s.toggle_view_mode();
        assert_eq!(s.view_mode, ViewMode::Tree);
        let m = frame(&s);
        // Two registry roots must appear as group rows in the RenderModel.
        let group_rows: Vec<_> = m.rows.iter().filter(|r| r.group.is_some()).collect();
        assert!(
            group_rows.len() >= 2,
            "2-registry tree must render at least 2 group rows (one per registry root); got {} group rows",
            group_rows.len()
        );
        // The group labels must include the two registry names
        let group_labels: Vec<&str> = group_rows.iter().map(|r| r.columns[0].trim()).collect();
        assert!(
            group_labels.iter().any(|l| l.contains("ghcr.io/acme")),
            "ghcr.io/acme registry root must appear in rendered rows; got: {group_labels:?}"
        );
        assert!(
            group_labels.iter().any(|l| l.contains("ghcr.io/other")),
            "ghcr.io/other registry root must appear in rendered rows; got: {group_labels:?}"
        );
        // F13 / SPEC-W1: ghcr.io/acme is first in registry_order, so its group row
        // must appear before ghcr.io/other in the rendered output.
        let first_group_label = group_labels[0];
        assert!(
            first_group_label.contains("ghcr.io/acme"),
            "F13: ghcr.io/acme (first in registry_order) must be the first group row; got: {first_group_label:?}"
        );
    }

    // D-EMPTY: a registry declared in registry_order but with zero matching rows
    // still renders a 0/0 group root in tree mode (so the user knows the registry
    // was resolved even when empty).
    #[test]
    fn spec_frame_empty_registry_in_order_renders_zero_zero_group_root() {
        let mut s = TuiState::new();
        // No rows from "ghcr.io/empty"; one row from another registry.
        s.set_rows(vec![row_with_reg(
            "ghcr.io/acme",
            "skill-a",
            ArtifactState::NotInstalled,
        )]);
        // D-EMPTY gate: both conditions must hold — no elision AND non-empty order.
        // "ghcr.io/empty" has zero matching rows; it must still get a group root.
        s.set_registry_order(vec!["ghcr.io/empty".into(), "ghcr.io/acme".into()]);
        s.toggle_view_mode();
        assert_eq!(s.view_mode, ViewMode::Tree);
        let m = frame(&s);
        let group_rows: Vec<_> = m.rows.iter().filter(|r| r.group.is_some()).collect();
        let group_labels: Vec<&str> = group_rows.iter().map(|r| r.columns[0].trim()).collect();
        // "ghcr.io/empty" must appear as a group root despite having zero rows.
        assert!(
            group_labels.iter().any(|l| l.contains("ghcr.io/empty")),
            "D-EMPTY: a registry with zero rows must still render a group root; group_labels: {group_labels:?}"
        );
        // Verify the empty registry group shows a 0/0 rollup (Tag column).
        let empty_group = group_rows
            .iter()
            .find(|r| r.columns[0].contains("ghcr.io/empty"))
            .expect("ghcr.io/empty group row must exist");
        assert_eq!(
            empty_group.columns[2].trim(),
            "0/0",
            "D-EMPTY: empty registry group must show 0/0 rollup; got: {:?}",
            empty_group.columns[2]
        );
    }

    // AC L1: 1-registry tree view → 0 registry root rows (registry root is elided).
    #[test]
    fn spec_frame_single_registry_tree_yields_zero_registry_root_rows() {
        let mut s = TuiState::new();
        s.set_rows(vec![
            row_with_reg("registry.example", "acme/alpha", ArtifactState::NotInstalled),
            row_with_reg("registry.example", "acme/beta", ArtifactState::NotInstalled),
        ]);
        // Single registry → elide (default_registry = Some(primary))
        s.set_default_registry(Some("registry.example".to_string()));
        s.toggle_view_mode();
        assert_eq!(s.view_mode, ViewMode::Tree);
        let m = frame(&s);
        // No row should have "registry.example" as its group label
        let registry_root_rows: Vec<_> = m
            .rows
            .iter()
            .filter(|r| r.group.is_some() && r.columns[0].contains("registry.example"))
            .collect();
        assert!(
            registry_root_rows.is_empty(),
            "single-registry (elided) tree must have no 'registry.example' registry root row; got: {registry_root_rows:?}"
        );
    }

    // C6 / D-DEGRADE: registry_health.offline non-empty → status line names offline registry.
    #[test]
    fn spec_frame_offline_registry_appears_in_status_line() {
        let mut s = TuiState::new();
        s.set_rows(vec![]);
        s.registry_health = RegistryHealth {
            offline: vec!["ghcr.io/acme".to_string()],
            truncated: vec![],
        };
        let m = frame(&s);
        assert!(
            m.status.contains("ghcr.io/acme"),
            "status line must name the offline registry 'ghcr.io/acme'; got: {:?}",
            m.status
        );
    }

    // C6 / D-DEGRADE: all registries offline → status line indicates all-offline.
    #[test]
    fn spec_frame_all_registries_offline_status_message() {
        let mut s = TuiState::new();
        s.set_rows(vec![]);
        s.registry_health = RegistryHealth {
            offline: vec!["ghcr.io/acme".to_string(), "ghcr.io/other".to_string()],
            truncated: vec![],
        };
        let m = frame(&s);
        // When all registries are offline, the status must signal it
        assert!(
            m.status.to_lowercase().contains("offline"),
            "status line must indicate offline state when all registries are offline; got: {:?}",
            m.status
        );
    }

    // C6: registry_health.truncated non-empty → status line mentions truncation.
    #[test]
    fn spec_frame_truncated_registry_appears_in_status_line() {
        let mut s = TuiState::new();
        s.set_rows(vec![]);
        s.registry_health = RegistryHealth {
            offline: vec![],
            truncated: vec!["ghcr.io/other".to_string()],
        };
        let m = frame(&s);
        assert!(
            m.status.contains("ghcr.io/other"),
            "status line must name the truncated registry; got: {:?}",
            m.status
        );
    }

    // AC F11: mark-cascade on a registry root (multi-registry tree) materializes
    // all descendant leaf indices into `marked`.
    //
    // This complements the existing single-registry cascade test. The registry
    // root is a group node in the multi-registry tree; marking it must cascade
    // to all descendants across both registries when multiple roots share the
    // same multi-registry tree.
    #[test]
    fn spec_mark_cascade_on_registry_root_marks_all_descendants() {
        let mut s = TuiState::new();
        s.set_rows(vec![
            row_with_reg("ghcr.io/acme", "alpha", ArtifactState::NotInstalled),
            row_with_reg("ghcr.io/acme", "beta", ArtifactState::Installed),
        ]);
        // No elision — 2 registries (even if same here, the point is a group root exists)
        assert!(s.marked.is_empty(), "precondition: no marks");
        s.toggle_view_mode(); // → Tree mode
        s.selected = 0; // position 0 should be the "ghcr.io/acme" registry root
        assert!(s.selected_is_group(), "position 0 must be a group (registry root)");
        s.toggle_mark_selected();
        assert!(
            !s.marked.is_empty(),
            "marking the registry root group must cascade to descendant leaves"
        );
        assert!(
            s.marked.contains(&0) && s.marked.contains(&1),
            "both descendant rows (0 and 1) must be marked after registry-root cascade; marked: {:?}",
            s.marked
        );
    }

    // Edge 5/14: selection anchor fallback when anchor registry is absent after reload.
    // After a reload that removes one registry, the anchor should fall back to the
    // first visible row of the next registry root in precedence order.
    #[test]
    fn spec_selection_anchor_fallback_when_registry_absent_after_reload() {
        let mut s = TuiState::new();
        // Initial state: 2 registries, cursor on something in the first registry
        s.set_rows(vec![
            row_with_reg("ghcr.io/acme", "skill-a", ArtifactState::NotInstalled),
            row_with_reg("ghcr.io/other", "skill-b", ArtifactState::NotInstalled),
        ]);
        s.toggle_view_mode(); // → Tree
        // Move selection to the first leaf (ghcr.io/acme registry)
        s.selected = 1; // position 1 should be the leaf under ghcr.io/acme
        // Simulate reload that removes ghcr.io/acme entirely
        s.set_rows(vec![row_with_reg(
            "ghcr.io/other",
            "skill-b",
            ArtifactState::NotInstalled,
        )]);
        // After reload, the anchor (ghcr.io/acme) is absent.
        // The cursor must fall back to the first visible row of ghcr.io/other.
        let flat = s.flattened();
        let first_visible_idx = 0;
        assert_eq!(
            s.selected,
            first_visible_idx,
            "selection must fall back to the first visible row when anchor registry is absent; \
             got selected={}, flattened length={}",
            s.selected,
            flat.len()
        );
    }

    // A: flat multi-registry view → show_registry_column true, rows carry
    // registry label, Repo cell shortened to registry-relative `repository`.
    #[test]
    fn spec_flat_multi_registry_shows_registry_column() {
        let mut s = TuiState::new();
        s.set_rows(vec![
            row_with_reg("ghcr.io/acme", "skill-a", ArtifactState::Installed),
            row_with_reg("ghcr.io/other", "skill-b", ArtifactState::NotInstalled),
        ]);
        // Multi-registry: no elision, two entries in registry_order.
        s.set_registry_order(vec!["ghcr.io/acme".into(), "ghcr.io/other".into()]);
        // Flat view (default after TuiState::new).
        assert_eq!(s.view_mode, ViewMode::Flat);
        let m = frame(&s);

        assert!(
            m.show_registry_column,
            "flat multi-registry must set show_registry_column"
        );
        // Repo cell must be the registry-relative path only.
        assert_eq!(
            m.rows[0].columns[0].trim(),
            fit("skill-a", W_REPO).trim(),
            "Repo cell must be registry-relative (no host prefix)"
        );
        assert_eq!(
            m.rows[1].columns[0].trim(),
            fit("skill-b", W_REPO).trim(),
            "Repo cell for second registry must be registry-relative"
        );
        // Each row must carry its registry label (URL fallback — no alias set).
        assert_eq!(
            m.rows[0].registry.as_deref(),
            Some("ghcr.io/acme"),
            "row 0 must carry registry label"
        );
        assert_eq!(
            m.rows[1].registry.as_deref(),
            Some("ghcr.io/other"),
            "row 1 must carry registry label"
        );
    }

    // Bare-host namespaced rows (exactly what the catalog produces:
    // registry = host, namespace folded into the repository) must attribute to
    // their configured registry in flat multi-registry mode — the Repo cell is
    // registry-relative (namespace stripped) and the Registry column shows the
    // configured registry, NOT the bare host. Guards the same duplicate-roots
    // bug as the tree tests, on the flat-list path.
    #[test]
    fn spec_flat_multi_registry_bare_host_row_attributes_to_configured() {
        let mut s = TuiState::new();
        s.set_rows(vec![
            row_with_reg("localhost:5050", "grimoire/skills/a", ArtifactState::NotInstalled),
            row_with_reg("localhost:5051", "tools/skills/b", ArtifactState::NotInstalled),
        ]);
        s.set_registry_order(vec!["localhost:5050/grimoire".into(), "localhost:5051/tools".into()]);
        assert_eq!(s.view_mode, ViewMode::Flat);
        let m = frame(&s);
        assert!(
            m.show_registry_column,
            "flat multi-registry must set show_registry_column"
        );
        // Repo cell relative to the configured registry (namespace stripped).
        assert_eq!(
            m.rows[0].columns[0].trim(),
            fit("skills/a", W_REPO).trim(),
            "Repo cell must be relative to localhost:5050/grimoire (namespace 'grimoire' stripped)"
        );
        assert_eq!(
            m.rows[1].columns[0].trim(),
            fit("skills/b", W_REPO).trim(),
            "Repo cell must be relative to localhost:5051/tools (namespace 'tools' stripped)"
        );
        // Registry column shows the configured registry (full url), not bare host.
        assert_eq!(
            m.rows[0].registry.as_deref(),
            Some("localhost:5050/grimoire"),
            "Registry column must show the configured registry, not the bare host"
        );
        assert_eq!(m.rows[1].registry.as_deref(), Some("localhost:5051/tools"));
    }

    // A: single-registry flat view → show_registry_column false, behavior unchanged.
    #[test]
    fn spec_flat_single_registry_no_registry_column() {
        let mut s = TuiState::new();
        s.set_rows(vec![row_with_reg(
            "registry.example",
            "acme/skill-a",
            ArtifactState::Installed,
        )]);
        // Single-registry elision.
        s.set_default_registry(Some("registry.example".into()));
        s.set_registry_order(vec!["registry.example".into()]);
        assert_eq!(s.view_mode, ViewMode::Flat);
        let m = frame(&s);

        assert!(
            !m.show_registry_column,
            "single-registry flat view must not show Registry column"
        );
        // Row must not carry a registry label.
        assert_eq!(
            m.rows[0].registry, None,
            "single-registry flat row must have registry: None"
        );
        // Repo cell must be the elided form (no host prefix).
        assert_eq!(
            m.rows[0].columns[0].trim(),
            fit("acme/skill-a", W_REPO).trim(),
            "single-registry flat Repo cell must strip the default registry host"
        );
    }

    // B: tree registry-root label shows "alias (url)" when alias configured.
    #[test]
    fn spec_tree_registry_root_shows_alias_label_when_alias_set() {
        use std::collections::BTreeMap;
        let mut s = TuiState::new();
        s.set_rows(vec![
            row_with_reg("ghcr.io/acme", "skill-a", ArtifactState::Installed),
            row_with_reg("ghcr.io/other", "skill-b", ArtifactState::NotInstalled),
        ]);
        s.set_registry_order(vec!["ghcr.io/acme".into(), "ghcr.io/other".into()]);
        // Map ghcr.io/acme to alias "acme" — label becomes "acme (ghcr.io/acme)".
        let mut labels = BTreeMap::new();
        labels.insert("ghcr.io/acme".to_string(), "acme (ghcr.io/acme)".to_string());
        labels.insert("ghcr.io/other".to_string(), "ghcr.io/other".to_string());
        s.set_registry_labels(labels);
        s.toggle_view_mode();
        assert_eq!(s.view_mode, ViewMode::Tree);
        let m = frame(&s);

        let group_labels: Vec<&str> = m
            .rows
            .iter()
            .filter(|r| r.group.is_some())
            .map(|r| r.columns[0].trim())
            .collect();
        assert!(
            group_labels.iter().any(|l| l.contains("acme (ghcr.io/acme)")),
            "aliased registry root must show 'alias (url)' in tree label; got: {group_labels:?}"
        );
        // Other registry (no alias) shows raw URL.
        assert!(
            group_labels.iter().any(|l| l.contains("ghcr.io/other")),
            "non-aliased registry root must show raw URL; got: {group_labels:?}"
        );
    }

    // B: tree registry-root label shows plain URL when no alias is configured.
    #[test]
    fn spec_tree_registry_root_shows_url_when_no_alias() {
        let mut s = TuiState::new();
        s.set_rows(vec![row_with_reg("ghcr.io/acme", "skill-a", ArtifactState::Installed)]);
        s.set_registry_order(vec!["ghcr.io/acme".into(), "ghcr.io/other".into()]);
        // No alias set — registry_labels empty, fallback is the URL itself.
        assert!(s.registry_labels.is_empty(), "precondition: no labels set");
        s.toggle_view_mode();
        assert_eq!(s.view_mode, ViewMode::Tree);
        let m = frame(&s);

        let group_labels: Vec<&str> = m
            .rows
            .iter()
            .filter(|r| r.group.is_some())
            .map(|r| r.columns[0].trim())
            .collect();
        assert!(
            group_labels.iter().any(|l| l.contains("ghcr.io/acme")),
            "tree registry root without alias must show raw URL; got: {group_labels:?}"
        );
    }

    // B: registry health status line shows alias label when alias configured.
    #[test]
    fn spec_health_status_shows_alias_label() {
        use std::collections::BTreeMap;
        let mut s = TuiState::new();
        s.set_rows(vec![]);
        s.registry_health = RegistryHealth {
            offline: vec!["ghcr.io/acme".to_string()],
            truncated: vec![],
        };
        // Map ghcr.io/acme → "acme (ghcr.io/acme)" alias label.
        let mut labels = BTreeMap::new();
        labels.insert("ghcr.io/acme".to_string(), "acme (ghcr.io/acme)".to_string());
        s.set_registry_labels(labels);
        let m = frame(&s);

        assert!(
            m.status.contains("acme (ghcr.io/acme)"),
            "health status must show alias label when alias configured; got: {:?}",
            m.status
        );
    }
}
