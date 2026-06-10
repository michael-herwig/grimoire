// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! The catalog tree projection.
//!
//! A pure builder that groups flat catalog rows into a hierarchy from the
//! OCI identifier: the registry host is the root (elided when it is the
//! effective default registry — shorter names), each `/` path component is
//! a group, and the final `/`-segment is additionally split on `.` so
//! dotted names nest (`acme/code.review` → `acme` ▸ `code` ▸ leaf
//! `review`). Hyphens are never separators (`code-review` stays one leaf).
//!
//! No I/O, no ratatui — every function is a pure transform over
//! [`TuiRow`], so the whole hierarchy is exhaustively unit-testable.

use std::collections::{BTreeMap, BTreeSet};

use super::state::{ArtifactState, TuiRow};

/// Aggregate install-state counts over a group's descendant leaves, so a
/// collapsed group can still summarize what it hides.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rollup {
    /// Total descendant leaves.
    pub total: usize,
    /// Leaves that are installed and intact.
    pub installed: usize,
    /// Leaves not present in this scope.
    pub not_installed: usize,
    /// Leaves whose locked pin is ahead of the record.
    pub outdated: usize,
    /// Leaves whose on-disk content drifted.
    pub modified: usize,
    /// Leaves whose install record cannot be honored.
    pub integrity_missing: usize,
}

impl Rollup {
    fn add(&mut self, state: ArtifactState) {
        self.total += 1;
        match state {
            ArtifactState::Installed => self.installed += 1,
            ArtifactState::NotInstalled => self.not_installed += 1,
            ArtifactState::Outdated => self.outdated += 1,
            ArtifactState::Modified => self.modified += 1,
            ArtifactState::IntegrityMissing => self.integrity_missing += 1,
        }
    }

    fn merge(&mut self, other: Rollup) {
        self.total += other.total;
        self.installed += other.installed;
        self.not_installed += other.not_installed;
        self.outdated += other.outdated;
        self.modified += other.modified;
        self.integrity_missing += other.integrity_missing;
    }

    /// The single [`ArtifactState`] that best represents the group, by
    /// worst-state precedence: a broken/drifted/outdated descendant
    /// dominates, then "any not installed", else all installed.
    pub fn worst(&self) -> ArtifactState {
        if self.integrity_missing > 0 {
            ArtifactState::IntegrityMissing
        } else if self.modified > 0 {
            ArtifactState::Modified
        } else if self.outdated > 0 {
            ArtifactState::Outdated
        } else if self.not_installed > 0 || self.total == 0 {
            ArtifactState::NotInstalled
        } else {
            ArtifactState::Installed
        }
    }
}

/// A node in the catalog hierarchy: an interior [`GroupNode`] or a
/// terminal [`LeafNode`] (one catalog row).
///
/// Closed internal enum — matches stay total, no `#[non_exhaustive]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Node {
    /// An interior path component grouping descendants.
    Group(GroupNode),
    /// A terminal catalog entry.
    Leaf(LeafNode),
}

/// An interior tree node — one path component (registry / org / project /
/// dotted prefix) grouping every descendant under it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupNode {
    /// The full path to this node (`/`-joined), stable across rebuilds —
    /// the key the collapsed-set is keyed by.
    pub key: String,
    /// This node's own path component (what the row renders).
    pub label: String,
    /// Indent depth (0 at the top level).
    pub depth: usize,
    /// Child nodes: groups first (sorted), then leaves (sorted by label).
    pub children: Vec<Node>,
    /// Every descendant leaf's `rows` index (sorted), so a group action
    /// targets the whole subtree with one keypress.
    pub rows: Vec<usize>,
    /// Aggregate install-state over the descendants.
    pub rollup: Rollup,
}

/// A terminal tree node — exactly one catalog row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeafNode {
    /// The full path to this leaf (`/`-joined).
    pub key: String,
    /// The bare artifact name (the final identifier component).
    pub label: String,
    /// Indent depth.
    pub depth: usize,
    /// The `rows` index this leaf projects.
    pub row: usize,
    /// The row's install state (snapshotted at build for the rollup).
    pub state: ArtifactState,
}

