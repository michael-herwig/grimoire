# ADR: Portable install state for shared GRIM_HOME / devcontainers

## Metadata

**Status:** Accepted
**Date:** 2026-06-13
**Deciders:** maintainer (architect proposal)
**Beads Issue:** N/A
**Related PRD:** N/A
**Tech Strategy Alignment:**
- [x] Decision follows Golden Path in `.claude/rules/product-tech-strategy.md` (no new tech; Rust + serde only)
**Domain Tags:** data, infrastructure
**Supersedes:** N/A

## Context

Grimoire records what it has materialized on disk in an **install-state**
file, separate from the **lock** file:

| Concern | File | Lifecycle | Intended commit |
|---|---|---|---|
| Intent / resolution ("what version is pinned") | `grimoire.lock` (TOML) | written by `grim lock` | **committed** (Cargo.lock-style) |
| Installation facts ("what is materialized, where, at what hash") | `state/*.json` | written by `grim install`/`update`/`uninstall` | machine-local |

Today install state lives entirely under `$GRIM_HOME`:

```
$GRIM_HOME/state/global.json
$GRIM_HOME/state/projects/<sha256(canonical_config_path)>.json
```

Each `InstallRecord` stores **absolute** target paths plus a per-client
`content_hash` used for drift detection (compare recorded hash vs a hash
recomputed from disk; mismatch ⇒ `modified`, guarding user-edited
artifacts). Verified against a live record at
`test/manual/.grim-home/state/projects/14ac9c65…json`.

This breaks in the target deployment: **one shared `GRIM_HOME` volume
mounted into many devcontainers, each mounting its own repo at
`/workspace`** (confirmed setup). Three defects surface:

1. **Project-state key collision (guaranteed, not hypothetical).** The
   project state filename is `sha256(canonicalize(config_path))`. Every
   container sees its config at `/workspace/grimoire.toml`, so *every
   project hashes to the same filename*. With a shared `GRIM_HOME`, all
   projects fight over one `state/projects/<sha>.json` — last writer wins,
   the rest get corrupt/foreign records.

2. **Absolute paths are non-portable.** Records store e.g.
   `/home/mherwig/dev/grimoire/test/manual/project/.claude/skills/…`.
   Inside a container the artifact is at `/workspace/.claude/skills/…`, and
   global records (`~/.claude/…`) break when the container's `$HOME`
   differs. `ClientRecord::current_hash()` reads `self.target` directly, so
   drift detection checks the wrong path or none.

3. **GRIM_HOME pollution.** Per-project ephemera accumulates in the shared
   volume keyed by an opaque, non-reversible hash — orphaned on every repo
   move/rename, and now cross-contaminating between unrelated projects.

**Secondary defect (the "`.copilot` twice" observation).** `InstallRecord`
carries top-level `target` + `content_hash` *and* a `clients[]` array. The
top-level pair is a legacy single-client mirror of the **primary** client
(`client_outputs()` back-compat shim). With multi-client records it simply
**duplicates `clients[0]`**. When Copilot is the primary client (e.g.
Claude not detected), the record shows a `.copilot/...` path at top level
**and** a `copilot` entry inside `clients[]` — the same vendor twice. Not
structural corruption; a denormalization smell. Source of truth should be
the per-client list alone.

## Decision Drivers

- **Portability** — install state must survive a `/workspace` bind mount
  and a different `$HOME`/`$GRIM_HOME` layout between host and container.
- **No collisions under a shared `GRIM_HOME`** — N projects, one volume.
- **VCS hygiene** — the committed lock must stay deterministic and
  machine-independent (no churn from which clients a teammate happens to
  have installed).
- **Keep lock cost low** — `grim lock` is resolution-only today (no blob
  fetch); don't regress it into a materialization step.
- **Preserve drift detection** — user-edit guard must still fire.
- **Minimal blast radius** — project is provisional; prefer the smallest
  change that removes the defect class over a speculative rewrite.

## Industry Context & Research

**Research artifact:** [`research_state_portability_v2.md`](./research_state_portability_v2.md)
(round-2 multi-agent research: devcontainer-id, layout conventions, shared-home
semantics, codebase blast radius). Round-1: [`research_state_portability.md`](./research_state_portability.md).

**`${devcontainerId}` keying — researched and rejected for project scope.** A
maintainer proposal to keep state in `$GRIM_HOME` keyed by `${devcontainerId}`
(instead of `sha256(path)`) does **not** fix the collision: the id is
`base32(SHA-256({devcontainer.local_folder, devcontainer.config_file}))` over the
**host** workspace + config path, so containers mounting the *same host dir* at
`/workspace` produce the *identical* id — same collision. It is also never
auto-injected (requires explicit `containerEnv`), and re-keying leaves the stored
*absolute* target paths broken, so it would need the anchoring rework *anyway*
(4-file change vs 2). Reserved as the right tool for the **global** follow-up
(per-host segmentation of `global.json`), where there is no workspace to relocate
into.

