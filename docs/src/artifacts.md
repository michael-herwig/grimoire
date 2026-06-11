# Artifact Reference

Grimoire ships four artifact kinds — skills, rules, agents, and bundles.
Each has its own source shape, frontmatter schema, and validation rules,
and until now those details lived scattered across the publishing,
agents, and vendor-metadata chapters.

When you author an artifact you need one page that answers: which fields
exist, which are required, what values are valid, and what a correct file
looks like. This page is that reference. Narrative background stays in
[Concepts](./concepts.md); publishing mechanics stay in
[Publishing](./publishing.md); vendor projection semantics stay in
[Vendor-Specific Metadata](./vendor-metadata.md).

## The four kinds {#kinds}

Every artifact is typed on the wire with an OCI `artifactType` plus a
Grimoire config media type, so registries and tooling can distinguish
kinds without downloading layers.

| Kind | Source shape | OCI `artifactType` | Installs as |
|------|--------------|--------------------|-------------|
| **Skill** | Directory with a `SKILL.md` index | `application/vnd.grimoire.skill.v1` | Directory tree under the client's `skills/` dir |
| **Rule** | Single `.md` file (+ optional sibling support directory) | `application/vnd.grimoire.rule.v1` | `rules/<name>.md` (+ `rules/<name>/…`), per-client transform |
| **Agent** | Single `.md` file | `application/vnd.grimoire.agent.v1` | One agent file per client, per-client rendering |
| **Bundle** | `.toml` member list | `application/vnd.grimoire.bundle.v1` | Never materializes itself — expands to its members |

The config media type follows the same pattern:
`application/vnd.grimoire.<kind>.config.v1+json`.

