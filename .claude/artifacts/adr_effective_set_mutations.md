# ADR: Effective-Set Mutations + Lock-Cached Bundle Manifests

## Metadata

**Status:** Accepted
**Date:** 2026-06-11
**Deciders:** Maintainer (mherwig) + Claude
**Beads Issue:** N/A
**Related PRD:** N/A
**Tech Strategy Alignment:**
- [x] Decision follows Golden Path in `.claude/rules/product-tech-strategy.md`
**Domain Tags:** data | api
**Supersedes:** N/A (extends `adr_agent_artifact_kind.md` lock semantics and the multi-provenance fix in `bugfix_plan_shared_bundle_members.md`)

## Context

Declaration mutations (`grim remove`, `grim uninstall`, TUI delete) edit the
config and then perform *surgical* lock edits: retain-by-name for a
skill/rule/agent, retain-by-provenance for a bundle. Two bugs proved this
per-operation surgery structurally incapable of being correct:

1. **Shared bundle members** (fixed via multi-provenance): two bundles
   agreeing on a member coalesce to one lock entry; evicting by a single
   recorded provenance deleted a member the sibling bundle still held.
2. **Direct + bundle interplay** (this ADR): a directly-declared artifact
   that a declared bundle *also* names. Removing the bundle correctly keeps
   the direct entry — but removing the **direct declaration** evicts the
   lock entry entirely and restamps the declaration hash, so the lock counts
   as fresh while silently missing a member the remaining bundle still
   implies. `grim uninstall` additionally deletes the files. Only a manual
   `grim lock` (network) heals the state.

Root cause of (2): when a direct declaration wins, the bundle's contribution
is suppressed at merge time — never resolved, never recorded. Bundle member
lists exist only transiently during `grim lock` expansion. An offline
`remove` cannot even *detect* that a bundle still names the removed
artifact.

## Decision Drivers

- **Offline-first** (product principle): `remove`/`uninstall` must stay
  network-free.
- **Correctness by construction**: each mutation re-deriving "what should
  the lock contain" beats four hand-rolled retain loops drifting apart.
- **Existing TODO**: TUI bundle rows cannot re-check floating-tag
  "outdated" because the lock records member pins but no bundle digest.
- Early-stage project: lock format changes are cheap now, expensive later.

## Considered Options

### Option 1: Effective-set mutations + lock-cached bundle manifests (chosen)

**Description:** The lock gains an optional `[[bundle]]` section caching
each declared bundle's expansion result (binding name, repo, tag, resolved
digest, member list). A pure function computes the *effective desired set*
`E(declaration, cached_bundles)` applying the existing conflict policy
(direct wins; agreeing bundles coalesce; disagreeing fail). Every mutation
computes `E_before` / `E_after` and acts on the difference: drop
`E_before \ E_after`, keep the intersection (re-deriving provenance), add
`E_after \ E_before`.

| Pros | Cons |
|------|------|
| Kills the bug class structurally (one seam, not four retain loops) | Lock format extension (old grim rejects bundle-bearing locks written by new grim) |
| Offline-correct: membership knowledge cached locally | Lock grows by one section per declared bundle |
| Fixes TUI outdated-bundle TODO free (bundle digest now recorded) | Cached member lists go stale between locks (same staleness model as member pins — acceptable) |
| Honest staleness for the id-mismatch case (detectable now) | |

### Option 2: Network re-resolution on remove

**Description:** `remove`/`uninstall` re-expand declared bundles to compute
the correct post-removal state.

| Pros | Cons |
|------|------|
| No lock format change | Breaks the offline contract for a config edit |
| Always-fresh member lists | Registry outage blocks `remove` |

### Option 3: Conservative staleness stopgap

**Description:** Removing a direct entry while *any* bundle is declared
skips the hash restamp, forcing `grim lock` before the next install.

| Pros | Cons |
|------|------|
| Tiny diff, honest | Degrades every remove in bundle-using projects to "re-lock required" |
| | Does not fix TUI file deletion or the outdated-bundle TODO |