/// The whole catalog hierarchy.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Tree {
    /// Top-level nodes (depth 0).
    pub roots: Vec<Node>,
}

/// One visible line of the flattened tree (collapsed groups omit their
/// descendants). Selection / rendering index this list in tree mode.
///
/// Closed internal enum — matches stay total, no `#[non_exhaustive]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DisplayRow {
    /// A group header line.
    Group {
        /// The collapsed-set key.
        key: String,
        /// The path component to render.
        label: String,
        /// Indent depth.
        depth: usize,
        /// Whether this group is collapsed (its descendants are hidden).
        collapsed: bool,
        /// Aggregate install-state of the descendants.
        rollup: Rollup,
        /// Every descendant leaf `rows` index — a group action's targets.
        rows: Vec<usize>,
    },
    /// A leaf (catalog row) line.
    Leaf {
        /// The bare artifact name.
        label: String,
        /// Indent depth.
        depth: usize,
        /// The `rows` index.
        row: usize,
        /// The row's install state.
        state: ArtifactState,
    },
}

/// Split one `registry/repository` reference into its group path segments
/// and the bare leaf name, eliding the registry root when it equals the
/// effective `default_registry`.
fn segments(repo: &str, default_registry: Option<&str>) -> (Vec<String>, String) {
    let (registry, repository) = match repo.split_once('/') {
        Some((r, p)) => (Some(r), p),
        // Malformed (no registry separator) — treat the whole string as a
        // single top-level leaf rather than crashing.
        None => (None, repo),
    };

    let mut segs: Vec<String> = Vec::new();
    if let Some(reg) = registry
        && default_registry != Some(reg)
    {
        segs.push(reg.to_string());
    }

    // Every `/` component except the last is a group; the last component
    // is additionally split on `.` so dotted names nest one level deeper.
    let parts: Vec<&str> = repository.split('/').collect();
    let Some((last, prefix)) = parts.split_last() else {
        return (segs, repository.to_string());
    };
    for p in prefix {
        segs.push((*p).to_string());
    }
    let dots: Vec<&str> = last.split('.').collect();
    let Some((leaf, dot_groups)) = dots.split_last() else {
        return (segs, (*last).to_string());
    };
    for d in dot_groups {
        segs.push((*d).to_string());
    }
    (segs, (*leaf).to_string())
}

/// A mutable trie used only while building; converted to [`Node`]s once
/// every row is inserted.
#[derive(Default)]
struct Trie {
    groups: BTreeMap<String, Trie>,
    leaves: Vec<(String, usize, ArtifactState)>,
}

impl Trie {
    fn insert(&mut self, groups: &[String], leaf: String, row: usize, state: ArtifactState) {
        match groups.split_first() {
            None => self.leaves.push((leaf, row, state)),
            Some((head, rest)) => self
                .groups
                .entry(head.clone())
                .or_default()
                .insert(rest, leaf, row, state),
        }
    }

    /// Convert this trie level into ordered [`Node`]s, returning the
    /// subtree's aggregate rollup and descendant `rows` for the parent.
    fn into_nodes(self, parent_key: &str, depth: usize) -> (Vec<Node>, Rollup, Vec<usize>) {
        let mut nodes = Vec::new();
        let mut rollup = Rollup::default();
        let mut rows = Vec::new();

        // Groups first (BTreeMap iterates label-sorted).
        for (label, child) in self.groups {
            let key = if parent_key.is_empty() {
                label.clone()
            } else {
                format!("{parent_key}/{label}")
            };
            let (children, child_rollup, mut child_rows) = child.into_nodes(&key, depth + 1);
            child_rows.sort_unstable();
            rollup.merge(child_rollup);
            rows.extend(child_rows.iter().copied());
            nodes.push(Node::Group(GroupNode {
                key,
                label,
                depth,
                children,
                rows: child_rows,
                rollup: child_rollup,
            }));
        }

        // Then leaves, sorted by label for a stable, deterministic order.
        let mut leaves = self.leaves;
        leaves.sort_by(|a, b| a.0.cmp(&b.0));
        for (label, row, state) in leaves {
            let key = if parent_key.is_empty() {
                label.clone()
            } else {
                format!("{parent_key}/{label}")
            };
            rollup.add(state);
            rows.push(row);
            nodes.push(Node::Leaf(LeafNode {
                key,
                label,
                depth,
                row,
                state,
            }));
        }

        (nodes, rollup, rows)
    }
}

