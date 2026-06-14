# ADR: Codex (OpenAI Codex CLI) as a fourth client vendor

## Metadata

**Status:** Accepted
**Date:** 2026-06-14
**Deciders:** maintainer (architect proposal)
**Beads Issue:** N/A
**Related PRD:** N/A
**Tech Strategy Alignment:**
- [x] Decision follows Golden Path in `.claude/rules/product-tech-strategy.md` (no new tech; Rust + serde + the already-present `toml` crate)
**Domain Tags:** integration
**Supersedes:** N/A

## Context

Grimoire materializes AI-config artifacts (skills, rules, agents) into each
supported client's native on-disk layout behind a `Vendor` trait seam
(`src/install/vendor.rs`) with a closed `ClientTarget` identity enum
(`src/install/client_target.rs`). Three vendors exist — Claude, OpenCode,
Copilot — established by
[`adr_tool_namespaced_metadata_rendering.md`](./adr_tool_namespaced_metadata_rendering.md)
and extended for agents by
[`adr_agent_artifact_kind.md`](./adr_agent_artifact_kind.md).

This ADR adds **Codex** (OpenAI Codex CLI) as a first-class fourth vendor
(`--client codex`, `[options].clients = ["codex"]`).

What Codex actually supports (verified against OpenAI Codex docs, June 2026):

