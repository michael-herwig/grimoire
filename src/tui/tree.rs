// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! The catalog tree projection.
//!
//! A pure builder that groups flat catalog rows into a hierarchy from the
//! OCI identifier: the registry host is the root (elided when it equals
//! the effective default registry — shorter names), path components become
//! nested groups, and configurable separators control further splitting.
//!
//! No I/O, no ratatui — every function is a pure transform over
//! [`TuiRow`], so the whole hierarchy is exhaustively unit-testable.

use std::collections::{BTreeMap, BTreeSet};

use super::state::{ArtifactState, TuiRow};

/// Options controlling how [`build`] partitions rows into the hierarchy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeBuildOptions {
    /// The effective default registry: its host is elided as the tree root
    /// so names stay short when browsing the primary registry.
    pub default_registry: Option<String>,
    /// When true, insert a type-level group (skill / rule / agent / bundle)
    /// between the registry root and the path segments.
    pub group_by_type: bool,
    /// Characters (each a single `String`) that split the repository path
    /// into nested groups. `/` is always honored structurally even if
    /// absent; an empty list defaults to `["/"]`.
    pub separators: Vec<String>,
}

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
    /// Add one leaf's state to this rollup.
    pub fn add(&mut self, state: ArtifactState) {
        self.total += 1;
        match state {
            ArtifactState::Installed => self.installed += 1,
            ArtifactState::NotInstalled => self.not_installed += 1,
            ArtifactState::Outdated => self.outdated += 1,
            ArtifactState::Modified => self.modified += 1,
            ArtifactState::IntegrityMissing => self.integrity_missing += 1,
        }
    }

    /// Merge another rollup's counts into this one.
    pub fn merge(&mut self, other: Rollup) {
        self.total += other.total;
        self.installed += other.installed;
        self.not_installed += other.not_installed;
        self.outdated += other.outdated;
        self.modified += other.modified;
        self.integrity_missing += other.integrity_missing;
    }

    /// The single [`ArtifactState`] that best represents the group, by
    /// worst-state precedence: IntegrityMissing > Modified > Outdated >
    /// NotInstalled > Installed.
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

/// An interior tree node — one path component (registry / type / org /
/// project segment) grouping every descendant under it.
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
        /// The full path key for this leaf (mirrors [`LeafNode::key`]).
        /// Used by the collapse/expand machinery and render to identify bundle
        /// leaves without a secondary `rows` lookup.
        key: String,
        /// Whether this leaf's catalog row has `kind == "bundle"`.
        /// Set in `walk` from `rows[l.row].kind`.
        is_bundle: bool,
        /// Whether this bundle leaf is currently in its collapsed state.
        ///
        /// Computed as `is_bundle && !expanded_bundles.contains(&key)`:
        /// bundle leaves default-collapsed (absent from `expanded_bundles` = collapsed).
        /// Always `false` for non-bundle leaves.
        collapsed: bool,
    },
    /// A virtual bundle-member child row — NOT backed by any `rows` index.
    ///
    /// Members are display-only (Phase 2 read-only; Phase 3 gains per-member
    /// install). They never enter `rows` / `filtered` / `marked`. A sentinel
    /// `row: usize` is deliberately absent — `usize::MAX` would be fragile.
    ///
    /// The `label` field holds the **raw** (untrusted) member name; render
    /// code MUST pass it through `sanitize_member_label` before display.
    ///
    /// Depth is always `parent bundle leaf depth + 1`; `collapse_or_jump_to_parent`
    /// uses this for the upward scan.
    Member {
        /// Raw member name — sanitize before terminal output.
        label: String,
        /// Indent depth = parent bundle leaf depth + 1.
        depth: usize,
        /// Artifact kind of this member.
        kind: crate::oci::ArtifactKind,
        /// Install state of this member in the active scope.
        state: ArtifactState,
        /// Whether this member's repo also appears as a real catalog leaf
        /// (the static related-highlight signal).
        related: bool,
        /// `registry/repository` reference of the parent bundle; used for
        /// the selection anchor and the detail pane.
        parent_bundle_repo: String,
        /// The `registry/repository` reference of **this member** (from
        /// [`MemberNode::member_repo`]). `None` for placeholder rows
        /// (Loading / Failed / Offline).
        ///
        /// Populated in Phase 1 (P1.2); consumed by the member action layer
        /// in Phase 4 (P4.1) to synthesize `TuiAction::MemberAction`.
        member_repo: Option<String>,
    },
}

/// Normalize the separators list: always include `/`, drop empty entries,
/// dedup. Returns a `Vec<char>` for O(1) membership tests while splitting.
///
/// `validate_tree_separators` in `project_config.rs` is the upstream guard
/// that rejects invalid entries (empty, multi-char, control, whitespace,
/// zero-width); the `filter` below is intentional belt-and-suspenders for
/// programmatic callers and future code paths that bypass config parsing.
fn normalize_separators(separators: &[String]) -> Vec<char> {
    let mut chars: Vec<char> = separators
        .iter()
        .filter(|s| !s.is_empty())
        .flat_map(|s| s.chars())
        .collect();
    if !chars.contains(&'/') {
        chars.push('/');
    }
    chars.sort_unstable();
    chars.dedup();
    chars
}

/// Split one `registry/repository` reference into its group path segments,
/// the bare leaf name, and a flag indicating whether the default registry
/// prefix was elided.
///
/// The returned tuple is `(groups, leaf, registry_elided)`.
/// - `groups` contains the group path segments (registry host when not
///   elided, plus any intermediate path components).
/// - `leaf` is the bare final-segment label.
/// - `registry_elided` is `true` when the `default_registry` prefix (including
///   any namespace) was stripped, `false` when it was kept as the root group.
///
/// This is the **single source of truth** for registry-elision logic;
/// callers (including `build()`) must consume `registry_elided` directly
/// rather than re-deriving it with a host-only comparison.
///
/// The `sep_chars` control further splitting of path components beyond the
/// structural `/` registry separator.
///
/// A4 note: `strip_default_registry` in `render.rs` applies the same full-prefix
/// match (`repo.strip_prefix(reg).and_then(|r| r.strip_prefix('/'))`), so both
/// functions agree on the namespace rule. Any future change to the elision logic
/// must update both sites in the same commit.
fn segments(repo: &str, default_registry: Option<&str>, sep_chars: &[char]) -> (Vec<String>, String, bool) {
    let mut segs: Vec<String> = Vec::new();

    // Determine the repository path after the registry portion. When `repo`
    // begins with the (possibly namespaced) `default_registry`, elide the
    // entire `default_registry + '/'` prefix — matching the flat view's
    // `strip_default_registry` so both views segment a row identically
    // (e.g. `default_registry = "ghcr.io/acme"` elides the whole namespace,
    // not just the `ghcr.io` host).
    let (repository, registry_elided): (&str, bool) = if let Some(reg) = default_registry
        && let Some(rest) = repo.strip_prefix(reg)
        && let Some(rest) = rest.strip_prefix('/')
    {
        // Default registry (including any namespace) elided — no root group.
        (rest, true)
    } else if let Some((reg, path)) = repo.split_once('/') {
        // A non-default registry: its host is the tree root group.
        segs.push(reg.to_string());
        (path, false)
    } else {
        // Malformed (no registry separator) — treat the whole string as a
        // single top-level leaf rather than crashing.
        (repo, true)
    };

    // Split the repository path on separators. Each piece except the last
    // is a group; the last piece is the leaf label.
    //
    // We split on the full separator set (which always contains '/').
    // First, split on '/' to get path components, then for each component
    // except the last, check if it contains other separators and split
    // further.
    //
    // Filter empty strings: leading/trailing/consecutive separators in the
    // repository path produce empty pieces from `str::split`. Empty pieces
    // become invisible empty-label group nodes in the tree, so we discard
    // them here. validate_tree_separators is the upstream guard; this is
    // belt-and-suspenders for malformed OCI paths that sneak through.
    let path_parts: Vec<&str> = repository.split('/').filter(|s| !s.is_empty()).collect();
    let Some((last_part, prefix_parts)) = path_parts.split_last() else {
        return (segs, repository.to_string(), registry_elided);
    };

    // All prefix path components (split on non-'/' separators too).
    for part in prefix_parts {
        split_on_extra_seps(part, sep_chars, &mut segs);
    }

    // The last path component: split on non-'/' separators. Everything
    // but the final piece becomes a group; the final piece is the leaf.
    let extra_seps: Vec<char> = sep_chars.iter().copied().filter(|&c| c != '/').collect();
    if extra_seps.is_empty() {
        // No extra separators — the whole last part is the leaf.
        let leaf = last_part.to_string();
        return (segs, leaf, registry_elided);
    }

    // Split the last component on extra separators. Filter empty pieces
    // (from leading/trailing/consecutive separators in the path component).
    let sub_parts: Vec<&str> = split_on_chars(last_part, &extra_seps)
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect();
    let Some((leaf_part, sub_groups)) = sub_parts.split_last() else {
        return (segs, last_part.to_string(), registry_elided);
    };
    for g in sub_groups {
        segs.push((*g).to_string());
    }
    (segs, (*leaf_part).to_string(), registry_elided)
}

