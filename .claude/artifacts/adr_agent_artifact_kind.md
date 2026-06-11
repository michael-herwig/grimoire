# ADR: Add the `agent` artifact kind with common-field projection and vendor overrides

## Metadata

**Status:** Accepted
**Date:** 2026-06-11
**Deciders:** Michael Herwig (maintainer)
**Tech Strategy Alignment:**
- [x] Decision follows Golden Path in `.claude/rules/product-tech-strategy.md`
      (extends the existing OCI artifact pipeline; no new dependencies)
**Domain Tags:** integration, api
**Supersedes:** N/A

## Context

Every supported client defines delegatable agents as a Markdown file with
YAML frontmatter whose body is the system prompt: Claude Code subagents
(`.claude/agents/`), OpenCode agents (`.opencode/agents/`), and Copilot
CLI custom agents (`.github/agents/`). The field sets diverge — Claude has
a rich camelCase surface (`permissionMode`, `maxTurns`, …), OpenCode uses
model-tuning fields (`temperature`, `mode`) and derives identity from the
filename, Copilot reads a small set with `tools` as a YAML list. Teams
copy near-identical files between repositories and clients with no
versioning or update path — exactly the problem Grimoire solves for
skills and rules (TODO.md "Support for Agents").

## Decision Drivers

- One canonical authored file, projected per client at install — reuse the
  proven `adr_tool_namespaced_metadata_rendering.md` machinery.
- Common attributes shared by all vendors belong in top-tier frontmatter
  (owner decision); vendor-unique capabilities stay namespaced metadata.
- Forward/backward compatibility: existing locks, declaration hashes, and
  agent-free configs must not change byte-wise.
- KISS/YAGNI: no model-alias translation tables, no object-valued
  metadata, no support directories until a real need appears.

## Decision Outcome

A fourth `ArtifactKind::Agent`:

1. **Canonical format** — single `.md`, frontmatter **required**:
   `name` (must equal the file stem — OpenCode's filename-as-identity,
   enforced for all clients), `description`, optional `model` and `tools`
   (comma string), `metadata` string map. Body = system prompt. Wire type
   `application/vnd.grimoire.agent.v1` (+ config media type fallback per
   `adr_oci_artifact_type.md`).
2. **Common-field projection with vendor override** — common fields are
   per-vendor defaults. Emit matrix: Claude keeps everything (canonical ==
   native; verbatim fast path when no tool keys); OpenCode drops
   `name`/`tools`, keeps `description`/`model`, always generated with a
   provenance header; Copilot keeps `name`/`description`, emits `tools` as
   a YAML list, drops `model`, always generated. A lifted
   `<vendor>.<field>` key whose native name collides with a projected
   common field (`claude.model`, `claude.tools`, `opencode.model`,
   `copilot.tools`) **silently overrides** it — the documented escape
   hatch (e.g. OpenCode's `provider/model-id` shape vs Claude aliases;
   grim does NOT translate model names). Any other collision keeps the
   existing override warning. Implemented as an `expected_overrides`
   parameter on `append_lifted` (`src/install/render.rs`); existing
   skill/rule call sites pass `&[]`.
3. **Kind detection stays shape-based** — `.md` without `--kind` is a
   rule; `--kind agent` is required for build/release. Frontmatter
   sniffing was rejected: rules are forward-compatible (arbitrary `extra`
   keys), so a rule carrying `name`+`description` is indistinguishable
   from an agent — magic detection would silently flip kinds. Mitigation:
   the rule path warns when frontmatter looks agent-shaped. `grim add`
   infers the kind from `artifactType`, no flag needed.
4. **Hash/lock compatibility, no version bump** — the declaration hash
   emits the `"agents"` JCS key only when non-empty (bundles precedent in
   `src/config/hash.rs`), so agent-free declarations hash identically to
   pre-agents ones; `DECLARATION_HASH_VERSION` stays 1, lock stays V1
   with an optional `[[agent]]` array. Trade-off (accepted, same as
   bundles): an *old* grim rejects an agent-bearing config/lock via
   `deny_unknown_fields`.
5. **Bundles accept agent members** — `[agents]` member table in bundle
   sources; expansion is kind-generic, member names still gate through
   `SkillName::parse` (CWE-22).
6. **v1 exclusions** — object-valued vendor fields (Claude
   `mcpServers`/`hooks`, OpenCode `permission`, Copilot `mcp-servers`)
   are not projectable (the metadata map is string-valued); no support
   directory (`pack_agent_file` emits exactly one `<name>.md`; a sibling
   dir is ignored — deliberately NOT reusing `pack_rule_file`).

New `FieldType` variants `Integer`, `Float`, `CommaList` support the
typed vendor registries (`CLAUDE_AGENT_FIELDS`, `OPENCODE_AGENT_FIELDS`,
`COPILOT_AGENT_FIELDS`). The `GrimoireLock::iter_artifacts()` helper is
the single skills→rules→agents chaining seam so a future kind cannot be
forgotten at individual call sites. The doc-parity test
(`docs_reference_matches_claude_registry`) now asserts the
skill-union-agent registry against `docs/src/vendor-metadata.md`.

### Consequences

**Positive:**
- One authored agent file serves three clients with native fidelity.
- Existing locks/configs/hashes untouched; idempotent re-release holds
  (deterministic annotations, no `created` timestamp).
- Global-scope installs land in native discovery dirs (`~/.claude/agents`,
  `~/.config/opencode/agents`, `~/.copilot/agents`) honoring
  `CLAUDE_CONFIG_DIR`/`OPENCODE_CONFIG_DIR`/`COPILOT_HOME` — unlike rules,
  Copilot agents have a real global home (no inert-install warning).

**Negative:**
- `--kind agent` is a foot-gun (an agent published as a rule installs to
  `rules/`); mitigated by the looks-like-an-agent warning and docs.
- Pass-through `model` can be wrong for OpenCode; mitigated by the
  documented `opencode.model` override.

**Risks:**
- Vendor formats drift (new frontmatter fields upstream) — registries are
  data tables; extending them is additive and the doc-parity test keeps
  the reference page honest.

## Links

- [adr_oci_artifact_type.md](./adr_oci_artifact_type.md) — wire-type contract
- [adr_tool_namespaced_metadata_rendering.md](./adr_tool_namespaced_metadata_rendering.md) — projection machinery this extends
- [adr_multifile_rules.md](./adr_multifile_rules.md) — support-dir contract agents deliberately do not adopt
- Vendor references: code.claude.com/docs/en/sub-agents, opencode.ai/docs/agents, docs.github.com Copilot CLI custom agents

---

## Changelog

| Date | Author | Change |
|------|--------|--------|
| 2026-06-11 | Michael Herwig (via Claude) | Initial decision record |