## Decision Outcome

**Chosen Option:** Option 1.

**Rationale:** Only option that is simultaneously offline-correct,
structural (future mutations inherit correctness), and pays down the
existing bundle-digest TODO. The lock is already the cache of resolution
results; caching the bundle expansion it was derived from completes the
model. Maintainer explicitly opted for the architectural fix over a
stopgap.

### Consequences

**Positive:**
- `remove`/`uninstall`/TUI delete share one effective-set seam.
- Removing a direct declaration keeps the artifact when a cached bundle
  still names it at the **same identifier** (provenance flips to the
  bundle) — fully offline.
- The **id-mismatch** case (bundle names the artifact at a different
  identifier than the removed direct pin) is now *detectable*: the entry is
  dropped, the hash restamp is skipped, and the user is told to run
  `grim lock` — honest staleness instead of silent omission.
- Bundle digest in the lock enables the TUI outdated re-check later.

**Negative:**
- Old grim rejects a lock carrying `[[bundle]]` (deny_unknown_fields) —
  same accepted trade-off as the `[[agent]]` array.
- A lock written before this change has no cache: mutations fall back to
  the previous behavior (drop + restamp) until the next `grim lock`
  refreshes it.

**Risks:**
- Conflict-policy drift between the resolver merge and the offline
  effective-set function → both route through shared, unit-tested policy
  semantics; acceptance tests pin the observable behavior.

## Technical Details

### Data Model

```toml
[[bundle]]
name = "starter-pack"                                  # config binding name
repo = "localhost:5050/grimoire/bundles/starter-pack"  # registry/repo
tag = "1"                                              # declared tag
pinned = "localhost:5050/grimoire/bundles/starter-pack@sha256:…"

[[bundle.member]]
kind = "skill"
name = "code-reviewer"
id = "localhost:5050/grimoire/skills/code-reviewer:1"
```

Absent section ⇒ pre-cache lock (fallback path). `lock_io::content_equal`
covers the section; serialization stays byte-stable.

### API Contract

```rust
/// (kind, name) -> how the effective declaration provides it.
enum Origin {
    Direct(Identifier),
    Bundles { id: Identifier, contributors: Vec<BundleProvenance> },
    Conflicted, // disagreeing bundles masked by nothing — keep, let `grim lock` fail closed
}
fn effective_set(set: &DesiredSet, bundles: &[LockedBundle])
    -> BTreeMap<(ArtifactKind, String), Origin>;
```

Mutation rule (remove/uninstall/TUI delete): compute `E_before`/`E_after`;
drop the difference; for kept keys whose origin changed Direct→Bundles,
keep the lock entry iff the bundle identifier equals the removed direct
identifier (digest already proven), else drop + skip hash restamp + warn.

## Implementation Plan

1. [x] ADR (this file) + index row in `arch-principles.md`
2. [x] Lock schema: `LockedBundle`, `[[bundle]]` wire, round-trip tests
3. [x] Resolver records expansion results into the lock (full + partial)
4. [x] `effective_set` module + unit tests (`src/lock/effective_set.rs`)
5. [x] Rework `drop_from_lock`/`undeclare_and_unlock`/TUI targets onto the seam
   (legacy surgical path kept as the pre-cache fallback)
6. [x] Acceptance regressions (direct+bundle interplay; failing-first)
7. [x] Docs: lock reference, remove/uninstall semantics

## Validation

- [x] Failing-first acceptance tests pass post-implementation
- [x] Full `task verify` green
- [x] Manual rig publishes the shared-member demo bundle (`review-pack`)

## Links

- [adr_agent_artifact_kind.md](./adr_agent_artifact_kind.md)
- [bugfix_plan_shared_bundle_members.md](./bugfix_plan_shared_bundle_members.md)

---

## Bug-fix decision: standalone↔bundle id-mismatch reconcile

**Status:** Accepted (amends the id-mismatch consequence above)
**Date:** 2026-06-20
**Deciders:** Maintainer (mherwig) + Claude

