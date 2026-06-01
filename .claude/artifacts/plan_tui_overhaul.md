# Plan: TUI Overhaul — multi-select, scope switch, status, polish

## Status

- **Plan:** plan_tui_overhaul
- **Active phase:** 7 — tree view + default-registry elision + group batch ops (impl complete)
- **Step:** awaiting /swarm-review
- **Last update:** 2026-05-16 (after Phase 7: tree view + group ops)

---

## Overview

**Status:** Approved
**Author:** Claude (builder)
**Date:** 2026-05-16
**Beads Issue:** N/A
**Related PRD:** N/A
**Related ADR:** N/A

## Objective

Make the catalog TUI a usable management surface: accurate per-artifact
state (installed / not / outdated / modified / integrity-missing / stale),
colorized + iconified rows, multi-select batch install/update/uninstall,
runtime Global⇄Project scope switching, and a clear refresh affordance.

## Scope

### In Scope

- Richer TUI status (mirror `command::status::derive_state` precedence)
- Plain-Unicode icons + ANSI color (no font dependency)
- Multi-select (`Space` mark) + batch install/update/uninstall
- New shared **uninstall seam** (files + install-state record + decl/lock)
  and a thin `grim uninstall` command so the seam is acceptance-testable
  (the TUI itself is excluded from pytest)
- Runtime scope toggle (Global ⇄ Project), badge/catalog reload
- Refresh UX clarification (key already exists)

### Out of Scope

- Network/registry changes; resolver changes
- Nerd-font glyphs (explicitly declined — plain Unicode only)
- New editor targets / transform changes
- Multi-registry browsing

## Technical Approach

### Architecture Changes

Preserve the existing pure/impure split:
`state` (pure model) → `event` (pure input→action) → `render.frame`
(pure projection) / `render.draw` (only ratatui) → `app` (only runtime).

```
state.rs   + marked:HashSet<usize>, active scope label, ArtifactState enum
event.rs   + Toggle/SelectAll/Clear/Delete/ScopeToggle inputs; Batch action
render.rs  + RenderRow carries StatusView{icon,color-key}+kind icon+mark col
           draw() maps StatusView → ratatui Style/Color (decision-free)
app.rs     + batch loop, uninstall call, scope rebuild
install/uninstall.rs  NEW shared seam (uninstall_all)
command/uninstall.rs  NEW thin command (acceptance surface)
command/tui.rs        precompute both scopes up front for runtime toggle
```

### Key Decisions

| Decision | Rationale |
|----------|-----------|
| Add a TUI `ArtifactState` mirroring `derive_state` precedence, not reuse 4-variant `StatusBadge` | User wants integrity-missing / dirty / stale granularity that `StatusBadge` collapses |
| Keep `frame()` pure: rows carry an icon+color-key, `draw()` maps to ratatui `Style` | Honors the documented headless-testability contract |
| New `uninstall_all` seam shared by TUI + new `grim uninstall` | TUI is pytest-excluded; the command gives an acceptance-testable surface for the same logic (no forked logic) |
| Full uninstall = editor files + state record + config/lock entry | User choice; "delete the skill" should leave nothing behind |
| Precompute Global+Project scopes in `command/tui.rs` | `scope_resolution::resolve` needs `&Context`, unavailable in the raw-mode loop; precompute both, toggle in-loop |
| Plain Unicode + ANSI color | User choice; no font dependency, renders in any terminal |

## Implementation Steps

### Phase 1: Richer status model + visual polish

- [ ] 1.1 `state.rs`: add pure `ArtifactState` (Installed, NotInstalled,
  Outdated, Modified, Missing, IntegrityMissing, Stale) + pure mapping
  helper; `TuiRow.badge` → `state`. Keep `StatusBadge` for non-TUI users.
- [ ] 1.2 `app.rs`: compute `ArtifactState` from `InstallState` records +
  `content_hash` (mirror `command::status` precedence), not just
  `derive_badge`.
- [ ] 1.3 `render.rs`: `RenderRow` gains `StatusView { glyph, color_key }`
  + kind glyph; `frame()` stays pure; `draw()` maps color_key→`Color`.
  Add a legend line.
- [ ] 1.4 Unit tests: state mapping precedence; frame projection snapshot.

### Phase 2: Multi-select + batch operations

- [ ] 2.1 `state.rs`: `marked: HashSet<usize>` (row indices) + toggle /
  select-visible / clear; selection-vs-mark precedence rule.
