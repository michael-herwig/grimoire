# Plan: Package Deprecation (issue #15)

## Status

- **State**: in-progress
- **Branch**: `feat/package-deprecation`
- **Goal**: single-commit feature — publishers mark a package deprecated;
  grim warns on acquisition and highlights it in search + TUI.
- **Last updated**: 2026-06-29

### Step checklist

- [x] S1 annotation seam (`oci/annotations.rs`): const + emit (4 kinds) + read helper
- [x] S2 bundle metadata (`config/project_config.rs`): `deprecated` field + TOML parse
- [x] S3 catalog read (`catalog/registry_catalog.rs`): `CatalogEntry.deprecated`
- [x] S4 catalog service (`catalog/catalog_service.rs`): `CatalogRow.deprecated`
- [x] S5 search (`api/search_report.rs` + `command/search.rs`): JSON field + plain marker
- [x] S6 add warning (`command/add.rs`): warn on deprecated acquisition
- [x] S7 TUI (`tui/state.rs`,`app.rs`,`detail.rs`,`render.rs`): row marker (flat+tree) + detail entry
- [x] S8 docs + catalog drift review (publishing.md, commands.md, 4 grim-authoring specs)
- [x] S9 acceptance tests (pytest `test_deprecation.py`, 9 cases) — full suite 306 pass
- [x] S10 review-fix loop + gates + single commit

### Review-fix loop (adversarial pass)

One actionable finding fixed: the explicit-`--kind` `add` warn path lacked a
test → added `test_add_explicit_kind_warns_on_deprecated_reference`.

Deferred (documented, not blocking):
- **Offline downgrade forward-compat**: an old binary reading a *new* cache
  that has a populated `deprecated` field fails under `deny_unknown_fields`
  in offline mode. Matches the established additive-field precedent
  (`summary`/`keywords`/`repository_url` added the same way, no
  `CatalogVersion` bump); online mode silently rebuilds. A V2 bump is a
  separate policy decision.
- **`⚠` East-Asian-width ambiguity**: U+26A0 is width-ambiguous; `fit()`
  measures `chars().count()`, so a CJK locale overflows the Repo cell by 1.
  Pre-existing across every status glyph (`✓ ↑ ✱ ✘ ◆ ▾ …`); a real fix
  means switching `fit()` to `unicode-width` crate-wide.

### Verification evidence

- `task rust:verify`: 1274 unit tests pass (fmt + clippy + license + build)
- `task catalog:verify`: all first-party packages build (schema gate)
- `task claude:tests`: 70 pass (incl. plan Status-block validation)
- acceptance: `test/` full suite 306 pass; MCP `grim_search` carries the
  field for free (reuses `SearchReport`)
- bonus surface: TUI tree-view leaf marker (parity with flat view)

## Problem

Issue #15: "Support package deprecation, issuing warnings and being
highlighted in tui/search." No existing yank/deprecate concept.

## Design Decisions

1. **Wire format**: optional OCI annotation `com.grimoire.deprecated`.
   Value = the deprecation **message** (a string, npm-style). Presence of a
   non-empty value = deprecated; absent/empty = not deprecated. Mirrors the
   existing `com.grimoire.summary` / `repository` pattern exactly — no
   manifest version bump, idempotent re-release preserved (no wall-clock).

2. **Authoring** (optional, all kinds):
   - skill / agent: `metadata.deprecated = "<message>"`
   - rule: top-level frontmatter `deprecated: "<message>"` (via `extra`)
   - bundle: top-level TOML `deprecated = "<message>"` (`BundleMetadata`)
   No new validation: a free-text message (unlike the HTTPS `repository`).

3. **Single read seam**: `annotations::deprecation_message(&annotations)`
   (trims, empty→None). Reused by the catalog build and `grim add`.

4. **Warn site = `grim add`** (the explicit acquisition moment). `add`
   already fetches the manifest for kind inference — reading deprecation
   there is free. `infer_kind` is refactored to also return the manifest so
   the kind-omitted path keeps a single fetch; the `--kind`-given path adds
   one best-effort fetch (acceptable: `add` resolves exactly one ref).

5. **Cut-line (v1)**: NO warning at `install` / `update` / `lock`. The
   resolver (`resolve_one`) only calls `resolve_digest`; it never fetches
   the manifest, so warning there would add an N+1 manifest fetch per
   artifact on every lock (a real perf regression). Discovery surfaces
   (search/TUI) highlight deprecation; acquisition (`add`) warns. A
   lock-field-backed install/update warning is a documented follow-up.

6. **Highlight is orthogonal to install status**: `StatusBadge` /
   `ArtifactState` stay unchanged (they mean install relationship).
   Deprecation rides as a separate `Option<String>` field on
   `CatalogEntry` / `CatalogRow` / `SearchEntry` / `TuiRow`.
   - search plain: a comma-suffixed `deprecated` on the `Status` cell (e.g.
     `installed,deprecated`); JSON gains a `deprecated` field (message|null).
   - TUI: a yellow `⚠ deprecated` appended after the install-status label in
     the `Status` column (flat + tree, explained in the legend) + a
     `Deprecated:` detail-pane `MetaEntry`. `CATALOG_WIDTH` reserves the marker
     width (`W_DEPRECATED`) so it never clips.
   - **Viz revision** (post-review feedback): started as a `⚠ `/`[deprecated]`
     *prefix* on the repo/blurb cell (shifted the cell, read as noise), then a
     dedicated trailing indicator column (clipped by the Catalog box border in
     the live TUI); settled on folding the signal into the `Status` column —
     comma-separated text in search, space-separated yellow `⚠` in the TUI.
     `CATALOG_WIDTH` reserves the extra width so the marker never clips.

## TDD order

Each step: write failing unit test(s) first, then implement, then green.
Acceptance tests (S9) after the Rust surface is green. Full `task verify`
+ review-fix loop before the single commit (S10).

## Out of scope (v1)

- install/update/lock warnings (needs lock field or extra fetch)
- `is:deprecated` search filter
- deprecation replacement-ref / reason split annotations
