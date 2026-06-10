# ADR: Distributing Lifecycle Hooks Across AI IDEs/Agents

## Metadata

**Status:** Proposed
**Date:** 2026-06-03
**Deciders:** Architect (/architect), maintainer
**Beads Issue:** N/A
**Related PRD:** N/A
**Tech Strategy Alignment:**
- [x] Decision follows Golden Path in `.claude/rules/product-tech-strategy.md`
      (Rust 2024, single binary; no new runtime dependency mandated)
- [ ] OR deviation justified in Rationale section
**Domain Tags:** integration | security | api
**Supersedes:** N/A
**Superseded By:** N/A

## Context

`product-context.md` lists **hooks** as an in-scope artifact type Grimoire
should distribute ("skills, rules, hooks, and prompt templates"). This ADR
answers the maintainer's question — *is hooks support even possible across
different IDEs?* — and decides how it lands in Grimoire's existing
OCI-backed install model.

A "hook" is a lifecycle event handler: a script the agent runs at a defined
event (before a tool call, on session start, on stop, …). The current
Grimoire model installs **two passive artifact kinds**:

- **Skill** → directory tree copied to `<editor>/skills/<name>/`
- **Rule** → single `.md` file written to `<editor>/rules/<name>.md` (with a
  per-editor transform)
- (**Bundle** is a grouping kind that never materializes — expands at resolve.)

Hooks break two assumptions baked into that model:

1. **Hooks are executable, not inert content.** The packer hardcodes mode
   `0o644` (`src/skill/skill_package.rs:154`) and the materializer writes
   bytes without `chmod`. A hook script needs the executable bit (or a
   documented interpreter invocation).
2. **A hook is useless as a loose file.** Skills/rules are *read* by the
   editor once placed. A hook must be **registered** in a config file the
   tool owns — `settings.json` (Claude), `hooks.json` (Cursor/Windsurf/
   Copilot), `config.toml` (Codex), `.gemini/settings.json` (Gemini) — that
   maps `event → matcher → script`. **File placement alone does not activate
   a hook.** This is a genuinely new capability: Grimoire must *merge into,
   and later cleanly remove from, a config file it does not own,* idempotently
   and reversibly.

The research (below) confirms cross-IDE support is feasible: 7 of 8 surveyed
tools have hook systems sharing a common primitive, with divergent schemas.

## Decision Drivers

- **Product scope** — hooks are an explicitly named artifact type; not
  supporting them leaves the vision incomplete.
- **Activation, not just placement** — the feature only delivers value if the
  installed hook actually fires; registration is the hard part.
- **Security** — hooks are arbitrary code execution at user privilege, firing
  hundreds of times per session, with an agent-loop injection path unique to
  AI hooks. This is the dominant NFR (Block-tier per `quality-core.md`).
- **Reversibility / idempotency** — install/update/uninstall/prune must
  mutate third-party config files without clobbering user edits, matching the
  existing integrity-gate + provenance + lock-orphan-prune machinery.
- **Forward compatibility** — `GrimoireLock` and `InstallState` use
  `deny_unknown_fields`; a new artifact kind must not break older `grim`.
- **Cross-IDE divergence** — event taxonomies differ; some tools (Continue,
  Aider) have no hook target at all. Portability must degrade gracefully.

## Industry Context & Research

**Research artifact:** [`research_ide_hooks.md`](./research_ide_hooks.md)

**Is it possible across IDEs?** Yes. Seven of eight tools surveyed have real
hook systems that converged independently on the same primitive: **shell
command + JSON on stdin + `exit 2` to block**. They differ in config location,
event names, naming convention, and handler types.

| Tool | Hooks | Config file | Naming | Notes |
|---|---|---|---|---|
| Claude Code | Yes (mature, 30+ events) | `.claude/settings.json` | Pascal | reference impl; 5 handler types |
| Codex CLI | Yes (10) | `.codex/hooks.json` / `config.toml` | Pascal | mirrors Claude; explicit trust model |
| Copilot | Yes (13, preview) | `.github/hooks/*.json` | camel(+Pascal) | accepts Claude format; HTTPS-only |
| Cursor | Yes (~20, beta) | `.cursor/hooks.json` | camel | `failClosed` per hook |
| Gemini CLI | Yes (11) | `.gemini/settings.json` | Pascal | parallel exec; `BeforeModel` |
| Windsurf | Yes (12) | `.windsurf/hooks.json` | snake | **category events**, no generic matcher |
| Kiro | Yes (10) | `.kiro/` | camel | file-watch + task hooks |
| Continue / Aider | **No** | — | — | no hook target |
| Zed | Partial | `tasks.json` | snake | AI hooks proposed, unshipped |

