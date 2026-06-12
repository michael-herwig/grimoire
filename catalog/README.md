# Catalog — First-Party Grimoire Packages

Grim-publishable AI-config packages, dogfooding grim's own packaging:
authored here, validated by `grim build` in CI, published to `grim.ocx.sh`.

## Layout

```
catalog/
├── publish.toml        # publish manifest: registry + per-package versions
├── scripts/publish.py  # release driver (repo tooling, not a package)
├── taskfile.yml        # catalog: subsystem tasks (verify, release)
├── skills/<name>/      # one dir per skill package (SKILL.md + references/)
├── bundles/<name>.toml # one file per bundle package
├── rules/<name>.md     # (when the first rule package lands)
└── agents/<name>.md    # (when the first agent package lands)
```

Skill internals follow the [agentskills.io specification] best practices:
supporting docs in `references/`, executable helpers in `scripts/`, static
files in `assets/`. The root `SKILL.md` is a short index/bootstrap; deep
knowledge lives in `references/` files loaded on demand. Every skill
carries `references/updating.md` — the maintainer re-research protocol
(procedure, durable search terms, canonical links).

## Content drift tiers

Declared per file; applied at authoring and review time:

| Tier | Content | Policy |
|------|---------|--------|
| 1 — inline | ADR-backed invariants (artifact kinds, name rules, metadata-location asymmetry, projection classes, exit-code classes) | State freely; survives minor releases |
| 2 — summarize + verify | Command flags, lifecycle behaviors | Narrative only + "confirm with `grim <cmd> --help`"; never reproduce flag tables |
| 3 — link only | Vendor key registries, exact limits, full command reference | Link the [docs site] anchors; never inline |

The grim-* skills open with a verify-before-acting protocol: on conflict
between skill content and live `--help` output, trust `--help`.

## Versioning

Versions live in `publish.toml`, independent per package, bumped via PR —
**no git tags** (`cliff.toml`'s unanchored `tag_pattern` would pick catalog
tags up and corrupt `--bumped-version`).

- **patch** — content fix
- **minor** — new sections or reference files
- **major** — restructure or renamed reference files

Registry refs are kind-segmented: `grim.ocx.sh/skills/<name>:<version>`,
`grim.ocx.sh/bundles/<name>:<version>`. Semver releases cascade (`1.2.3`
also moves `1.2`, `1`, `latest`). Bundle members reference the floating
major tag (`:0` while on the 0.x line) and bundles publish without
`--pin`, so skill patches
reach bundle consumers via plain `grim update`.

## Local loop

```sh
task catalog:verify                       # grim build every package (builds grim if stale)
grim login grim.ocx.sh -u <user>          # once, interactive
task catalog:release -- --dry-run         # preview full publish plan, zero writes
task catalog:release                      # publish everything per publish.toml
task catalog:release -- --only grim-usage # publish one package by hand
task catalog:release -- --tag canary      # ad-hoc movable tag, manifest untouched
```

Semver always comes from `publish.toml` — there is no version argument, so
the repo records exactly what was published. `--tag` rejects semver values.

CI publishes two ways, both via `publish-catalog.yml` (environment
`grim.ocx.sh`): the manually dispatched `Publish Catalog` workflow, and a
cargo-dist post-announce job on every grim release — idempotent when
catalog versions are unchanged (same digest re-push is a no-op), loud
failure when content changed without a `publish.toml` bump. Skills publish
before bundles so bundle members always resolve. Never auto-publish on
plain pushes to main.

## Keeping content honest

- `task catalog:verify` runs in CI on every PR — the real parser is the
  schema gate.
- When `docs/src/{artifacts,publishing,vendor-metadata,commands}.md` or
  `src/command/**` change, review `catalog/skills/grim-usage` and
  `catalog/skills/grim-authoring` for drift (each package's
  `references/updating.md` describes the re-research procedure).
- Hard numbers (vendor limits, activation rates) drift fastest — re-verify
  against the sources in `references/updating.md` before trusting.

[agentskills.io specification]: https://agentskills.io/specification
[docs site]: https://michael-herwig.github.io/grimoire/