### 1. Root cause (precise)

`drop_from_lock` at `src/command/remove.rs:163-178`: when a removed **direct**
declaration unmasks a surviving `Origin::Bundles { id, contributors }`
(`entry.bundles.is_empty()` is true), the code re-points the lock entry only
when `direct_id_of(set_before, …) == Some(id)` — i.e. the removed direct
**identifier** equals the bundle member's **identifier**, tag included
(`remove.rs:167-168`). The id comparison uses `Identifier`'s full equality,
so `…/grim-authoring:latest` (the removed direct pin) ≠
`…/grim-authoring:0` (the bundle member id from
`catalog/bundles/grim-essentials.toml:17`). The branch falls to the `else`
at `remove.rs:171-177`: `stale = true`, a "run `grim lock`" note is pushed,
the entry is dropped, and the hash restamp is skipped (`remove.rs:213-215`).
The next `grim install` then fails the lock-freshness guard until the user
runs `grim lock` (RCA Risk 4). The same path is reached from
`uninstall.rs:171` (`undeclare_and_unlock`) and the TUI
`perform_member_uninstall → undeclare_and_unlock` seam (`src/tui/app.rs:1628,
306`). This is a **real bug**: the code goes stale in a case where the lock
already holds a concrete surviving binding, but the comparison is over the
wrong thing (the floating *tag*, not the *content*).

### 2. Is offline reconciliation possible? **NO — not safely from the snapshot.**

The reconcile candidate (A) would re-point the surviving entry to "the bundle
snapshot's binding (digest from snapshot)". **The snapshot does not carry a
per-member digest.** Proof from the structs:

- `LockedBundle.members: Vec<BundleMember>` (`src/lock/locked_bundle.rs:35-36`).
- `BundleMember.id: String` is the member ref **verbatim from the bundle
  manifest layer** — documented as "floating `registry/repo:tag` **or** pinned
  `registry/repo@sha256:…`" (`src/oci/bundle.rs:44-47`).
- The resolver copies the layer members into the snapshot unmodified:
  `snapshots.push(LockedBundle { … members: members.clone() })`
  (`src/resolve/resolver.rs:334-340`). Members are **not** pinned to a digest
  during expansion — `expand_bundles` only parses each `member.id` into an
  `Identifier` for the *transient* work list (`resolver.rs:359`), never writes
  a resolved digest back into the snapshot.
- For the real repro the bundle is published **without `--pin`** with members
  at floating `:0` (`catalog/bundles/grim-essentials.toml:5-17`), so
  `BundleMember.id == "grim.ocx.sh/skills/grim-authoring:0"` — a tag, **no
  digest**.

The only digest in the snapshot is `LockedBundle.pinned`
(`locked_bundle.rs:32-33`) — the **bundle manifest** digest, not the member's
content digest. `effective_set` likewise surfaces only the parsed member
`Identifier` in `Origin::Bundles { id, … }`
(`src/lock/effective_set.rs:73, 91-96`), which for this case is a tag-only id
with no digest reachable in `drop_from_lock`.

The surviving lock entry's own `pinned` digest (`LockedArtifact.pinned`) is the
digest the **removed `:latest` direct pin** resolved to — which may or may not
equal what `:0` currently resolves to. Keeping that digest while flipping
provenance to the bundle at tag `:0` would assert "the bundle provides this
content" without proof — exactly the **wrong-digest pin** the ADR's invariant
("digest already proven", line 161) exists to prevent. Re-pointing offline is
therefore **infeasible without a network round-trip**, which `remove`/
`uninstall` must not do (Offline-first, Decision Drivers).

(Had the bundle author pinned members with `--pin`, `BundleMember.id` would
carry `@sha256:…` and a *future* enhancement could reconcile by digest. v1
does not special-case this; see Option B note below.)

### 3. Decision (binding): **Option C — keep honest staleness; fix the message and surface it in the TUI.**

