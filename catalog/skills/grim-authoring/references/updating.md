# Updating This Guide

You loaded this file because you maintain the grim-authoring package and
need to refresh its claims against the current grim release.

## Schema Authority

This skill distills, it does not define. The chain of truth, strongest
first:

1. **The installed binary** — `grim build` output and `grim build --help`
   reflect the schema actually compiled in.
2. **The docs site** — [Artifact Reference][artifacts],
   [Vendor-Specific Metadata][vendor], [Publishing][publishing],
   [Agent Artifacts][agents].
3. **The source** — frontmatter structs in [`src/skill/`][src-skill]
   (skill/rule/agent frontmatter, name rules) and the vendor registries
   in [`src/install/`][src-install] (`vendor_claude.rs` and siblings).

## Refresh Protocol

On every grim **minor** release:

1. Re-run `grim build` against this package and the minimal examples in
   each `references/*-spec.md`; fix anything newly rejected or warned.
2. Diff the four docs pages above against the field tables and pitfalls
   tables here — new fields, new registries, changed limits.
3. Re-verify the volatile numbers: name length cap, description cap,
   bundle member/size limits, enum value sets. Registries grow fastest.
4. Bump the "Verified against grim X.Y.x" footer in `SKILL.md`.

## Durable Search Terms

- `grimoire grim build exit 65 DataError validation`
- `grim vendor metadata projection claude opencode copilot codex registry`
- `codex subagents TOML agent developer_instructions`
- `grim catalog metadata summary keywords repository annotation`
- `grim bundle pin floating members cascade tags`
- `agentskills.io specification metadata map string values`

## Canonical Links

[artifacts]: https://michael-herwig.github.io/grimoire/artifacts.html
[vendor]: https://michael-herwig.github.io/grimoire/vendor-metadata.html
[publishing]: https://michael-herwig.github.io/grimoire/publishing.html
[agents]: https://michael-herwig.github.io/grimoire/agents.html
[src-skill]: https://github.com/michael-herwig/grimoire/tree/main/src/skill
[src-install]: https://github.com/michael-herwig/grimoire/tree/main/src/install
