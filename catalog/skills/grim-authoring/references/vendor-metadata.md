# Vendor Metadata

You loaded this file because you are adding `claude.*`, `opencode.*`,
`copilot.*`, or `codex.*` keys to an artifact, or a publish failed on a vendor literal.

Contents: [Mental Model](#mental-model) · [Outcome Classes](#outcome-classes) ·
[Literal Discipline](#literal-discipline) ·
[Where the Registries Live](#where-the-registries-live) ·
[Worked Example](#worked-example) · [Legacy Migration](#legacy-migration)

## Mental Model

A published artifact stays spec-compliant: client-specific capabilities
are authored as **string-valued** `<vendor>.<field>` keys inside the
artifact's `metadata` map. At install time grim looks each key up in the
target vendor's registry and **projects** it — converts the string to
its native type and lifts it into top-level frontmatter of the written
file. Each client sees only its own namespace; one canonical file serves
all clients. The four recognized namespaces are `claude`, `opencode`, `copilot`, and
`codex`; any other prefix (e.g. `vendor.x`) is plain metadata and passes
through untouched. Note that Codex supports skills and agents only — rules
are unsupported and grim warns and skips them. Codex skills use the
universal agentskills shape (no `codex.*` skill namespace exists); only
agents carry `codex.*` metadata.

## Outcome Classes

Every vendor key lands in exactly one of these classes — memorize them,
they explain every vendor-metadata surprise:

| Input | Outcome |
|---|---|
| Known key, valid literal | Projected: converted to native type, lifted to top-level frontmatter |
| Known key, **bad literal** | **Hard error** — publish fails exit 65; install fails MaterializeFailed |
| Unknown key in your **own** namespace (typo: `claude.efort`) | Warning + dropped — the typo guard; silent data loss if the warning is ignored |
| Key in a **foreign** namespace (e.g. `opencode.*` rendering for Claude) | Dropped silently — by design, that is multi-client serving |

Two corollaries: the OpenCode, Copilot, and Codex *skill* registries are
empty (no vendor namespace exists for skills in those clients), so any
`opencode.*`/`copilot.*`/`codex.*` key on a skill is always unknown →
warn + drop. And when a namespaced key collides with a same-named top-level
field, the namespaced key wins — with a warning in the legacy-migration
case, silently for the agent `model`/`tools` override escape hatch.

## Literal Discipline

All `metadata` values are strings; grim converts at install time. The
conversion is what fails publishes:

- **bool** — exactly `"true"` or `"false"`, quoted.
- **enum** — a closed set per key (e.g. `claude.effort` accepts
  `low|medium|high|xhigh|max`); anything else is exit 65.
- **integer** — base-10 digits only, quoted (`claude.max-turns: "20"`).
- **float** — any finite float, quoted (`opencode.temperature: "0.2"`).
- **comma list / string** — never fail.

Object-valued native fields (Claude's `hooks`/`mcpServers`, OpenCode's
`permission`, Copilot's `mcp-servers`) cannot be expressed as a string
and are **not authorable at all**.

## Where the Registries Live

Do not work from memory — the key registries are versioned with grim and
grow over time. The authoritative tables:

- [`claude.*` skill registry][claude-reg]
- [`claude.*` agent registry][claude-agent-reg]
- [`opencode.*` agent registry][opencode-agent-reg]
- [`copilot.*` agent registry][copilot-agent-reg]
- [`codex.*` agent registry][codex-agent-reg] (`codex.model`, `codex.reasoning-effort`, `codex.sandbox-mode`)
- [Rule-level keys][rule-keys] (today: `copilot.exclude-agent` only)
- [Empty skill registries][empty-reg] (OpenCode, Copilot, Codex)

## Worked Example

```yaml
---
name: deep-review
description: A thorough security and correctness review.
metadata:
  claude.user-invocable: "true"
  claude.effort: "high"
---
```

Installed for Claude Code, the written `SKILL.md` carries native typed
frontmatter: `user-invocable: true` (a YAML bool) and `effort: high`;
for OpenCode or Copilot both keys drop and the render is universal.
`grim build` runs this projection for *every* supported client before
anything publishes, printing the full union of warnings — errors are
caught at your desk, not the consumer's ([validation][publish-val]).

## Legacy Migration

A pre-grim `SKILL.md` may carry Claude fields as top-level keys
(`user-invocable: true`). That installs verbatim — no breakage — but
build/release warn per key; move each into `metadata` under `claude.*`
to silence the nudge and gain type conversion ([migration][migration]).

## Further Reading

- [Why tool keys live in metadata][why] — the design rationale.
- [Projection semantics][projection] — the full outcome table.
- [Publish-time validation][publish-val] — when the gate runs.

[why]: https://michael-herwig.github.io/grimoire/vendor-metadata.html#why-metadata
[projection]: https://michael-herwig.github.io/grimoire/vendor-metadata.html#projection-semantics
[claude-reg]: https://michael-herwig.github.io/grimoire/vendor-metadata.html#claude-registry
[claude-agent-reg]: https://michael-herwig.github.io/grimoire/vendor-metadata.html#claude-agent-registry
[opencode-agent-reg]: https://michael-herwig.github.io/grimoire/vendor-metadata.html#opencode-agent-registry
[copilot-agent-reg]: https://michael-herwig.github.io/grimoire/vendor-metadata.html#copilot-agent-registry
[codex-agent-reg]: https://michael-herwig.github.io/grimoire/vendor-metadata.html#codex-agent-registry
[rule-keys]: https://michael-herwig.github.io/grimoire/vendor-metadata.html#rule-keys
[empty-reg]: https://michael-herwig.github.io/grimoire/vendor-metadata.html#empty-registries
[publish-val]: https://michael-herwig.github.io/grimoire/vendor-metadata.html#publish-validation
[migration]: https://michael-herwig.github.io/grimoire/vendor-metadata.html#migration
