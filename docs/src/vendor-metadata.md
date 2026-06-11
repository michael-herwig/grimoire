# Vendor-Specific Metadata

## Why tool keys live in metadata {#why-metadata}

A canonical `SKILL.md` obeys the [agentskills specification][agentskills-spec].
That format defines a fixed set of top-level fields — `name`, `description`,
`license`, `compatibility`, `allowed-tools` — and a `metadata` map of
string-valued key/value pairs for everything else.

Each client tool adds its own capability fields on top of those. [Claude
Code][claude-skills-docs] reads `user-invocable`, `effort`, `context`, and
others. [OpenCode][opencode-skills-docs] and [GitHub Copilot][copilot-instructions-docs]
read neither of those. If every client's fields lived at the top level,
the canonical artifact would violate the specification and become
unreadable to any other agentskills tooling.

The solution is to author capabilities as string-valued keys inside the
`metadata` map, namespaced by the target client. At install time grim
reads the registry for the target and converts each matching key to its
native YAML type, lifting it into the top-level frontmatter of the file
written to disk. The published artifact stays spec-compliant. Authors
maintain one `SKILL.md`.

## Common vs. vendor-unique capabilities {#common-vs-unique}

Not all capabilities need the `<vendor>.<field>` pattern. The authoring
convention follows one rule: a capability **common to several vendors** is
authored once as a canonical top-level frontmatter field and projected per
vendor; a capability **unique to one vendor** is authored as a
`<vendor>.<field>` string key inside the `metadata` map.

`paths` is the clearest example of a common capability: it is a scoping
concept every client understands, even if each client stores it differently
on disk. A rule author writes `paths:` once in canonical frontmatter.
[Claude Code][claude-memory-docs] receives it verbatim; [GitHub
Copilot][copilot-instructions-docs] receives it joined as a single
`applyTo:` string. The author does not repeat themselves.

`copilot.exclude-agent`, by contrast, is a Copilot-only concept. There is
no parallel field for other clients. It belongs in `metadata` under its
vendor namespace.

`keywords` and `summary` stay top-level in every kind (skills, rules,
bundles) — they are catalog fields shared by all clients and by grim's own
`grim search` display; they are not vendor-specific.

## Authoring example — skill {#authoring-example}

A skill intended for [Claude Code][claude-skills-docs] with a specific
effort and invocation mode looks like this:

```yaml
---
name: deep-review
description: A thorough security and correctness review.
metadata:
  keywords: review,security
  claude.user-invocable: "true"
  claude.effort: "high"
  claude.when-to-use: "when you want a thorough review of a pull request"
---
# Deep Review
…
```

The `metadata` map values are always strings — that is the agentskills
contract. grim converts them to native types at install time.

When grim installs this skill for [Claude Code][claude-code-docs], it
writes a `SKILL.md` whose frontmatter contains `user-invocable: true`
(a YAML bool), `effort: high`, and `when_to_use: "when you want a
thorough review of a pull request"` — native fields Claude reads.

When grim installs the same artifact for [OpenCode][opencode-skills-docs]
or [GitHub Copilot][copilot-instructions-docs], those tool-namespaced
keys are dropped and neither client receives them.

## Authoring example — rule {#rule-authoring-example}

A rule with [Copilot][copilot-instructions-docs]-specific behavior authored
alongside canonical `paths` scoping looks like this:

```yaml
---
paths: ["**/*.rs"]
keywords: rust,style
metadata:
  copilot.exclude-agent: code-review
---
```

`paths` is top-level (common capability). `copilot.exclude-agent` is
inside `metadata` (vendor-unique capability). A vendor-namespaced key
authored at the top level is **not projected** — publish emits a migration
warning:

```
top-level rule frontmatter key 'copilot.exclude-agent' is not projected;
author it inside 'metadata' instead
```

`keywords` and `summary` stay top-level even in rule frontmatter — they
are not vendor-specific.

## Projection semantics {#projection-semantics}

The projection rules are implemented in `src/install/render.rs` and apply
at both install time and publish-time validation.