/// Build the catalog hierarchy from flat `rows`. `default_registry`, when
/// it matches a row's registry host, elides that registry root so its
/// entries sit at the top level (shorter names).
pub fn build(rows: &[TuiRow], default_registry: Option<&str>) -> Tree {
    let mut trie = Trie::default();
    for (i, r) in rows.iter().enumerate() {
        let (groups, leaf) = segments(&r.repo, default_registry);
        trie.insert(&groups, leaf, i, r.state);
    }
    let (roots, _, _) = trie.into_nodes("", 0);
    Tree { roots }
}

/// Flatten the tree to the visible lines: a preorder walk where a
/// collapsed group emits its header but not its descendants.
pub fn flatten(tree: &Tree, collapsed: &BTreeSet<String>) -> Vec<DisplayRow> {
    let mut out = Vec::new();
    walk(&tree.roots, collapsed, &mut out);
    out
}

fn walk(nodes: &[Node], collapsed: &BTreeSet<String>, out: &mut Vec<DisplayRow>) {
    for node in nodes {
        match node {
            Node::Group(g) => {
                let is_collapsed = collapsed.contains(&g.key);
                out.push(DisplayRow::Group {
                    key: g.key.clone(),
                    label: g.label.clone(),
                    depth: g.depth,
                    collapsed: is_collapsed,
                    rollup: g.rollup,
                    rows: g.rows.clone(),
                });
                if !is_collapsed {
                    walk(&g.children, collapsed, out);
                }
            }
            Node::Leaf(l) => out.push(DisplayRow::Leaf {
                label: l.label.clone(),
                depth: l.depth,
                row: l.row,
                state: l.state,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(repo: &str, state: ArtifactState) -> TuiRow {
        TuiRow {
            kind: "skill".to_string(),
            repo: repo.to_string(),
            description: "d".to_string(),
            summary: String::new(),
            keywords: vec![],
            latest_tag: "latest".to_string(),
            version: "1.0.0".to_string(),
            pinned_version: None,
            state,
        }
    }

    /// Collect the flattened (label, depth, is_group) tuples for assertions.
    fn shape(tree: &Tree) -> Vec<(String, usize, bool)> {
        flatten(tree, &BTreeSet::new())
            .into_iter()
            .map(|d| match d {
                DisplayRow::Group { label, depth, .. } => (label, depth, true),
                DisplayRow::Leaf { label, depth, .. } => (label, depth, false),
            })
            .collect()
    }

    #[test]
    fn segments_splits_slash_groups_and_dotted_leaf() {
        // Registry kept (not the default), `/` groups, dotted final segment.
        let (g, leaf) = segments("localhost:5000/acme/code.review", None);
        assert_eq!(g, vec!["localhost:5000", "acme", "code"]);
        assert_eq!(leaf, "review");

        // Hyphen is NOT a separator.
        let (g, leaf) = segments("localhost:5000/acme/code-review", None);
        assert_eq!(g, vec!["localhost:5000", "acme"]);
        assert_eq!(leaf, "code-review");

        // Multi-dot final segment nests every dot but the last.
        let (g, leaf) = segments("reg/a/b.c.d", None);
        assert_eq!(g, vec!["reg", "a", "b", "c"]);
        assert_eq!(leaf, "d");
    }

    #[test]
    fn default_registry_root_is_elided() {
        let (g, leaf) = segments("localhost:5000/acme/tool", Some("localhost:5000"));
        assert_eq!(g, vec!["acme"], "default registry root dropped");
        assert_eq!(leaf, "tool");

        // A non-default registry keeps its host as the root group.
        let (g, _) = segments("ghcr.io/acme/tool", Some("localhost:5000"));
        assert_eq!(g, vec!["ghcr.io", "acme"]);
    }

    #[test]
    fn malformed_repo_without_slash_is_a_top_level_leaf() {
        let (g, leaf) = segments("noslash", None);
        assert!(g.is_empty());
        assert_eq!(leaf, "noslash");
        let t = build(&[row("noslash", ArtifactState::NotInstalled)], None);
        assert_eq!(shape(&t), vec![("noslash".to_string(), 0, false)]);
    }

    #[test]
    fn build_groups_before_leaves_sorted() {
        let t = build(
            &[
                row("reg/acme/zeta", ArtifactState::Installed),
                row("reg/acme/group/inner", ArtifactState::NotInstalled),
                row("reg/acme/alpha", ArtifactState::Installed),
            ],
            Some("reg"),
        );
        // reg elided ⇒ top level is the `acme` group; inside it the
        // `group` subgroup comes before the `alpha`/`zeta` leaves.
        assert_eq!(
            shape(&t),
            vec![
                ("acme".to_string(), 0, true),
                ("group".to_string(), 1, true),
                ("inner".to_string(), 2, false),
                ("alpha".to_string(), 1, false),
                ("zeta".to_string(), 1, false),
            ]
        );
    }

    #[test]
    fn collapsed_group_hides_descendants() {
        let t = build(
            &[
                row("reg/acme/a", ArtifactState::Installed),
                row("reg/acme/b", ArtifactState::Installed),
            ],
            Some("reg"),
        );
        let mut collapsed = BTreeSet::new();
        collapsed.insert("acme".to_string());
        let flat = flatten(&t, &collapsed);
        assert_eq!(flat.len(), 1, "only the collapsed header is visible");
        match &flat[0] {
            DisplayRow::Group {
                collapsed,
                rows,
                rollup,
                ..
            } => {
                assert!(*collapsed);
                assert_eq!(rows, &vec![0, 1], "descendant rows still tracked");
                assert_eq!(rollup.total, 2);
                assert_eq!(rollup.installed, 2);
            }
            other => panic!("expected group, got {other:?}"),
        }
    }

    #[test]
    fn rollup_aggregates_and_worst_state_precedence() {
        let t = build(
            &[
                row("reg/g/a", ArtifactState::Installed),
                row("reg/g/b", ArtifactState::Outdated),
                row("reg/g/c", ArtifactState::Modified),
            ],
            Some("reg"),
        );
        let Node::Group(g) = &t.roots[0] else {
            panic!("expected group root");
        };
        assert_eq!(g.rollup.total, 3);
        assert_eq!(g.rollup.installed, 1);
        assert_eq!(g.rollup.outdated, 1);
        assert_eq!(g.rollup.modified, 1);
        // Modified outranks Outdated outranks Installed.
        assert_eq!(g.rollup.worst(), ArtifactState::Modified);
    }

    #[test]
    fn nested_group_rollup_merges_subtrees() {
        let t = build(
            &[
                row("reg/org/team/x", ArtifactState::IntegrityMissing),
                row("reg/org/y", ArtifactState::Installed),
            ],
            Some("reg"),
        );
        let Node::Group(org) = &t.roots[0] else {
            panic!("expected org group");
        };
        assert_eq!(org.label, "org");
        assert_eq!(org.rows, vec![0, 1], "all descendants, sorted");
        assert_eq!(org.rollup.total, 2);
        assert_eq!(org.rollup.worst(), ArtifactState::IntegrityMissing);
    }
}
