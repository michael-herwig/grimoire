---
name: ai-config-authoring
description: Craft effective AI agent configuration — skills, rules, agents, and instructions — for any AI coding client. Use when writing or reviewing a SKILL.md, rule file, or agent definition; when deciding between a skill, rule, hook, or always-on instruction; when a config file grows past its context budget; or when a skill description fails to trigger.
license: Apache-2.0
metadata:
  summary: Vendor-neutral craft guide for authoring AI agent config
  keywords: authoring,skills,rules,agents,progressive-disclosure,context,descriptions,best-practices
  repository: https://github.com/grimoire-rs/grimoire
---

# AI Config Authoring

## Core Principle: Context Is a Budget

Every always-loaded line competes with the user's actual task for model
attention, and attention degrades as volume grows — bloated config makes
agents ignore the instructions that matter. Pay the always-on price only
for content needed every session; defer everything else behind on-demand
loading (progressive disclosure). For each line, apply the deletion test:
would removing it cause mistakes? If not, cut it.

## Budget Table

| Artifact | Budget | Cost is paid |
|---|---|---|
| Always-on instruction file | < 200 lines | Every session, every turn |
| Glob-scoped rule | < 200 lines each | Only while matching files are in play |
| Skill metadata (name + description) | ~100 tokens per skill | Every session, all skills |
| Skill body (SKILL.md) | < 500 lines / < 5k tokens | Only when the skill triggers |
| Skill bundled files | Effectively unlimited | Only when read or executed |
| Subagent | Isolated window | Separate budget; only its summary returns |
| Hook | Zero context | Never — scripts run outside the context |

## The Artifact-Type Landscape

The first matching row picks the type. Full comparison — vendor support,
failure modes, migration paths — in
[references/choosing-types.md](references/choosing-types.md).

| The content is... | Use |
|---|---|
| Mechanical, must happen 100% of the time, no judgment | Hook |
| Identity, commands, conventions relevant to every task | Always-on instruction file |
| A standard that applies while editing certain files | Glob-scoped rule |
| An occasional procedure or piece of domain knowledge | Skill |
| A side-effectful workflow to run only on explicit request | Manual-only skill |
| Context-heavy research, parallel work, separate privileges | Subagent |
| Something that must port across clients | Skill — the one open standard |
| Logic a machine can run rather than prose to read | Hook, or a script inside a skill |

## Root-as-Index Pattern

A root file is a table of contents, not a textbook: state the principle,
compress the comparison, route to depth one level down. This file is the
worked example — it stays inside the budgets it teaches, and every detail
lives in `references/`, loaded only when a row below matches your task.

## Routing Table

| Read... | ...when |
|---|---|
| [references/choosing-types.md](references/choosing-types.md) | Picking an artifact type, or migrating content between types |
| [references/skill-design.md](references/skill-design.md) | Writing or restructuring a SKILL.md and its bundled files |
| [references/rule-design.md](references/rule-design.md) | Writing always-on instructions or glob-scoped rules |
| [references/agent-design.md](references/agent-design.md) | Defining a subagent or designing delegation between agents |
| [references/descriptions.md](references/descriptions.md) | A skill or agent fails to trigger, or before finalizing any description |
| [references/guardrails.md](references/guardrails.md) | You want a copy-pastable always-on enforcement card |
| [references/checklist.md](references/checklist.md) | Reviewing a config package before publishing or installing it |
| [references/updating.md](references/updating.md) | Maintaining this guide itself — re-research protocol and search terms |

## Distributing Config

Config worth sharing across repositories belongs in a package manager,
not copy-paste — versioning, provenance, and an update path matter as
much for config as for code. This skill itself is distributed as an OCI
artifact via [grim][grimoire]. To package and publish your artifact with
grim — frontmatter schemas, validation, vendor metadata — read the
companion skill `grim-authoring` at
[`../grim-authoring/SKILL.md`](../grim-authoring/SKILL.md); both ship
together in the `grim-essentials` bundle. If that file is missing,
install it by identifier:

```sh
grim add grim.ocx.sh/skills/grim-authoring:0 && grim install
# fresh project (no grimoire.toml yet): run `grim init` first
```

## Further Reading

- [Agent Skills specification][spec] — the cross-vendor SKILL.md standard:
  frontmatter constraints, directory semantics, size guidance.
- [Skill authoring best practices][bp] — Anthropic's authoring guidance:
  descriptions, disclosure patterns, eval-first workflow, anti-patterns.
- [Effective context engineering for AI agents][ctx] — the theory behind
  every budget above: attention as a finite resource, just-in-time loading.
- [GitHub Copilot: about agent skills][cop] — Copilot's skill discovery
  paths and supported surfaces.
- [OpenCode skills documentation][oc] — OpenCode's skill discovery paths
  and activation model.

[grimoire]: https://github.com/grimoire-rs/grimoire
[spec]: https://agentskills.io/specification
[bp]: https://platform.claude.com/docs/en/agents-and-tools/agent-skills/best-practices
[ctx]: https://www.anthropic.com/engineering/effective-context-engineering-for-ai-agents
[cop]: https://docs.github.com/en/copilot/concepts/agents/about-agent-skills
[oc]: https://opencode.ai/docs/skills/