| Input key | Outcome |
|---|---|
| Known `<target>.<field>` key — valid literal | Converted to native type, lifted to top-level frontmatter |
| Known `<target>.<field>` key — invalid literal | Hard error: publish fails (exit 65 DataError), install fails with MaterializeFailed |
| Unknown `<target>.<field>` key | Warning emitted, key dropped (typo guard) |
| Foreign-namespace key (e.g. `opencode.*` when rendering Claude) | Dropped silently |
| Plain metadata key (non-tool prefix, e.g. `vendor.x`) | Passes through unchanged |
| No tool-namespaced keys at all | Fast path: verbatim install, byte-identical to canonical |

The three recognized tool namespaces are `claude`, `opencode`, and
`copilot`. Any key whose prefix is not one of these three is plain
metadata and is never treated as a tool key.

When a namespaced key collides with a top-level key of the same name,
the namespaced key wins and a warning is emitted. This situation arises
when a legacy `SKILL.md` carries both a top-level field and the
namespaced form — the namespaced form is the authoritative value after
migration.

## The claude.* skill registry {#claude-registry}

The table below is the authoritative list of fields grim recognizes for
[Claude Code][claude-skills-docs]. Every row is a direct mapping from
the `CLAUDE_SKILL_FIELDS` constant in `src/install/vendor_claude.rs`.

| Key | Native field | Type | Notes |
|---|---|---|---|
| `claude.disable-model-invocation` | `disable-model-invocation` | bool | `"true"` or `"false"` only; other literals are a hard error |
| `claude.user-invocable` | `user-invocable` | bool | `"true"` or `"false"` |
| `claude.model` | `model` | string | |
| `claude.effort` | `effort` | enum | Accepted values: `low`, `medium`, `high`, `xhigh`, `max` |
| `claude.context` | `context` | enum | Accepted values: `fork` |
| `claude.agent` | `agent` | string | |
| `claude.argument-hint` | `argument-hint` | string | |
| `claude.when-to-use` | `when_to_use` | string | Note: the native key uses an underscore, not a hyphen |
| `claude.arguments` | `arguments` | string | |
| `claude.disallowed-tools` | `disallowed-tools` | string | |
| `claude.shell` | `shell` | enum | Accepted values: `bash`, `powershell` |
| `claude.paths` | `paths` | string | Comma-separated glob patterns |

`hooks` is not in this registry. It is an object-valued field that
cannot be expressed as a single string metadata value; a separate ADR
governs that surface.

## Agent common fields and override precedence {#agent-overrides}

Agents follow the same common-vs-unique rule with one addition. The
canonical agent frontmatter models four common fields — `name`,
`description`, `model`, `tools` — which grim projects per vendor (see
[Agent Artifacts][agents-doc] for the full emit matrix). Everything else
is a `<vendor>.<field>` metadata key.

Two of those vendor keys deliberately shadow a common field: when a
vendor's registry lifts a key to the same native name a projected common
field uses, the vendor key **overrides** the common value for that vendor
— silently, with no warning, because the collision is the documented
escape hatch. Example: `model: sonnet` plus `claude.model: opus` installs
`model: opus` for [Claude Code][claude-subagents-docs] while
[OpenCode][opencode-agents-docs] still receives the common `sonnet`.

## The claude.* agent registry {#claude-agent-registry}

The table below is the authoritative list of agent fields grim recognizes
for [Claude Code][claude-subagents-docs] subagents. Every row is a direct
mapping from the `CLAUDE_AGENT_FIELDS` constant in
`src/install/vendor_claude.rs`.