- [ ] 2.2 `event.rs`: `TuiInput::{Toggle, SelectAll, ClearMarks, Delete}`;
  `TuiAction::Batch { op: BatchOp, indices: Vec<usize> }`. Single-key acts
  on selection when no marks; on the mark set otherwise.
- [ ] 2.3 `app.rs`: batch loop reusing `run_artifact_action`; aggregated
  status line (`n ok, m failed`).
- [ ] 2.4 Unit tests: mark toggle/clamp, batch action fan-out, search-mode
  safety (marks unaffected by typing).

### Phase 3: Uninstall seam + delete

- [ ] 3.1 `install/uninstall.rs`: `uninstall_all(...)` removing recorded
  editor outputs + `InstallState` record; returns per-artifact outcomes.
- [ ] 3.2 `command/uninstall.rs` + clap/`command.rs` wiring: thin command
  (config/lock drop via `remove::drop_from_lock` + seam), `RemoveReport`-
  style output. Update `subsystem-cli-commands.md`.
- [ ] 3.3 `app.rs`: wire TUI `Delete`/batch-delete → seam; refresh states.
- [ ] 3.4 Tests: Rust unit for seam; `test/tests/test_uninstall.py`
  acceptance for the command (TUI logic covered by pure tests).

### Phase 4: Runtime scope switch

- [ ] 4.1 `command/tui.rs`: resolve Global + Project scopes up front
  (Project optional); pass both into `TuiContext`.
- [ ] 4.2 `state.rs`/`event.rs`: `ScopeToggle` input, active-scope in
  model + title.
- [ ] 4.3 `app.rs`: on toggle swap scope-dependent paths, recompute
  states, reload; graceful message when Project absent.
- [ ] 4.4 Unit tests: scope toggle model + title; absent-project guard.

### Phase 5: Review & Documentation

- [ ] 5.1 Spec-compliance + code-quality review (swarm-review low/auto)
- [ ] 5.2 Update `test/manual/README.md` TUI scenario; `subsystem-cli-commands.md`
- [ ] 5.3 `task verify`; finalize commits

## Files to Modify

| File | Action | Description |
|------|--------|-------------|
| `src/tui/state.rs` | Modify | ArtifactState, marks, scope label |
| `src/tui/event.rs` | Modify | new inputs/actions, batch |
| `src/tui/render.rs` | Modify | StatusView, color/icons, legend |
| `src/tui/app.rs` | Modify | batch loop, uninstall, scope rebuild |
| `src/install/uninstall.rs` | Create | shared uninstall seam |
| `src/command/uninstall.rs` | Create | thin command (acceptance surface) |
| `src/command.rs` / cli | Modify | wire `uninstall` subcommand |
| `src/command/tui.rs` | Modify | precompute both scopes |
| `test/tests/test_uninstall.py` | Create | acceptance for seam via command |
| `.claude/rules/subsystem-cli-commands.md` | Modify | document `uninstall` |

## Testing Strategy

TUI runtime (`app.rs`) is pytest-excluded by design; behavior is covered
by pure unit tests in `state`/`event`/`render` and by acceptance tests of
the **shared seam** through the new `grim uninstall` command.

### Unit Tests

| Component | Behavior | Edge Cases |
|-----------|----------|------------|
| state ArtifactState map | precedence == `derive_state` | stale beats modified beats outdated; integrity-missing vs not-installed |
| state marks | toggle/clear/select-visible | empty filter, search mode, clamp |
| event batch | single→selection, marks→set | no selection, no marks |
| uninstall seam | removes files+record+lock | absent record, partial outputs, locally-modified |

### Acceptance Tests

| User Action | Expected | Error Cases |
|-------------|----------|-------------|
| `grim uninstall <name>` | files gone, state record gone, decl/lock dropped | not installed → benign; modified → still removed (explicit) |

## Risks

| Risk | Mitigation |
|------|------------|
| TUI not acceptance-tested | Push logic into pure modules + shared seam tested via command |
| Scope toggle when no Project config | Detect at precompute; disable toggle + status hint |
| Color unreadable on some themes | Use named ANSI (not 256/rgb); keep glyph as primary signal |
| Uninstall data loss surprise | Single-artifact + explicit `Delete` key; batch only over explicit marks |

## Phase 7 — Tree view + default-registry elision + group batch ops

User feedback: flat-list names are long (full `registry/repository` every
row). Add a grouped tree.

