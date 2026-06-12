# ADR: `grim publish` — manifest-driven batch release

**Status:** Proposed
**Date:** 2026-06-12
**Deciders:** maintainer (command name, path convention, per-entry tables pre-decided), architect (remaining decisions)
**Research:** [research_publish_manifest.md](./research_publish_manifest.md)
**Supersedes:** `catalog/scripts/publish.py` (deleted on completion)

## Context

`catalog/publish.toml` + `catalog/scripts/publish.py` batch-drive `grim release`
for the first-party catalog: fixed kind ordering, semver validation, source-path
convention, `--only` / `--tag` / `--dry-run` / `--force`, default
`--skip-existing`. This is a generic capability dressed as a project script —
every multi-package author must reinvent the same loop. Promote it to a
built-in command.

**Pre-decided by maintainer:**

1. Subcommand name `grim publish`.
2. Source-path convention = current catalog layout, overridable per entry.
3. Per-entry tables: `[skills.foo]` with `version` + optional `path`.

**Open question resolved here:** how grim distinguishes a publish manifest
from a bundle TOML (both plain TOML; `detect_kind` currently maps any `.toml`
to `Bundle`).

## Decision

### D1 — Command shape

```
grim publish [--manifest <path>] [--only <name>]... [--tag <tag>]
             [--dry-run] [--force] [--registry <registry>]
```

- New `src/command/publish.rs` + `Publish(PublishArgs)` variant in `main.rs`,
  dispatch arm in `app.rs` — standard command pattern.
- `--manifest` defaults to `./publish.toml`.
- `--only NAME` repeatable filter; any name not in the manifest → DataError (65).
- `--tag` must be non-semver (movable channel tag, e.g. `canary`); semver
  rejected with DataError (65) — semver releases always come from the manifest
  so the repo records exactly what was published.
- The global `--registry` *flag* overrides the manifest `registry` value —
  keeps acceptance tests and staging publishes possible without manifest
  edits. The env/config default-registry tiers (`GRIM_DEFAULT_REGISTRY`,
  `[options].default_registry`) deliberately do **not** apply: the manifest
  value is explicit input, like a fully-qualified reference.

### D2 — Manifest schema (`publish.toml`)

```toml
registry = "grim.ocx.sh"          # required

[skills.grim-usage]
version = "0.1.1"                  # required, strict X.Y.Z

[rules.custom-rule]
version = "0.2.0"
path = "shared/custom-rule.md"     # optional — overrides convention

[agents.helper]
version = "0.1.0"

[bundles.grim-essentials]
version = "0.1.0"
pin = true                         # optional, bundle-only; default false
```

- Parsed with serde derive + `#[serde(deny_unknown_fields)]`, read via
  `config::read_capped` (64 KiB cap) — matches `BundleSource::from_toml_str`
  precedent in `src/config/project_config.rs`.
- Path convention relative to the manifest's directory:
  `skills/{name}/`, `rules/{name}.md`, `agents/{name}.md`, `bundles/{name}.toml`.
- `pin` on a non-bundle entry → DataError (65). Mirrors `grim release --pin`
  (bundle-only member-tag freeze); without it the manifest could not express a
  pinned bundle release and users would fall back to `grim release`.
- Source path must exist; version must be strict semver — both validated for
  the **whole manifest before any push** (publish.py parity, fail before
  side effects).

### D3 — Default = skip-existing; `--force` to move semver tags

Batch default is idempotent skip-existing: an already-published exact version
is skipped, only bumped versions push. `--force` moves existing exact-version
tags (conflicts with the default, so the two are mutually exclusive modes, not
combinable flags). `--dry-run` validates + packs + plans without pushing.

Industry context: helm chart-releaser's `--skip-existing` is the canonical CI
norm for re-runnable OCI publishing; crates.io-style hard error is a registry
guarantee, not a tooling default (research axis 3). cargo-release/melos use
dry-run-as-default instead — rejected below.

**Amendment (2026-06-12, from Codex cross-model review):** a `--tag`
channel run always moves the tag — `skip_existing` is disabled and the
exact-tag overwrite guard force-waived for that run. Without this,
skip-existing would freeze `canary` at its first digest forever, defeating
the "movable channel tag" contract. Manifest semver tags are unaffected
(a tag run never touches them). publish.py had the same latent freeze;
the built-in fixes it. Status mapping additionally reads the release
report data (pushed flag + tag set) instead of echoing CLI flags, so an
already-published entry under `--dry-run` honestly reports `skipped`.
`--force` conflicts with `--tag` at the clap level — a tag run already
implies a forced move, so combining them is rejected as a usage error.

### D4 — Ordering and failure semantics

- Publish order: skills → rules → agents → bundles, alphabetical within kind.
  Fixed kind order (not topological sort): the only intra-manifest dependency
  is bundle→member; YAGNI on a graph.
- **Fail-fast**: first failing entry stops the batch. The report still renders:
  completed entries with their status, the failed entry with status `failed`,
  remaining entries unreported. Exit code = `classify_error` result of the
  failure. Skip-existing default makes re-run-from-top safe; `--only` covers
  surgical recovery.

### D5 — Implementation seam: compose `release::run` per entry

`publish.rs` builds a `ReleaseArgs` per manifest entry (path, reference
`{registry}/{namespace}/{name}:{tag-or-version}`, kind, dry_run, force,
skip_existing = !force, pin) and calls the existing
`pub async fn release::run(ctx, &args)` directly, collecting each
`ReleaseReport` into a `PublishReport`.

