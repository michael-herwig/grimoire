# Code Review: install-state-portability (tier=max)

## Summary

- **Verdict: Request Changes** — 2 unresolved Block-tier findings (both actionable, both cross-model corroborated).
- **Tier:** max · **Baseline:** `main` · **Target:** `HEAD` (branch `feat/install-state-portability`, WIP 577caee)
- **Diff:** 28 files, +4950 / −350, subsystems: install (file-structure), command (cli seam), tui, tests, docs, rules
- **Overlays:** breadth=adversarial, reviewer=opus, doc-reviewer=sonnet, rca=on, codex=on
- **Cross-model (Codex):** ran (exit 0) — independently corroborated B1 (split into 2 sites) + B2; surfaced installer write-ordering + dup-key items (triaged Deferred)
- **Context:** swarm-execute already ran 3 Claude review rounds + a Codex gate on this same diff. This is an independent fresh-eyes pass on the converged state. Both Blocks slipped the prior passes because **they only manifest off-Linux / on a UI path the prior reviews didn't cross-check**.

### Block-tier (must fix before merge)

| ID | Location | Finding |
|----|----------|---------|
| **B1** | `src/tui/app.rs:922, 1039` | TUI install/update + uninstall call `reap_legacy_project_state` **unconditionally**; the 3 CLI seams gate it on `!legacy_migration_lossy()`. A lossy V1→V2 migration via the TUI deletes the legacy file — the only surviving record of dropped outputs — turning recoverable loss into permanent loss. `legacy_migration_lossy` is referenced **nowhere** in `src/tui/`. Cross-model corroborated (Codex Block ×2). |
| **B2** | `src/install/path_anchor.rs:366` + `src/install/install_state.rs:649` | Store-time canonicalize broadened from `#[cfg(windows)]` (design, stated 4×) to `#[cfg(any(windows, target_os="macos"))]`. `dunce::canonicalize` requires the path to **exist**. `convert_v1_records` calls `from_target` on legacy absolute targets that may no longer exist → on macOS `Err(NotFound)` → `UnknownAnchor` → output dropped + `lossy=true` + legacy file never reaped → **silent data loss on a supported platform**. Linux unaffected (lexical strip). Also violates the documented `from_target` lexical caller-invariant on macOS fresh installs (canonicalize resolves symlinks). Likely **RED on macOS CI** (migration unit tests seed JSON without creating target files). |

### Warn-tier actionable

| ID | Location | Finding | Fix |
|----|----------|---------|-----|
| W1 | `path_anchor.rs:441`, `error.rs:187,413` | `AnchorError::MigrationFailure` is dead (defined/classified/tested, never constructed — converter is infallible by design). YAGNI. | Remove variant + classify arm + test, or document `// reserved`. |
| W2 | `path_anchor.rs:265` | Layer-2 containment skipped for **dangling** symlinks (`candidate.exists()` is false → returns `Ok(root.join(symlink))`). No actual escape (OS op fails on dangling target) but the documented "Layer 2 catches symlink escape" invariant doesn't hold. | `if candidate.exists() \|\| candidate.is_symlink()`. |
| W3 | `install_state.rs` (`records_from_bytes`/`VersionProbe`) | A future V3 file read by a V2 binary surfaces as opaque `InvalidData` I/O error, not a user-facing "written by a newer grim; upgrade". | On probe failure, compare raw version int > `V2 as u8`, emit upgrade message. |
| W4 | `CLAUDE.md:91-92`, `CHANGELOG.md:31` | (a) Vendor-override rows omit **agents** (`CLAUDE_CONFIG_DIR`/`COPILOT_HOME`/`OPENCODE_CONFIG_DIR` now also affect agent install paths); `OPENCODE_CONFIG` says "no effect on skill paths" → should be "skill/agent paths". (b) CHANGELOG reap-trigger list omits the TUI path (couples to B1 — once B1 fixed, mention TUI). | Update text per subsystem-file-structure.md (already correct). |
| W5 | `test_state_portability.py` + unit | Coverage gaps: (a) **case-insensitive FS / macOS** store-time match — would have caught B2; (b) non-UTF8 path component → `UnknownAnchor`; (c) `.copilot`-twice denorm absence assertion (defect class 4, the headline secondary motivation); (d) store-time empty-remainder (`abs == root`) → `UnknownAnchor`. | Add the 4 targeted tests; (a) is `#[cfg(target_os="macos")]`, runs on deep CI. |

### Deferred (human judgment / follow-up — not merge-blocking)