Rejected **A** (reconcile offline): no member digest exists in the snapshot
(§2); flipping provenance while retaining the `:latest` digest would pin an
unproven digest, violating the safety invariant the ADR was written to uphold.
Honest staleness is the *correct* behavior whenever no concrete proven binding
survives — and here none does.

Rejected **B** (reconcile when a concrete binding survives, else stay stale):
in this case no *proven* binding survives. B would only differ from C if
`BundleMember.id` carried a digest (`--pin`ed bundle). That is a real but
**separate, additive** enhancement (digest-pinned-member fast path) and is
explicitly out of scope for this fix — adding it now would be YAGNI against the
reported bug, whose bundle is unpinned. Recorded as a follow-up below.

Chosen **C**: the current drop+skip-restamp+note behavior is *semantically
correct* — the bug is that the outcome is (a) worded as if it were an error and
(b) invisible in the TUI (the note goes only to `tracing::warn`). The fix is
UX, not lock-semantics:

- **Message**: reword the note at `remove.rs:173-176` from an imperative
  warning into an explanatory status. Current:
  `"… still provided by a declared bundle at a different reference; run
  `grim lock` to re-resolve it"`. Replacement (lowercase, no trailing period
  per `quality-rust-errors.md` C-GOOD-ERR — this string is also rendered by the
  CLI but originates library-side):
  `"{kind} '{name}' is now provided by bundle '{bundle}' at {tag}; the lock is
  marked stale — run `grim lock` to pin the bundle's version"`. The point: the
  artifact is **not lost**, it is intentionally awaiting a re-resolve. Use the
  surviving `contributors` to name the bundle (`repo`/`tag`) rather than a bare
  "a declared bundle".

This keeps the invariant intact (never pins an unproven digest), stays fully
offline, and removes the "functional dead-end" *perception* — the next
`grim lock` is a one-command, expected heal, now clearly explained at the exact
moment the user triggers it.

### 4. TUI feedback (required for all options)

`perform_member_uninstall` (`src/tui/app.rs:1628`) currently returns
`anyhow::Result<()>`; the dispatch arm (`app.rs:306-315`) maps `Ok(())` to the
fixed string `"uninstalled"` and then sets the status line to
`"{repo}: uninstalled"`. The stale note never reaches the user — it is emitted
only via `tracing::warn` inside `drop_from_lock`'s callers, and the Bug-2 fix
will redirect that stream to a file.

**Seam change**: thread the `UndeclareOutcome.notes` (already produced at
`remove.rs:151-216` / surfaced through `undeclare_and_unlock`) up to the TUI so
the status line can reflect the real outcome.

- Change `perform_member_uninstall` to return
  `anyhow::Result<Vec<String>>` (the notes), or a small
  `MemberUninstallOutcome { stale: bool, notes: Vec<String> }`.
- This requires `undeclare_and_unlock` (`src/command/uninstall.rs:150-178`) to
  **return the notes** instead of swallowing them into `tracing::warn`
  (`uninstall.rs:172-174`). It currently returns `bool` (declared); widen to
  `(bool, Vec<String>)` or a named struct. The CLI `uninstall::run`
  (`uninstall.rs:114`) keeps logging them via `tracing::warn` (CLI boundary is
  unchanged); only the return value is added.
- Dispatch arm (`app.rs:306-315`): when notes are non-empty, set the status to
  the first note (e.g. `"{repo}: lock marked stale — run grim lock"`) instead
  of the bland `"uninstalled"`; otherwise keep `"uninstalled"`. The bundle row
  also already flips to the `stale` badge via `recompute_states`
  (`app.rs:318`) because the lock hash was not restamped — consistent with the
  acceptance test's `states.get("stack") == "stale"` expectation.

### 5. Implementation spec

**`src/command/remove.rs`**

- *Message only* (`remove.rs:171-177`): replace the `notes.push(format!(…))`
  string per §3. Pull the bundle name/tag from `contributors` (its first
  `BundleProvenance` gives `repo` + `tag`) so the note names the bundle. No
  change to `stale = true` or to the `false` return (entry still dropped). No
  change to the restamp-skip at `remove.rs:213-215`. The `Origin::Conflicted`
  note (`remove.rs:188-192`) is left as-is.