**Trending approaches / precedent:**
- **Cargo / npm / Poetry split**: a *committed* lock (resolution, machine
  independent) vs *uncommitted* build/artifact state under a cache dir.
  Grimoire already mirrors this; the bug is only the *location and path
  encoding* of the uncommitted half.
- **Anchor-relative path storage** (git's `$GIT_DIR`-relative paths,
  XDG-relative configs): durable references store a *root token + relative
  remainder*, resolving the root at runtime so the record survives
  relocation and env-override changes. This is the key technique applied
  below.
- **Devcontainer convention**: the whole `/workspace` is bind-mounted, so a
  workspace-local file travels into the container **regardless of git
  status** — gitignored machine-local state still rides along.

**Key insight:** The committed lock decision (confirmed) means the
deterministic part (a render hash *could* be committed) and the
machine-local part (which clients are present, where) have *different
lifecycles*. The user's proposal to fold the content hash into the lock is
directionally right (co-locate state with the project) but targets the
wrong file: the committed lock cannot absorb machine-local facts without
VCS churn. The fix is to **relocate and anchor-relativize the install-state
file**, not to merge it into the lock.

## Considered Options

### Option 1: Relocate + anchor-relativize install state (keep lock/state split)

**Description:** Move project install state out of `$GRIM_HOME` into the
workspace (`<workspace>/.grimoire/state.json`, gitignored); keep global
state at `$GRIM_HOME/state/global.json`. Replace absolute `target` paths
with an `(anchor, relative_path)` pair resolved at runtime. Drop the
denormalized top-level `target`/`content_hash` (fixes the `.copilot`-twice
smell). Lock is untouched.

| Pros | Cons |
|------|------|
| Kills all three defects at the source (no GRIM_HOME key, per-workspace file, relative paths) | New `.grimoire/` dotdir — but grim self-manages `.grimoire/.gitignore` (uv/pixi pattern), so consumers never hand-edit their root `.gitignore` |
| Travels via the `/workspace` bind mount with zero git involvement | One-time migration of existing absolute-path state |
| Lock stays cheap (resolution-only) and deterministic | Anchor-resolution code on every read |
| Preserves the clean lock=intent / state=facts separation | |
| Smallest change; reversible | |

### Option 2: Fold materialization facts into the lock (the original proposal)

**Description:** Extend `LockedArtifact` with per-client `{target,
content_hash}` and delete the separate state files. Project lock travels
with the repo; global lock with `GRIM_HOME`.

| Pros | Cons |
|------|------|
| One file; content hash sits next to the digest it derives from | Lock is **committed** → install facts (which clients present, drift baseline) churn the committed artifact and conflict across teammates |
| Matches the user's stated instinct literally | "Which clients installed here" is machine-local — not legitimately committable |
| | Computing render hashes at lock time forces blob fetch ⇒ regresses `grim lock` from cheap resolution to full materialization |
| | Conflates two different lifecycles in one file |

### Option 3: Hybrid — deterministic expected hash in lock, machine-local facts in relocated state

**Description:** Option 1's relocation/relativization, **plus** hoist the
*deterministic* expected per-client content hash into the committed lock as
a supply-chain/render-integrity value; the relocated state keeps only
machine-local facts (clients present, anchors).

| Pros | Cons |
|------|------|
| Committed lock gains cross-machine render-integrity (rendered output verified against lock) | Requires render hashes at lock time (blob fetch) or install-time write-back churn |
| Honors the user's proposal for the part that is legitimately committable | Two files to keep coherent |
| | Render-integrity-in-lock is not the user's actual need (portability is) — pays cost for an unrequested benefit |

### Option 4: Stateless — derive everything from lock + workspace + content store

**Description:** Delete the state subsystem. Target paths are already
deterministic (`ClientTarget::path_for`); recompute the expected hash on
demand from the cached blob; detect presence by probing the filesystem.

| Pros | Cons |
|------|------|
| Maximal KISS/YAGNI — no portability problem because nothing is stored | Drift check needs the content-store blob present (fine if `GRIM_HOME` shared; fails offline without cache) |
| Nothing machine-local to relocate | Orphan uninstall gap: removing an artifact from `grimoire.toml` then re-locking loses the record needed to find stale files |
| | Cannot distinguish grim-managed from hand-placed files without a hash match |
| | Largest behavioral change to a working subsystem |

## Decision Outcome

