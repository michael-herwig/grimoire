# Bugfix Plan: Shared Bundle Members Evicted on Bundle Removal

## Status

- **Phase**: Fix
- **Owner**: Claude
- **Updated**: 2026-06-11

## Reproduce

1. Publish skill `M`. Publish bundle `A` (repo `acme/stack-a`) and bundle `B`
   (repo `acme/stack-b`), both declaring member `M` at the **same identifier**.
2. Declare both bundles in `grimoire.toml`, run `grim lock` — members agree,
   coalesce to one lock entry.
3. `grim remove bundle <binding-of-A>` (or TUI delete on A's row).
4. **Observed**: `M` is evicted from the lock (TUI delete also deletes `M`'s
   materialized files) even though bundle `B` still declares it. The lock's
   declaration hash is restamped to the post-removal set, so a following
   `grim install` trusts the lock and silently installs **without** `M` until
   a manual `grim lock` re-expands `B`.
5. Removing `B` first instead leaves `M` untouched — the outcome depends on
   bundle expansion order (provenance of `group[0]`), i.e. it is asymmetric.

## Root Cause Analysis

The lock models **one** contributing bundle per member:
`LockedArtifact { bundle: Option<String>, bundle_tag: Option<String> }`.

`resolver::merge_bundle_members` coalesces agreeing members into a single
`WorkItem` carrying only `group[0]`'s `(bundle_repo, bundle_tag)` — the other
contributing bundles' provenance is dropped at coalesce time.

`remove::drop_from_lock` evicts members matching the removed bundle's
`(repo, tag)`. With single provenance there is no way to know, offline, that
another still-declared bundle also contributes the member.

Introduced with the original bundle provenance design (single-provenance
lock fields); not a regression from the agent work.

## Edge Cases

| # | Scenario | Current | Correct |
|---|---|---|---|
| 1 | Bundles A+B (different repos) share member M; remove A (provenance holder) | M evicted; TUI delete also deletes files | M stays (B still declares) |
| 2 | Same, remove B (non-holder) first | M stays — correct by expansion-order luck | M stays, symmetric |
| 3 | Same repo, different tags, disjoint members | correct (`test_remove_bundle_keeps_sibling_at_same_repo`) | — |
| 4 | Same bundle (repo+tag) declared under two binding names; remove one | members evicted though sibling binding remains | members stay |
| 5 | Member also declared directly | direct wins, provenance `None` — correct | — |
| 6 | TUI `derive_bundle_state`: bundle whose shared member coalesced under the sibling's provenance aggregates the wrong member set | wrong badge | members attributed to all contributors |
| 7 | TUI delete of bundle A deletes shared member files | data loss for B | files deleted only when A is the sole contributor |

## Fix Design

**Multi-provenance**: `LockedArtifact.bundles: Vec<BundleProvenance>`
(`{ repo, tag }`), replacing the scalar pair. All contributing bundles are
recorded at coalesce time (sorted, deduped — deterministic).

- **Wire (dual shape, precedent: direct entries omit bundle fields to stay
  byte-identical to pre-bundle locks)**: exactly one provenance serializes as
  the legacy `bundle = "…"` + `bundle_tag = "…"` pair (byte-identical locks
  for the dominant case, old grim keeps reading them); two or more serialize
  as `bundles = [{ repo = "…", tag = "…" }, …]`. Read path accepts both
  shapes; both present on one entry is a parse error.
- **Eviction** (`drop_from_lock`): strip the removed `(repo, tag)` from each
  member's provenance list; drop the member only when its list becomes empty.
  Direct entries (empty list) are never touched.
- **Duplicate-binding guard** (case 4): before evicting, check the
  post-removal `set.bundles` — if any remaining binding resolves to the same
  `(registry_repository, tag_or_latest)`, skip eviction entirely.
- **TUI**: `derive_bundle_state` + `bundle_members_lock` match via
  `bundles.iter().any(…)`; `perform_uninstall` deletes files only for members
  whose **every** provenance points at the removed bundle's repo (sole
  contributor), so shared members keep their files.
- **Status**: `source` column joins multiple provenances
  (`bundle: repoA, repoB`); single stays `bundle: repo`.

## Regression Tests

Acceptance (`test/tests/test_bundles.py`), written before the fix, failing on
current code:

1. Two bundles (different repos) share a member → remove first-declared
   bundle → member stays in lock; remove second → member gone.
2. Reversed removal order → same outcome (kills the asymmetry).
3. Duplicate binding (same repo+tag, two names) → remove one → members stay.
4. Status `source` lists both contributing bundles for a shared member.

Unit (Rust, per module): merge records all provenances; drop_from_lock
strip/keep/drop; duplicate-binding guard; lock wire round-trip of both
shapes; TUI state derivation + delete-target exclusivity.

## Verification

- Regression tests pass post-fix; full `task verify` green.
- Manual rig scenario (added in the same change set): two bundles sharing
  `code-reviewer`, delete one in TUI, member survives with files intact.