- No comparison/logic change to `direct_id_of` or the id-equality test at
  `remove.rs:168` — it remains the correct gate for the *same-id* flip
  (`remove_direct_flips_to_bundle_provenance_when_ids_agree`).

**`src/command/uninstall.rs`**

- `undeclare_and_unlock` (`:150-178`): change return type to carry the notes
  (e.g. `anyhow::Result<UndeclareResult { declared: bool, notes: Vec<String> }>`
  or a tuple). Keep the `tracing::warn` loop (`:172-174`) for the CLI path; add
  the notes to the return value. Update the CLI caller `uninstall::run`
  (`:114-122`) to ignore the new field (status logic unchanged — `declared`
  still drives `UninstallStatus`).

**`src/tui/app.rs`**

- `perform_member_uninstall` (`:1628-1652`): return the notes from
  `undeclare_and_unlock` (it already routes through `perform_uninstall` →
  `undeclare_and_unlock`; thread the value back).
- Dispatch arm (`:306-315`): surface the first note as the status when present.

**`src/lock/effective_set.rs`**: **no change.** The reconcile path is *not*
taken; the snapshot lacks the digest that would make it safe.

**Tests**

- *Unit* — `remove.rs` test
  `remove_direct_with_id_mismatch_drops_and_skips_restamp` (`remove.rs:439-465`)
  stays valid (still drops, still skips restamp). Update its `notes.contains`
  assertion to match the reworded message (assert it names the bundle and still
  contains `"grim lock"`). Add an assertion that the note mentions the bundle
  binding/tag so the new wording is locked in.
- *Acceptance* — `test/tests/test_bundles.py:428`
  `test_remove_direct_with_bundle_id_mismatch_goes_stale` is the nearest. It
  uses synthetic `:direct` vs `:bundled` tags and already asserts the desired
  end-state (note mentions `lock`, `stack` row is `stale`, `grim lock` heals).
  **Keep it** (its semantics are unchanged under Option C) but it does not match
  the real-world `:latest` vs `:0` repro. **Add a NEW test**
  `test_remove_standalone_skill_held_by_bundle_at_floating_tag_marks_stale_not_lost`
  that reproduces the report exactly:
  1. publish a skill `grim-authoring` at floating tag `latest` **and** `0`
     (two cascade tags / two pushes);
  2. `write_config` declaring it standalone at `:latest` **and** declaring a
     bundle whose member pins `grim-authoring` at `:0`;
  3. `grim lock`; assert `status` shows `code-review`/`grim-authoring`
     `source == "direct"`;
  4. `grim remove skill grim-authoring`;
  5. assert stderr contains the reworded explanatory note (names the bundle,
     says "stale", mentions `grim lock`) and that it does **not** read as a
     hard error;
  6. assert `status` shows the bundle row `state == "stale"` and the artifact
     is **still present** (not silently omitted) awaiting re-resolve;
  7. assert `grim lock` heals: the artifact's `source` flips to `bundle:…` and
     its `pinned` digest matches the `:0` content.
  This proves the user-visible outcome is "explained, recoverable staleness",
  not a dead-end.

### Follow-up (out of scope)

When a bundle is published with `--pin`, `BundleMember.id` carries
`@sha256:…`. A future additive fast path could, in the id-mismatch branch,
reconcile offline **iff** the surviving `Origin::Bundles { id }` is
digest-pinned: re-point the lock entry to that proven digest, flip provenance,
DO restamp, no stale. This is Option B narrowed to the digest-proven case and
never pins an unproven digest. Tracked separately; not required by the reported
bug (its bundle is unpinned).

---

## Bug-fix decision: uninstall must not delete bundle-held files (file-retention gate)

**Status:** Accepted (completes the file side of this ADR)
**Date:** 2026-06-21
**Deciders:** Maintainer (mherwig) + Claude

### 1. Root cause (precise)