**Chosen Option:** **Option 1** — relocate and anchor-relativize install
state; keep the lock/state split; fix the denormalization.

**Rationale:**

- It removes the entire defect *class* (collision, non-portability,
  pollution) with the smallest change, and it does so without touching the
  committed lock — so VCS hygiene and the cheap `grim lock` path are both
  preserved.
- The two confirmed constraints rule out the alternatives the user leaned
  toward: **committed lock** kills Option 2 (machine-local facts can't live
  in a committed, deterministic file), and **shared GRIM_HOME** is exactly
  what makes the current location untenable.
- Option 3's only extra over Option 1 is render-integrity *in the committed
  lock*, which costs a blob fetch at lock time and is not the stated need.
  It remains a clean **future enhancement** layered on Option 1 if
  supply-chain verification of the render step is later wanted.
- Option 4 is the elegant end state but the orphan-uninstall gap and the
  blob-availability dependency make it a riskier bet than the project needs
  right now. Option 1 does not foreclose it — relativizing first is a
  prerequisite step toward it anyway.

This **adopts the correct kernel of the original proposal** (store the
per-vendor, per-artifact content hash co-located with the project, so it
travels) while correcting the target: a relocated, gitignored,
bind-mount-portable state file — not the committed lock.

### Consequences

**Positive:**
- Multiple projects under one shared `GRIM_HOME` no longer collide.
- Project state rides the `/workspace` bind mount into any container; drift
  detection works against `/workspace`-anchored paths.
- Global state survives a differing container `$HOME` (anchored to the
  resolved vendor root, honoring `CLAUDE_CONFIG_DIR` etc.).
- `$GRIM_HOME` holds only genuinely global data (content store, global
  state, global lock, config).
- The `.copilot`-twice redundancy disappears; `clients[]`/`outputs` is the
  single source of truth.

**Negative:**
- Consumer repos gain a `.grimoire/` dir. grim writes a self-managed
  `.grimoire/.gitignore` containing `*` on first project install (uv/pixi
  precedent), so the dir is ignored without the consumer editing their root
  `.gitignore`; the dot-dir matches `.git/`/`.terraform/`/`.pixi/` convention.
- A migration step (or lazy rebuild) is required for existing state.
- *(Considered and dropped — research v2)* A `GRIM_STATE_DIR` override for a
  read-only `/workspace` is **not needed**: project-scope install materializes
  skills into `<workspace>/.claude/…` etc., so a read-only workspace already
  fails at materialization, long before the state write — redirecting state
  rescues nothing. Additive later if a genuine need appears (YAGNI).
- **Scope limit (cross-model review):** Option 1 de-collides **project**-scope state. **Global**-scope `global.json` under a shared `GRIM_HOME` remains last-writer-wins across machines running `--global` installs — a residual of the same collision class, out of scope here and tracked as a follow-up (plan Deferred Q3). The "removes the entire defect class" framing above is scoped to **project** state; path portability of global state across differing `$HOME` is fully solved by anchoring, but record-set sharing in a single `global.json` is not.

**Risks:**
- *Path-traversal on reconstruction.* Re-joining `anchor + relative` must
  re-validate containment (no `../` escape) per `quality-security.md` —
  a stored relative path is now untrusted input at read time. **Mitigation:**
  canonicalize and assert the resolved path stays within the anchor root
  before any filesystem op.
- *Migration loses drift baseline.* A lazy rebuild re-baselines a
  user-dirtied artifact as clean. **Mitigation:** prefer an explicit
  one-time converter (old abs path → anchor+relative); if rebuilding,
  warn that drift state resets.
- *Anchor set drift.* If a vendor root model changes, the anchor enum must
  evolve with a versioned state schema (`version` already gates this).

## Technical Details

### Architecture

```
Shared GRIM_HOME volume (one, mounted into every container)
  $GRIM_HOME/
    blobs/                 content store (shared, immutable)
    grimoire.lock          global lock (committed-elsewhere / shared)
    state/global.json      GLOBAL install facts — anchors: vendor roots
    catalog.json, config

Per project (bind-mounted at /workspace in each container)
  /workspace/
    grimoire.toml          intent  (committed)
    grimoire.lock          resolution (committed) — UNCHANGED by this ADR
    .grimoire/state.json   PROJECT install facts (gitignored) — anchor: Workspace
    .claude/ .opencode/ .github/   materialized artifacts
```

### Data Model

Replace the absolute `PathBuf` target + denormalized top-level pair with an
anchored, relative encoding and a single per-client list.