| Key | Native field | Type | Notes |
|---|---|---|---|
| `claude.model` | `model` | string | **Overrides** the common `model` field for Claude |
| `claude.tools` | `tools` | string | **Overrides** the common `tools` field for Claude (comma-separated string, Claude's native shape) |
| `claude.disallowed-tools` | `disallowedTools` | string | |
| `claude.permission-mode` | `permissionMode` | enum | Accepted values: `default`, `acceptEdits`, `auto`, `dontAsk`, `bypassPermissions`, `plan` |
| `claude.max-turns` | `maxTurns` | integer | |
| `claude.skills` | `skills` | comma list | Comma-separated string → YAML list |
| `claude.memory` | `memory` | enum | Accepted values: `user`, `project`, `local` |
| `claude.background` | `background` | bool | `"true"` or `"false"` |
| `claude.effort` | `effort` | enum | Accepted values: `low`, `medium`, `high`, `xhigh`, `max` |
| `claude.isolation` | `isolation` | enum | Accepted values: `worktree` |
| `claude.color` | `color` | enum | Accepted values: `red`, `blue`, `green`, `yellow`, `purple`, `orange`, `pink`, `cyan` |
| `claude.initial-prompt` | `initialPrompt` | string | |

`mcpServers` and `hooks` are not in this registry — both are object-valued
fields that cannot be expressed as a single string metadata value.

## The opencode.* agent registry {#opencode-agent-registry}

Unlike its empty skill registry, [OpenCode][opencode-agents-docs] has a
rich native agent frontmatter. Every row maps from the
`OPENCODE_AGENT_FIELDS` constant in `src/install/vendor_opencode.rs`.

| Key | Native field | Type | Notes |
|---|---|---|---|
| `opencode.model` | `model` | string | **Overrides** the common `model` field for OpenCode — the escape hatch when the common value is not `provider/model-id`-shaped |
| `opencode.mode` | `mode` | enum | Accepted values: `primary`, `subagent`, `all` |
| `opencode.temperature` | `temperature` | float | |
| `opencode.top-p` | `top_p` | float | Note: the native key uses an underscore |
| `opencode.steps` | `steps` | integer | Maximum agentic iterations |
| `opencode.prompt` | `prompt` | string | Custom system prompt reference |
| `opencode.disable` | `disable` | bool | |
| `opencode.hidden` | `hidden` | bool | |
| `opencode.color` | `color` | string | Hex color or theme name |

`permission` (an object) and the deprecated object-valued `tools` map are
not in this registry.

## The copilot.* agent registry {#copilot-agent-registry}

[GitHub Copilot CLI][copilot-agents-docs] custom agents recognize one
projectable vendor key, mapped from `COPILOT_AGENT_FIELDS` in
`src/install/vendor_copilot.rs`.

| Key | Native field | Type | Notes |
|---|---|---|---|
| `copilot.tools` | `tools` | comma list | **Overrides** the common `tools` field for Copilot; comma-separated string → YAML list |

`mcp-servers` (an object) is not in this registry.

## Empty registries for OpenCode and Copilot skills {#empty-registries}

The skill registries for [OpenCode][opencode-skills-docs] and [GitHub
Copilot][copilot-instructions-docs] are intentionally empty. Both tools
read only the universal agentskills fields from a `SKILL.md`; neither
has client-specific skill capabilities that need projection.

Any key prefixed with `opencode.` or `copilot.` in the `metadata` map
of a skill is therefore always unknown. grim emits a warning and drops
it when it encounters one. This behavior is the typo guard: if you
accidentally write `opencode.some-key`, you get a warning at publish
time rather than silent data loss.

Because both registries are empty, [OpenCode][opencode-skills-docs] and
[GitHub Copilot][copilot-instructions-docs] produce byte-identical rendered
skill files — the *unified universal render*. A skill installed by grim
for [Claude Code][claude-code-docs] is also discovered by both other
tools, which ignore the lifted Claude fields as unknown keys. This means
installing for Claude effectively covers all three clients for skill
discovery, with no extra work for authors.

## Skill discovery locations {#discovery-locations}

grim installs skills into the directories each client scans for
`SKILL.md` files.

**Project scope** (per-workspace, discovered by all three clients):

| Client | Directory |
|---|---|
| [Claude Code][claude-code-docs] | `.claude/skills/<name>/` |
| [GitHub Copilot][copilot-skills-docs] | `.github/skills/<name>/`, `.claude/skills/<name>/`, `.agents/skills/<name>/` |
| [OpenCode][opencode-skills-docs] | `.opencode/skills/<name>/`, `.claude/skills/<name>/`, `.agents/skills/<name>/` |

**Global scope** (user-level; grim installs directly into each client's
native discovery directory, honoring the client's own directory-override
environment variable):

| Client | Directory | Env override |
|---|---|---|
| [Claude Code][claude-code-docs] | `~/.claude/skills/<name>/` | `$CLAUDE_CONFIG_DIR/skills/<name>/` — the variable replaces the entire `~/.claude` tree ([claude-directory reference][claude-dir-docs]) |
| [GitHub Copilot][copilot-skills-docs] | `~/.copilot/skills/<name>/` | `$COPILOT_HOME/skills/<name>/` — the variable replaces the entire `~/.copilot` path ([Copilot CLI config-dir reference][copilot-config-dir-docs]) |
| [OpenCode][opencode-skills-docs] | `~/.config/opencode/skills/<name>/` (or `$XDG_CONFIG_HOME/opencode/skills/<name>/`) | `$OPENCODE_CONFIG_DIR/skills/<name>/` — OpenCode's *additive* scan directory ([OpenCode config docs][opencode-config-docs]): the XDG default stays scanned either way; grim prefers the override as install target when set. `$OPENCODE_CONFIG` (a config *file* path) does not affect skill discovery and plays no role here |

When neither the override variable nor `$HOME` can be resolved (rare CI
environments), grim falls back to the workspace layout under `$GRIM_HOME`
for the affected client.

[GitHub Copilot][copilot-skills-docs] skills install natively to
`~/.copilot/skills` per the [Copilot CLI add-skills][copilot-skills-docs]
documentation. Global **rules** for [GitHub Copilot][copilot-skills-docs]
have no documented user-level instructions path; grim writes them under
the workspace layout and emits a warning at install time.

## Rule-level vendor keys {#rule-keys}

Rule frontmatter is distinct from the agentskills `metadata` map. The
canonical structure follows the common-vs-unique principle: the `paths`
field is top-level (common across multiple clients), and any
vendor-unique capability is authored inside a `metadata:` map under its
`<vendor>.<field>` namespace.

The mapping table for rules:

| Client | Field | Source | Output field | Notes |
|---|---|---|---|---|
| [Claude Code][claude-memory-docs] | `paths` | top-level | `paths` | Verbatim — no transform; Claude reads it directly |
| [GitHub Copilot][copilot-instructions-docs] | `paths` | top-level | `applyTo` | Comma-joined into a single string (Copilot does not accept a list) |
| [GitHub Copilot][copilot-instructions-docs] | `copilot.exclude-agent` | `metadata` | `excludeAgent` | Enum: `code-review` or `cloud-agent` (registry in `src/install/vendor_copilot.rs`) |
| [OpenCode][opencode-rules-docs] | — | — | — | No per-file rule frontmatter; loading is registered via `opencode.json` |

A rule's `paths` list is native [Claude Code][claude-memory-docs]
frontmatter and passes through verbatim. For [GitHub
Copilot][copilot-instructions-docs], grim transforms the rule into a
`.instructions.md` file whose frontmatter maps `paths` to a single
comma-joined `applyTo:` string, then writes the body with a provenance
comment.

The optional `copilot.exclude-agent` key is authored inside the `metadata`
map (not top-level) and may take the values `code-review` or
`cloud-agent`. Any other value is a hard error at install and publish
time.

[OpenCode][opencode-rules-docs] has no per-file rule frontmatter. grim
writes the rule body (stripping the frontmatter) with a provenance comment,
and registers a managed glob in `opencode.json` so OpenCode loads it.

A rule with neither `paths` nor `copilot.exclude-agent` gets no frontmatter
block in its [Copilot][copilot-instructions-docs] transform.

## Claude rule install behavior {#claude-rule-install}

[Claude Code][claude-memory-docs] reads `paths:` natively in rule
frontmatter, so most rules install verbatim — the file written to disk
is byte-identical to the canonical source, and is recorded as
`generated: false`.

The exception is a rule that carries **tool-namespaced metadata keys**.
When grim detects any `<vendor>.<field>` entry inside the rule's
`metadata` map, it re-renders the rule for [Claude Code][claude-memory-docs]:

- Own-namespace keys (`claude.*`) are looked up in the [Claude Code][claude-code-docs]
  rule registry. That registry is empty today — unknown own-namespace keys
  warn and drop.
- Foreign-vendor keys (e.g. `copilot.exclude-agent`) drop silently.
- Plain metadata keys, `paths`, `keywords`, `summary`, and any
  forward-compat extras survive unchanged.

The written file carries `generated: true`. If the cleaned frontmatter
would be empty after this process, the frontmatter block is omitted
entirely. This behavior mirrors how rendered skills are handled and keeps
the installed file as clean as possible.

## OpenCode instructions registration {#opencode-registration}

[OpenCode][opencode-config-docs] loads instruction files through its
`instructions` config array. grim manages exactly one entry in that array —
a glob pointing at the directory where it writes rules — and keeps it in
sync with the install state.

The entry is added when the first OpenCode rule installs, and removed
when the last one uninstalls. Install, update, uninstall, and the TUI all
converge through the same sync call.

For a **project-scope** install, grim edits `opencode.jsonc` when it
exists in the workspace root, otherwise `opencode.json`, and writes the
workspace-relative glob `.opencode/rules/*.md`.

For a **global-scope** install, grim edits the file at `$OPENCODE_CONFIG`
when that variable is set, otherwise the [XDG Base Directory][xdg-spec]
default (`$XDG_CONFIG_HOME/opencode/opencode.json`, falling back to
`~/.config/opencode/opencode.json`). The glob in a global config is an
absolute path rooted at `$GRIM_HOME`.

Config editing is conservative. A config file that does not parse — even
after stripping JSONC comments and trailing commas — is never rewritten.
grim returns a sync error instead. A parseable JSONC file is rewritten as
plain JSON; any JSONC comments it contained are lost. grim emits a warning
when that happens.

## Drift detection for rendered files {#drift}

A rendered `SKILL.md` and a transformed rule are both recorded as
`generated: true` in the install state. grim computes the integrity hash
against the **expected rendered bytes**, not the canonical input bytes.

If you edit a rendered file by hand, grim detects the mismatch on the
next `grim update` or `grim status` and reports drift. The same drift
detection that covers verbatim-installed files applies here.

## Publish-time validation {#publish-validation}

`grim build` and `grim release` run the projection for every supported
client before pushing. The full union of warnings is printed. Any invalid
literal in a known namespaced field stops the publish with exit code 65
(DataError) — the artifact never reaches the registry.

This means errors in metadata keys are caught locally, at the author's
desk, rather than discovered when a consumer tries to install.

## Legacy top-level key migration {#migration}

A `SKILL.md` that was authored before this feature may carry Claude-specific
fields as top-level frontmatter keys (e.g. `user-invocable: true`). Those
fields land in the `extra` map and install verbatim — no breakage.

`grim build` and `grim release` emit a migration-nudge warning for each
such key:

```
top-level frontmatter key 'user-invocable' is not an agentskills field;
author it as metadata 'claude.user-invocable' instead
```

This is a warning, not an error. Move the field into `metadata` under the
claude namespace to silence it and gain proper type conversion.

<!-- external -->
[agentskills-spec]: https://agentskills.io/specification
[claude-subagents-docs]: https://code.claude.com/docs/en/sub-agents
[opencode-agents-docs]: https://opencode.ai/docs/agents/
[copilot-agents-docs]: https://docs.github.com/en/copilot/concepts/agents/copilot-cli/about-custom-agents
[claude-code-docs]: https://code.claude.com
[claude-skills-docs]: https://code.claude.com/docs/en/skills
[claude-memory-docs]: https://code.claude.com/docs/en/memory
[claude-dir-docs]: https://code.claude.com/docs/en/claude-directory
[copilot-instructions-docs]: https://docs.github.com/en/copilot/customizing-copilot/adding-custom-instructions-for-github-copilot
[copilot-skills-docs]: https://docs.github.com/en/copilot/how-tos/copilot-cli/customize-copilot/add-skills
[copilot-config-dir-docs]: https://docs.github.com/en/copilot/reference/copilot-cli-reference/cli-config-dir-reference
[opencode-skills-docs]: https://opencode.ai/docs/skills
[opencode-rules-docs]: https://opencode.ai/docs/rules
[opencode-config-docs]: https://opencode.ai/docs/config
[xdg-spec]: https://specifications.freedesktop.org/basedir-spec/latest/

<!-- internal -->
[publishing-metadata]: ./publishing.md#metadata
[concepts-clients]: ./concepts.md#clients
[agents-doc]: ./agents.md