The effective-set rework (Option 1) made the **lock** mutation
(`drop_from_lock`) bundle-aware, but `grim uninstall` / TUI delete is **two
steps** and only step 2 was healed:

1. **File deletion** (`command::uninstall::run` step 1 / `tui::app::perform_uninstall`
   non-bundle arm / `tui::app::perform_member_uninstall`) deleted the
   materialized files + dropped the install record **unconditionally**.
2. **Declaration mutation** (`undeclare_and_unlock` → `drop_from_lock`) kept the
   lock entry when a declared bundle still provided the artifact.

So removing the **direct** declaration of an artifact a declared bundle also
provides kept the lock entry (correct) but **still deleted the files** —
leaving a `missing` artifact that is still desired. The ADR Context already
flagged this ("`grim uninstall` additionally deletes the files") but Option 1
only fixed the lock; the file surface was never gated, and the half-fix was
codified by `test_uninstall_direct_keeps_lock_entry_held_by_bundle`
(`state == "missing"`). The TUI **bundle**-row delete *was* already gated
(`bundle_uninstall_targets` → `drop_from_lock`), making the standalone path an
inconsistent outlier.

### 2. Decision (binding): keep files whenever a declared bundle still provides the artifact

A directly-declared artifact that a declared bundle still names — at **any**
identifier — stays in the effective desired set once the direct declaration is
removed. Uninstall therefore **degrades to `grim remove`** for that artifact:
drop the direct declaration, reconcile the lock, **keep the files and the
install record**. New pure gate
`effective_set::bundle_holds_after_direct_removal(set, cached, kind, name)`
applied at all three delete sites.

Key sub-decisions:

- **Broader than lock retention (deliberate).** The lock entry survives only on
  an exact-identifier flip (§ id-mismatch above); the *files* survive whenever a
  bundle names the artifact at any id. Rationale: files are content on disk —
  deleting content a declared bundle still wants is the destructive surprise the
  maintainer reported. Version reconciliation is `grim lock` + update's job; the
  id-mismatch path keeps the files, marks the bundle row stale, and heals on the
  next `grim lock` (no file ever lost).