- New pure module `src/tui/tree.rs`: trie `build` + `flatten` (no I/O).
  Registry root → group; `/` path components → groups; the final
  `/`-segment additionally splits on `.` so dotted names nest
  (`acme/code.review` → `acme` ▸ `code` ▸ leaf `review`); hyphens never
  split. The browsed registry (effective default) is elided from the root
  so leaf names stay short. `Rollup` aggregates descendant
  `ArtifactState` (worst-state precedence) for collapsed groups.
- `state.rs`: `ViewMode {Flat,Tree}`, `collapsed: BTreeSet<String>`,
  `default_registry`. Tree built lazily (no cache → rollups always
  reflect freshest per-row state). Tree-aware `selected_row_index` /
  `action_targets` / `toggle_mark_selected` (group = all descendants,
  smart toggle) / `move_selection`; `toggle_view_mode`,
  `expand/collapse/toggle_collapse_selected`. Entering search forces flat.
- `render.rs`: `frame` branches on `view_mode`; `render_leaf` shared by
  flat + tree; groups show `▾/▸ label` + `n/total installed` rollup;
  leaves show the bare name indented. Help overlay + hint tiers gain
  `t tree` and `→/← expand/collapse`.
- `event.rs`: `TuiInput::{ViewToggle,Expand,Collapse}`; `t` toggles view,
  `→/←` (mapped from `KeyCode::Right/Left` in `app.rs`) expand/collapse,
  `Enter` on a group folds it (leaf/flat → detail as before). `i/u/d`
  unchanged — `action_targets` resolves a selected group to its subtree.
- `app.rs`: browsed `ctx.registry` threaded as the effective default
  registry into `TuiState`.

**Manual scenario** (mirrors the existing manual rig): with
`GRIM_DEFAULT_REGISTRY` set to the browsed registry, run `grim tui`
against a registry with mixed repos including a dotted name. Press `t`:
the default-registry host has no root node; `acme/code.review` nests to
leaf `review`; `code-review` stays one leaf. `→/←/Enter` collapse/expand;
`space` on a group marks every descendant (`▣`); `i/u/d` on a group with
no marks acts on the whole subtree (status reports `n/total`). `/` forces
flat; `t` returns to tree; marks survive the toggle.

Verification: `task verify` green — 429 Rust unit tests (incl. new
`tui::tree`, tree-aware `tui::state` / `tui::event` / `tui::render`
tests) + 66 pytest acceptance tests.

## Notes

Refresh already exists (`r` → `TuiAction::Refresh`); Phase 1 only adds a
visible legend/affordance, no new mechanism.

---

## Progress Log

| Date | Update |
|------|--------|
| 2026-05-16 | Plan created; seams mapped; delete=full-uninstall, icons=plain-unicode decided |
| 2026-05-16 | Phase 1 landed (b7a3086): ArtifactState + color/icon polish |
| 2026-05-16 | Phase 2 done: multi-select marks + batch install/update; `.claude/rules` deletion incident (restored from git, cause unconfirmed — see dev/vi stale-path note) |
| 2026-05-16 | Phase 3 done: uninstall seam + `grim uninstall` command + acceptance tests + TUI delete (BatchOp) |
| 2026-05-16 | Phase 4 done: runtime Global⇄Project scope toggle (`g`), in-place scope swap + state recompute, scope in title |
| 2026-05-16 | Phase 5: manual-rig README TUI scenario documented. Impl complete (all 5 phases); formal /swarm-review left to the user (billed/user-triggered). `task verify` green throughout. |
| 2026-05-16 | Polish (user feedback): fixed-width `fit()` columns (no skew on long ids), full per-cell colorization + colored borders/legend/selection, `?` help overlay, scope shown in title; +6 unit tests (40 TUI total) |
| 2026-05-16 | Phase 7 (user feedback): tree view (`t`) — registry/`/`/dotted-leaf grouping, default-registry root elided, collapsible groups (`→/←`, `Enter`), whole-group mark/install/update/delete (`action_targets` resolves a group to its subtree). New pure `src/tui/tree.rs`. `task verify` green, 429 Rust + 66 pytest. |
| 2026-05-16 | Phase 6 (user feedback): UX polish + version picker. (a) header underline padded to full Status column; (b) grayed `type / to search` placeholder; (c) scope moved to a colored `PROJECT/GLOBAL MODE` box right of search; (d) persistent key hint right-aligned on legend line so `? help` survives transient status; (e) explicit highest-semver version in Tag column (`pick_highest_version` + `CatalogEntry.version`, serde-default backward-compatible); (f) modal version picker (`v`), lazy `list_tags` on open, pin sets install target. `task rust:verify` green, 412 tests. |