/// Split a path component on all separator chars (not `/`) and push the
/// resulting pieces as group segments.
fn split_on_extra_seps(part: &str, sep_chars: &[char], segs: &mut Vec<String>) {
    let extra: Vec<char> = sep_chars.iter().copied().filter(|&c| c != '/').collect();
    if extra.is_empty() {
        if !part.is_empty() {
            segs.push(part.to_string());
        }
    } else {
        for piece in split_on_chars(part, &extra) {
            // Filter empty pieces from leading/trailing/consecutive separators.
            if !piece.is_empty() {
                segs.push(piece.to_string());
            }
        }
    }
}

/// Split `s` on any of the chars in `seps`, returning the pieces.
fn split_on_chars<'a>(s: &'a str, seps: &[char]) -> Vec<&'a str> {
    // Use split() with a closure that returns true for any separator.
    s.split(|c: char| seps.contains(&c)).collect()
}

/// A mutable trie used only while building; converted to [`Node`]s once
/// every row is inserted.
#[derive(Default)]
struct Trie {
    /// Sub-tries keyed by group label (BTreeMap gives label-sorted order).
    groups: BTreeMap<String, Trie>,
    /// Leaf entries: (label, rows_index, state).
    leaves: Vec<(String, usize, ArtifactState)>,
}

