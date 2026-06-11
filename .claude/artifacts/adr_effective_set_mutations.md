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

## Changelog

| Date | Author | Change |
|------|--------|--------|
| 2026-06-11 | Claude (approved by maintainer) | Initial accepted draft |