**Common portable core** (present in all hook-capable tools): `session-start`,
`pre-tool`, `post-tool`. **Lowest common denominator** a portable hook must
satisfy: shell script, JSON stdin, `exit 0`/`exit 2`, JSON stdout.

**Key insight:** *no tool ships a portable hook abstraction.* A package
manager that installs **one** canonical hook and emits the **correct config
per target** fills a real gap — but Windsurf's category events and Gemini's
divergent surface need adapter logic, and Continue/Aider/Zed must be flagged
unsupported. Single-definition coverage is realistic **today for Claude Code,
Codex, Copilot, and Cursor** (matcher-based, Pascal/camel).

**Trending:** the hook pattern reached critical mass in 12–18 months; new
tools ship hooks at launch. The execution primitive is settled.

## Considered Options

### Option 1: Passive script drop (no registration)

**Description:** Treat a hook like a skill/rule — place the script at
`<editor>/hooks/<name>.<ext>` and stop. The user manually wires it into
`settings.json`. Minimal change: reuse the existing materializer (plus an
exec-bit fix).

| Pros | Cons |
|------|------|
| Tiny change; reuses materializer + lock/state as-is | Does **not** activate the hook — leaves the hard, error-prone part to the user |
| No third-party-config mutation → no clobber risk | Uninstall/prune can't deregister (the entry is user-authored) |
| No new security surface beyond the file itself | Fails the product promise ("install a hook") in spirit |

### Option 2: Native per-target hook + reversible config registration (no canonical translation)

**Description:** A hook is published **per target** (the publisher ships the
Claude-flavored config fragment, the Cursor-flavored one, etc.). Grimoire
materializes the script **with the exec bit** and **merges the native
registration entry** into the right config file for that target, idempotently
and reversibly (managed marker + provenance, mirroring the existing
conflict/provenance/prune work). No canonical event translation — Grimoire
moves bytes and edits config; portability is the publisher's job.

| Pros | Cons |
|------|------|
| Delivers real activation — the hook fires after install | Publisher must ship N variants for N tools (no write-once) |
| Builds the genuinely-new machinery (exec materialization + reversible config merge) without the divergence risk of translation | No portability layer — the headline differentiator is deferred |
| Claude-Code-first is a small, provable target surface | Config-merge-into-foreign-file risk must be solved now (unavoidable for any activating design) |

### Option 3: Canonical portable hook manifest + per-target translators

**Description:** Define a neutral `HookManifest` (canonical event + matcher +
script ref + handler type + portability tier). On install, Grimoire (1)
materializes the shared script with the exec bit, (2) **translates** the
canonical event → per-target event name/format, (3) merges the registration
into each enabled target's config file. Continue/Aider/Zed surfaced as
unsupported.

| Pros | Cons |
|------|------|
| Write-once, install-everywhere — the unique market gap | Largest surface; translation correctness across 6 schemas is hard |
| Portability tier (`core`/`extended`/`tool-specific`) sets honest expectations | Windsurf category-events / Gemini divergence force per-target adapters anyway |
| Strongest realization of the product vision | Big-bang risk while the registration machinery is itself unproven |

## Decision Outcome

**Chosen Option:** **Option 2 first, evolving to Option 3** — adopt `Hook` as
a first-class `ArtifactKind` whose distinguishing machinery is **executable
materialization + reversible config registration**, gated behind an explicit
security opt-in, implemented **Claude-Code-first** but with a
**canonical-by-design manifest schema** so the Option 3 translation layer is
purely additive.

**Rationale:**

- **Option 1 is rejected** — it does not activate hooks, so it fails the
  feature's purpose and can't deregister on uninstall/prune.
