# ADR: Catalog short-summary annotation + width-aware search display

## Metadata

**Status:** Accepted
**Date:** 2026-06-10
**Deciders:** Michael Herwig (maintainer)
**Tech Strategy Alignment:**
- [x] Decision follows Golden Path in `.claude/rules/product-tech-strategy.md`
      (OCI distribution substrate unchanged; reuses the existing annotation
      mechanism and `crossterm`, already a dependency — no new crate)
**Domain Tags:** api, integration
**Supersedes:** N/A

## Context

`grim search` renders a five-column plain table
(`Kind | Repo | Description | Latest Tag | Status`). The `Description`
column printed the full `org.opencontainers.image.description` — for skills
a 1–1024 char field — which wrapped narrow terminals and made the catalog
hard to read.

Search must keep matching the description text: both the CLI
(`CatalogEntry::matches`) and the TUI (`TuiState::recompute_filter`)
already substring-match it. So the problem is **display**, not matching:
the value needs to be short on screen while staying fully searchable and
fully present in machine output (`--format json`).

A skill carries only one human description in frontmatter; there was no
short blurb to show instead.

## Decision Drivers

- Make the catalog readable without losing any search capability.
- Keep the full description available for machine consumers and for piped
  output (scripts, files).
- Don't bloat the OCI wire contract or the frontmatter struct.
- KISS / YAGNI — reuse existing patterns (`keywords`), existing deps
  (`crossterm`), existing helpers (`fit`-style truncation).

## Considered Options

### Option 1 — Short-summary annotation + width-aware truncation — CHOSEN

Add an optional `com.grimoire.summary` annotation, sourced at publish from
`metadata["summary"]` (skills) / `extra["summary"]` (rules) — the same
forward-compatible map convention `keywords` already uses. The catalog
reads it into `CatalogEntry.summary`. The CLI shows `summary ?? description`
in the blurb column, truncated to a terminal-width-clamped budget; piped /
non-TTY output stays full and untruncated. Search matches
`summary | description | keywords | repo`.

| Pros | Cons |
|------|------|
| Curated short blurb where authors provide one; graceful fallback otherwise | A second metadata key to document |
| No frontmatter struct change, no wire-contract change (additive annotation) | Display logic now consults terminal width (TTY-only) |
| Reuses the `keywords` map pattern, `crossterm::terminal::size`, and the TUI `fit` truncation idiom | |
| Full description preserved in JSON and in piped plain output | |

### Option 2 — Truncate the description only (no new annotation)

Show the existing description, truncated to a width budget.

| Pros | Cons |
|------|------|
| Smallest change — display layer only | Truncating a long sentence mid-thought reads worse than a curated blurb |
| | Authors cannot control what the catalog shows |

### Option 3 — Reuse `org.opencontainers.image.title`

Show the existing title (skill name / rule stem) instead of a description.

| Pros | Cons |
|------|------|
| No new key | Title is the identifier, already implied by the Repo column — redundant, not informative |

## Decision Outcome

**Chosen:** Option 1. An optional `com.grimoire.summary` annotation, with
the displayed blurb preferring summary over description and truncated to a
terminal-width-clamped window on a TTY; full text in JSON and when piped.
Search matches the summary in addition to the existing fields.

### Wire / read model

- **Publish** (`src/oci/annotations.rs`): `annotations_for_skill` emits
  `com.grimoire.summary` from `metadata["summary"]`; `annotations_for_rule`
  from `extra["summary"]` (scalar string). Emitted only when present, so
  re-release stays idempotent (deterministic, no volatile fields).
  Bundles carry the same metadata at the top level of the bundle `.toml`
  (`summary` / `keywords` / `description`), parsed by a dedicated
  `BundleSource` (so the consumer `ProjectConfig` stays strict); a bundle
  `description` overrides the default `grimoire bundle of N members`.
- **Keywords are a string everywhere.** The OCI annotation value is a
  string, so `com.grimoire.keywords` is comma-separated. To keep authoring
  uniform, keywords are string-only in every format — the rule
  frontmatter's earlier YAML-list support was dropped. Skill metadata stays
  under the spec `metadata` map; rule + bundle keys are top-level. This
  skill-vs-rule/bundle key-location asymmetry is intentional (the skill
  `metadata` map is dictated by the upstream SKILL.md spec).
- **Catalog** (`src/catalog/registry_catalog.rs`): `CatalogEntry` gains
  `summary: Option<String>` (`#[serde(default, skip_serializing_if =
  "Option::is_none")]`). `CatalogVersion` stays `V1` — additive optional
  field, old caches load with `summary = None` and refresh online.
  `matches` adds a summary branch.

### Display model

- **CLI** (`src/api/search_report.rs`, `src/cli/printer.rs`): the blurb
  cell is `summary.or(description)`. On a TTY the column budget is
  `clamp(term_width − fixed_columns − gaps, 24, 60)` and the cell is
  truncated with `truncate_ellipsis` (char-boundary, `…`). When stdout is
  not a terminal, `terminal_width()` returns `None` → no truncation, so
  piped output is full and byte-deterministic (git/ls convention).
  `print_table` stays pure (truncation happens before it).
- **TUI** (`src/tui/*`): catalog rows already omit the description
  (columns are Repo/Kind/Tag/Status), so no row verbosity change.
  `TuiRow` gains `summary`; `recompute_filter` matches it; the detail pane
  shows a `summary:` line above the description when present.
- **JSON**: `SearchEntry` serializes `summary` plus the full,
  untruncated `description`.

### Consequences

**Positive:**
- Readable catalog; authors can curate the one-line blurb.
- No wire-contract or frontmatter-struct change; existing artifacts and
  registries are unaffected.
- Search and machine output lose nothing.

**Negative / Risks:**
- Display now depends on terminal width (TTY only). Mitigated by the
  non-TTY full-text path and clamped [24, 60] window, and covered by unit
  tests that exercise the piped (no-truncation) path.

## Validation

- Rust unit tests: skill/rule summary → `com.grimoire.summary` (and
  omitted when absent); rule keywords string-only (a YAML list is ignored);
  `annotations_for_bundle` emits summary/keywords and overrides the default
  description; `BundleSource` parses top-level metadata, rejects a keywords
  array and an unknown key; catalog `matches` hits the summary;
  `build_entry` reads the annotation; `truncate_ellipsis` (char boundary,
  multibyte, ellipsis budget); `blurb_budget` clamps; search report prefers
  summary, falls back to description, emits full text when not a TTY; JSON
  carries summary + full description; TUI filter matches summary.
- Acceptance (`test/tests/test_metadata.py`): a real `grim release` of a
  skill, rule, and bundle lands the authored `summary`/`keywords`/
  `description` in the manifest annotations (every metadata type × every
  kind); a rule keyword *list* is ignored (string-only); a bundle without
  metadata keeps the default `grimoire bundle of N members`.
- Acceptance (`test/tests/test_search.py`): an artifact published with a
  `com.grimoire.summary` annotation surfaces `summary` in JSON alongside
  the full, untruncated `description`; an artifact without one serializes
  `summary: null`.

## Links

- Related ADR: [`adr_oci_artifact_type.md`](./adr_oci_artifact_type.md)
  (kind via OCI `artifactType` — untouched here)
- [OCI image-spec — annotations](https://github.com/opencontainers/image-spec/blob/main/annotations.md)

---

## Changelog

| Date | Author | Change |
|------|--------|--------|
| 2026-06-10 | Michael Herwig | Initial draft, accepted |