- **Installer write-before-containment** (`installer.rs:243-304`, Codex Block downgraded): `materialize` writes before `from_target` runs; a symlinked own config dir (`.claude`, `.github/instructions`) escapes the anchor at write time (CWE-59). **Pre-existing** (install always wrote to `path_for(dest)` on `main`), **same-uid** (documented out-of-scope threat model), out of strict diff scope. Hardening: canonicalize parent before write or `O_NOFOLLOW`-style creation. *reason: pre-existing + same-uid threat model the design explicitly accepts; revisit if threat model changes.*
- **Dup-key last-writer-wins on load** (`install_state.rs:418`, Codex Warn): duplicate `(kind,name)` in a hostile/corrupt local state file silently collapses via `BTreeMap` fold. Same trust boundary as the file itself. *reason: machine-local file grim writes; consider warn-on-duplicate in follow-up.*
- **PERF-01..05** + `prune.rs:167` per-record `String` clone — Two-Hats optimization pass; unmeasurable at current N (tens of records, human-invoked CLI). *reason: behavior-preserving structural change, don't mix into feature diff.*
- **`prune.rs:77`** `#[non_exhaustive]` unknown-variant default = "reap" (delete) on a destructive op; fail-safe would bias "propagate". *reason: maintainer call on destructive-default policy.*
- **Reap-gate triplication** → extract `persist_project_state(scope, state, roots)` (3 CLI + TUI). *Fixing this is the systemic fix for B1 — see RCA cluster 1.*
- **Legacy-sha canonicalize duplicated** (`install_state.rs:507, 572`) across load + reap; compute once in scope.
- **TOCTOU intermediate-symlink hardening** (Codex-1 from execute) + **cap-std `openat2(RESOLVE_BENEATH)`** upgrade path (Linux 5.6+; cap-std ≥3.4.1 CVE-clean). *reason: handle-based refactor; same-uid threat model.*
- **Global `global.json` last-writer-wins under shared GRIM_HOME** (Q3) — no `serial`/generation counter (Terraform pattern). *reason: tracked v1 residual; per-host segmentation is the follow-up.*
- **`ClientOutput.client: String`** stringly-typed (pre-existing rename, not new). **Pre-write `.backup`** (Terraform pattern; uv/pixi don't either). **E2E V1→V2 project-migration acceptance** test (unit coverage strong).

## Root-Cause Analysis (Five Whys, clustered)

### Cluster 1 — B1 (TUI reap) + W4b (changelog) + reap-triplication

- **Why** does the TUI destroy the lossy-migration breadcrumb? It reaps unconditionally.
- **Why** unconditionally? The `!legacy_migration_lossy()` guard (the "Codex-2 data-safety fix" added during execute) was applied only to the 3 CLI command seams.
- **Why** only the CLI seams? The persist→reap sequence is **duplicated** across 4 sites (3 commands + TUI), not centralized; the fix touched 3, missed the 4th.
- **Why** duplicated? No single storage-side persist helper existed; each call site open-codes `ensure_dir → save → conditional reap`.
- **Why** did 3 review rounds + execute Codex miss it? Those reviews focused on the CLI command flow; the TUI's parallel copy wasn't cross-checked against the same contract.
- **Systemic fix:** extract `InstallState::persist_project_state(scope, state, roots)` doing ensure-dir → save → lossy-gated reap; call from all 4 sites. Kills B1, the changelog gap, and the architect's DRY finding in one move. **Related findings:** B1, W4b, Deferred reap-triplication.

### Cluster 2 — B2 (macOS canonicalize) + W5a (case-insensitive test gap)

- **Why** does macOS migration drop deleted-file records? `from_target` canonicalizes, which fails on absent paths.
- **Why** canonicalize on macOS? To handle APFS case-insensitivity in the prefix match.
- **Why** an existence-requiring mechanism on the migration path (whose inputs are possibly-absent)? The design under-specified case-insensitivity (named only Windows); the implementer broadened the cfg without amending the design or choosing an existence-independent mechanism.
- **Why** did it survive every prior gate? All prior gates ran on **Linux**, where the lexical `strip_prefix` arm is compiled — the macOS arm is literally invisible on Linux, and no macOS-gated test exercises it (migration tests seed JSON without creating files on disk).
- **Systemic fix:** replace existence-requiring `canonicalize` with an existence-independent, case-insensitive **lexical** component compare (preserves the design's "lexical at store time" invariant and the original-case stored remainder); add a `#[cfg(target_os="macos")]` test with an absent target; amend plan §1.5 / ADR / subsystem-file-structure.md to record the macOS decision. **Related findings:** B2, W5a, spec-compliance's "Windows-only invariant broadened" deviation.

## What's solid (preserve)

- Two-layer guard lives in the storage type (`AnchoredPath::resolve`), not consumers — correct layer; returns canonicalized path to close the read-time TOCTOU window.
- Security-class prune split (`is_security_class`: TraversalAttempt/EscapedAnchor → exit 65, never reap; AnchorRootAbsent → reap) — falsifiably tested at the real flow.
- `arch2` coherence test: hermetic 18-triple round-trip with an exhaustiveness counter — locks anchor-table drift (OCP guard).
- `serde_repr` version enum + two-struct wire converter + bare-`load` V1 rejection — SOTA-correct, forward-compatible to V3.
- EscapedAnchor Display redacts the resolved path (SEC-06 / CWE-209); exit-code mapping correct and boundary-tested.
- SOTA affirmation: `.grimoire/` dir + self-managed `*` gitignore matches pixi/uv exactly; sha256(host-path) key was the antipattern this correctly removes.

## Handoff

`/swarm-execute` review-fix loop on the actionable set (2 Block + 5 Warn). Recommended order: **Cluster-1 systemic fix** (extract `persist_project_state` → fixes B1) → **Cluster-2 fix** (existence-independent case-insensitive lexical compare → fixes B2) → W1/W2/W3 → W4/W5 docs+tests. Re-run `task verify` + (if available) macOS deep CI after Cluster-2.