- **Big-bang Option 3 is rejected for now** — translation correctness across
  six divergent schemas is high-risk *while the registration engine itself is
  unbuilt and unproven*. Divergence (Windsurf category events, Gemini) means
  adapters are needed regardless, so nothing is lost by sequencing.
- **Option 2 builds exactly the new machinery that every activating design
  needs** (exec-bit materialization, reversible foreign-config merge,
  deregistration on prune) against the **smallest, best-documented target**
  (Claude Code — also the format Copilot/Codex already accept). Designing the
  manifest schema canonically from day one means Phase 2 adds translators +
  targets without reworking storage, lock, or state.

This is a **two-way door per phase**: Phase 1 ships a working Claude-Code hook
installer; Phase 2 generalizes. Each phase is independently shippable and
revertible.

### Consequences

**Positive:**
- Hooks become installable *and active*, not just placed.
- The reversible-config-merge engine is reusable for any future "register into
  a foreign config" need.
- Canonical schema positions Grimoire for the unfilled portability gap.

**Negative:**
- Grimoire now writes into config files it does not own → new failure mode
  (clobber/corruption) that must be defended continuously.
- New security surface (RCE distribution) demands signing + approval UX.
- `ArtifactKind` is a closed total-match enum: adding `Hook` is a
  compiler-enforced edit across ~12 sites (`oci/artifact_kind.rs`,
  `editor_target.rs`, `installer.rs`, `prune.rs`, lock/state/config/bundle).

**Risks:**
- **Foreign-config corruption** → mitigate: parse-merge-write (never
  blind-append), Grimoire-owned **managed marker** per entry, integrity hash
  before overwrite (reuse install integrity gate), atomic write + backup,
  never touch unmanaged keys.
- **RCE / supply chain** → mitigate: signed manifests (Sigstore/cosign);
  **default to NOT auto-registering** (registration is explicit opt-in);
  user approval on first activation and on any script-hash change (Codex
  model); `observer` vs `gatekeeper` tier in the manifest; `grim hooks list`.
- **Forward-compat break** → mitigate: add `hooks` to lock/state with
  `#[serde(default)]`; bump `DECLARATION_HASH_VERSION`.
- **Unsupported targets** (Continue/Aider/Zed) → mitigate: surface explicitly
  at install (`unsupported target: skipped`), never silent.

## Technical Details

### Architecture (C4 — Component placement)

```
grimoire.toml [hooks]  ──► DesiredSet.hooks ──► resolve_lock ──► GrimoireLock.hooks
                                                                       │
                                            install_all / install_one  ▼
   ┌───────────────────────────────────────────────────────────────────────┐
   │ per LockedArtifact(kind=Hook):                                          │
   │   integrity gate (script + config entry hashes)                         │
   │   fetch blob → HookMaterializer (writes script, SETS EXEC BIT)  ◄── NEW │
   │   for each enabled EditorTarget:                                        │
   │       HookRegistrar::register(event, matcher, script_path, config) ◄ NEW│
   │           parse target config → upsert MANAGED entry → atomic write     │
   │   record InstallRecord{ script paths + registered config entries }      │
   └───────────────────────────────────────────────────────────────────────┘
uninstall / prune ──► HookRegistrar::deregister (remove only managed entries)
```

Two new seams parallel the existing `ArtifactMaterializer` + `EditorTarget`:

- **`HookMaterializer`** — like `DefaultMaterializer` but sets the exec bit.
- **`HookRegistrar`** (per target) — the new concern: reversible merge into a
  foreign config file. Trait dispatch (per `arch-principles.md` Strategy
  pattern): `ClaudeRegistrar` first; `CodexRegistrar`, `CopilotRegistrar`,
  `CursorRegistrar`, … in Phase 2.

### API Contract (canonical hook frontmatter — designed for Phase 2)

```yaml
# hook frontmatter (canonical, target-neutral)
name: format-on-write
event: PreToolUse            # canonical event (Claude taxonomy as reference)
matcher: "Edit|Write"        # tool matcher (empty = all)
handler: command             # command | http | prompt | agent  (command = portable)
command: ./format.sh         # script ref, relative to artifact root
timeout: 30
tier: gatekeeper             # observer (read-only) | gatekeeper (can block)
targets: [claude]            # Phase 1: explicit; Phase 2: canonical → translated
```

