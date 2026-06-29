# Agent Spec

You loaded this file because you are authoring or fixing a grim agent —
a single `.md` defining a delegatable assistant — for `grim build` or
`grim release`.

Contents: [The #1 Pitfall](#the-1-pitfall) · [File Shape](#file-shape) ·
[Frontmatter](#frontmatter) · [Vendor Overrides](#vendor-overrides) ·
[Per-Client Emit](#per-client-emit) · [Limitations](#limitations) ·
[Minimal Example](#minimal-example) · [Validation Pitfalls](#validation-pitfalls)

## The #1 Pitfall

**`--kind agent` is required at build and release:**

```sh
grim build ./reviewer.md --kind agent
grim release ./reviewer.md ghcr.io/acme/reviewer:1.0.0 --kind agent
```

A bare `.md` path is indistinguishable from a rule by shape, and grim
never guesses from content — without the flag your agent **silently
packs as a rule**. This is not an error; the only signal is a warning
when a rule carries both `name` and `description`. Consumers need no
flag: `grim add` infers the kind from the published `artifactType`.

## File Shape

One `.md` file. Unlike rules, frontmatter is **required** — every client
needs at least a `description` to route work to the agent. The body
below the frontmatter is the system prompt and installs verbatim for
every client.

## Frontmatter

| Field | Required | Notes |
|---|---|---|
| `name` | yes | Must equal the file stem (`reviewer.md` → `name: reviewer`); standard name rules |
| `description` | yes | When a client should delegate to this agent |
| `model` | no | Passed through verbatim — **no alias translation** between clients |
| `tools` | no | Comma-separated allowlist, projected per client (string vs. list) |
| `metadata` | no | Catalog keys (`summary`, `keywords`, `repository`, `deprecated`) **plus** vendor keys — agent catalog metadata lives inside `metadata`, like a skill |

## Vendor Overrides

`model` and `tools` are *defaults*. When a vendor key lifts to the same
native field, the vendor key wins **for that vendor — silently**; the
collision is the documented escape hatch:

```yaml
model: sonnet
metadata:
  claude.model: opus                            # Claude gets opus
  opencode.model: anthropic/claude-sonnet-4-5   # OpenCode gets this
```

This matters most for `model`: Claude reads aliases like `sonnet`, while
OpenCode expects `provider/model-id`. Set `opencode.model` whenever the
common value is not OpenCode-shaped. Everything one vendor understands
(`claude.permission-mode`, `opencode.temperature`, `copilot.tools`, …)
is a string key in `metadata` — registries are linked from
[vendor-metadata.md](vendor-metadata.md).

## Per-Client Emit

The canonical format *is* Claude Code's native subagent format: a plain
agent (no vendor keys) installs for Claude byte-identical. OpenCode
drops `name` (filename is its identity) and `tools`; Copilot drops
`model` and emits `tools` as a YAML list; both add a provenance comment.
Full matrix: [emit matrix][emit-matrix].

## Limitations

- **No object-valued vendor fields** — `metadata` is string-valued, so
  Claude's `mcpServers`/`hooks`, OpenCode's `permission`, and Copilot's
  `mcp-servers` cannot be authored; add them post-install.
- **No support directory** — an agent packs to exactly one `<name>.md`;
  a sibling folder sharing the stem is **silently ignored**.
- **No model translation** — see vendor overrides above.

## Minimal Example

```yaml
# reviewer.md
---
name: reviewer
description: Reviews a diff for correctness, style, and missing tests.
---

You are a code reviewer. Examine the diff...
```

## Validation Pitfalls

| Pitfall | Outcome |
|---|---|
| Forgetting `--kind agent` | **Not an error** — packs as a rule; grim warns only that the rule looks agent-shaped |
| No frontmatter at all | Hard error, exit 65 — frontmatter is required for agents |
| Missing `name` or `description` | Hard error, exit 65 — frontmatter parse |
| `name` ≠ file stem | Hard error, exit 65 — name mismatch |
| Known vendor key, bad literal (`claude.permission-mode: yolo`) | Hard error, exit 65 — publish stops |
| Typo'd own-namespace key (`opencode.temprature`) | Warning + dropped |
| Sibling dir sharing the stem | Silently ignored — agents have no support dir |
| Vendor key shadowing a common field | Silent override per vendor — a feature, but a surprise when unintended |
| `repository` not `https://` | Hard error, exit 65 |

## Further Reading

- [Agent Artifacts][agents-doc] — canonical format, locations, consuming.
- [Agent schema][artifacts-agents] — the authoritative field table.
- [Override precedence][precedence] — the shadow semantics in full.
- [Agent vendor registries][registries] — every projectable key per vendor.
- [Catalog metadata for agents][pub-agent] — `metadata` map placement.

[agents-doc]: https://michael-herwig.github.io/grimoire/agents.html
[artifacts-agents]: https://michael-herwig.github.io/grimoire/artifacts.html#agents
[precedence]: https://michael-herwig.github.io/grimoire/agents.html#override-precedence
[emit-matrix]: https://michael-herwig.github.io/grimoire/agents.html#emit-matrix
[registries]: https://michael-herwig.github.io/grimoire/vendor-metadata.html#claude-agent-registry
[pub-agent]: https://michael-herwig.github.io/grimoire/publishing.html#metadata-agent
