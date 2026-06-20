// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! Detail-pane content and scroll geometry — pure, ratatui-free.
//!
//! [`detail_lines`] builds the pane's semantic lines for a selected row;
//! [`scroll_max`] bounds the vertical scroll offset by counting the
//! post-wrap rows those lines occupy in the live viewport. Both
//! [`super::state`] (clamping the offset at mutation time) and
//! [`super::render`] (projecting + drawing) consume this module, so the
//! content layout and its scroll bound stay one source of truth.
//!
//! The catalog column widths live here too: the detail viewport is
//! whatever is left of the terminal after the catalog takes its fixed
//! width, so the geometry is one concern.

use super::bundle_members::MemberNode;
use super::state::TuiRow;

/// Catalog column widths (chars) — the projection pads/truncates to
/// these so the table aligns regardless of how long an identifier is.
pub const W_KIND: usize = 8;
pub const W_REPO: usize = 46;
pub const W_TAG: usize = 12;
/// Status column width — wide enough for the longest label
/// (`✘ integrity-missing`, 19 chars) so the header underline spans the
/// full column instead of stopping at `Status`.
pub const W_STATUS: usize = 19;
/// Total terminal columns the Catalog needs to show every fixed-width
/// column un-truncated: 2 (mark) + repo + 2 + kind + 2 + tag + 2 + status,
/// plus 2 block borders. Selection is shown by row highlight (no leading
/// symbol). Sized to exactly this side-by-side so Detail gets all slack.
pub const CATALOG_WIDTH: u16 = (2 + W_REPO + 2 + W_KIND + 2 + W_TAG + 2 + W_STATUS) as u16 + 2 /* borders */;
/// Narrowest usable Detail column (the side-by-side layout threshold).
pub const DETAIL_MIN_WIDTH: u16 = 30;

/// One semantic line of the Detail pane. Pure data — `draw` maps each
/// kind to concrete styling with zero logic of its own.
///
/// Closed internal enum — matches stay total, no `#[non_exhaustive]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DetailLine {
    /// Blank spacer.
    Blank,
    /// The artifact reference — centered, bold, accent color.
    Identifier(String),
    /// An underlined section label; the colon is part of the label
    /// (e.g. `Summary:`).
    SectionLabel(&'static str),
    /// `label value` on one line; the label includes the colon
    /// (e.g. `Keywords:`).
    MetaEntry {
        /// The highlighted key, colon included.
        label: &'static str,
        /// The plain value rendered on the same line.
        value: String,
    },
    /// Plain wrapped body text.
    Text(String),
}

/// Build the Detail pane's semantic lines for the selected row.
///
/// Layout: the centered identifier framed by blank lines, an underlined
/// `Summary:` section (the short blurb, `-` when absent), an optional
/// `Description:` section (only when a description exists), then a
/// `Metadata:` section of `Label: value` rows. Version and status are
/// deliberately NOT repeated here — the catalog row already shows both
/// (Tag column, status glyph). `Pinned:` appears only when the picker
/// pinned a version.
pub fn detail_lines(row: Option<&TuiRow>) -> Vec<DetailLine> {
    let Some(r) = row else {
        return vec![DetailLine::Text("no selection".to_string())];
    };
    let keywords = if r.keywords.is_empty() {
        "-".to_string()
    } else {
        r.keywords.join(", ")
    };
    let summary = if r.summary.is_empty() { "-" } else { r.summary.as_str() };

    let mut lines = vec![
        DetailLine::Blank,
        DetailLine::Identifier(r.repo.clone()),
        DetailLine::Blank,
        DetailLine::SectionLabel("Summary:"),
        DetailLine::Blank,
        DetailLine::Text(summary.to_string()),
    ];
    if !r.description.is_empty() {
        lines.extend([
            DetailLine::Blank,
            DetailLine::SectionLabel("Description:"),
            DetailLine::Blank,
            DetailLine::Text(r.description.clone()),
        ]);
    }
    lines.extend([
        DetailLine::Blank,
        DetailLine::SectionLabel("Metadata:"),
        DetailLine::Blank,
        DetailLine::MetaEntry {
            label: "Keywords:",
            value: keywords,
        },
        DetailLine::MetaEntry {
            label: "Repository:",
            value: r.repository_url.clone().unwrap_or_else(|| "-".to_string()),
        },
    ]);
    if let Some(p) = &r.pinned_version {
        lines.push(DetailLine::MetaEntry {
            label: "Pinned:",
            value: p.clone(),
        });
    }
    lines
}