impl Trie {
    /// Insert a leaf reachable via `groups` path from this trie level.
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
        let mut rows: Vec<usize> = Vec::new();

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

/// Build the catalog hierarchy from the filtered subset of `rows`.
///
/// Only the rows at indices `filtered` are included as leaves; ancestor
/// groups are created automatically. `opts` controls registry elision,
/// type-level grouping, and the path separators.
pub fn build(rows: &[TuiRow], filtered: &[usize], opts: &TreeBuildOptions) -> Tree {
    let sep_chars = normalize_separators(&opts.separators);
    let mut trie = Trie::default();

    for &i in filtered {
        let Some(r) = rows.get(i) else {
            continue;
        };
        // `registry_elided` is the single source of truth from `segments()`.
        // It is true when the full `default_registry` prefix (including any
        // namespace such as "ghcr.io/acme") was stripped. `build()` must NOT
        // re-derive this with a host-only comparison — that was A1's bug.
        let (mut groups, leaf, registry_elided) = segments(&r.repo, opts.default_registry.as_deref(), &sep_chars);

        // Insert a type-level group between the registry root and the path
        // segments when `group_by_type` is enabled. The insertion point is:
        //   - index 0 when the registry was elided (type group is the new root)
        //   - index 1 when the registry was kept (type group sits after the host)
        if opts.group_by_type {
            let insert_at = if registry_elided { 0 } else { 1 };
            groups.insert(insert_at, r.kind.clone());
        }

        trie.insert(&groups, leaf, i, r.state);
    }

    let (roots, _, _) = trie.into_nodes("", 0);
    Tree { roots }
}

/// Flatten the tree to the visible lines: a preorder walk where a
/// collapsed group emits its header but not its descendants.
///
/// `rows` is the full catalog row slice, threaded into [`walk`] so bundle
/// leaves can populate their new `is_bundle` / `key` / `collapsed` fields.
///
/// `expanded_bundles` carries the bundle-leaf expand state (separate from the
/// group `collapsed` set — see plan D3a / GAP-1). For bundle leaf visibility
/// in the output this parameter is informational only in P1 (the member-splice
/// gate in [`flatten_with_members`] is where it is enforced in P3).
pub fn flatten(
    tree: &Tree,
    collapsed: &BTreeSet<String>,
    expanded_bundles: &BTreeSet<String>,
    rows: &[TuiRow],
) -> Vec<DisplayRow> {
    let mut out = Vec::new();
    walk(&tree.roots, collapsed, expanded_bundles, rows, &mut out);
    out
}

/// Flatten the tree and splice virtual `DisplayRow::Member` rows
/// immediately after each `DisplayRow::Leaf` whose bundle member cache is
/// `Ready` (and the bundle leaf is visible — not inside a collapsed group).
///
/// # Contract (C-4)
///
/// - Each `DisplayRow::Member` appears immediately after the `DisplayRow::Leaf`
///   whose corresponding `rows[row].kind == "bundle"` in `Ready` member order.
/// - A bundle leaf with no cache entry produces zero member rows (identical
///   to today's `flatten`).
/// - `Loading`, `Failed`, `Offline` each produce exactly ONE placeholder
///   `Member`-shaped row.
/// - Member rows report depth = parent bundle leaf depth + 1.
/// - Pure: no I/O; deterministic given inputs (same inputs → same output).
/// - Index isolation: the produced `Vec<DisplayRow>` introduces no new
///   `rows`/`filtered`/`marked` indices; `Member` carries no `row: usize`.
///
/// `scope` is the active scope label (`TuiState::scope_label`) used as
/// the first component of the `BundleMemberKey`.
/// Flatten the tree and splice virtual `DisplayRow::Member` rows
/// immediately after each `DisplayRow::Leaf` whose bundle member cache is
/// `Ready` (and the bundle leaf is visible — not inside a collapsed group).
///
/// The `expanded_bundles` set gates member splicing: a bundle leaf's members
/// are only spliced when its key is present in `expanded_bundles`. This is
/// ORTHOGONAL to `collapsed`, which gates group descendants (GAP-1 / D4):
///
/// - `collapsed` is consumed by the inner [`flatten`] call to hide group
///   descendants. It is **never** replaced by `expanded_bundles`.
/// - `expanded_bundles` is the NEW additional parameter that gates whether
///   a bundle leaf's members appear. Bundle leaves default-collapsed (absent
///   from `expanded_bundles` = no member rows). Empty `expanded_bundles`
///   (P1 default) means no members are spliced — identical to Phase 2 behavior
///   when this function is called with the real gate in P3.1.
///
/// Note (P1 / GAP-1): during the stub phase the splice still fires on cache
/// presence (existing behavior is preserved) so existing tests continue to
/// pass. P3.1 adds the `expanded_bundles.contains(&leaf_key)` gate.
pub fn flatten_with_members(
    tree: &Tree,
    collapsed: &BTreeSet<String>,
    expanded_bundles: &BTreeSet<String>,
    bundle_members: &std::collections::HashMap<
        super::bundle_members::BundleMemberKey,
        super::bundle_members::BundleMemberCache,
    >,
    scope: &str,
    rows: &[TuiRow],
) -> Vec<DisplayRow> {
    use super::bundle_members::BundleMemberCache;

    // Start with the plain flattened tree (collapsed groups already handled).
    // Pass BOTH collapsed AND expanded_bundles so walk can populate the new
    // Leaf fields; expanded_bundles does NOT replace collapsed here (GAP-1).
    let flat = flatten(tree, collapsed, expanded_bundles, rows);

    // Post-pass: splice Member rows after each bundle leaf that has a cache
    // entry. Non-bundle leaves and leaves with no cache entry are passed through
    // unchanged — identical to the plain `flatten` output for those rows.
    let mut out = Vec::with_capacity(flat.len());

    for display_row in flat {
        match &display_row {
            DisplayRow::Leaf {
                row,
                depth,
                key: leaf_key,
                ..
            } => {
                let row_idx = *row;
                let leaf_depth = *depth;
                // F3: leaf_key is no longer needed for the splice gate (now uses
                // tui_row.repo); only row_idx and leaf_depth are forwarded.
                let _ = leaf_key;

                // Push the leaf first (bundle or not, it is always visible).
                out.push(display_row);

                // Check if this leaf's row is a bundle kind.
                let Some(tui_row) = rows.get(row_idx) else {
                    continue;
                };
                if tui_row.kind != "bundle" {
                    continue;
                }

                // P3.1: Gate member splice on expanded_bundles membership.
                // A bundle leaf absent from expanded_bundles is collapsed —
                // produce zero member rows regardless of cache state.
                // F3: key is the FULL bundle repo (rows[row].repo), not the
                // display-path leaf key, so the gate is stable even when the
                // default-registry changes or two bundles share a final path
                // component.
                if !expanded_bundles.contains(tui_row.repo.as_str()) {
                    continue;
                }

                // Look up the cache for this (scope, bundle_repo) pair.
                let key: super::bundle_members::BundleMemberKey = (scope.to_string(), tui_row.repo.clone());
                let Some(cache_entry) = bundle_members.get(&key) else {
                    // No cache entry → zero member rows (identical to plain flatten).
                    continue;
                };

                let member_depth = leaf_depth + 1;
                let parent_bundle_repo = tui_row.repo.clone();

                match cache_entry {
                    BundleMemberCache::Ready(members) => {
                        for m in members {
                            out.push(DisplayRow::Member {
                                label: m.label.clone(),
                                depth: member_depth,
                                kind: m.kind,
                                state: m.state,
                                related: m.related,
                                parent_bundle_repo: parent_bundle_repo.clone(),
                                // P1.2: propagate the member's own repo so the
                                // action layer can synthesize TuiAction::MemberAction.
                                member_repo: m.member_repo.clone(),
                            });
                        }
                    }
                    BundleMemberCache::Loading => {
                        // ASCII label: no glyph-guard mechanism exists for the
                        // TUI, so U+2026 (HORIZONTAL ELLIPSIS) is replaced with
                        // plain ASCII `...` per the plan's ASCII-fallback rule.
                        out.push(DisplayRow::Member {
                            label: "loading...".to_string(),
                            depth: member_depth,
                            kind: crate::oci::ArtifactKind::Skill,
                            state: ArtifactState::NotInstalled,
                            related: false,
                            parent_bundle_repo,
                            // Placeholders carry no actionable repo.
                            member_repo: None,
                        });
                    }
                    BundleMemberCache::Failed(reason) => {
                        // Sanitize the reason at the display boundary.
                        // ASCII label: U+2014 (EM DASH) replaced with plain `-`.
                        let sanitized = super::render::sanitize_member_label(reason);
                        out.push(DisplayRow::Member {
                            label: format!("error - {sanitized}"),
                            depth: member_depth,
                            kind: crate::oci::ArtifactKind::Skill,
                            state: ArtifactState::NotInstalled,
                            related: false,
                            parent_bundle_repo,
                            member_repo: None,
                        });
                    }
                    BundleMemberCache::Offline => {
                        // ASCII label: U+2014 (EM DASH) replaced with plain `-`.
                        out.push(DisplayRow::Member {
                            label: "(offline - members unavailable)".to_string(),
                            depth: member_depth,
                            kind: crate::oci::ArtifactKind::Skill,
                            state: ArtifactState::NotInstalled,
                            related: false,
                            parent_bundle_repo,
                            member_repo: None,
                        });
                    }
                }
            }
            // Groups and Members pass through as-is (Members would only exist
            // if this function were called recursively, which it is not).
            DisplayRow::Group { .. } | DisplayRow::Member { .. } => {
                out.push(display_row);
            }
        }
    }

    out
}

fn walk(
    nodes: &[Node],
    collapsed: &BTreeSet<String>,
    expanded_bundles: &BTreeSet<String>,
    rows: &[TuiRow],
    out: &mut Vec<DisplayRow>,
) {
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
                    walk(&g.children, collapsed, expanded_bundles, rows, out);
                }
            }
            Node::Leaf(l) => {
                // P1.1: populate the three new Leaf fields from the rows slice
                // and the expanded_bundles set.
                let is_bundle = rows.get(l.row).map(|r| r.kind == "bundle").unwrap_or(false);
                // F3: expanded_bundles is keyed by the FULL bundle repo (rows[l.row].repo),
                // NOT by the display-path leaf key (l.key). The `collapsed` field
                // must use the same key — a bundle is expanded iff its full repo is
                // in the set. Non-bundle leaves always report false.
                let leaf_collapsed = if is_bundle {
                    let full_repo = rows.get(l.row).map(|r| r.repo.as_str()).unwrap_or("");
                    !expanded_bundles.contains(full_repo)
                } else {
                    false
                };
                out.push(DisplayRow::Leaf {
                    label: l.label.clone(),
                    depth: l.depth,
                    row: l.row,
                    state: l.state,
                    key: l.key.clone(),
                    is_bundle,
                    collapsed: leaf_collapsed,
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::state::TuiRow;

    // ── helpers ──────────────────────────────────────────────────────────────

    fn row(repo: &str, kind: &str, state: ArtifactState) -> TuiRow {
        TuiRow {
            kind: kind.to_string(),
            repo: repo.to_string(),
            description: String::new(),
            summary: String::new(),
            keywords: vec![],
            repository_url: None,
            latest_tag: "latest".to_string(),
            version: "1.0.0".to_string(),
            pinned_version: None,
            state,
        }
    }

    fn skill_row(repo: &str, state: ArtifactState) -> TuiRow {
        row(repo, "skill", state)
    }

    fn opts_default(default_registry: Option<&str>) -> TreeBuildOptions {
        TreeBuildOptions {
            default_registry: default_registry.map(|s| s.to_string()),
            group_by_type: false,
            separators: vec!["/".to_string()],
        }
    }

    /// Flatten the tree with no collapsed groups and collect
    /// `(label, depth, is_group)` tuples for easy assertions.
    fn shape(tree: &Tree) -> Vec<(String, usize, bool)> {
        // Use empty expanded_bundles and a dummy empty rows slice — shape()
        // only checks group/leaf topology, not bundle-kind fields.
        flatten(tree, &BTreeSet::new(), &BTreeSet::new(), &[])
            .into_iter()
            .map(|d| match d {
                DisplayRow::Group { label, depth, .. } => (label, depth, true),
                DisplayRow::Leaf { label, depth, .. } => (label, depth, false),
                DisplayRow::Member { label, depth, .. } => (label, depth, false),
            })
            .collect()
    }

    // ── Step 3.1: tree::build / segments ─────────────────────────────────────

    // Default separators `["/"]`: only slashes split into groups.
    // `acme/code.review` → groups `[acme]`, leaf `code.review`
    // (dot does NOT split under default separators).
    #[test]
    fn default_separators_slash_only_dot_stays_in_leaf() {
        let rows = vec![skill_row("registry.io/acme/code.review", ArtifactState::Installed)];
        let t = build(&rows, &[0], &opts_default(Some("registry.io")));
        let s = shape(&t);
        // registry.io elided (default); acme is a group; code.review is the leaf
        assert_eq!(
            s,
            vec![("acme".to_string(), 0, true), ("code.review".to_string(), 1, false),],
            "dot must NOT split under default separators"
        );
    }

    // With `["/", "."]`: `acme/code.review` → groups `[acme, code]`, leaf `review`
    #[test]
    fn dot_separator_splits_final_segment() {
        let opts = TreeBuildOptions {
            default_registry: Some("reg".to_string()),
            group_by_type: false,
            separators: vec!["/".to_string(), ".".to_string()],
        };
        let rows = vec![skill_row("reg/acme/code.review", ArtifactState::Installed)];
        let t = build(&rows, &[0], &opts);
        let s = shape(&t);
        assert_eq!(
            s,
            vec![
                ("acme".to_string(), 0, true),
                ("code".to_string(), 1, true),
                ("review".to_string(), 2, false),
            ],
            "dot separator must produce acme → code → review nesting"
        );
    }

    // With `["/", "-"]`: `acme/code-review` → groups `[acme, code]`, leaf `review`
    #[test]
    fn hyphen_separator_splits_when_configured() {
        let opts = TreeBuildOptions {
            default_registry: Some("reg".to_string()),
            group_by_type: false,
            separators: vec!["/".to_string(), "-".to_string()],
        };
        let rows = vec![skill_row("reg/acme/code-review", ArtifactState::Installed)];
        let t = build(&rows, &[0], &opts);
        let s = shape(&t);
        assert_eq!(
            s,
            vec![
                ("acme".to_string(), 0, true),
                ("code".to_string(), 1, true),
                ("review".to_string(), 2, false),
            ],
            "hyphen separator must nest acme → code → review when configured"
        );
    }

    // Hyphen does NOT split under default separators.
    #[test]
    fn hyphen_stays_in_leaf_under_default_separators() {
        let rows = vec![skill_row("reg/acme/code-review", ArtifactState::Installed)];
        let t = build(&rows, &[0], &opts_default(Some("reg")));
        let s = shape(&t);
        assert_eq!(
            s,
            vec![("acme".to_string(), 0, true), ("code-review".to_string(), 1, false),],
            "hyphen must not split under default separators"
        );
    }

    // Default-registry root is elided from display.
    #[test]
    fn default_registry_root_is_elided() {
        let rows = vec![skill_row("myregistry.io/acme/tool", ArtifactState::Installed)];
        let t = build(&rows, &[0], &opts_default(Some("myregistry.io")));
        let s = shape(&t);
        // Root of hierarchy starts with "acme", not "myregistry.io"
        assert!(
            s.iter().all(|(label, _, _)| label != "myregistry.io"),
            "default registry must be elided from the tree root"
        );
        assert_eq!(s[0].0, "acme");
    }

    // Codex-M regression: a namespaced default_registry (host + namespace,
    // e.g. "ghcr.io/acme") must elide the WHOLE prefix from the tree root —
    // matching the flat view's `strip_default_registry` — not just the host.
    #[test]
    fn namespaced_default_registry_root_is_fully_elided() {
        let rows = vec![skill_row("ghcr.io/acme/skills/code-review", ArtifactState::Installed)];
        let t = build(&rows, &[0], &opts_default(Some("ghcr.io/acme")));
        let s = shape(&t);
        // Neither the host nor the namespace survives as a group.
        assert!(
            s.iter().all(|(label, _, _)| label != "ghcr.io" && label != "acme"),
            "namespaced default registry (host + namespace) must be fully elided; got: {s:?}"
        );
        // The remaining path groups under "skills" with leaf "code-review".
        assert_eq!(s[0].0, "skills", "first group is the path after the elided registry");
        assert!(
            s.iter().any(|(label, _, is_group)| label == "code-review" && !is_group),
            "leaf is the bare final component; got: {s:?}"
        );
    }

    // Non-default registry is kept as a root group.
    #[test]
    fn non_default_registry_kept_as_root_group() {
        let rows = vec![skill_row("ghcr.io/acme/tool", ArtifactState::Installed)];
        let t = build(&rows, &[0], &opts_default(Some("myregistry.io")));
        let s = shape(&t);
        assert_eq!(
            s[0],
            ("ghcr.io".to_string(), 0, true),
            "non-default registry must appear as a root group"
        );
    }

    // Consecutive separators produce no empty-label group nodes.
    // `"acme//tool"` with separator `["/"]` must yield the same shape as
    // `"acme/tool"` — the empty string between the two slashes is dropped.
    #[test]
    fn consecutive_separators_produce_no_empty_label_groups() {
        let rows = vec![skill_row("reg/acme//tool", ArtifactState::Installed)];
        let t = build(&rows, &[0], &opts_default(Some("reg")));
        let s = shape(&t);
        // There must be no node with an empty label.
        assert!(
            s.iter().all(|(label, _, _)| !label.is_empty()),
            "no empty-label group must appear for consecutive separators; got: {s:?}"
        );
        // The hierarchy must still be: acme (group) → tool (leaf).
        assert_eq!(
            s,
            vec![("acme".to_string(), 0, true), ("tool".to_string(), 1, false),],
            "consecutive separators must collapse to the same shape as a single separator"
        );
    }

    // Leading separator in the repository path produces no empty root group.
    #[test]
    fn leading_separator_produces_no_empty_label_group() {
        // Repository path after registry elision starts with "/tool" (leading slash).
        let rows = vec![skill_row("reg//tool", ArtifactState::Installed)];
        let t = build(&rows, &[0], &opts_default(Some("reg")));
        let s = shape(&t);
        assert!(
            s.iter().all(|(label, _, _)| !label.is_empty()),
            "no empty-label group must appear for leading separator; got: {s:?}"
        );
    }

    // Malformed repo without `/` → single top-level leaf.
    #[test]
    fn malformed_repo_without_slash_is_top_level_leaf() {
        let rows = vec![skill_row("noslash", ArtifactState::NotInstalled)];
        let t = build(&rows, &[0], &opts_default(None));
        let s = shape(&t);
        assert_eq!(s.len(), 1);
        assert_eq!(s[0], ("noslash".to_string(), 0, false));
    }

    // Empty `separators` normalizes to `["/"]`.
    #[test]
    fn empty_separators_normalizes_to_slash() {
        let opts_empty = TreeBuildOptions {
            default_registry: Some("reg".to_string()),
            group_by_type: false,
            separators: vec![],
        };
        let opts_slash = opts_default(Some("reg"));
        let rows = vec![skill_row("reg/acme/code.review", ArtifactState::Installed)];
        let t_empty = build(&rows, &[0], &opts_empty);
        let t_slash = build(&rows, &[0], &opts_slash);
        // Both should produce the same shape (dot not split, slash only)
        assert_eq!(
            shape(&t_empty),
            shape(&t_slash),
            "empty separators must behave identically to ['/']"
        );
    }

    // Groups sort before leaves; both sorted by label.
    #[test]
    fn groups_before_leaves_both_sorted_by_label() {
        let rows = vec![
            skill_row("reg/acme/zeta", ArtifactState::Installed),
            skill_row("reg/acme/group/inner", ArtifactState::NotInstalled),
            skill_row("reg/acme/alpha", ArtifactState::Installed),
        ];
        let t = build(&rows, &[0, 1, 2], &opts_default(Some("reg")));
        let s = shape(&t);
        // acme group at root, inside: group subgroup first, then alpha + zeta leaves sorted
        assert_eq!(
            s,
            vec![
                ("acme".to_string(), 0, true),
                ("group".to_string(), 1, true),
                ("inner".to_string(), 2, false),
                ("alpha".to_string(), 1, false),
                ("zeta".to_string(), 1, false),
            ]
        );
    }

    // `group_by_type = true` inserts a type-group level between registry
    // root and path segments.
    #[test]
    fn group_by_type_inserts_type_level() {
        let opts = TreeBuildOptions {
            default_registry: Some("reg".to_string()),
            group_by_type: true,
            separators: vec!["/".to_string()],
        };
        let rows = vec![
            row("reg/acme/tool", "skill", ArtifactState::Installed),
            row("reg/acme/style-guide", "rule", ArtifactState::NotInstalled),
        ];
        let t = build(&rows, &[0, 1], &opts);
        let s = shape(&t);
        // There must be at least one group with label "skill" or "rule"
        let type_groups: Vec<&str> = s
            .iter()
            .filter(|(_, _, is_group)| *is_group)
            .map(|(label, _, _)| label.as_str())
            .filter(|l| *l == "skill" || *l == "rule")
            .collect();
        assert!(
            !type_groups.is_empty(),
            "group_by_type must insert type-level groups (skill/rule); got: {s:?}"
        );
    }

    // Build over a `filtered` subset yields only matching leaves + their
    // ancestor groups.
    #[test]
    fn filtered_subset_yields_only_matching_leaves_and_ancestors() {
        let rows = vec![
            skill_row("reg/acme/alpha", ArtifactState::Installed),
            skill_row("reg/acme/beta", ArtifactState::NotInstalled),
            skill_row("reg/other/gamma", ArtifactState::Installed),
        ];
        // Only include row 0 (alpha) and row 2 (gamma) in the filtered set
        let t = build(&rows, &[0, 2], &opts_default(Some("reg")));
        let s = shape(&t);
        // beta (row 1) must not appear; acme and other groups must appear
        // as ancestors of alpha and gamma respectively
        assert!(
            s.iter().all(|(label, _, _)| label != "beta"),
            "beta must not appear — it is not in filtered"
        );
        let leaf_labels: Vec<&str> = s
            .iter()
            .filter(|(_, _, is_group)| !is_group)
            .map(|(label, _, _)| label.as_str())
            .collect();
        assert!(leaf_labels.contains(&"alpha"), "alpha must be a leaf");
        assert!(leaf_labels.contains(&"gamma"), "gamma must be a leaf");
    }

    // `group_by_type = true` combined with `["/", "-"]` separators:
    // `reg/acme/code-review` → type group `skill` → path group `acme` →
    // sub-group `code` → leaf `review`.
    #[test]
    fn group_by_type_with_hyphen_separator_produces_correct_nesting() {
        let opts = TreeBuildOptions {
            default_registry: Some("reg".to_string()),
            group_by_type: true,
            separators: vec!["/".to_string(), "-".to_string()],
        };
        let rows = vec![row("reg/acme/code-review", "skill", ArtifactState::Installed)];
        let t = build(&rows, &[0], &opts);
        let s = shape(&t);
        // Expected nesting: skill (type) → acme (org) → code (sub) → review (leaf)
        // The registry "reg" is elided (it equals default_registry).
        // With group_by_type: type group inserted at index 0 (registry was elided).
        // So groups: [skill, acme, code], leaf: review
        assert_eq!(
            s.len(),
            4,
            "must produce 4 display rows (3 groups + 1 leaf); got: {s:?}"
        );
        assert_eq!(s[0], ("skill".to_string(), 0, true), "depth-0 group must be the type");
        assert_eq!(s[1], ("acme".to_string(), 1, true), "depth-1 group must be the org");
        assert_eq!(
            s[2],
            ("code".to_string(), 2, true),
            "depth-2 group must be the hyphen-split prefix"
        );
        assert_eq!(s[3], ("review".to_string(), 3, false), "leaf must be the final segment");
    }

    // ── Rollup::add / merge / worst ───────────────────────────────────────────

    #[test]
    fn rollup_add_increments_correct_bucket() {
        let mut r = Rollup::default();
        r.add(ArtifactState::Installed);
        assert_eq!(r.total, 1);
        assert_eq!(r.installed, 1);
        r.add(ArtifactState::NotInstalled);
        assert_eq!(r.total, 2);
        assert_eq!(r.not_installed, 1);
        r.add(ArtifactState::Outdated);
        assert_eq!(r.outdated, 1);
        r.add(ArtifactState::Modified);
        assert_eq!(r.modified, 1);
        r.add(ArtifactState::IntegrityMissing);
        assert_eq!(r.integrity_missing, 1);
        assert_eq!(r.total, 5);
    }

    #[test]
    fn rollup_merge_sums_all_fields() {
        let mut a = Rollup {
            total: 2,
            installed: 1,
            not_installed: 1,
            outdated: 0,
            modified: 0,
            integrity_missing: 0,
        };
        let b = Rollup {
            total: 3,
            installed: 0,
            not_installed: 0,
            outdated: 1,
            modified: 1,
            integrity_missing: 1,
        };
        a.merge(b);
        assert_eq!(a.total, 5);
        assert_eq!(a.installed, 1);
        assert_eq!(a.not_installed, 1);
        assert_eq!(a.outdated, 1);
        assert_eq!(a.modified, 1);
        assert_eq!(a.integrity_missing, 1);
    }

    // Precedence: IntegrityMissing > Modified > Outdated > NotInstalled > Installed
    #[test]
    fn rollup_worst_precedence() {
        let mut r = Rollup::default();
        r.add(ArtifactState::Installed);
        assert_eq!(r.worst(), ArtifactState::Installed);

        r.add(ArtifactState::NotInstalled);
        assert_eq!(r.worst(), ArtifactState::NotInstalled);

        r.add(ArtifactState::Outdated);
        assert_eq!(r.worst(), ArtifactState::Outdated);

        r.add(ArtifactState::Modified);
        assert_eq!(r.worst(), ArtifactState::Modified);

        r.add(ArtifactState::IntegrityMissing);
        assert_eq!(r.worst(), ArtifactState::IntegrityMissing);
    }

    // Empty group (total == 0) → NotInstalled.
    #[test]
    fn rollup_worst_empty_is_not_installed() {
        let r = Rollup::default();
        assert_eq!(r.worst(), ArtifactState::NotInstalled);
    }

    // ── flatten / collapse ────────────────────────────────────────────────────

    // Preorder: a collapsed group emits only its header (hides descendants),
    // but its `rows` field still tracks all descendant leaf indices.
    #[test]
    fn flatten_collapsed_group_hides_descendants_keeps_rows() {
        let rows = vec![
            skill_row("reg/acme/a", ArtifactState::Installed),
            skill_row("reg/acme/b", ArtifactState::NotInstalled),
        ];
        let t = build(&rows, &[0, 1], &opts_default(Some("reg")));
        let mut collapsed = BTreeSet::new();
        // Collapse the "acme" group
        collapsed.insert("acme".to_string());
        let flat = flatten(&t, &collapsed, &BTreeSet::new(), &rows);
        assert_eq!(flat.len(), 1, "collapsed group hides its descendants");
        match &flat[0] {
            DisplayRow::Group {
                collapsed,
                rows,
                rollup,
                ..
            } => {
                assert!(*collapsed, "the group must report itself as collapsed");
                assert_eq!(rows.len(), 2, "both descendant row indices tracked");
                assert!(rows.contains(&0) && rows.contains(&1));
                assert_eq!(rollup.total, 2);
            }
            other => panic!("expected a group, got {other:?}"),
        }
    }

    // ── A1 regression: namespaced default_registry + group_by_type ─────────────
    //
    // `default_registry = "ghcr.io/acme"` and `group_by_type = true`: the type
    // group must be at ROOT (no registry group, no namespace group).
    // Shape: `<type>(skill) → skills → code-review` — NOT `skills → <type> → code-review`.
    //
    // This test FAILS before the fix: the old host-only re-derivation in `build()`
    // wrongly concludes the registry was NOT elided (because
    // `"ghcr.io/acme" != "ghcr.io"`) and inserts the type group at index 1
    // instead of index 0, producing `skills → skill → code-review`.
    #[test]
    fn group_by_type_namespaced_default_registry_type_group_at_root() {
        let opts = TreeBuildOptions {
            default_registry: Some("ghcr.io/acme".to_string()),
            group_by_type: true,
            separators: vec!["/".to_string()],
        };
        // repo: "ghcr.io/acme/skills/code-review"
        // After eliding "ghcr.io/acme/", repository path = "skills/code-review"
        // groups: ["skills"], leaf: "code-review"
        // With group_by_type: type group "skill" inserted at index 0 (registry was elided)
        // Expected: skill(0) → skills(1) → code-review leaf(2)
        let rows = vec![row(
            "ghcr.io/acme/skills/code-review",
            "skill",
            ArtifactState::Installed,
        )];
        let t = build(&rows, &[0], &opts);
        let s = shape(&t);
        // The type group must be at root (depth 0), NOT nested inside a path group.
        let type_group_pos = s.iter().position(|(label, _, is_group)| label == "skill" && *is_group);
        assert!(type_group_pos.is_some(), "type group 'skill' must appear; got: {s:?}");
        assert_eq!(
            type_group_pos.unwrap(),
            0,
            "type group 'skill' must be the first (root) group; got: {s:?}"
        );
        // Neither "ghcr.io" nor "acme" must appear as a group.
        assert!(
            s.iter().all(|(label, _, _)| label != "ghcr.io" && label != "acme"),
            "namespaced default registry must be fully elided; got: {s:?}"
        );
    }

    // `group_by_type = true` with a non-default registry (insert_at = 1 branch):
    // the registry host is kept as the root group and the type group is nested
    // directly under it (at depth 1), not at the root (depth 0).
    //
    // Rows: `["ghcr.io/acme/tool"]` kind `skill`, `default_registry = "reg"`.
    // `ghcr.io` is NOT the default registry, so it is kept as a root group.
    // With `group_by_type`: type group `skill` is inserted at index 1 (after
    // the registry host), so the shape is:
    //   ghcr.io  (depth 0, group)
    //     skill  (depth 1, group)
    //       acme (depth 2, group)
    //         tool (depth 3, leaf)
    #[test]
    fn group_by_type_non_default_registry_type_group_nested_under_registry() {
        let opts = TreeBuildOptions {
            default_registry: Some("reg".to_string()),
            group_by_type: true,
            separators: vec!["/".to_string()],
        };
        let rows = vec![row("ghcr.io/acme/tool", "skill", ArtifactState::Installed)];
        let t = build(&rows, &[0], &opts);
        let s = shape(&t);
        // Expected: ghcr.io(depth 0, group) → skill(depth 1, group) → acme(depth 2, group) → tool(depth 3, leaf)
        assert_eq!(
            s.len(),
            4,
            "must produce 4 display rows (3 groups + 1 leaf); got: {s:?}"
        );
        assert_eq!(
            s[0],
            ("ghcr.io".to_string(), 0, true),
            "depth-0 group must be the non-default registry host; got: {s:?}"
        );
        assert_eq!(
            s[1],
            ("skill".to_string(), 1, true),
            "depth-1 group must be the type (insert_at=1 branch); got: {s:?}"
        );
        assert_eq!(
            s[2],
            ("acme".to_string(), 2, true),
            "depth-2 group must be the org path component; got: {s:?}"
        );
        assert_eq!(
            s[3],
            ("tool".to_string(), 3, false),
            "depth-3 must be the leaf; got: {s:?}"
        );
    }

    // ── C-4 flatten_with_members ──────────────────────────────────────────────
    //
    // All of these tests FAIL until P3 implements `flatten_with_members`.
    // They call the stub, which panics with `unimplemented!`.

    use crate::oci::ArtifactKind;
    use crate::tui::bundle_members::{BundleMemberCache, BundleMemberKey, MemberNode};
    use std::collections::HashMap;

    fn make_member(label: &str, kind: ArtifactKind, related: bool) -> MemberNode {
        MemberNode {
            kind,
            label: label.to_string(),
            member_repo: Some(format!("reg/acme/{label}")),
            state: ArtifactState::NotInstalled,
            related,
        }
    }

    fn empty_cache() -> HashMap<BundleMemberKey, BundleMemberCache> {
        HashMap::new()
    }

    #[test]
    fn flatten_with_members_no_cache_produces_same_output_as_flatten() {
        // C-4: bundle leaf with no cache entry → zero member rows.
        // flatten_with_members with an empty cache must produce the same rows
        // as plain flatten (no member rows injected).
        let rows = vec![
            row("reg/acme/bundle-x", "bundle", ArtifactState::Installed),
            skill_row("reg/acme/alpha", ArtifactState::Installed),
        ];
        let t = build(&rows, &[0, 1], &opts_default(Some("reg")));
        let collapsed = BTreeSet::new();
        let cache = empty_cache();

        let expanded_bundles = BTreeSet::new();
        let with_members = flatten_with_members(&t, &collapsed, &expanded_bundles, &cache, "project", &rows);
        let without_members = flatten(&t, &collapsed, &expanded_bundles, &rows);

        assert_eq!(
            with_members, without_members,
            "C-4: no-cache must produce identical output to flatten"
        );
    }

    #[test]
    fn flatten_with_members_ready_cache_splices_member_rows_after_bundle_leaf() {
        // C-4: Ready cache entry → member rows appear immediately after the
        // bundle leaf, in cache order, depth = bundle_depth + 1.
        let rows = vec![row("reg/acme/bundle-x", "bundle", ArtifactState::Installed)];
        let t = build(&rows, &[0], &opts_default(Some("reg")));
        let collapsed = BTreeSet::new();
        let mut cache: HashMap<BundleMemberKey, BundleMemberCache> = HashMap::new();
        cache.insert(
            ("project".to_string(), "reg/acme/bundle-x".to_string()),
            BundleMemberCache::Ready(vec![
                make_member("skill-a", ArtifactKind::Skill, false),
                make_member("skill-b", ArtifactKind::Skill, true),
            ]),
        );

        // P3.1: key must be in expanded_bundles for members to appear.
        let mut expanded_bundles = BTreeSet::new();
        expanded_bundles.insert("reg/acme/bundle-x".to_string()); // F3: full repo key
        let flat = flatten_with_members(&t, &collapsed, &expanded_bundles, &cache, "project", &rows);

        // Expected: group(acme) + Leaf(bundle-x) + Member(skill-a) + Member(skill-b)
        assert_eq!(flat.len(), 4, "C-4: group + leaf + 2 members; got {flat:?}");

        // The bundle leaf is at index 1.
        let bundle_depth = match &flat[1] {
            DisplayRow::Leaf { depth, .. } => *depth,
            other => panic!("expected Leaf at index 1, got {other:?}"),
        };

        // Members immediately after leaf.
        match &flat[2] {
            DisplayRow::Member {
                label, depth, related, ..
            } => {
                assert_eq!(label, "skill-a", "C-4: first member label");
                assert_eq!(*depth, bundle_depth + 1, "C-4: member depth = leaf_depth + 1");
                assert!(!related, "C-4: skill-a is not related");
            }
            other => panic!("expected Member at index 2, got {other:?}"),
        }
        match &flat[3] {
            DisplayRow::Member { label, related, .. } => {
                assert_eq!(label, "skill-b", "C-4: second member label");
                assert!(*related, "C-4: skill-b is related");
            }
            other => panic!("expected Member at index 3, got {other:?}"),
        }
    }

    #[test]
    fn flatten_with_members_loading_produces_one_placeholder_member() {
        // C-4: Loading cache entry → exactly ONE placeholder Member row.
        let rows = vec![row("reg/acme/bundle-x", "bundle", ArtifactState::Installed)];
        let t = build(&rows, &[0], &opts_default(Some("reg")));
        let collapsed = BTreeSet::new();
        let mut cache: HashMap<BundleMemberKey, BundleMemberCache> = HashMap::new();
        cache.insert(
            ("project".to_string(), "reg/acme/bundle-x".to_string()),
            BundleMemberCache::Loading,
        );

        // P3.1: key must be in expanded_bundles for members to appear.
        let mut expanded_bundles = BTreeSet::new();
        expanded_bundles.insert("reg/acme/bundle-x".to_string()); // F3: full repo key
        let flat = flatten_with_members(&t, &collapsed, &expanded_bundles, &cache, "project", &rows);

        // group + leaf + 1 placeholder = 3
        assert_eq!(flat.len(), 3, "C-4: Loading → exactly one placeholder; got {flat:?}");
        assert!(
            matches!(&flat[2], DisplayRow::Member { .. }),
            "C-4: placeholder must be a Member variant"
        );
    }

    #[test]
    fn flatten_with_members_failed_produces_one_placeholder_member() {
        // C-4: Failed cache entry → exactly ONE placeholder Member row.
        let rows = vec![row("reg/acme/bundle-x", "bundle", ArtifactState::Installed)];
        let t = build(&rows, &[0], &opts_default(Some("reg")));
        let collapsed = BTreeSet::new();
        let mut cache: HashMap<BundleMemberKey, BundleMemberCache> = HashMap::new();
        cache.insert(
            ("project".to_string(), "reg/acme/bundle-x".to_string()),
            BundleMemberCache::Failed("503 error".to_string()),
        );

        // P3.1: key must be in expanded_bundles for members to appear.
        let mut expanded_bundles = BTreeSet::new();
        expanded_bundles.insert("reg/acme/bundle-x".to_string()); // F3: full repo key
        let flat = flatten_with_members(&t, &collapsed, &expanded_bundles, &cache, "project", &rows);

        assert_eq!(flat.len(), 3, "C-4: Failed → exactly one placeholder; got {flat:?}");
        assert!(
            matches!(&flat[2], DisplayRow::Member { .. }),
            "C-4: Failed placeholder must be a Member variant"
        );
    }

    #[test]
    fn flatten_with_members_offline_produces_one_placeholder_member() {
        // C-4: Offline cache entry → exactly ONE placeholder Member row.
        let rows = vec![row("reg/acme/bundle-x", "bundle", ArtifactState::Installed)];
        let t = build(&rows, &[0], &opts_default(Some("reg")));
        let collapsed = BTreeSet::new();
        let mut cache: HashMap<BundleMemberKey, BundleMemberCache> = HashMap::new();
        cache.insert(
            ("project".to_string(), "reg/acme/bundle-x".to_string()),
            BundleMemberCache::Offline,
        );

        // P3.1: key must be in expanded_bundles for members to appear.
        let mut expanded_bundles = BTreeSet::new();
        expanded_bundles.insert("reg/acme/bundle-x".to_string()); // F3: full repo key
        let flat = flatten_with_members(&t, &collapsed, &expanded_bundles, &cache, "project", &rows);

        assert_eq!(flat.len(), 3, "C-4: Offline → exactly one placeholder; got {flat:?}");
        assert!(
            matches!(&flat[2], DisplayRow::Member { .. }),
            "C-4: Offline placeholder must be a Member variant"
        );
    }

    #[test]
    fn flatten_with_members_scope_keyed_cache_only_matches_correct_scope() {
        // C-4: scope isolation — cache for scope_a must not produce members
        // for scope_b.
        let rows = vec![row("reg/acme/bundle-x", "bundle", ArtifactState::Installed)];
        let t = build(&rows, &[0], &opts_default(Some("reg")));
        let collapsed = BTreeSet::new();
        let mut cache: HashMap<BundleMemberKey, BundleMemberCache> = HashMap::new();
        // Only "scope_a" has a Ready entry.
        cache.insert(
            ("scope_a".to_string(), "reg/acme/bundle-x".to_string()),
            BundleMemberCache::Ready(vec![make_member("skill-x", ArtifactKind::Skill, false)]),
        );

        // P3.1: key must be in expanded_bundles for members to appear.
        let mut expanded_bundles = BTreeSet::new();
        expanded_bundles.insert("reg/acme/bundle-x".to_string()); // F3: full repo key

        // When called with "scope_b", no members should be injected (wrong scope key).
        let flat_b = flatten_with_members(&t, &collapsed, &expanded_bundles, &cache, "scope_b", &rows);
        // Expected: group + leaf only (2 rows)
        assert_eq!(
            flat_b.len(),
            2,
            "C-4: scope_b must not read scope_a's cache; got {flat_b:?}"
        );

        // When called with "scope_a", members are injected.
        let flat_a = flatten_with_members(&t, &collapsed, &expanded_bundles, &cache, "scope_a", &rows);
        assert_eq!(
            flat_a.len(),
            3, // group + leaf + 1 member
            "C-4: scope_a must see its cached member; got {flat_a:?}"
        );
    }

    #[test]
    fn flatten_with_members_member_carries_no_rows_index() {
        // C-4: Index isolation — Member rows must carry no `row: usize` field.
        // Verified structurally: DisplayRow::Member exists without a `row` field
        // (the enum definition itself). We construct one and confirm the variant.
        let m = DisplayRow::Member {
            label: "my-skill".to_string(),
            depth: 2,
            kind: ArtifactKind::Skill,
            state: ArtifactState::NotInstalled,
            related: false,
            parent_bundle_repo: "reg/acme/bundle".to_string(),
            member_repo: None,
        };
        // The variant compiles without a `row` field — this is the structural proof.
        assert!(matches!(m, DisplayRow::Member { .. }), "C-4: Member variant must exist");
    }

    #[test]
    fn flatten_with_members_is_deterministic_same_inputs_same_output() {
        // C-4: Determinism — same tree + cache → same output every call.
        let rows = vec![row("reg/acme/bundle-x", "bundle", ArtifactState::Installed)];
        let t = build(&rows, &[0], &opts_default(Some("reg")));
        let collapsed = BTreeSet::new();
        let mut cache: HashMap<BundleMemberKey, BundleMemberCache> = HashMap::new();
        cache.insert(
            ("project".to_string(), "reg/acme/bundle-x".to_string()),
            BundleMemberCache::Ready(vec![
                make_member("m1", ArtifactKind::Skill, false),
                make_member("m2", ArtifactKind::Rule, false),
            ]),
        );

        let out1 = flatten_with_members(&t, &collapsed, &BTreeSet::new(), &cache, "project", &rows);
        let out2 = flatten_with_members(&t, &collapsed, &BTreeSet::new(), &cache, "project", &rows);
        assert_eq!(out1, out2, "C-4: flatten_with_members must be deterministic");
    }

    #[test]
    fn flatten_with_members_collapsed_bundle_group_hides_all_members() {
        // C-4: A bundle leaf inside a collapsed group must not produce member rows
        // (the leaf is hidden along with its members).
        let rows = vec![row("reg/acme/bundle-x", "bundle", ArtifactState::Installed)];
        let t = build(&rows, &[0], &opts_default(Some("reg")));
        // Collapse the "acme" group.
        let mut collapsed = BTreeSet::new();
        collapsed.insert("acme".to_string());
        let mut cache: HashMap<BundleMemberKey, BundleMemberCache> = HashMap::new();
        cache.insert(
            ("project".to_string(), "reg/acme/bundle-x".to_string()),
            BundleMemberCache::Ready(vec![make_member("skill-a", ArtifactKind::Skill, false)]),
        );

        let flat = flatten_with_members(&t, &collapsed, &BTreeSet::new(), &cache, "project", &rows);
        // Only the collapsed group header is visible; no leaf or member rows.
        assert_eq!(flat.len(), 1, "C-4: collapsed group must hide leaf and members");
        assert!(
            matches!(&flat[0], DisplayRow::Group { .. }),
            "C-4: only the group header should be visible"
        );
    }

    // Expanded group shows header + descendants in preorder.
    #[test]
    fn flatten_expanded_group_shows_descendants_in_preorder() {
        let rows = vec![
            skill_row("reg/acme/alpha", ArtifactState::Installed),
            skill_row("reg/acme/beta", ArtifactState::NotInstalled),
        ];
        let t = build(&rows, &[0, 1], &opts_default(Some("reg")));
        let flat = flatten(&t, &BTreeSet::new(), &BTreeSet::new(), &rows);
        // Header + two leaves = 3 rows
        assert_eq!(flat.len(), 3, "expanded group shows header + 2 leaves");
        assert!(
            matches!(&flat[0], DisplayRow::Group { .. }),
            "first row is the group header"
        );
        assert!(matches!(&flat[1], DisplayRow::Leaf { .. }), "second row is a leaf");
        assert!(matches!(&flat[2], DisplayRow::Leaf { .. }), "third row is a leaf");
    }
}

// ── P2 Specify tests — phase 2 contracts ─────────────────────────────────────
//
// These tests encode the contracts C-1 through C-12 from plan_tui_member_nodes.
// They MUST compile and MUST FAIL against the P1 stubs/unimplemented logic.
// Do NOT implement production logic here — tests only.
#[cfg(test)]
mod p2_member_node_tests {
    use std::collections::{BTreeSet, HashMap};

    use crate::oci::ArtifactKind;
    use crate::tui::bundle_members::{BundleMemberCache, BundleMemberKey, MemberNode};
    use crate::tui::state::{ArtifactState, TuiRow};
    use crate::tui::tree::{DisplayRow, TreeBuildOptions, build, flatten, flatten_with_members};

    fn tui_row(repo: &str, kind: &str, state: ArtifactState) -> TuiRow {
        TuiRow {
            kind: kind.to_string(),
            repo: repo.to_string(),
            description: String::new(),
            summary: String::new(),
            keywords: vec![],
            repository_url: None,
            latest_tag: "latest".to_string(),
            version: "1.0.0".to_string(),
            pinned_version: None,
            state,
        }
    }

    fn bundle_row(repo: &str) -> TuiRow {
        tui_row(repo, "bundle", ArtifactState::NotInstalled)
    }

    fn skill_row(repo: &str) -> TuiRow {
        tui_row(repo, "skill", ArtifactState::Installed)
    }

    fn opts(default_registry: &str) -> TreeBuildOptions {
        TreeBuildOptions {
            default_registry: Some(default_registry.to_string()),
            group_by_type: false,
            separators: vec!["/".to_string()],
        }
    }

    fn make_member(label: &str, repo: &str) -> MemberNode {
        MemberNode {
            kind: ArtifactKind::Skill,
            label: label.to_string(),
            member_repo: Some(repo.to_string()),
            state: ArtifactState::NotInstalled,
            related: false,
        }
    }

    fn ready_cache(
        scope: &str,
        bundle_repo: &str,
        members: Vec<MemberNode>,
    ) -> HashMap<BundleMemberKey, BundleMemberCache> {
        let mut m = HashMap::new();
        m.insert(
            (scope.to_string(), bundle_repo.to_string()),
            BundleMemberCache::Ready(members),
        );
        m
    }

    // ── C-2: default-collapsed ────────────────────────────────────────────────
    //
    // A bundle leaf with a Ready cache entry and an empty `expanded_bundles`
    // must produce ZERO member rows (bundle leaves default-collapsed).

    #[test]
    fn c2_default_collapsed_ready_cache_empty_expanded_bundles_yields_no_members() {
        let rows = vec![bundle_row("reg/acme/bundle-x")];
        let t = build(&rows, &[0], &opts("reg"));
        let cache = ready_cache(
            "project",
            "reg/acme/bundle-x",
            vec![make_member("skill-a", "reg/acme/skill-a")],
        );
        let expanded_bundles = BTreeSet::new(); // no bundle expanded

        let flat = flatten_with_members(&t, &BTreeSet::new(), &expanded_bundles, &cache, "project", &rows);

        let member_count = flat.iter().filter(|r| matches!(r, DisplayRow::Member { .. })).count();
        assert_eq!(
            member_count, 0,
            "C-2: Ready cache + empty expanded_bundles must produce ZERO member rows; flat={flat:?}"
        );
    }

    // ── C-2: walk populates DisplayRow::Leaf.collapsed ────────────────────────
    //
    // A bundle leaf absent from `expanded_bundles` must have `collapsed = true`.
    // A non-bundle leaf must have `collapsed = false`.

    #[test]
    fn c4_walk_bundle_leaf_collapsed_true_when_not_in_expanded_bundles() {
        let rows = vec![bundle_row("reg/acme/bundle-x")];
        let t = build(&rows, &[0], &opts("reg"));
        let expanded_bundles = BTreeSet::new();

        let flat = flatten(&t, &BTreeSet::new(), &expanded_bundles, &rows);

        let bundle_leaf = flat
            .iter()
            .find(|r| matches!(r, DisplayRow::Leaf { is_bundle: true, .. }));
        assert!(bundle_leaf.is_some(), "C-4: must have a bundle leaf");
        if let Some(DisplayRow::Leaf {
            collapsed, is_bundle, ..
        }) = bundle_leaf
        {
            assert!(*is_bundle, "C-4: is_bundle must be true for bundle kind row");
            assert!(
                *collapsed,
                "C-4: bundle leaf absent from expanded_bundles must be collapsed"
            );
        }
    }

    #[test]
    fn c4_walk_non_bundle_leaf_collapsed_always_false() {
        let rows = vec![skill_row("reg/acme/my-skill")];
        let t = build(&rows, &[0], &opts("reg"));
        let expanded_bundles = BTreeSet::new();

        let flat = flatten(&t, &BTreeSet::new(), &expanded_bundles, &rows);

        let non_bundle_leaf = flat
            .iter()
            .find(|r| matches!(r, DisplayRow::Leaf { is_bundle: false, .. }));
        assert!(non_bundle_leaf.is_some(), "C-4: must have a non-bundle leaf");
        if let Some(DisplayRow::Leaf {
            collapsed, is_bundle, ..
        }) = non_bundle_leaf
        {
            assert!(!*is_bundle, "C-4: is_bundle must be false for skill kind row");
            assert!(!*collapsed, "C-4: non-bundle leaf must always have collapsed = false");
        }
    }

    #[test]
    fn c4_walk_bundle_leaf_key_populated() {
        let rows = vec![bundle_row("reg/acme/bundle-x")];
        let t = build(&rows, &[0], &opts("reg"));
        let expanded_bundles = BTreeSet::new();

        let flat = flatten(&t, &BTreeSet::new(), &expanded_bundles, &rows);

        let bundle_leaf = flat
            .iter()
            .find(|r| matches!(r, DisplayRow::Leaf { is_bundle: true, .. }));
        if let Some(DisplayRow::Leaf { key, .. }) = bundle_leaf {
            assert!(!key.is_empty(), "C-4: bundle leaf key must be populated");
            // Per plan D1/D2: key = full path after registry elision.
            // "reg/acme/bundle-x" with default_registry="reg" → key="acme/bundle-x".
            assert_eq!(
                key, "acme/bundle-x",
                "C-4: key must be the full path (not just the leaf label)"
            );
        } else {
            panic!("C-4: expected a bundle leaf in the flat output");
        }
    }

    // ── C-3: splice iff key ∈ expanded_bundles ────────────────────────────────

    #[test]
    fn c3_members_spliced_when_key_in_expanded_bundles() {
        let rows = vec![bundle_row("reg/acme/bundle-x")];
        let t = build(&rows, &[0], &opts("reg"));
        let cache = ready_cache(
            "project",
            "reg/acme/bundle-x",
            vec![
                make_member("skill-a", "reg/acme/skill-a"),
                make_member("skill-b", "reg/acme/skill-b"),
            ],
        );
        let mut expanded_bundles = BTreeSet::new();
        expanded_bundles.insert("reg/acme/bundle-x".to_string()); // F3: full repo key // key ∈ expanded_bundles

        let flat = flatten_with_members(&t, &BTreeSet::new(), &expanded_bundles, &cache, "project", &rows);

        let member_count = flat.iter().filter(|r| matches!(r, DisplayRow::Member { .. })).count();
        assert_eq!(
            member_count, 2,
            "C-3: key ∈ expanded_bundles must splice 2 member rows; flat={flat:?}"
        );
    }

    #[test]
    fn c3_members_not_spliced_when_key_absent_from_expanded_bundles() {
        let rows = vec![bundle_row("reg/acme/bundle-x")];
        let t = build(&rows, &[0], &opts("reg"));
        let cache = ready_cache(
            "project",
            "reg/acme/bundle-x",
            vec![make_member("skill-a", "reg/acme/skill-a")],
        );
        let expanded_bundles = BTreeSet::new(); // key absent

        let flat = flatten_with_members(&t, &BTreeSet::new(), &expanded_bundles, &cache, "project", &rows);

        let member_count = flat.iter().filter(|r| matches!(r, DisplayRow::Member { .. })).count();
        assert_eq!(
            member_count, 0,
            "C-3: key absent from expanded_bundles must produce 0 member rows; flat={flat:?}"
        );
    }

    #[test]
    fn c3_member_depth_is_parent_leaf_depth_plus_one() {
        // Bundle leaf is inside a group → leaf depth = 1. Member depth must be 2.
        let rows = vec![bundle_row("reg/acme/bundle-x")];
        let t = build(&rows, &[0], &opts("reg"));
        let cache = ready_cache(
            "project",
            "reg/acme/bundle-x",
            vec![make_member("skill-a", "reg/acme/skill-a")],
        );
        let mut expanded_bundles = BTreeSet::new();
        expanded_bundles.insert("reg/acme/bundle-x".to_string()); // F3: full repo key

        let flat = flatten_with_members(&t, &BTreeSet::new(), &expanded_bundles, &cache, "project", &rows);

        // Find the leaf depth
        let leaf_depth = flat
            .iter()
            .find_map(|r| {
                if let DisplayRow::Leaf { depth, .. } = r {
                    Some(*depth)
                } else {
                    None
                }
            })
            .expect("C-3: must have a leaf");

        let member_depth = flat
            .iter()
            .find_map(|r| {
                if let DisplayRow::Member { depth, .. } = r {
                    Some(*depth)
                } else {
                    None
                }
            })
            .expect("C-3: must have a member after expanding");

        assert_eq!(
            member_depth,
            leaf_depth + 1,
            "C-3: member depth must be parent leaf depth + 1"
        );
    }

    #[test]
    fn c3_members_appear_immediately_after_bundle_leaf() {
        let rows = vec![bundle_row("reg/acme/bundle-x")];
        let t = build(&rows, &[0], &opts("reg"));
        let cache = ready_cache(
            "project",
            "reg/acme/bundle-x",
            vec![make_member("skill-a", "reg/acme/skill-a")],
        );
        let mut expanded_bundles = BTreeSet::new();
        expanded_bundles.insert("reg/acme/bundle-x".to_string()); // F3: full repo key

        let flat = flatten_with_members(&t, &BTreeSet::new(), &expanded_bundles, &cache, "project", &rows);

        // Find the leaf position, then check the immediately-following row is a Member.
        let leaf_pos = flat
            .iter()
            .position(|r| matches!(r, DisplayRow::Leaf { .. }))
            .expect("C-3: must have a leaf");
        assert!(
            matches!(flat.get(leaf_pos + 1), Some(DisplayRow::Member { .. })),
            "C-3: member must appear immediately after the bundle leaf; flat={flat:?}"
        );
    }

    // ── C-2b: collapsed / expanded_bundles orthogonality ─────────────────────

    #[test]
    fn c2b_collapsed_group_hides_bundle_leaf_regardless_of_expanded_bundles() {
        // Group `acme` collapsed AND bundle-x in expanded_bundles:
        // group collapse wins — neither the leaf nor members appear.
        let rows = vec![bundle_row("reg/acme/bundle-x")];
        let t = build(&rows, &[0], &opts("reg"));
        let cache = ready_cache(
            "project",
            "reg/acme/bundle-x",
            vec![make_member("skill-a", "reg/acme/skill-a")],
        );
        let mut collapsed = BTreeSet::new();
        collapsed.insert("acme".to_string()); // group collapsed
        let mut expanded_bundles = BTreeSet::new();
        expanded_bundles.insert("reg/acme/bundle-x".to_string()); // F3: full repo key // bundle would be expanded

        let flat = flatten_with_members(&t, &collapsed, &expanded_bundles, &cache, "project", &rows);

        // Only the group header is visible (1 row).
        assert_eq!(flat.len(), 1, "C-2b: collapsed group hides leaf+members; flat={flat:?}");
        assert!(
            matches!(flat[0], DisplayRow::Group { .. }),
            "C-2b: only the group header should be visible"
        );
    }

    #[test]
    fn c2b_expanded_group_with_expanded_bundle_shows_group_leaf_and_members() {
        // Group `acme` expanded AND bundle-x in expanded_bundles → group header + leaf + members.
        let rows = vec![bundle_row("reg/acme/bundle-x")];
        let t = build(&rows, &[0], &opts("reg"));
        let cache = ready_cache(
            "project",
            "reg/acme/bundle-x",
            vec![make_member("skill-a", "reg/acme/skill-a")],
        );
        let collapsed = BTreeSet::new(); // group expanded
        let mut expanded_bundles = BTreeSet::new();
        expanded_bundles.insert("reg/acme/bundle-x".to_string()); // F3: full repo key

        let flat = flatten_with_members(&t, &collapsed, &expanded_bundles, &cache, "project", &rows);

        let groups = flat.iter().filter(|r| matches!(r, DisplayRow::Group { .. })).count();
        let leaves = flat.iter().filter(|r| matches!(r, DisplayRow::Leaf { .. })).count();
        let members = flat.iter().filter(|r| matches!(r, DisplayRow::Member { .. })).count();

        assert_eq!(groups, 1, "C-2b: must have 1 group header");
        assert_eq!(leaves, 1, "C-2b: must have 1 bundle leaf");
        assert_eq!(members, 1, "C-2b: must have 1 member");
    }

    #[test]
    fn c2b_expanded_group_with_collapsed_bundle_shows_group_and_leaf_only() {
        // Group `acme` expanded AND bundle-x NOT in expanded_bundles → group header + leaf only.
        let rows = vec![bundle_row("reg/acme/bundle-x")];
        let t = build(&rows, &[0], &opts("reg"));
        let cache = ready_cache(
            "project",
            "reg/acme/bundle-x",
            vec![make_member("skill-a", "reg/acme/skill-a")],
        );
        let collapsed = BTreeSet::new(); // group expanded
        let expanded_bundles = BTreeSet::new(); // bundle collapsed (absent)

        let flat = flatten_with_members(&t, &collapsed, &expanded_bundles, &cache, "project", &rows);

        let members = flat.iter().filter(|r| matches!(r, DisplayRow::Member { .. })).count();
        assert_eq!(
            members, 0,
            "C-2b: collapsed bundle (absent from expanded_bundles) must show 0 members"
        );
        assert_eq!(
            flat.len(),
            2,
            "C-2b: group header + leaf only (no members); flat={flat:?}"
        );
    }

    // ── C-10: member_repo population ──────────────────────────────────────────

    #[test]
    fn c10_ready_member_carries_member_repo() {
        let rows = vec![bundle_row("reg/acme/bundle-x")];
        let t = build(&rows, &[0], &opts("reg"));
        let cache = ready_cache(
            "project",
            "reg/acme/bundle-x",
            vec![make_member("skill-a", "reg/acme/skill-a")],
        );
        let mut expanded_bundles = BTreeSet::new();
        expanded_bundles.insert("reg/acme/bundle-x".to_string()); // F3: full repo key

        let flat = flatten_with_members(&t, &BTreeSet::new(), &expanded_bundles, &cache, "project", &rows);

        let member = flat.iter().find_map(|r| {
            if let DisplayRow::Member { member_repo, .. } = r {
                Some(member_repo)
            } else {
                None
            }
        });
        assert!(member.is_some(), "C-10: must have a member row");
        assert_eq!(
            member.unwrap().as_deref(),
            Some("reg/acme/skill-a"),
            "C-10: Ready member must carry its member_repo"
        );
    }

    #[test]
    fn c10_loading_placeholder_has_none_member_repo() {
        let rows = vec![bundle_row("reg/acme/bundle-x")];
        let t = build(&rows, &[0], &opts("reg"));
        let mut cache: HashMap<BundleMemberKey, BundleMemberCache> = HashMap::new();
        cache.insert(
            ("project".to_string(), "reg/acme/bundle-x".to_string()),
            BundleMemberCache::Loading,
        );
        let mut expanded_bundles = BTreeSet::new();
        expanded_bundles.insert("reg/acme/bundle-x".to_string()); // F3: full repo key

        let flat = flatten_with_members(&t, &BTreeSet::new(), &expanded_bundles, &cache, "project", &rows);

        let member = flat.iter().find_map(|r| {
            if let DisplayRow::Member { member_repo, .. } = r {
                Some(member_repo)
            } else {
                None
            }
        });
        assert!(member.is_some(), "C-10: Loading must produce a placeholder member");
        assert!(
            member.unwrap().is_none(),
            "C-10: Loading placeholder must have member_repo = None"
        );
    }

    #[test]
    fn c10_failed_placeholder_has_none_member_repo() {
        let rows = vec![bundle_row("reg/acme/bundle-x")];
        let t = build(&rows, &[0], &opts("reg"));
        let mut cache: HashMap<BundleMemberKey, BundleMemberCache> = HashMap::new();
        cache.insert(
            ("project".to_string(), "reg/acme/bundle-x".to_string()),
            BundleMemberCache::Failed("network error".to_string()),
        );
        let mut expanded_bundles = BTreeSet::new();
        expanded_bundles.insert("reg/acme/bundle-x".to_string()); // F3: full repo key

        let flat = flatten_with_members(&t, &BTreeSet::new(), &expanded_bundles, &cache, "project", &rows);

        let member = flat.iter().find_map(|r| {
            if let DisplayRow::Member { member_repo, .. } = r {
                Some(member_repo)
            } else {
                None
            }
        });
        assert!(member.is_some(), "C-10: Failed must produce a placeholder member");
        assert!(
            member.unwrap().is_none(),
            "C-10: Failed placeholder must have member_repo = None"
        );
    }
}
