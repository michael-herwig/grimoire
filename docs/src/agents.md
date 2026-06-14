# Agent Artifacts

Skills teach an agent a capability and rules constrain it; an **agent
artifact** defines an agent itself — a named, delegatable assistant with
its own system prompt, model, and tool access.

Every major AI client has grown such a definition format: [Claude Code
subagents][claude-subagents-docs], [OpenCode agents][opencode-agents-docs],
[Copilot CLI custom agents][copilot-agents-docs], and [OpenAI Codex
agents][codex-subagents-docs]. Three of the four read a Markdown file with
YAML frontmatter whose body is the system prompt — but each with its own
field names, its own directory, and its own quirks. Codex takes a different
path entirely: it reads TOML. Teams end up copy-pasting near-identical
agent files between repositories and editing four variants by hand.

Grimoire treats an agent like any other artifact: author **one canonical
file**, publish it once, and let `grim install` project it into each
client's native format — the same model that powers
[vendor-specific metadata][vendor-metadata] for skills and rules.

## The canonical format {#format}

An agent is a single `.md` file. Unlike a [rule](./concepts.md), the
frontmatter is **required** — every client needs at least a description
to route work to the agent:

```yaml
# code-reviewer.md
---
name: code-reviewer
description: Reviews diffs for correctness, security, and style.
model: sonnet
tools: Read,Grep,Bash
metadata:
  summary: Multi-pass diff reviewer
  keywords: review,quality
  claude.memory: project
  opencode.mode: subagent
  opencode.temperature: "0.2"
---
You are a code reviewer. Analyze the diff and report specific,
actionable findings.
```

The body below the frontmatter is the agent's system prompt and installs
verbatim for every client.

### Common fields {#common-fields}

| Field | Required | Type | Validation |
|---|---|---|---|
| `name` | yes | string | Must equal the file stem (`code-reviewer.md` ⇒ `code-reviewer`); lowercase letters, digits, hyphens |
| `description` | yes | string | Free text — when a client should delegate to this agent |
| `model` | no | string | Passed through verbatim to each client; **no alias translation** |
| `tools` | no | string | Comma-separated tool list, projected into each client's native shape |
| `metadata` | no | string→string map | Catalog keys (`summary`, `keywords`) plus [vendor-namespaced keys][vendor-metadata] (`<vendor>.<field>`) |

The name-equals-stem rule exists because [OpenCode][opencode-agents-docs]
derives an agent's identity from its filename; Grimoire enforces the rule
for every client so the identity is consistent everywhere.

Everything a single vendor understands — Claude's `permissionMode`,
OpenCode's `temperature`, Copilot's tool restrictions — is authored as a
`<vendor>.<field>` string key inside `metadata`. The full key tables live
in the [vendor metadata reference][vendor-agent-registries].

### Override precedence {#override-precedence}

The common `model` and `tools` fields are *defaults*. When a vendor key
lifts to the same native field, the vendor key **wins for that vendor** —
silently, because the collision is the documented escape hatch:

```yaml
model: sonnet
metadata:
  claude.model: opus                       # Claude installs model: opus
  opencode.model: anthropic/claude-sonnet-4-5  # OpenCode gets this instead of "sonnet"
```

This matters most for `model`: [Claude Code][claude-subagents-docs] reads
aliases like `sonnet`, while [OpenCode][opencode-agents-docs] expects a
`provider/model-id` string. Grimoire deliberately does **not** translate
between the two — set `opencode.model` when the common value is not what
OpenCode needs.

## What each client receives {#emit-matrix}

On install, grim projects the canonical file per client:

