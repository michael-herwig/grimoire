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

## Changelog

| Date | Author | Change |
|------|--------|--------|
| 2026-06-11 | Claude (approved by maintainer) | Initial accepted draft |
| 2026-06-20 | Claude (approved by maintainer) | Add standalone↔bundle id-mismatch reconcile bug-fix decision (Option C: honest staleness retained, message reworded, outcome surfaced in TUI status line) |