```rust
// Resolved at runtime; honors env overrides (CLAUDE_CONFIG_DIR, COPILOT_HOME, …).
enum PathAnchor {
    Workspace,        // project scope: <workspace>/...
    ClaudeRoot,       // ~/.claude or $CLAUDE_CONFIG_DIR
    CopilotRoot,      // ~/.copilot or $COPILOT_HOME
    OpenCodeSkills,   // XDG / $OPENCODE_CONFIG_DIR
    GrimHome,         // $GRIM_HOME (e.g. global OpenCode rules)
}

struct AnchoredPath { anchor: PathAnchor, relative: String } // relative is forward-slash, validated

struct ClientOutput {            // was ClientRecord
    client: String,
    target: AnchoredPath,
    content_hash: Digest,
    support_dir: Option<AnchoredPath>,
}

struct InstallRecord {
    kind: ArtifactKind,
    name: String,
    pinned: PinnedIdentifier,
    outputs: Vec<ClientOutput>,  // SINGLE source of truth — drop top-level target/content_hash
}

struct InstallStateFile { version: InstallStateVersion, records: Vec<InstallRecord> }
```

- **Reconstruct** absolute path: `resolve(anchor, scope, env).join(relative)`
  then **validate containment**.
- **Project state location:** `<workspace>/.grimoire/state.json` (one file
  per workspace; the *location* is the key, so no `sha256(path)` filename).
- **Global state location:** `$GRIM_HOME/state/global.json` (unchanged
  location, anchored paths).

### API / behavior contract

- `grim install/update/uninstall/status` resolve the project state path
  from the discovered workspace root (already computed during config
  discovery), not from a `GRIM_HOME` hash.
- `status`/integrity gate: recompute on-disk hash at
  `resolve(output.target)`; compare to `output.content_hash`. Unchanged
  semantics, portable inputs.
- Back-compat read shim: if a loaded record has no `outputs` but legacy
  top-level fields, synthesize one `ClientOutput` (mirrors today's
  `client_outputs()`), so old files still read during migration.

### Migration

1. On load, detect legacy state (absolute paths and/or top-level
   `target`/`content_hash`, and/or a `state/projects/<sha>.json` file).
2. Preferred: **convert** — for each record, classify the absolute path
   against the known anchor roots for its scope, derive `(anchor,
   relative)`, validate, rewrite into the new location; delete the old
   `projects/<sha>.json`.
3. Fallback: **lazy rebuild** with a one-time warning that drift baselines
   reset (acceptable given provisional status, but converter is preferred
   precisely to preserve the user-edit guard).

## Implementation Plan

1. [ ] Introduce `PathAnchor` + `AnchoredPath` with runtime resolve +
   containment validation (unit-tested against traversal).
2. [ ] Rework `InstallRecord`/`ClientRecord` → `outputs: Vec<ClientOutput>`;
   remove denormalized top-level fields; bump state schema with read shim.
3. [ ] Move project state to `<workspace>/.grimoire/state.json`; drop the
   `sha256(config_path)` filename scheme; update `scope_resolution`.
4. [ ] Update install/uninstall/status to resolve+validate anchored paths.
5. [ ] Migration (converter + lazy-rebuild fallback).
6. [ ] grim writes a self-managed `.grimoire/.gitignore` (= `*`) when it
   creates the `.grimoire/` dir on first project install (idempotent; never
   touches the consumer's root `.gitignore`); document the dir in grim's docs.
7. [ ] Update `subsystem-file-structure.md` (state layout + anchors) and
   any CLI docs; drift-review the first-party catalog skills per
   `catalog/README.md`.

## Validation

- [ ] Acceptance test: two projects, both at `/workspace`-style paths,
   shared `GRIM_HOME` → no state collision (was: collision).
- [ ] Acceptance test: install on host path, relocate dir, `status` still
   resolves + reports clean (path portability).
- [ ] Acceptance test: drift still detected after relocation (edit a
   support-dir file → `modified`).
- [ ] Unit test: anchored-path traversal attempt (`../`) rejected.
- [ ] Migration test: legacy abs-path `projects/<sha>.json` converts and is
   removed.
- [ ] Security review of the resolve+validate boundary.

## Links

- `.claude/rules/subsystem-file-structure.md` — storage layout (to update)
- `.claude/rules/quality-security.md` — path-traversal / symlink-escape
- `.claude/rules/arch-principles.md` — command flow, where features land

---

## Changelog

| Date | Author | Change |
|------|--------|--------|
| 2026-06-13 | architect | Initial draft |
| 2026-06-13 | architect | Round-2 research (`research_state_portability_v2.md`) confirms Option 1. Refinements: grim self-manages `.grimoire/.gitignore` (Q1); `GRIM_STATE_DIR` dropped as unneeded (Q4); global `global.json` stays shared, `devcontainerId` keying reserved for the global follow-up (Q3); `devcontainerId` keying rejected for project scope. |
