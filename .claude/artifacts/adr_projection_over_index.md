# ADR: The TUI tree is a pure projection over `rows`/`filtered`/`marked`

**Status:** Proposed (authored alongside `plan_tui_tree_view_phase2`)
**Date:** 2026-06-20
**Decision drivers:** TUI Tree View Phase 2 (bundle membership) needs to show
virtual member nodes that do not correspond to real catalog artifacts.

> **Stub note:** this is a short, intentionally narrow ADR. It records the
> single invariant Phase 2 must not regress. Flesh out the Consequences
> section if later phases (per-member install) revisit the boundary.

## Context

`TuiState` has three index spaces that form its data contract:

- `rows: Vec<TuiRow>` — the real catalog artifacts (the only source of truth).
- `filtered: Vec<usize>` — indices into `rows` matching the active query.
- `marked: BTreeSet<usize>` — indices into `rows` the user has marked for a
  batch action.

`selected` indexes the active *view* (the `filtered` list in flat mode, the
`flattened()` display list in tree mode) — never `rows` directly.
`action_targets()` and `selected_row_index()` return `Vec<usize>` / `usize`
into `rows`. `set_rows()` resorts and clears marks.

The tree (`tree::build` → `tree::flatten`) is **pure and synchronous**: it
projects `rows[filtered]` into a `Vec<DisplayRow>` and does no I/O. Group
collapse state persists only as `collapsed: BTreeSet<String>`, keyed by a
path-derived group key — never by index.

Phase 2 introduces **virtual member nodes**: a bundle's members, fetched lazily
or read from the lock snapshot, shown as children of the bundle leaf. They do
not exist as catalog artifacts and have no `rows` index.

## Decision

**The tree stays a pure projection over `rows`/`filtered`/`marked`. Virtual
members live entirely OUTSIDE that index space.**

Concretely:

1. Virtual members are stored in an ephemeral, scope-keyed cache
   (`bundle_members: HashMap<(scope_label, bundle_repo), BundleMemberCache>`)
   on `TuiState`, separate from `rows`/`filtered`/`marked`.
2. The new `DisplayRow::Member` variant carries **no `row: usize`** — it is a
   display-only node, not a pointer into `rows`.
3. Members are spliced at flatten time (`flatten_with_members`), never injected
   into `rows` or `build`. `build` remains pure over `rows[filtered]`.
4. A `Member` selection yields `selected_row_index() → None`,
   `action_targets() → []`, and a mark no-op — so no batch operation can ever
   act on a phantom artifact.

## Rationale

- **`set_rows` would corrupt them.** A member placed in `rows` would be
  resorted and have its marks cleared on every catalog reload, and its index
  would shift unpredictably.
- **Batch ops would act on phantoms.** `marked`/`action_targets` are `rows`
  indices consumed by install/update/delete. A virtual member index there
  would target a non-existent artifact (or, with a sentinel like `usize::MAX`,
  an out-of-bounds read).
- **Closed-enum exhaustiveness is a feature.** Adding `DisplayRow::Member`
  (no `#[non_exhaustive]`, per `arch-principles.md`) turns every consumer that
  must handle members into a compile error — the boundary is enforced by the
  type system, not by convention.
- **Offline-first + lazy fetch demand a separate cache.** Members come from the
  lock snapshot (offline) or a background fetch; that lifecycle (Loading /
  Ready / Failed / Offline, scope-keyed, generation-stamped) is orthogonal to
  the catalog row lifecycle and belongs in its own structure.

## Alternatives Considered

- **Reuse `DisplayRow::Leaf` with a sentinel `row` (e.g. `usize::MAX`).**
  Rejected: fragile; risks an out-of-bounds index into `rows`; defeats the
  compile-time exhaustiveness guarantee.
- **Inject members into `rows`/`filtered`.** Rejected: breaks the `set_rows`
  resort/mark-clear contract and lets batch ops target phantoms.

## Consequences

- Phase 2 adds `DisplayRow::Member`, a `bundle_members` cache, and
  `flatten_with_members`; the index contract is unchanged.
- Future per-member install (Phase 3) must introduce a *separate* targeting
  path for members — it must NOT promote them into `marked`/`action_targets`
  without revisiting this ADR.
- Any code reading `selected` must continue to branch on the view mode and
  handle `DisplayRow::Member` as a non-`rows` selection.

## References

- `arch-principles.md` — "Internal enum exhaustiveness" (closed enums, no
  `#[non_exhaustive]`).
- `.claude/state/plans/plan_tui_tree_view_phase2.md` — the implementing plan.
- `.claude/artifacts/phase2_understand_map.md` — verified ground truth for the
  index-space contract (Section 4, "Virtual Node Model").
