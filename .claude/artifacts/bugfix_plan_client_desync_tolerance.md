# Bug Fix Plan: Client-set desync poisons install/uninstall + systematic defensive-tolerance hardening

## Status

- **Plan:** bugfix_client_desync_tolerance
- **Active phase:** 6 — Review-Fix Loop (complete)
- **Step:** finalized
- **Last update:** 2026-06-18 (/finalize: rebased 8→6 commits on main 74c221a, fast-forward-ready; `task verify` green — 957 unit + 274 acceptance)
- **Deferred (follow-ups, not blocking):** docs Suggest ×4 (docs/src/commands.md #status/#install; catalog consume.md/troubleshooting.md drift); `active: &[ClientTarget]` wide signature ripple → context-struct refactor (separate `refactor:`); extra acceptance coverage (global-scope desync, bidirectional client swap); direct `active_outputs` unit test. (TUI sync_config sites and the unparsable-client edge are now FIXED, not deferred.)

---

## Context

A user installed a **bundle**, then changed which **clients** (claude / copilot /
opencode) were enabled, then re-installed. The bundle reported **not-installed**
even though every artifact it references was installed for the *active* client(s);
in a related path the install **hard-failed**. Root issue: Grimoire's install state
records `InstallRecord.outputs: Vec<ClientOutput>` for **all** clients targeted *at
install time*, but every consumer iterates those outputs **without reconciling
against the currently-active client set**.

## Defensive-Tolerance Principle

> If the requested action (install / uninstall / remove / sync / status) **can still
> be fulfilled** despite a missing file, absent dir, absent config entry, or malformed
> vendor config, **complete it** (treat the missing piece as a no-op). Only fail when
> the goal genuinely cannot be achieved (e.g. would corrupt unknown-schema data, or a
> *currently-active* client's required write fails).

Distinction: **out-of-scope / not-present client → skip silently**; **active client,
genuine failure → still surface**.

## Causes Inventory (9 causes)

| # | Severity | Cause | Site |
|---|----------|-------|------|
| C1 | Block | Integrity gate `?`-hard-fails on unresolvable recorded output | `installer.rs` |
| C2 | Block | `AlreadyInstalled` ignores target client set → silent incomplete add | `installer.rs` |
| C3 | Block | Record write overwrites outputs → drops other clients' outputs | `installer.rs` |
| C4 | Block | Read-side derivations count removed-client outputs as broken | `status.rs`, `tui/app.rs`, `status_badge.rs` |
| C5 | Block | Uninstall `?`-hard-fails on unresolvable recorded output | `uninstall.rs` |
| C6 | Warn | Unparseable vendor config hard-fails on removal | `opencode_config.rs` |
| C7 | Warn | Non-array `instructions` hard-fails on removal | `opencode_config.rs` |
| C8 | Warn | `sync_config` `?`-fails command after primary action persisted | `install.rs`, `uninstall.rs` |
| C9 | Suggest | TUI `load_scope_declaration` `?` on missing config during delete | `tui/app.rs` |

## Decisions (defaulted)

- **D1**: read paths use `detect_clients` to learn the active client set; an output
  whose client is not active is skipped. A present client with a missing file still flags.
- **D2**: install record reconciliation = **merge** (preserve other clients' outputs);
  a non-target prior output whose anchor root is absent (out-of-scope client) is dropped.
- **D3**: orphaned files of a removed client are left in place (no active deletion).
- **D4**: malformed vendor config on add → do not clobber (strict); on removal → no-op.

## Tolerance semantics for anchor errors

- `AnchorRootAbsent` (client root not present on this machine) → tolerate (skip).
- `TraversalAttempt` / `EscapedAnchor` (security) → always surface.
- `Io` (read/canonicalize failure) → surface for active clients.

## Shared Fix Architecture

1. Active-client filter helper in `install_state.rs` beside `ClientOutput`.
2. Read paths (`status`, `tui`, `derive_badge`) compute active set via `detect_clients`.
3. Tolerant resolve in installer gate + uninstall: skip `AnchorRootAbsent`.
4. `AlreadyInstalled` only when recorded clients cover all target clients.
5. Merge-on-write: re-attach recorded non-target outputs (drop unresolvable ones).
6. `opencode_config`: `want=false` (removal) tolerant of absent/unparseable/wrong-type.
7. C8 sync warn-only at call sites; C9 tolerant config load in TUI cleanup.

## Verification

- [ ] All new regression tests FAIL on current HEAD.
- [ ] `task rust:verify` + pytest acceptance suite pass.
- [ ] `task verify` green; `cargo fmt` clean.
- [ ] Diff limited to the planned files; tolerance via reused patterns.

## Execution Order

1. Write all regression tests (red). 2. C1-C3 installer. 3. C4 read paths + shared helper.
4. C5 uninstall. 5. C6-C7 opencode_config. 6. C8 sync warn-only. 7. C9 TUI config load.
8. `task verify` + manual. 9. Review-Fix loop. 10. Commit by cause-group.

## Review Round 1 (/swarm-review high+codex, 2026-06-18) — Request Changes

### BLOCK-1 — partial-client version bump strands other clients (Codex cross-model)

Merge-on-write (C3) re-attaches a prior client's `ClientOutput` verbatim while the
record's single `pinned` is overwritten to the new install's pin. Outputs carry
`content_hash` but **no per-output pin**, so a preserved output silently diverges in
version from `record.pinned`. Consequences for `[claude,copilot]@A` then
`install @B --client claude`:

1. **status lies**: copilot file matches its own recorded `hashA`; `record.pinned==lock==B`
   → reports `Installed` though copilot is at A.
2. **short-circuit strands**: later `install @B --client copilot` → `covers_targets` true
   (output exists) + `all_intact` + `pin B==B` → `AlreadyInstalled`; copilot never reaches B.

Pre-fix this was self-healing (output was clobbered → `covers_targets` false → re-materialize).
The C3 merge preserved files correctly but didn't extend the version model. This is cause C2
reborn (silent incomplete install + false status).

**DECISION: option (b)** — version is an artifact-level property; all clients move together.
On any install where `artifact.pinned != rec.pinned`, re-materialize **all currently-active
recorded clients** to the new pin (not just the `--client` target), keeping the invariant
"every output in a record is at `record.pinned`" true. A subset `--client` install at a NEW
version therefore also rewrites the other active clients' files. (Out-of-scope clients whose
anchor root is absent stay dropped, as today.)

Regression test (must fail on current HEAD):
`[claude,copilot]@A` → `install @B --client claude` → record `pinned=B` with **both** outputs
at `hashB` and both files rewritten to B; then `install @B --client copilot` → `AlreadyInstalled`
(now legitimately, because b already bumped it). Plus: `status` never reports a stale client as
`Installed`.

### Actionable Warn cluster
- **W1** TUI `sync_config` hard-fails after persisted action (tui/app.rs ~955, ~1080) → warn-only for C8 parity (closes the long-standing deferral).
- **W2** `c.to_string()` allocs in comparison loops (installer.rs:168, 353) → add `ClientTarget::as_str() -> &'static str`, compare `out.client == c.as_str()`.
- **W3** `tracing::warn!` lacks structured fields (install.rs, uninstall.rs, update.rs) → `client = %client, error = %e`.
- **W4** merge skip condition ambiguous (installer.rs:354) → split `in_target` and `root.is_none()` into two commented `continue` branches.
- **W5** no CHANGELOG `### Fixed` entry → add bullet.
- **W6** acceptance tests `check=False` lose stderr (test_client_desync.py:122, 173) → `check=True` / assert returncode.
- **W7** no all-clients-removed → `Missing` test (status.rs) → add `all_clients_removed_yields_missing`.

### Deferred (human judgment, not this loop)
- Docs Suggest x4 (docs/src/commands.md #status/#install; catalog consume.md/troubleshooting.md).
- `active: &[ClientTarget]` wide ripple → context-struct refactor (separate `refactor:`).
- Global-scope + bidirectional-swap acceptance coverage; direct `active_outputs` unit test.

### Cleared (no action): security ✅, D2/D4 ✅, ConfigSync removal ✅, tolerance boundary ✅.