No extraction of release's private helpers (`move_tags`,
`guard_existing_version`, `release_bundle`): `release::run` is already the
exact per-item semantic publish.py invokes as a subprocess; calling it
in-process is the minimal, behavior-identical seam.

### D6 — Report type

`src/api/publish_report.rs` (flat report-module layout — the `api/data/`
path originally written here described an architecture that was never
built): `PublishReport { entries: Vec<PublishEntry> }` following the
multi-item pattern (`InstallReport`/`UpdateReport`):

- `PublishEntry { reference, kind, digest, tags, status }`,
  `PublishStatus` enum (`pushed | skipped | dry-run | failed`) with
  `Display` + `Serialize`.
- Custom `Serialize` flattens to a bare JSON array; `print_plain` = single
  `print_table` with static headers (subsystem-cli-api single-table rule).

### D7 — Manifest vs bundle TOML disambiguation

**Filename convention + structural schema + guard-rail errors. No marker key.**

The two schemas are already structurally disjoint:

| | Bundle TOML | Publish manifest |
|---|---|---|
| Top level | `summary`/`description`/`keywords`/`repository` strings | required `registry` string |
| Kind tables | `[skills]` flat map name → OCI reference string | `[skills.name]` sub-tables with `version` |

`deny_unknown_fields` + value-shape mismatch (string vs table) means neither
parser accepts the other's file. Resolution is therefore UX, not correctness:

1. `grim publish` discovers `publish.toml` by filename (or `--manifest`).
2. Guard rail in the publish parser: if a kind table holds string values
   (bundle shape), error names the confusion — "looks like a bundle file; use
   `grim release --kind bundle`".
3. Guard rail in `read_bundle_members`: if the TOML has a top-level `registry`
   key (publish-manifest shape), error — "looks like a publish manifest; use
   `grim publish`". Covers `grim release publish.toml` mistakes.

Rejected: marker key (`manifest = "publish/v1"` / `[publish]` table) — ceremony
the disjoint schemas don't need; Chart.yaml-style `apiVersion` solves a shared-
filename problem grim doesn't have. Rejected: distinct filename
(`grim-publish.toml`) — breaks the existing catalog convention for no gain.

## Options Considered

### O1 — Default behavior on existing versions

| Option | Idempotent CI | Accident safety | Parity with publish.py | Verdict |
|---|---|---|---|---|
| **A: skip-existing default, `--force` opt-in** | yes — re-run from top safe | good — semver tags never move silently | exact | **Chosen** (also maintainer decision) |
| B: dry-run default, `--execute` opt-in (cargo-release/melos) | yes | best | no — breaks muscle memory vs `grim release` | rejected — inconsistent with `grim release`, where pushing is the default |
| C: hard-error default (crates.io/npm) | no — CI re-runs fail | good | no | rejected — registry-guarantee semantics, wrong layer |

### O2 — Reuse seam

| Option | Diff size | Coupling | Behavior drift risk | Verdict |
|---|---|---|---|---|
| **A: call `release::run` per entry with constructed `ReleaseArgs`** | small | publish → release module fn (same crate) | none — same code path as today's subprocess loop | **Chosen** (KISS/YAGNI; crate is provisional single binary) |
| B: extract shared `push_artifact()` from release internals | medium | release + publish → new shared fn | low, but refactor of working code | rejected for v1 — extract when a third caller appears |
| C: reimplement push loop in publish.rs | large | duplicated logic | high | rejected — DRY violation |

### O3 — Disambiguation (detail in D7)

Filename + structural schema + guard errors **(chosen)** vs marker key vs
distinct filename — see D7 rejections.

## Consequences

- `catalog/scripts/publish.py` deleted; `catalog/taskfile.yml` `release` task
  calls `grim publish` directly; `.github/workflows/publish-catalog.yml`
  updated; Python leaves the publish path.
- `catalog/publish.toml` migrates from flat `[skills] name = "ver"` to
  `[skills.name] version = "ver"` tables in the same change.
- Manifest schema becomes public API. Evolution is additive (new optional
  keys); `deny_unknown_fields` makes old binaries fail loudly on new
  manifests — acceptable while the product is provisional.
- Naming: the CLI now has `release` (single artifact, explicit reference) and
  `publish` (manifest batch). `product-context.md` "CLI at a Glance" and
  `subsystem-cli-commands.md` currently show `grim publish <path> <ref>` as
  the illustrative single-push — both must be updated in the same change
  (product-context Update Protocol trigger 5).
- Catalog drift duty (catalog/README.md): `grim-usage` skill + `docs/src/
  publishing.md` / `commands.md` need the new command documented.
- No new attack surface: same OCI push path, same auth; manifest input capped
  at 64 KiB and strictly validated. `GRIM_OFFLINE` blocks push exactly as
  `release` does (exit 81).

## NFRs

- **Operability:** JSON report (bare array) for CI consumption; per-entry
  status; deterministic ordering.
- **Latency:** sequential pushes (publish.py parity). Parallel push rejected
  for v1 — registry index writes for the catalog are serialized deliberately
  (see publish-catalog.yml concurrency comment).
- **Security:** input validation up front; no secrets in manifest (registry
  auth stays in docker config / `grim login`).
