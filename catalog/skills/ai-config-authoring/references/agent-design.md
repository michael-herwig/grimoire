# Agent Design

You loaded this file because you are defining a subagent, tuning
delegation between agents, or deciding whether a task deserves an agent at
all.

Contents: [When a Subagent Earns Its Cost](#when-a-subagent-earns-its-cost) ·
[Definition File](#definition-file) ·
[Description-Driven Delegation](#description-driven-delegation) ·
[Tools and Models](#tools-and-models) ·
[What Does Not Transfer](#what-does-not-transfer) ·
[Minimal Anchored Preamble](#minimal-anchored-preamble) ·
[Orchestration Anti-Patterns](#orchestration-anti-patterns) ·
[Portability](#portability)

## When a Subagent Earns Its Cost

Subagents buy **context isolation**: the orchestrating session pays only
for the returned summary while the agent burns its own window. The price
is amnesia and multiplication — a subagent starts fresh (no conversation
history, no already-read files), summaries are lossy by design, and
multi-agent systems have measured ~15x the token use of a single chat
(as of 2026; re-verify). Reach for a subagent for context-heavy research,
parallel workstreams, least-privilege tool sets, or per-role model
selection. If the task merely needs occasional knowledge, a skill is far
cheaper.

## Definition File

An agent definition is a Markdown file — frontmatter plus a body that
becomes the agent's system prompt. Across clients, only `name` and
`description` are conceptually required; everything else is vendor
surface. Keep the body focused: role, method, output contract.

## Description-Driven Delegation

The orchestrator chooses agents by `description` — it is the delegation
routing surface, exactly like a skill description is the trigger surface.
State *when* to delegate, in third person, with concrete trigger
keywords. Phrases like "use proactively" or "use immediately after
writing code" measurably raise delegation rates (vendor-documented). The
same craft rules apply as for skills — triggering conditions, never a
summary of the agent's procedure; see
[descriptions.md](descriptions.md).

## Tools and Models

**Minimize tools deliberately.** On major clients, omitting the tool list
means the agent inherits *everything*. Grant the narrowest set the role
needs — a reviewer needs read and search, not write and execute. Least
privilege is also a context win: fewer tools means fewer schemas loaded
and fewer wrong turns available.

**Route models by role.** Cheap, fast models for exploration and
retrieval; mid-tier for implementation and review; top-tier for
architecture and judgment. In published packs, prefer "inherit" or model
aliases over pinned model IDs — pinned names age badly.

## What Does Not Transfer

A spawned agent does not see the parent conversation, the files the
parent read, or the session's dynamic state. Worse, the surrounding
config layer thins out: skills showed **0% auto-activation inside
spawned agents** in published tests (as of 2026; re-verify), and scoped
rules do not fire inside them either. Whatever the agent must know:
put it in the definition body, preload it explicitly (where the client
supports attaching skills), or pass it in the delegation prompt.

A good delegation prompt has four parts: **objective, output format,
tool guidance, task boundaries.** Vague prompts measurably produce
duplicated investigation and over-spawning.

## Minimal Anchored Preamble

Do not paste rulebooks into agent bodies. Duplicated rules drift from
their source, and nobody notices until behavior diverges. Instead:

1. Point at the project's catalog/index file for the full rule map.
2. Inline at most ~5 critical invariants the agent must never violate.
3. Tag each inlined anchor with its source file, so drift is visible at
   review time.

This keeps the definition short, keeps a single source of truth, and
still puts the non-negotiables in front of an agent whose context starts
empty.

## Orchestration Anti-Patterns

| Anti-pattern | Consequence |
|---|---|
| Multi-agenting tightly coupled work | Most coding tasks have tight inter-step dependencies — parallel agents add cost without parallelism gains |
| Parallel writers touching the same files | Conflicting edits and chaos; parallelize reads, serialize writes |
| Vague delegation prompts | Duplicated investigation, over-spawning, agents doing each other's work |
| Chasing every reviewer finding | A reviewer prompted to find gaps will report some even when the work is sound; fix what matters, defer the rest |
| Deep nesting | Nesting support is vendor-specific and in flux (as of 2026); design for one level of delegation |
| Treating the summary as lossless | Summaries are condensed by design; pull primary artifacts (files, diffs) when fidelity matters |

## Portability

Agents are the **least portable** artifact type — four incompatible
envelopes (as of 2026):

| Client | Path | Format | Notes |
|---|---|---|---|
| Claude Code | `.claude/agents/*.md` | Markdown | Richest field set; tools inherit-all when omitted |
| OpenCode | `.opencode/agents/*.md` | Markdown | Primary/subagent modes; per-tool permission map |
| Copilot | `.github/agents/*.agent.md` | Markdown | Body capped at 30,000 chars (as of 2026; re-verify) |
| Codex | `.codex/agents/<name>.toml` | TOML | Body in `developer_instructions`; `tools` dropped with warning |

The only cross-read: VS Code Copilot also detects `.claude/agents/*.md`.
OpenCode and Codex read neither foreign path. Portable strategy: keep the
prompt body vendor-neutral and generate the per-client envelopes; only
`description` is conceptually common to all four.

## Further Reading

- [Claude Code: sub-agents][cc-agents] — frontmatter schema, delegation
  mechanics, "use proactively" guidance, tool inheritance.
- [OpenCode: agents][oc-agents] — primary vs subagent tiers, the
  per-agent permission matrix.
- [Copilot: custom agents configuration][cop-agents] — the `.agent.md`
  schema, tool aliases, body cap.
- [VS Code: custom agents][vsc-agents] — the chatmode-to-agent migration
  and the `.claude/agents/` cross-read.
- [How a multi-agent research system was built][mars] — orchestrator-worker
  pattern, delegation prompt anatomy, the 15x token figure, when
  multi-agent is wrong.
- [Effective context engineering for AI agents][ctx] — subagents as
  context sandboxes returning distilled summaries.
- [Writing tools for agents][tools] — description-as-prompt-surface
  craft that transfers directly to agent descriptions.

[cc-agents]: https://code.claude.com/docs/en/sub-agents
[oc-agents]: https://opencode.ai/docs/agents/
[cop-agents]: https://docs.github.com/en/copilot/reference/custom-agents-configuration
[vsc-agents]: https://code.visualstudio.com/docs/agent-customization/custom-agents
[mars]: https://www.anthropic.com/engineering/multi-agent-research-system
[ctx]: https://www.anthropic.com/engineering/effective-context-engineering-for-ai-agents
[tools]: https://www.anthropic.com/engineering/writing-tools-for-agents
