---
name: grim-authoring
description: Author, validate, and package grim-publishable artifacts — skill directories, rule files, agent definitions, and bundle TOMLs. Use when creating or editing an artifact for grim build or grim release; when choosing frontmatter or catalog metadata fields; when adding claude, opencode, or copilot vendor keys; or when grim build fails validation with exit code 65.
license: Apache-2.0
compatibility: grim>=0.6
metadata:
  summary: Deep authoring guide for grim skill, rule, agent, and bundle artifacts
  keywords: grim,grimoire,authoring,frontmatter,validation,vendor-metadata,skill,rule,agent,bundle,packaging
  repository: https://github.com/michael-herwig/grimoire
---

# Grim Artifact Authoring

Grim publishes four artifact kinds to OCI registries. Each has its own
source shape, frontmatter schema, and validation gates. This root file
holds the invariants that apply to every kind; per-kind depth lives in
`references/`, loaded via the routing table below.

## The Four Kinds

`grim build` and `grim release` infer the kind from the path — except
agents, which always need `--kind agent` or they silently pack as rules.

| Kind | Source shape | Inference | Installs as |
|---|---|---|---|
| Skill | Directory with a `SKILL.md` index | directory → skill | Directory tree under the client's `skills/` dir |
| Rule | Single `.md` file | `.md` → rule | `rules/<name>.md`, per-client transform |
| Rule + support dir | `<name>.md` + sibling `<name>/` dir | sibling dir auto-discovered | Index file + `rules/<name>/…` side by side |
| Agent | Single `.md`, frontmatter required | **never — `--kind agent` mandatory** | One agent file per client, per-client render |
| Bundle | `.toml` member list | `.toml` → bundle | Never materializes — expands to its members |

## Universal Invariants

- Names use `[a-z0-9-]` only — non-empty, ≤ 64 chars, no leading or
  trailing hyphen, no consecutive hyphens (`a--b` is invalid).
- A skill's `name` must equal its directory name; an agent's `name` must
  equal its file stem. Rule and bundle names come from the file stem and
  obey the same character rules.
- Any violation fails `grim build`/`grim release` with exit code 65.
- Unknown top-level frontmatter keys are *preserved* round-trip (forward
  compatibility) — never rejected, so a typo'd optional key is silent.

## The Metadata-Location Asymmetry

Where catalog metadata (`summary`, `keywords`, `repository`, `deprecated`)
is authored differs by kind. This is the #1 authoring confusion — misplaced
keys are not errors, they just silently never reach the catalog:

| Kind | `summary` / `keywords` / `repository` / `deprecated` live… |
|---|---|
| Skill | inside the `metadata:` map of `SKILL.md` frontmatter |
| Agent | inside the `metadata:` map of the agent frontmatter |
| Rule | at the **top level** of the rule frontmatter (not in `metadata`) |
| Bundle | as **top-level TOML keys**, above the member tables |

In every kind, `keywords` is one comma-separated string and `repository`
must be an `https://` URL (anything else fails the release with 65). The
`deprecated` notice (grim 0.6.x) obeys the same per-kind location; an
empty or whitespace-only value means *not* deprecated and emits no
annotation — detail in [Publishing][publishing].

## Companion: Content Craft

This skill covers grim **packaging and validation** only — including opt-in
git provenance at build/release time (`--git`); confirm flags with
`grim release --help`. For the craft of
the content itself — progressive disclosure, context budgets, description
triggering, choosing skill vs rule vs agent — read the companion skill
`ai-config-authoring` at
[`../ai-config-authoring/SKILL.md`](../ai-config-authoring/SKILL.md);
both ship together in the `grim-essentials` bundle. When creating a new
artifact from scratch, read it FIRST — write good content, then package
it here. If that file is missing, install it by identifier:

```sh
grim add grim.ocx.sh/skills/ai-config-authoring:0 && grim install
# fresh project (no grimoire.toml yet): run `grim init` first
```

## Routing Table

| Read… | …when |
|---|---|
| [references/skill-spec.md](references/skill-spec.md) | Authoring a skill directory or its `SKILL.md` frontmatter |
| [references/rule-spec.md](references/rule-spec.md) | Authoring a rule file, its globs, or a support directory |
| [references/agent-spec.md](references/agent-spec.md) | Authoring an agent definition or its vendor overrides |
| [references/bundle-spec.md](references/bundle-spec.md) | Authoring a bundle TOML or choosing pinning strategy |
| [references/vendor-metadata.md](references/vendor-metadata.md) | Adding `claude.*` / `opencode.*` / `copilot.*` keys |
| [references/release-checklist.md](references/release-checklist.md) | Before `grim release`, or triaging an exit-65 failure |
| [references/updating.md](references/updating.md) | Maintaining this skill package itself |

## Schema Authority

This skill teaches the craft and the pitfalls; the authoritative schema
reference is the Grimoire docs site. When a field table here feels
incomplete, the docs page is the source of truth:
[Artifact Reference][artifacts] · [Vendor-Specific Metadata][vendor] ·
[Publishing][publishing] · [Agent Artifacts][agents].

## Verify Before Acting

`grim build <path>` validates without pushing — run it after every edit;
its output is ground truth for the grim version actually installed. On
any conflict between this skill and `grim build` output or `grim --help`,
trust the tool. Treat this skill as the map, not the territory.

---

Verified against grim 0.6.1.

[artifacts]: https://michael-herwig.github.io/grimoire/artifacts.html
[vendor]: https://michael-herwig.github.io/grimoire/vendor-metadata.html
[publishing]: https://michael-herwig.github.io/grimoire/publishing.html
[agents]: https://michael-herwig.github.io/grimoire/agents.html