- Follow the forward-compatible frontmatter pattern (`#[serde(flatten)] extra`,
  per `skill_frontmatter.rs:83`).
- `tier` is a security classification (observer vs gatekeeper) surfaced at
  approval time.

### Data Model (deltas)

- `ArtifactKind::Hook` (`oci/artifact_kind.rs`); `subdir()` → `"hooks"`;
  `is_dir_artifact()` → `false` (single-file). Update **all** match sites.
- `GrimoireLock { …, #[serde(default)] hooks: Vec<LockedArtifact> }`.
- `InstallRecord` gains optional `registered_entries: Vec<RegisteredEntry>`
  (`Option` + `#[serde(default)]`) recording *what was merged where*, so
  deregistration is exact.
- `RawConfig` gains `[hooks]` table; `DesiredSet.hooks`; `declaration_hash()`
  includes hooks (bump `DECLARATION_HASH_VERSION`).
- `BundleMember.kind` already carries `ArtifactKind` → bundles group hooks for
  free once the variant exists.
- OCI: `annotations_for_hook(...)`; layer media type
  `application/vnd.grimoire.artifact.layer.v1.tar` reused; `com.grimoire.kind`
  = `"hook"`.

### CLI surface

- `[hooks]` in `grimoire.toml`; `grim add/remove` learn the hook kind.
- `grim hooks list` — active hooks, source, target, last-verified hash, tier.
- `grim install` requires explicit hook activation (e.g. `--enable-hooks` or
  per-hook approval prompt) before any config registration.

## Implementation Plan

**Phase 1 — Claude-Code hook installer (Option 2 scope, canonical schema):**
1. [ ] Exec-bit support: preserve mode in packer OR `chmod` in
       `HookMaterializer` (decide in builder spec).
2. [ ] `ArtifactKind::Hook` + update all ~12 match sites (compiler-guided).
3. [ ] `HookFrontmatter` (canonical schema above) + `validate_hook_file` /
       `pack_hook_file`.
4. [ ] `HookRegistrar` trait + `ClaudeRegistrar` (parse-merge-write
       `settings.json` with managed marker; atomic + backup).
5. [ ] Lock/state/config deltas with `#[serde(default)]`;
       `DECLARATION_HASH_VERSION` bump.
6. [ ] Reversible deregistration wired into `uninstall` + `prune_orphans`;
       integrity gate respects user edits to `settings.json`.
7. [ ] Security: signed-manifest verify, explicit activation opt-in, approval
       on first use + hash change; `grim hooks list`.
8. [ ] pytest acceptance tests: install→fire→update→uninstall→prune; local
       edit preserved; unsupported-target surfaced.

**Phase 2 — portability (Option 3, additive):**
9. [ ] Canonical-event translator + `CodexRegistrar`, `CopilotRegistrar`,
       `CursorRegistrar` (matcher-based, lowest risk first).
10. [ ] `GeminiRegistrar`, `WindsurfRegistrar` (category-event adapters);
        portability tiers; explicit unsupported flags for Continue/Aider/Zed.

## Validation

- [ ] Security review (RCE distribution, foreign-config merge) — **required
      handoff to /security-auditor before Phase 1 merge**
- [ ] Acceptance tests: registration is reversible, idempotent, edit-preserving
- [ ] Forward-compat: older `grim` ignores `hooks` lock/state fields cleanly
- [ ] No clobber: unmanaged keys in target config untouched across the
      install/update/uninstall/prune cycle

## Links

- [`research_ide_hooks.md`](./research_ide_hooks.md) — cross-IDE hook survey
- `.claude/rules/arch-principles.md` — Strategy/trait dispatch, enum
  exhaustiveness, command flow
- `.claude/rules/product-context.md` — hooks as an in-scope artifact type
- `.claude/rules/subsystem-cli-commands.md` — CLI command index

---

## Changelog

| Date | Author | Change |
|------|--------|--------|
| 2026-06-03 | Architect (/architect) | Initial draft |
