# Bugfix Plan: TUI bundle install fails with tar materialize error

## Status

- Phase: Implementation
- Owner: Claude (session 2026-06-11)
- Branch: main (maintainer authorized main commits this session)

## Reproduce

1. Local registry serves bundle `localhost:5050/grimoire/bundles/starter-pack:latest`.
2. `grim tui`, select the bundle row, press install.
3. Status: `installed 0/1, 1 failed — localhost:5050/grimoire/bundles/starter-pack: failed to materialize artifact: cannot read tar entry: failed to read entire block`.
4. Side effect: `grimoire.toml` gains `starter-pack = ".../bundles/starter-pack:latest"` under `[skills]` (corrupt declaration).

## Root Cause

`src/tui/app.rs::perform` (and `perform_uninstall`) map the catalog row kind with

```rust
let kind = match row.kind.as_str() {
    "rule" => ArtifactKind::Rule,
    _ => ArtifactKind::Skill,   // "bundle" falls here
};
```

A bundle row is declared as a **skill** in `[skills]`, relocked as a direct
skill entry (pinning the bundle manifest), and `install_all` then feeds the
bundle's members-layer (JSON, `application/vnd.grimoire.bundle.v1+json`) to
the tar materializer → `failed to read entire block`.

The assumption `unreachable!("the TUI never operates on bundles")` is false:
the catalog includes bundle rows and the install/update/delete keys operate
on them. The CLI path is correct (`command/add.rs:122-143` declares
`[bundles]` + full-resolves so members expand) but that bundle dispatch was
never extracted as a shared seam, so the TUI forked around it.

Proximate cause: tar reader on a non-tar blob. Root cause: TUI kind
dispatch silently coerces bundle → skill instead of reusing the
kind-dispatching declare/relock logic `grim add` has.

## Fix

Extract the bundle-aware logic into shared seams and consume them in the TUI:

1. `command/add.rs`: `declare(set, kind, name, id)` (3-way insert) and
   `relock_declared(...)` (bundle → full `resolve_lock`, else
   `relock_entry`). `add::run` and TUI `perform` both consume them.
2. `command/remove.rs::drop_from_lock` → `pub(crate)`;
   `command/uninstall.rs::undeclare_and_unlock` handles
   `ArtifactKind::Bundle` (capture declared `(repo, tag)` before removal,
   evict provenance-matched lock members via `drop_from_lock`).
3. `tui/app.rs`:
   - row kind mapping includes `"bundle"`;
   - `perform` projects bundle members out of the resolved lock by
     provenance `(bundle repo, tag)` and hands them to `install_all`;
   - `perform_uninstall` expands a bundle row into its member
     `(kind, name)` targets (from lock provenance) before the shared
     uninstall + undeclare seams;
   - badge derivation aggregates member states for bundle rows
     (worst-of: IntegrityMissing > Modified > NotInstalled > Outdated >
     Installed; no members ⇒ NotInstalled).
4. `build_row_check` keeps returning `None` for bundle rows (the lock
   records no bundle digest, so a floating-tag baseline does not exist) —
   recorded as a TODO follow-up.

## Regression Tests (written before the fix)

- `tui/app.rs`: `perform` on a bundle row against `MemoryRegistry`
  (bundle members-layer + member skill tar). Asserts `[bundles]`
  declaration, provenance-stamped member lock entry, and materialized
  member files. Fails pre-fix (declares `[skills]`, materialize error).
- `command/uninstall.rs`: `undeclare_and_unlock` with a bundle entry —
  pre-fix this is an `unreachable!` panic.
- Pure-function tests for the member projection and bundle badge
  aggregation.

## Verify

`task rust:verify` per loop; `task verify` final gate. Manual repro check
against the local registry if available.