/// Build the Detail pane's semantic lines for a selected virtual bundle
/// member row.
///
/// # Contract (C-7)
///
/// Returns `[Identifier(sanitized label), Blank, SectionLabel("Metadata:"),
/// Blank, MetaEntry{Kind}, MetaEntry{State}, MetaEntry{"Via bundle:", parent_repo}]`.
///
/// Never reads `TuiRow` — `MemberNode` carries all the information needed.
/// The `label` is sanitized via `render::sanitize_member_label` before being
/// placed into the `Identifier` line.
///
/// `parent_bundle_repo` is the `registry/repository` of the bundle that owns
/// this member (from `DisplayRow::Member::parent_bundle_repo`); rendered as
/// the "Via bundle:" metadata line so the user can trace the virtual row back
/// to its parent.
pub fn detail_lines_for_member(node: &MemberNode, parent_bundle_repo: &str) -> Vec<DetailLine> {
    // Sanitize the raw label at the display boundary (never stored sanitized).
    let sanitized_label = super::render::sanitize_member_label(&node.label);

    // The identifier shown is the sanitized label. When a resolved `member_repo`
    // is available, prefer that for the canonical reference; fall back to the
    // sanitized label so the pane never shows an empty identifier.
    // Defense-in-depth: sanitize member_repo at the display boundary too —
    // even though it comes from Identifier::parse (charset-constrained), every
    // registry-derived string shown in the terminal passes through the sanitizer.
    let raw_identifier = node.member_repo.as_deref().unwrap_or(&sanitized_label);
    let identifier = super::render::sanitize_member_label(raw_identifier);

    vec![
        DetailLine::Blank,
        DetailLine::Identifier(identifier),
        DetailLine::Blank,
        DetailLine::SectionLabel("Metadata:"),
        DetailLine::Blank,
        DetailLine::MetaEntry {
            label: "Kind:",
            value: node.kind.to_string(),
        },
        DetailLine::MetaEntry {
            label: "State:",
            value: node.state.to_string(),
        },
        DetailLine::MetaEntry {
            label: "Via bundle:",
            // F9: sanitize at display boundary — parent_bundle_repo is
            // registry-controlled and must not reach the terminal raw.
            value: super::render::sanitize_member_label(parent_bundle_repo),
        },
    ]
}

/// The visible text of one semantic detail line (the wrap-count input;
/// tests reuse it to assert content without caring about styling).
pub fn detail_line_text(line: &DetailLine) -> String {
    match line {
        DetailLine::Blank => String::new(),
        DetailLine::Identifier(s) | DetailLine::Text(s) => s.clone(),
        DetailLine::SectionLabel(l) => (*l).to_string(),
        DetailLine::MetaEntry { label, value } => format!("{label} {value}"),
    }
}

/// The Detail pane's *inner* (border-less) size for a terminal of
/// `(width, height)` — mirrors the layout math in `render::draw`: 5 rows
/// of fixed chrome (title 1, search 3, legend 1), then side-by-side when
/// the catalog plus a usable detail column fit, else a stacked band of
/// at most 8 rows below the list.
pub fn viewport(term: (u16, u16)) -> (u16, u16) {
    let (w, h) = term;
    let content_h = h.saturating_sub(5);
    let (dw, dh) = if w >= CATALOG_WIDTH + DETAIL_MIN_WIDTH {
        (w - CATALOG_WIDTH, content_h)
    } else {
        // Stacked: the list keeps its Min(3) first, the band caps at 8.
        (w, content_h.saturating_sub(3).min(8))
    };
    (dw.saturating_sub(2), dh.saturating_sub(2))
}

/// Rows `text` occupies after greedy word-wrap at `width` columns —
/// the same strategy ratatui's `Wrap` uses (words longer than a row are
/// hard-broken). An empty line still occupies one row.
fn wrapped_rows(text: &str, width: u16) -> usize {
    let width = usize::from(width.max(1));
    let mut rows = 1usize;
    let mut col = 0usize;
    for word in text.split_whitespace() {
        let len = word.chars().count();
        // A separating space is needed when the row already has content.
        let needed = if col == 0 { len } else { len + 1 };
        if col + needed <= width {
            col += needed;
        } else if len <= width {
            rows += 1;
            col = len;
        } else {
            // Longer than a full row: hard-broken across rows.
            if col > 0 {
                rows += 1;
            }
            let mut remaining = len;
            while remaining > width {
                rows += 1;
                remaining -= width;
            }
            col = remaining;
        }
    }
    rows
}

