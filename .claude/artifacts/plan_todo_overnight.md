# Plan — TODO.md Overnight Sweep (goat)

Date: 2026-06-11. Orchestrated multi-agent run: address all TODO.md items,
self + codex review, release-ready.

## Locked product decisions

- **TUI layout**: flat list, grouped/sorted by kind then name (case-insensitive
  name). Tree machinery (ViewMode, tree.rs, expand/collapse, `t` key) removed.
- **Kind icons**: removed from rows; name + kind column suffice. Table status
  icons already match legend (`✓ ↑ ✱ ✘ ·`) — stale TODO premise; only kind
  glyphs `◆▸•` and tree arrows were out-of-legend.
- **Search semantics** (CLI + TUI aligned via shared matcher
  `crate::catalog::SearchQuery`): whitespace-split terms, AND, each term
  substring-matches name/kind/summary/description/keywords; bare kind keywords
  (skill/skills/rule/rules/bundle/bundles) filter by kind; build prefilter uses
  the sole text term only when exactly one.
- **Default clients**: all *detected* clients when none configured.
  Project: vendor dir present (`.claude`/`.opencode`); Copilot needs tighter
  marker (`.github/copilot-instructions.md` or `.github/instructions/`).
  Global: vendor dir/config incl. env overrides; OpenCode config file counts.
  Explicit config > detection; `--client` flag > all. Surfaced TUI-only
  (status line).
- **Registry precedence**: CLI flag > `GRIM_DEFAULT_REGISTRY` > project config
  > global config. Centralized helper.
- **Smart cache**: confirmed full on-the-fly async checks in TUI — bounded
  concurrency (Semaphore=8, mpsc 256, 300 ms search coalesce), JoinSet +
  abort_all, installed/locked rows only, Installed→Outdated flips only,
  respects `GRIM_OFFLINE`. Reuses `Catalog::load_or_refresh(force=true)`.
- **`grim tui --refresh`**: mirrors search's flag.

## Workstreams

| WS | Scope | Surface | Mode |
|----|-------|---------|------|
| A | Upgrade acceptance tests (add/remove-on-upgrade, force-gated modified deletion). src already correct — test-only. | `test/tests/` | wave 1, worktree |
| B | bootstrap.sh multi-version publish matrix + starter-pack v2 (adds AND removes members) | `test/manual/` | wave 1, worktree |
| C | Shared search matcher + CLI alignment + `tui --refresh` plumbing | `src/catalog/`, `src/command/` | wave 1, worktree |
| E | Clients detection + registry precedence + TUI clients plumbing | `src/install/`, `src/config/`, `src/command/` | wave 1, worktree |
| D | TUI flat list, tree removal, kind-glyph removal, clients status line | `src/tui/` | wave 2, sequential |
| F | Background async update checks (`src/tui/update_check.rs`) | `src/tui/`, `src/command/tui.rs` | wave 2, sequential, last |

Patch apply order: C → E → A → B (3-way; C/E co-touch search.rs, tui.rs,
app.rs in different hunks). Then D, then F directly on goat.

## Pipeline

1. Plan workflow (6 Opus planners + conflict synthesizer) — done.
2. Wave 1 implement (4 worktree agents → patches) — running.
3. Apply patches sequentially, subsystem verify + commit each.
4. Wave 2: D then F, direct, verify + commit each.
5. Review loop (bounded 2 rounds): swarm-style self review + codex
   cross-model review → fix → re-verify.
6. `task --force verify`, `task release:changelog:preview`, TODO.md cleanup,
   final commit. No push.

Spec JSONs (planner output): `/tmp/grim-specs/ws-{A..F}.json`, `synthesis.json`.