- **Discriminator = "directly declared".** The gate fires only when the artifact
  is *currently* declared directly. A **bundle-only member** is never gated, so
  the TUI member-delete feature (delete a member's files → re-installable) is
  preserved. This cleanly separates "remove my standalone declaration" (keep
  files the bundle backs) from "delete this member" (delete files).
- **Pre-cache fallback.** When the lock carries no usable `[[bundle]]` snapshot
  (pre-cache lock), membership is unknowable offline → the gate returns `false`
  and uninstall deletes (the prior behavior). Fully offline.

### 3. Implementation

- `src/lock/effective_set.rs`: `bundle_holds_after_direct_removal` (pure) + unit
  tests (same-id, different-id, no-bundle, bundle-only member, incomplete cache,
  bundle kind).
- `src/command/uninstall.rs`: compute `held_by_bundle` before step 1; skip file
  deletion + record drop + config-sync when held.
- `src/tui/app.rs`: `direct_removal_keeps_files` I/O wrapper; gate the
  `perform_uninstall` standalone arm and `perform_member_uninstall`.
- Tests: `test/tests/test_bundles.py` —
  `test_uninstall_standalone_skill_held_by_bundle_keeps_files` (same-id) and
  `…_at_other_tag_keeps_files` (id-mismatch, heals via `grim lock`); replaces the
  now-incorrect `test_uninstall_direct_keeps_lock_entry_held_by_bundle`.

---

## Bug-fix decision: TUI bundle-delete must delete orphaned member files (file-deletion via the effective-set diff)

**Status:** Accepted (completes the *delete* side of the file surface)
**Date:** 2026-06-21
**Deciders:** Maintainer (mherwig) + Claude

### 1. Root cause (precise)

The file-retention gate above fixed the *keep* direction (don't delete files a
bundle still holds). The mirror *delete* direction was still wrong on the TUI
bundle-row delete. `tui::app::bundle_uninstall_targets` derived the
file-deletion set from **lock entries** (`previous.iter_artifacts()` minus the
`drop_from_lock` survivors). But a member only the bundle provides whose direct
declaration was removed earlier at a **different identifier** has its lock entry
**dropped as honestly stale** (the id-mismatch rule of this ADR) while its
install record + files persist. Deleting the bundle — the member's last holder —
then computed an empty/short target set (the lock entry was already gone), so
the member's files were **orphaned on disk**. This is the inverse of the
retention bug: the lock was set-aware, the file surface was not.

Sequence that orphans (maintainer's repro): install skill standalone (`:latest`)
→ install bundle (pins the same skill `:1.0.0`) → delete the skill (kept; lock
entry dropped stale on the id mismatch) → delete the bundle → **files remain**.

### 2. Decision (binding): derive deletion targets from `E_before \ E_after`

`bundle_uninstall_targets` now computes the file-deletion set from the
**effective-set difference** — `effective_set(set_before) \ effective_set(set_after)`
— the same set theory `drop_from_lock` uses, but applied to the **file** surface.
The effective set expands the bundle's `[[bundle]]` snapshot, so it sees a
snapshot-only member whose lock entry is gone; its install record (keyed by
`(kind, name)`) is still present, so `install::uninstall` deletes the files. A
member another declaration still holds stays in `E_after` and is therefore not a
target. Falls back to the prior lock-entry diff when the effective set is
incomputable offline (pre-cache lock / snapshot mismatch).

Companion TUI fixes in the same change:

- **Binding resolution (Codex [high]).** The bundle row carries only the repo;
  the `[bundles]` binding can be any name (`grim add --name`). `perform_uninstall`
  now resolves the real binding from the declaration (`resolve_bundle_binding`:
  exactly one repo match → that binding; none → repo-basename legacy/foreign
  fallback; **>1 alias → refuse** before any mutation) and reuses it for **both**
  target selection and the undeclare, so an aliased bundle can no longer have its
  files deleted while its declaration is left dangling.
- **Stale member badges.** `recompute_states` (run after every batch / member
  action) now also re-derives the cached bundle-member node states
  (`refresh_member_states`), so an expanded bundle's members reflect an
  install/uninstall immediately instead of the state captured at expand time.
  Member derivation stays keyed by repo identity (kind + registry/repository),
  matching catalog-row semantics; binding-name keying was considered and
  **deferred** (would diverge member rows from row derivation).

### 3. Implementation

- `src/tui/app.rs`: `bundle_uninstall_targets` effective-set-diff path (+ legacy
  fallback); `resolve_bundle_binding`; `perform_uninstall` resolves the binding
  under the flock; `refresh_member_states` called from `recompute_states`.
- Tests (`src/tui/app.rs`): `deleting_bundle_deletes_member_files_orphaned_by_prior_skill_delete`
  (orphan repro, id-mismatch), `deleting_aliased_bundle_row_undeclares_and_deletes_members`
  ([high] regression), `recompute_states_refreshes_stale_bundle_member_states`
  (stale-badge repro); fixture `registry_with_bundle` gains a second skill tag.

---

## Bug-fix decision: bundle-provided members are protected from deletion; derivation is snapshot-aware

**Status:** Accepted (supersedes the "member-delete deletes files" sub-decision above)
**Date:** 2026-06-21
**Deciders:** Maintainer (mherwig) + Claude

### 1. Root cause (two faces of one gap — snapshot membership treated as second-class)

- **Bug A — a bundle-provided member could be deleted (files removed).** The
  file-retention gate `bundle_holds_after_direct_removal` short-circuited to
  `false` for any artifact not *directly declared* (the `directly_declared`
  precondition), so a bundle-only member's files were deleted at every delete
  site. This was the prior "Discriminator = directly declared" sub-decision —
  the maintainer reversed it: a member a declared bundle provides must not be
  individually deletable; to remove it you remove the bundle.
- **Bug B — a bundle-provided member (files on disk) often showed
  NotInstalled.** `derive_artifact_state` *required* a top-level lock entry. The
  id-mismatch stale-drop (`drop_from_lock`, `Origin::Bundles` branch) deliberately
  drops the top-level entry while keeping files + install record + `[[bundle]]`
  snapshot; derivation then read NotInstalled until `grim lock` re-created the
  entry. The *read* path ignored snapshot membership just as the *gate* did.

### 2. Decision (binding)

- **Protect any artifact a declared bundle provides.** Generalize the gate
  (renamed `declared_bundle_provides`): drop the `directly_declared` precondition
  so it fires for bundle-only members too. A directly-declared+bundle artifact
  still degrades to dropping the direct declaration (files kept); a bundle-only
  member keeps everything and the TUI surfaces "provided by a bundle — remove the
  bundle to remove it". Removing the **bundle** still deletes its members
  (`bundle_uninstall_targets`, unchanged). The old member-delete-deletes-files
  feature is dropped.
- **Make derivation snapshot-aware.** `derive_artifact_state` no longer *requires*
  a top-level lock entry: if absent, a `[[bundle]]` snapshot that names the
  artifact (parsed like `member_node_from`) plus files + record yields `Installed`
  (→ `ViaBundle`). The top-level fast path stays (it alone can distinguish
  `Outdated`). New helper `bundle_snapshot_provides`.

### 3. Implementation

- `src/lock/effective_set.rs`: `bundle_holds_after_direct_removal` →
  `declared_bundle_provides` (drop `directly_declared` precondition; `Bundle` kind
  still false). Unit test `does_not_hold_for_bundle_only_member` →
  `holds_for_bundle_only_member` (now true).
- `src/tui/app.rs`: two-tier `derive_artifact_state` + `bundle_snapshot_provides`;
  `direct_removal_keeps_files` → `bundle_provides_files`; `perform_member_uninstall`
  surfaces the "provided by a bundle" note; `aliased_member_uninstall_targets_member_name_not_basename`
  → `aliased_bundle_member_is_protected_from_deletion`; new repros
  `deleting_bundle_only_member_keeps_files`, `bundle_member_stays_via_bundle_after_idmismatch_lock_drop`.
- `src/command/uninstall.rs`: gate rename; CLI `grim uninstall` of a
  bundle-provided member now keeps files.
- **Deferred (follow-up):** `grim status` / `status_badge` share the
  top-level-lock-entry-only assumption, so an id-mismatch member is omitted from
  `grim status` until `grim lock` heals it — TUI/CLI parity not yet unified.

## Changelog

| Date | Author | Change |
|------|--------|--------|
| 2026-06-11 | Claude (approved by maintainer) | Initial accepted draft |
| 2026-06-20 | Claude (approved by maintainer) | Add standalone↔bundle id-mismatch reconcile bug-fix decision (Option C: honest staleness retained, message reworded, outcome surfaced in TUI status line) |
| 2026-06-21 | Claude (approved by maintainer) | Add file-retention gate: uninstall / TUI delete must not delete files a declared bundle still provides (degrade to `remove`); gate `bundle_holds_after_direct_removal` at all three delete sites |
| 2026-06-21 | Claude (approved by maintainer) | Complete the delete side: TUI bundle-delete derives file-deletion targets from the effective-set diff `E_before \ E_after` (deletes snapshot-only members orphaned by a prior id-mismatch removal); resolve the real `[bundles]` binding before deleting (Codex [high]); refresh stale member badges in `recompute_states` |
| 2026-06-21 | Claude (approved by maintainer) | Protect bundle-provided members from deletion (generalize gate → `declared_bundle_provides`, no `directly_declared` precondition); snapshot-aware `derive_artifact_state` (two-tier: top-level entry, else declaration-matched `[[bundle]]` snapshot via `snapshot_declared_repos`/`effective_set`, so a stale/retagged snapshot provides nothing — Codex [medium]); CLI `UninstallStatus::KeptByBundle` for the protected no-op (Codex [medium]) |