| grim kind | Codex native target |
|---|---|
| **Skill** | Cross-vendor open standard `.agents/skills/<name>/` — project `<repo>/.agents/skills`, global `$HOME/.agents/skills`. Universal `SKILL.md` (no Codex-specific frontmatter). Auto-discovered. **Independent of `$CODEX_HOME`.** ([codex/skills](https://developers.openai.com/codex/skills)) |
| **Agent** | Auto-discovered TOML at `.codex/agents/<name>.toml` (**TOML, not Markdown**). Keys `name`, `description`, `developer_instructions` (= grim body), optional `model`. Global `$CODEX_HOME`\|`~/.codex` + `agents/`. ([codex/subagents](https://developers.openai.com/codex/subagents)) |
| **Rule** | No native target. Codex has **no glob / `applyTo` scoping anywhere** — AGENTS.md is always-on, directory-granular, session-fixed. Hooks cannot synthesize it: `PreToolUse` rejects `additionalContext`; `UserPromptSubmit`/`SessionStart` see only prompt text / fire once. |

## Decision Drivers

- One-struct-per-vendor pattern must keep composing; adding a client should
  stay "one new struct + one enum arm".
- A grim **rule** exists to do path-glob context separation (`paths:`).
  Faithfully representing that requires a per-file path-scoping mechanism the
  target client reads. Codex has none.
- Determinism contract: every generated file must regenerate byte-identical
  so drift detection hashes the *expected* bytes.

## Decision Outcome

Add `CodexVendor` and a `ClientTarget::Codex` arm. Three sub-decisions:

### 1. Mapping decisions

- **Skill → native, verbatim.** Empty vendor registries ⇒ the universal
  agentskills render (identical to OpenCode/Copilot); a plain skill installs
  byte-identical. Target dir is the cross-vendor `.agents/skills` standard,
  rooted on the **workspace** (project) or **`$HOME`** (global) — *not*
  `$CODEX_HOME`. Two new `PathAnchor`s: `AgentsSkills`
  (`$HOME/.agents/skills`) and `CodexRoot` (`$CODEX_HOME` else `~/.codex`).
- **Agent → native TOML transform.** Codex is the **first TOML-emitting
  vendor**. `agent_index` serializes a flat `CodexAgent` struct with the
  `toml` crate (already a dependency) — never hand-rolled strings — for
  correct multi-line-string escaping and deterministic struct-field key
  order. Provenance is a TOML `#` comment (`toml_provenance`), since the
  shared HTML-comment `provenance` is invalid in TOML. `tools` has no Codex
  equivalent and is dropped with a warning. An optional `codex.*` agent
  metadata registry (`codex.model`, `codex.reasoning-effort`,
  `codex.sandbox-mode`) lets authors set Codex-only knobs; `codex.model`
  overrides the projected common `model` silently (the documented escape
  hatch). `codex` is registered in `render::KNOWN_NAMESPACES`.
- **Rule → deferred (warn + skip).** No inert files written.

### 2. The `Vendor::supports_kind` gate

The `Vendor` trait previously forced all three kinds and the installer always
materialized — there was no "this vendor declines this kind" concept. Add:

```rust
fn supports_kind(&self, _kind: ArtifactKind) -> bool { true }
```

`CodexVendor` overrides it to return `false` for `ArtifactKind::Rule`. The
installer's per-client loop skips a declining client (warn, no dest, no
materialize, no `ClientOutput`); `report_target` selects the first
kind-supporting client. A rule whose only selected client is Codex records
**zero outputs** but still declares the artifact in lock/state — honest
"installed nothing here" over a silent inert file. `candidate_anchors`'s
`(Codex, Rule)` arm is `unreachable!()` (the gate fires before anchoring),
matching the existing `Bundle`-arm convention.

### 3. Rules-deferred rationale

A grim rule's whole purpose is path-scoped instruction separation. Codex
offers no faithful mechanism (no globs/`applyTo`; hooks rejected upstream).
Writing an always-on AGENTS.md-style file would silently change a
path-scoped rule into a global one — a correctness lie. Skipping with a
warning is the honest behavior. Revisit if Codex adopts the proposed
`globs:` frontmatter.

## Considered Options (rule handling)

### Option A: Skip rules with a warning (chosen)

| Pros | Cons |
|------|------|
| No silent semantic change (path-scoped → global) | A Codex-only rule install writes nothing |
| No inert files to reap | Asymmetric with the other three vendors |
| `supports_kind` gate is reusable for any future decline | New trait method to maintain |

### Option B: Materialize rules as an always-on AGENTS.md fragment

| Pros | Cons |
|------|------|
| Symmetric: every vendor takes every kind | Silently drops `paths:` scoping — a correctness lie |
| No new trait method | Inert/duplicative file; unclear reap semantics |

## Consequences

- `ClientTarget::ALL` is now length 4; detection, the TUI display, and the
  publish-time `validate_*` loops pick up Codex automatically.
- `RenderError` gains a `Serialization` variant so the TOML emit path stays
  panic-free (no `.expect()` in library code) — unreachable in practice for
  the flat string table.
- `AnchorRoots` gains `agents_skills` + `codex_root` fields; the
  anchor-remainder table and `from_target`/`resolve` coherence test extend to
  Codex (skill → `AgentsSkills`, agent → `CodexRoot`).

## Related

- [adr_tool_namespaced_metadata_rendering.md](./adr_tool_namespaced_metadata_rendering.md) — per-vendor `Vendor` trait, namespaced metadata projection
- [adr_agent_artifact_kind.md](./adr_agent_artifact_kind.md) — agent kind, common-field projection, vendor override hatch
- [adr_install_state_portability.md](./adr_install_state_portability.md) — `PathAnchor` set this ADR extends

## Follow-ups (deferred review findings)

Surfaced by the max-tier `/swarm-review` of the implementing commit. Each is
deferred (not a v1 blocker); track before the vendor surface is widely
published. Fixed in the implementing commit: the zero-output no-op (the gate
no longer short-circuits an empty-outputs record) and the phantom report
target (`ArtifactInstall.target` / `InstallEntry.target` are now
`Option<PathBuf>`).

- **`supports_kind` bool → `KindSupport{Native,Degraded,Declined}`.** The bool
  cannot model Copilot's "supported-but-inert" global rule (a third state
  currently a hard-coded branch in `installer.rs`). A tri-state, scope-aware
  `support(kind, scope)` would fold the Codex-rule skip and the Copilot inert
  warning into one data-driven seam. Worth its own ADR amendment.
- **Derive `KNOWN_NAMESPACES` from `ClientTarget::ALL`.** Today it is a
  hand-maintained literal in `render.rs` — the one non-compile-forced edit a
  new vendor needs; forgetting it silently passes the vendor's own metadata
  through unprojected. Derive from `ALL.map(|c| c.vendor().name())`.
- **Correct the "one struct + one enum arm" invariant.** Adding Codex touched
  ~9 sites (anchors, namespaces, gate, fields); most are compile-forced, but
  the slogan in the `vendor.rs` module doc and the Design section above
  overstates the cost. Reword to the honest contract.
- **Freeze the one-way-door strings before publish.** The serialized anchor
  tags (`agents-skills`, `codex-root`) persist into every install-state file
  and the `codex.*` knob names are an authoring format — both costly to rename
  later. Decide consciously whether `agents-skills` is a shared cross-vendor
  anchor (keep generic) or Codex-specific (rename for symmetry).
- **Robustness / defense-in-depth.** `candidate_anchors` `(Codex,Rule)` uses
  `unreachable!()`; persisted/forged state should surface a typed anchor error
  instead of a panic. `ClientTarget::materialize` could enforce `supports_kind`
  at the method boundary rather than relying solely on the installer gate.
- **Pre-existing (not Codex-specific).** The integrity gate compares recorded
  outputs' intactness but never the *selected* vs *recorded* client set, so
  growing the selection can no-op for the new client; and direct (non-bundle)
  binding names from local `grimoire.toml` are not charset-validated before
  path construction. Both predate this change and affect all vendors.