| Canonical field | [Claude Code][claude-subagents-docs] | [OpenCode][opencode-agents-docs] | [Copilot CLI][copilot-agents-docs] | [Codex][codex-subagents-docs] |
|---|---|---|---|---|
| `name` | kept | **dropped** (filename is the identity) | kept | kept (`name` key in TOML) |
| `description` | kept | kept | kept | kept (`description` key in TOML) |
| `model` | kept | kept (see [precedence](#override-precedence)) | **dropped** (no documented field) | kept (optional `model` key in TOML) |
| `tools` | kept (comma string) | **dropped** (deprecated upstream) | emitted as a YAML **list** | **dropped with warning** (no Codex equivalent) |
| plain `metadata` / unknown keys | kept | dropped | dropped | dropped |
| body | verbatim | verbatim | verbatim | `developer_instructions` key in TOML |
| output format | Markdown + YAML frontmatter | Markdown (no frontmatter) | Markdown + YAML frontmatter | **TOML** (`<name>.toml`) |
| provenance comment | none | yes | yes | none |

The canonical format **is** Claude Code's native subagent format, so a
plain agent — one with no `<vendor>.<field>` metadata keys — installs for
Claude byte-identical to the published file (`generated: false`). The
OpenCode and Copilot files are always generated transforms and carry a
provenance comment; editing them by hand is detected as
[drift][vendor-drift], exactly like any generated file.

## Install locations {#locations}

**Project scope:**

| Client | Path |
|---|---|
| [Claude Code][claude-subagents-docs] | `.claude/agents/<name>.md` |
| [OpenCode][opencode-agents-docs] | `.opencode/agents/<name>.md` |
| [Copilot CLI][copilot-agents-docs] | `.github/agents/<name>.md` |
| [Codex][codex-subagents-docs] | `.codex/agents/<name>.toml` |

**Global scope** (native user-level discovery directories, honoring each
client's directory-override variable — the same resolution as
[skill discovery][vendor-discovery]):

| Client | Path | Env override |
|---|---|---|
| [Claude Code][claude-subagents-docs] | `~/.claude/agents/<name>.md` | `$CLAUDE_CONFIG_DIR/agents/` |
| [OpenCode][opencode-agents-docs] | `~/.config/opencode/agents/<name>.md` (XDG) | `$OPENCODE_CONFIG_DIR/agents/` |
| [Copilot CLI][copilot-agents-docs] | `~/.copilot/agents/<name>.md` | `$COPILOT_HOME/agents/` |
| [Codex][codex-subagents-docs] | `~/.codex/agents/<name>.toml` | `$CODEX_HOME/agents/` |

Unlike global rules, Copilot agents have a real user-level home — no
inert-install warning applies.

## Publishing {#publishing}

`grim build` and `grim release` need `--kind agent` for an agent file:

```sh
grim build ./code-reviewer.md --kind agent
grim release ./code-reviewer.md ghcr.io/acme/code-reviewer:1.0.0 --kind agent
```

The flag is required because a bare `.md` path is indistinguishable from a
[rule](./publishing.md) by shape — and rules accept arbitrary frontmatter,
so guessing from content would silently flip kinds. When a file released
as a rule carries both `name` and `description`, grim warns that it looks
like an agent definition.

Publishing runs the same gate as skills and rules: every
`<vendor>.<field>` metadata key is validated against the vendor
registries, and an invalid literal (say `claude.permission-mode: yolo`)
fails the release with exit 65 before anything reaches the registry. The
artifact publishes with `artifactType`
`application/vnd.grimoire.agent.v1`, so [`grim add`](./commands.md#add)
infers the kind with no flag.

Catalog metadata (`summary`, `keywords`) is authored in the `metadata`
map, like a skill — see [catalog metadata](./publishing.md#metadata).

## Consuming {#consuming}

Agents ride the standard lifecycle. Declarations live in an `[agents]`
table of `grimoire.toml`; the lock carries `[[agent]]` entries; and
[bundles](./concepts.md#bundles) accept agent members alongside skills
and rules:

```sh
grim add ghcr.io/acme/code-reviewer:1     # kind inferred from artifactType
grim install                               # projects into every selected client
grim status                                # shows the agent row
grim uninstall agent code-reviewer         # removes files + declaration
```

## Limitations {#limitations}

- **Object-valued vendor fields** cannot be authored: the `metadata` map
  is string-valued by the agentskills contract, so Claude's `mcpServers`
  and `hooks`, OpenCode's `permission`, and Copilot's `mcp-servers` are
  not projectable. Add them by editing the installed file (Claude/Copilot)
  or the client's own config.
- **No support directory.** An agent packs to exactly one `<name>.md`; a
  sibling folder sharing the stem is ignored (unlike
  [rules](./concepts.md#rule-support-dir)).
- **No model translation.** The common `model` passes through verbatim;
  use `opencode.model` when the OpenCode side needs a
  `provider/model-id` value.
- **[Codex][codex-subagents-docs] `tools` field dropped.** [Codex][codex-subagents-docs]
  has no native equivalent for the `tools` field. When installing for
  [Codex][codex-subagents-docs], grim drops `tools` entirely and emits a
  warning. This is a hard drop, not a projection — no Codex TOML key
  carries the value.
- **[Codex][codex-subagents-docs] agents are TOML, not Markdown.** The
  installed file at `.codex/agents/<name>.toml` (or
  `$CODEX_HOME/agents/<name>.toml` globally) is a TOML document, not a
  Markdown file. The canonical agent body lands in the
  `developer_instructions` key; `name`, `description`, and optionally
  `model` are top-level TOML keys. Vendor-namespaced `codex.*` keys in
  `metadata` (e.g., `codex.reasoning-effort`, `codex.sandbox-mode`) lift
  to their native TOML counterparts.

<!-- external -->
[claude-subagents-docs]: https://code.claude.com/docs/en/sub-agents
[opencode-agents-docs]: https://opencode.ai/docs/agents/
[copilot-agents-docs]: https://docs.github.com/en/copilot/concepts/agents/copilot-cli/about-custom-agents
[codex-subagents-docs]: https://developers.openai.com/codex/subagents

<!-- internal -->
[vendor-metadata]: ./vendor-metadata.md
[vendor-agent-registries]: ./vendor-metadata.md#claude-agent-registry
[vendor-discovery]: ./vendor-metadata.md#discovery-locations
[vendor-drift]: ./vendor-metadata.md#drift