/// Upper bound for the detail scroll offset: the content's post-wrap row
/// count minus the viewport height, so the content's last row stops at
/// the pane's bottom edge (no scrolling into blank space). Zero when the
/// content fits the pane.
pub fn scroll_max(lines: &[DetailLine], viewport: (u16, u16)) -> u16 {
    let (vw, vh) = viewport;
    let rows: usize = lines.iter().map(|l| wrapped_rows(&detail_line_text(l), vw)).sum();
    u16::try_from(rows.saturating_sub(usize::from(vh))).unwrap_or(u16::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── C-7 detail_lines_for_member ───────────────────────────────────────────
    //
    // These tests FAIL until P3 implements `detail_lines_for_member`.

    use crate::oci::ArtifactKind;
    use crate::tui::bundle_members::MemberNode;
    use crate::tui::state::ArtifactState;

    fn make_member_node(
        label: &str,
        kind: ArtifactKind,
        member_repo: Option<&str>,
        state: ArtifactState,
    ) -> MemberNode {
        MemberNode {
            kind,
            label: label.to_string(),
            member_repo: member_repo.map(|s| s.to_string()),
            state,
            related: false,
        }
    }

    #[test]
    fn detail_lines_for_member_returns_identifier_and_metadata_entries() {
        // C-7: detail pane for a Member must include Identifier (sanitized label),
        // and MetaEntry rows for Kind, State, and "Via bundle:" parent repo.
        // Layout: [Identifier(sanitized), Blank, SectionLabel("Metadata:"), Blank,
        //          MetaEntry{Kind}, MetaEntry{State}, MetaEntry{"Via bundle:", parent_repo}]
        let node = MemberNode {
            kind: ArtifactKind::Skill,
            label: "my-skill".to_string(),
            member_repo: Some("reg/acme/my-skill".to_string()),
            state: ArtifactState::Installed,
            related: false,
        };
        // Per C-7, parent_repo comes from DisplayRow::Member.parent_bundle_repo
        // and is threaded in by the call site.
        let parent_repo = "reg.example.io/acme/my-bundle";
        let lines = detail_lines_for_member(&node, parent_repo);
        // Must be non-empty.
        assert!(!lines.is_empty(), "C-7: detail lines for member must be non-empty");
        // Must have an Identifier line (the sanitized label).
        let has_identifier = lines.iter().any(|l| matches!(l, DetailLine::Identifier(_)));
        assert!(
            has_identifier,
            "C-7: must include a Identifier line for the member label"
        );
        // Must have "Metadata:" section label.
        let has_metadata = lines.iter().any(|l| matches!(l, DetailLine::SectionLabel("Metadata:")));
        assert!(has_metadata, "C-7: must include a Metadata: section label");
        // Must have a MetaEntry for Kind.
        let has_kind = lines
            .iter()
            .any(|l| matches!(l, DetailLine::MetaEntry { label: "Kind:", .. }));
        assert!(has_kind, "C-7: must include MetaEntry{{Kind:}}");
        // Must have a MetaEntry for State.
        let has_state = lines
            .iter()
            .any(|l| matches!(l, DetailLine::MetaEntry { label: "State:", .. }));
        assert!(has_state, "C-7: must include MetaEntry{{State:}}");
        // Must have a MetaEntry for "Via bundle:" with the parent repo value (B2 assertion).
        let via_bundle = lines.iter().find_map(|l| match l {
            DetailLine::MetaEntry {
                label: "Via bundle:",
                value,
            } => Some(value.clone()),
            _ => None,
        });
        assert!(
            via_bundle.is_some(),
            "C-7: must include MetaEntry{{\"Via bundle:\", ...}} line"
        );
        assert_eq!(
            via_bundle.as_deref(),
            Some(parent_repo),
            "C-7: Via bundle: value must be the parent bundle repo"
        );
    }

    #[test]
    fn detail_lines_for_member_never_reads_tui_row() {
        // C-7 invariant: the function operates only on MemberNode, never a TuiRow.
        // This is a structural test — if the function compiled and returns lines
        // from a MemberNode alone, it proves TuiRow independence. We verify it
        // produces output for a "minimal" MemberNode (rule kind, no member_repo).
        let node = make_member_node("some-rule", ArtifactKind::Rule, None, ArtifactState::NotInstalled);
        let lines = detail_lines_for_member(&node, "reg/acme/test-bundle");
        assert!(
            !lines.is_empty(),
            "C-7: even a rule member with no repo must produce lines"
        );
        let has_identifier = lines.iter().any(|l| matches!(l, DetailLine::Identifier(_)));
        assert!(has_identifier, "C-7: rule member must include an Identifier line");
    }

    #[test]
    fn detail_lines_for_member_with_unparseable_id_still_renders() {
        // C-7 edge case: `member_repo = None` (Identifier::parse failed, fail-soft).
        // The node must still render — nothing panics, non-empty output.
        let node = make_member_node(
            "bad-id:://invalid",
            ArtifactKind::Skill,
            None,
            ArtifactState::NotInstalled,
        );
        let lines = detail_lines_for_member(&node, "reg/acme/test-bundle");
        assert!(
            !lines.is_empty(),
            "C-7: unparseable-id member (member_repo=None) must still render, got empty"
        );
    }

    #[test]
    fn detail_lines_for_member_label_appears_in_identifier_line() {
        // C-7: the Identifier line value must contain the (sanitized) label.
        let node = make_member_node(
            "code-review",
            ArtifactKind::Skill,
            Some("reg/acme/code-review"),
            ArtifactState::Installed,
        );
        let lines = detail_lines_for_member(&node, "reg/acme/test-bundle");
        let identifier_value = lines.iter().find_map(|l| match l {
            DetailLine::Identifier(s) => Some(s.clone()),
            _ => None,
        });
        assert!(identifier_value.is_some(), "C-7: must have an Identifier line");
        assert!(
            identifier_value.as_ref().unwrap().contains("code-review"),
            "C-7: Identifier line must contain the label; got: {:?}",
            identifier_value
        );
    }

    #[test]
    fn wrapped_rows_counts_word_wrap() {
        // Empty and short lines occupy one row.
        assert_eq!(wrapped_rows("", 10), 1);
        assert_eq!(wrapped_rows("abc def", 10), 1);
        // Exact fit stays one row; one char over wraps.
        assert_eq!(wrapped_rows("abcd efghi", 10), 1);
        assert_eq!(wrapped_rows("abcde fghij", 10), 2);
        // Words pack greedily with single separating spaces.
        assert_eq!(wrapped_rows("aa bb cc dd", 5), 2);
        assert_eq!(wrapped_rows("aaa bbb ccc", 5), 3);
        // A word longer than the row is hard-broken like ratatui does.
        assert_eq!(wrapped_rows(&"x".repeat(25), 10), 3);
        // …also when it follows content on the current row.
        assert_eq!(wrapped_rows(&format!("ab {}", "x".repeat(25)), 10), 4);
        // Degenerate width never divides by zero.
        assert_eq!(wrapped_rows("ab", 0), 2);
    }

    #[test]
    fn viewport_mirrors_the_layout_split() {
        // Wide: side-by-side — detail gets all slack minus borders.
        let (w, h) = viewport((CATALOG_WIDTH + 60, 30));
        assert_eq!((w, h), (58, 23));
        // Narrow: stacked — full width, band capped at 8 (6 inner).
        let (w, h) = viewport((80, 30));
        assert_eq!((w, h), (78, 6));
        // Short terminal: the band shrinks before the list's Min(3).
        let (_, h) = viewport((80, 10));
        assert_eq!(h, 0);
        // Tiny: saturates, never underflows.
        assert_eq!(viewport((0, 0)), (0, 0));
    }

    #[test]
    fn scroll_max_stops_at_the_content_end() {
        let lines: Vec<DetailLine> = (0..10).map(|i| DetailLine::Text(format!("line {i}"))).collect();
        // Content taller than the pane: last row aligns with the bottom.
        assert_eq!(scroll_max(&lines, (40, 4)), 6);
        // Content fits: no scrolling at all.
        assert_eq!(scroll_max(&lines, (40, 10)), 0);
        assert_eq!(scroll_max(&lines, (40, 20)), 0);
        // Wrapping at a narrow pane raises the row count.
        let long = vec![DetailLine::Text("a".repeat(100))];
        assert_eq!(scroll_max(&long, (10, 4)), 6);
    }
}