`grim build` and `grim release` infer the kind from the path — a directory
is a skill, a `.md` file is a rule, a `.toml` file is a bundle. Agents are
the exception: an agent `.md` is indistinguishable from a rule by shape, so
`--kind agent` is required (see [Agent Artifacts](./agents.md#publishing)).

## Names {#names}

Every skill and agent carries a `name` in frontmatter, and grim validates
it at build time. The same character rules apply to rule and bundle names
taken from the file stem.

A valid name:

- contains only lowercase letters, digits, and hyphens (`[a-z0-9-]`),
- does not start or end with a hyphen,
- does not contain consecutive hyphens,
- is not empty.

For skills the `name` must equal the directory name containing `SKILL.md`;
for agents it must equal the file stem (`reviewer.md` → `name: reviewer`).
A mismatch fails the build with exit code 65 (data error).

## Skills {#skills}

A skill is a directory: the `SKILL.md` index plus any supporting files
(scripts, templates, references). Everything in the tree is packed into a
single tar layer and installed verbatim — only `SKILL.md` itself is ever
re-rendered, and only when it carries vendor-namespaced metadata keys.

The frontmatter follows the [agentskills specification][agentskills-spec].
Parsing is forward-compatible: unknown top-level keys are preserved
round-trip rather than rejected.

| Field | Required | Type | Notes |
|-------|----------|------|-------|
| `name` | yes | string | Must equal the skill directory name; see [Names](#names) |
| `description` | yes | string | What the skill does and when to use it |
| `license` | no | string | SPDX-style identifier (e.g. `Apache-2.0`); emitted as the OCI license annotation |
| `compatibility` | no | string | Editor/runtime hint (free text) |
| `allowed-tools` | no | string | Comma-separated tool allowlist |
| `metadata` | no | string→string map | Catalog keys + vendor extensions, see below |
| *(any other key)* | no | any YAML | Preserved verbatim (forward compatibility) |

Inside `metadata`, all values are strings. Three plain keys are read by
grim itself; everything else either passes through untouched or is a
[vendor extension](#vendor-extensions):

| Metadata key | Read by | Meaning |
|--------------|---------|---------|
| `summary` | catalog | Short one-line blurb for `grim search` / the TUI |
| `keywords` | catalog | Comma-separated tags, matched by search |
| `author` | nothing (convention) | Attribution; passes through verbatim |
| `<vendor>.<field>` | install renderer | Lifted into native client frontmatter, see [Vendor extensions](#vendor-extensions) |

### Example — minimal skill {#skill-example-minimal}

The smallest valid skill is a directory with a two-field `SKILL.md`:

```yaml
# hello-world/SKILL.md
---
name: hello-world
description: A minimal smoke-test skill that prints a greeting.
---

# Hello World

Say hello.
```

### Example — full-featured skill {#skill-example-full}

A skill using every top-level field, catalog metadata, and a Claude-only
capability key:

```yaml
# code-reviewer/SKILL.md
---
name: code-reviewer
description: Review a diff for SOLID/DRY violations, missing tests, and
  risky changes. Use when asked to review a pull request or audit a patch.
license: Apache-2.0
compatibility: claude>=2
allowed-tools: Read,Grep,Bash
metadata:
  summary: Multi-pass diff reviewer
  keywords: review,quality,solid,dry,audit
  author: acme-platform-team
  claude.user-invocable: "true"
  claude.effort: high
---

# Code Reviewer

Run the review in three passes...
```

The `claude.*` keys are string-valued here and become typed native
frontmatter (`user-invocable: true`, `effort: high`) in the file Claude
Code receives; other clients never see them. The projection rules live in
[Vendor-Specific Metadata](./vendor-metadata.md#projection-semantics).

## Rules {#rules}

A rule is a single Markdown file. Frontmatter is entirely optional — a
bare `.md` with no `---` fence is a valid rule whose body is the whole
document. When grim needs a description for the catalog it derives one
from the first Markdown heading or first non-empty line.

| Field | Required | Type | Notes |
|-------|----------|------|-------|
| `paths` | no | list of strings | Glob patterns the rule auto-loads on; empty/absent = always active |
| `summary` | no | string | Short one-line blurb for the catalog |
| `keywords` | no | string or list | Comma-separated tags (a YAML list is comma-joined) |
| `metadata` | no | string→string map | Vendor extensions (e.g. `copilot.exclude-agent`) |
| *(any other key)* | no | any YAML | Preserved verbatim (forward compatibility) |

Note the asymmetry with skills: rule `summary`/`keywords` are **top-level**
frontmatter keys, not `metadata` entries.

A rule may also carry a sibling support directory sharing its stem
(`architecture-guide.md` + `architecture-guide/`); both pack into one
artifact and install side by side — see
[Rules with a support directory](./publishing.md#rule-support-dir).

### Example — minimal rule {#rule-example-minimal}

```markdown
# commit-style.md

Use Conventional Commits. Subject ≤ 50 characters.
```

No fence at all — valid. The catalog description becomes the first
heading-less line.

### Example — path-scoped rule with catalog metadata {#rule-example-scoped}

```yaml
# rust-style.md
---
paths:
  - "**/*.rs"
  - "**/Cargo.toml"
summary: Idiomatic Rust style rules
keywords: rust,style,lints,quality
---

# Rust Style

Prefer `&str` over `String` parameters...
```

### Example — rule with a vendor extension {#rule-example-vendor}

```yaml
# security-baseline.md
---
paths:
  - "**/*.rs"
summary: Security review baseline
metadata:
  copilot.exclude-agent: code-review
---

# Security Baseline

Validate all external input at system boundaries...
```

`copilot.exclude-agent` becomes `excludeAgent: code-review` in the
Copilot instructions file and is invisible to Claude and OpenCode — see
[Rule-level vendor keys](./vendor-metadata.md#rule-keys).

## Agents {#agents}

An agent is a single `.md` defining a delegatable assistant. Unlike rules,
agent frontmatter is **required**: every client needs at least a
`description` to decide when to route work to the agent.

| Field | Required | Type | Notes |
|-------|----------|------|-------|
| `name` | yes | string | Must equal the file stem; see [Names](#names) |
| `description` | yes | string | When a client should delegate to this agent |
| `model` | no | string | Passed through verbatim, no alias translation; override per vendor via `<vendor>.model` |
| `tools` | no | string | Comma-separated allowlist, projected per client (string vs. list) |
| `metadata` | no | string→string map | Catalog keys (`summary`, `keywords`) + vendor extensions |
| *(any other key)* | no | any YAML | Preserved verbatim (forward compatibility) |

Like skills, agent `summary`/`keywords` live **inside** `metadata`. When a
vendor key lifts to the same native field as a common field (`model`,
`tools`), the vendor key silently wins for that client — the documented
override escape hatch
([override precedence](./agents.md#override-precedence)).

### Example — minimal agent {#agent-example-minimal}

```yaml
# reviewer.md
---
name: reviewer
description: Reviews a diff for correctness, style, and missing tests.
---

You are a code reviewer. Examine the diff...
```

### Example — agent with common fields and vendor overrides {#agent-example-vendor}

```yaml
# release-bot.md
---
name: release-bot
description: Prepares release notes and version bumps on request.
model: sonnet
tools: Read,Grep,Bash
metadata:
  summary: Release preparation agent
  keywords: release,changelog,versioning
  claude.permission-mode: plan
  claude.max-turns: "20"
  opencode.model: anthropic/claude-sonnet-4-5
  opencode.temperature: "0.2"
  copilot.tools: read,grep
---

You prepare releases. Collect commits since the last tag...
```

Claude Code receives `model: sonnet` plus `permissionMode: plan` and
`maxTurns: 20`; OpenCode receives `model: anthropic/claude-sonnet-4-5`
(its vendor key overrides the common `model`) and `temperature: 0.2`;
Copilot receives a `tools:` list of `read, grep`. The full emit matrix is
in [Agent Artifacts](./agents.md#emit-matrix).

## Bundles {#bundles}

A bundle is a curated set of references to other artifacts. Its source is
a `.toml` file; the published artifact carries only a JSON members
document, so a bundle never materializes files of its own — installing it
expands to installing its members.

Top-level keys and member tables:

| Key / table | Required | Type | Notes |
|-------------|----------|------|-------|
| `summary` | no | string | Short one-line blurb for the catalog |
| `keywords` | no | string | Comma-separated tags |
| `description` | no | string | Longer description; defaults to a deterministic `grimoire bundle of N members` |
| `[skills]` | no | name → ref table | Skill members |
| `[rules]` | no | name → ref table | Rule members |
| `[agents]` | no | name → ref table | Agent members |

Each member entry maps the **config binding name** (the name the member is
declared under when the bundle is added) to a fully-qualified reference —
`registry/repo:tag` or `registry/repo@sha256:…`. Floating tags re-resolve
on `grim update`; digest pins never move
([floating versus pinned members](./concepts.md#bundle-pinning)).

Limits enforced at parse time: at most 512 members per bundle, and the
members document is capped at 512 KiB. Nested bundles are invalid — a
bundle member must be a skill, rule, or agent.

### Example — bundle with all member kinds {#bundle-example}

```toml
# starter-pack.toml
summary = "Curated starter pack"
keywords = "starter,review,style,security"
description = "The code-review skill plus the Rust style rule and review agent"

[skills]
code-reviewer = "registry.example.com/grimoire/skills/code-reviewer:1"

[rules]
rust-style = "registry.example.com/grimoire/rules/rust-style:1"

[agents]
reviewer = "registry.example.com/grimoire/agents/reviewer@sha256:8f4b…"
```

## Vendor extensions {#vendor-extensions}

Client-specific capabilities are authored as string-valued
`<vendor>.<field>` keys in the artifact's `metadata` map and lifted into
native typed frontmatter at install time. The published artifact stays
spec-compliant; each client sees only its own namespace.

The recognized keys per vendor and kind — full type and projection detail
in [Vendor-Specific Metadata](./vendor-metadata.md):

| Vendor | Skills | Rules | Agents |
|--------|--------|-------|--------|
| `claude.*` | `disable-model-invocation`, `user-invocable`, `model`, `effort`, `context`, `agent`, `argument-hint`, `when-to-use`, `arguments`, `disallowed-tools`, `shell`, `paths` ([registry](./vendor-metadata.md#claude-registry)) | *(none today — unknown keys warn + drop)* | `model`, `tools`, `disallowed-tools`, `permission-mode`, `max-turns`, `skills`, `memory`, `background`, `effort`, `isolation`, `color`, `initial-prompt` ([registry](./vendor-metadata.md#claude-agent-registry)) |
| `opencode.*` | *(none — universal fields only)* | *(none)* | `model`, `mode`, `temperature`, `top-p`, `steps`, `prompt`, `disable`, `hidden`, `color` ([registry](./vendor-metadata.md#opencode-agent-registry)) |
| `copilot.*` | *(none — universal fields only)* | `exclude-agent` ([registry](./vendor-metadata.md#rule-keys)) | `tools` ([registry](./vendor-metadata.md#copilot-agent-registry)) |

Every value is authored as a string and converted at install time:

| Declared type | Accepted literals | On bad literal |
|---------------|-------------------|----------------|
| bool | `"true"`, `"false"` | hard error (exit 65) |
| enum | the closed set listed in the registry | hard error (exit 65) |
| integer | base-10 digits | hard error (exit 65) |
| float | any finite float | hard error (exit 65) |
| comma list | any; split on `,` into a YAML list | never fails |
| string | any | never fails |

A **known** key with a bad literal fails the publish. An **unknown** key in
your own namespace (a typo like `claude.efort`) warns and drops. A key in a
**foreign** namespace drops silently — that is how one canonical file
serves several clients
([publish-time validation](./vendor-metadata.md#publish-validation)).

## Catalog annotations {#annotations}

On the wire, catalog metadata travels as OCI manifest annotations. grim
emits standard [OCI image-spec annotation][oci-annotations] keys plus two
Grimoire-specific ones, sourced per kind as follows:

| Annotation | Source | Emitted |
|------------|--------|---------|
| `org.opencontainers.image.title` | artifact name | always |
| `org.opencontainers.image.description` | `description` field, or derived from the rule body | always |
| `org.opencontainers.image.version` | release version | always |
| `org.opencontainers.image.licenses` | skill `license` field | when present |
| `org.opencontainers.image.source` | authored `repository` HTTPS URL (skill/agent `metadata.repository`; rule top-level `repository`; bundle `repository`); falls back to the tagless release ref | always on release |
| `com.grimoire.summary` | skill/agent `metadata.summary`; rule top-level `summary`; bundle `summary` | when present |
| `com.grimoire.keywords` | skill/agent `metadata.keywords`; rule top-level `keywords`; bundle `keywords` | when present |

An authored `repository` must be an `https://` URL — anything else fails
the publish (exit 65). Readers distinguish a real repository URL from the
legacy release-ref fallback by that `https://` prefix; on registries that
honor the key (e.g. [ghcr.io][ghcr-source-label]) the source annotation
also links the package back to its repository.

`org.opencontainers.image.created` is deliberately omitted so re-releasing
identical content stays byte-identical (idempotent re-release).

<!-- external -->
[agentskills-spec]: https://agentskills.io/specification
[oci-annotations]: https://github.com/opencontainers/image-spec/blob/main/annotations.md
[ghcr-source-label]: https://docs.github.com/en/packages/working-with-a-github-packages-registry/working-with-the-container-registry#labelling-container-images
