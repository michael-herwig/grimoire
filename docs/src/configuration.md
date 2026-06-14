# Configuration

Grimoire keeps configuration in two small files and a handful of environment
variables. The files are managed by the commands — you rarely edit them by
hand — but it helps to know their shape.

## `grimoire.toml`

The declaration file. An `[options]` table holds defaults, and `[skills]` /
`[rules]` / `[agents]` map each binding name to a reference:

```toml
#:schema https://michael-herwig.github.io/grimoire/schemas/grimoire-config.schema.json
[options]
default_registry = "ghcr.io/acme"
clients = ["claude", "opencode"]

[skills]
code-review = "ghcr.io/acme/code-review:1"
commit-helper = "ghcr.io/acme/commit-helper:1"

[rules]
rust-style = "ghcr.io/acme/rust-style:2"

[agents]
code-reviewer = "ghcr.io/acme/code-reviewer:1"
```

`default_registry` lets you write short references; `clients` selects which
[AI clients](./concepts.md#clients) `grim install` and `grim update` materialize
into. It accepts a TOML array of client names (`claude`, `opencode`, `copilot`);
when absent, the **detected** clients for the scope are targeted — every client
whose vendor directory or marker is present — falling back to all clients when
none are detected. Unknown keys are rejected on parse, so a typo surfaces
immediately rather than silently doing nothing.

An optional `[bundles]` table declares [bundles](./concepts.md#bundles), each
mapping a binding name to a bundle reference. A bundle expands into its member
skills, rules, and [agents](./agents.md) at lock time:

```toml
[bundles]
python-stack = "ghcr.io/acme/python-stack:1"

[skills]
# A direct declaration overrides a bundle member of the same name.
code-review = "ghcr.io/acme/code-review:2"
```

Bundle references follow the same rules as skills and rules — a bare reference
defaults to `:latest`. Per `(kind, name)`, a direct declaration wins over any
bundle, agreeing bundles coalesce, and disagreeing bundles fail closed; see the
[conflict policy](./concepts.md#bundle-conflicts).

## Multiple registries {#multiple-registries}

A project that pulls artifacts from more than one registry can declare all
of them in a `[[registries]]` array instead of juggling `--registry` flags.
When the array is present it becomes the authoritative browse set for
[`grim search`](./commands.md#search) and the [MCP server](./commands.md#mcp);
an explicit `--registry` flag still collapses the browse to exactly that one
registry. The [TUI](./commands.md#tui) currently browses a single registry
and does not yet consume `[[registries]]` — multi-registry TUI support is
planned for a future release.

Each entry has one required field and two optional fields:

| Field | Required | Description |
|-------|----------|-------------|
| `url` | yes | Registry host and optional namespace, e.g. `ghcr.io/acme`. Same form as `[options].default_registry`. |
| `alias` | no | Short name for use in [qualified references](#qualified-references). Must be unique across the array. |
| `default` | no | Marks this entry as the primary registry short identifiers expand against. At most one entry may set it; when none do, the first entry is primary. |

```toml
#:schema https://michael-herwig.github.io/grimoire/schemas/grimoire-config.schema.json
[[registries]]
alias = "acme"
url = "ghcr.io/acme"
default = true

[[registries]]
alias = "internal"
url = "registry.corp.example/team"
```

The same `[[registries]]` array can appear in the global config
(`$GRIM_HOME/grimoire.toml`). Project entries take precedence over global
entries; duplicate URLs are deduped, first occurrence wins.

**Backward compatibility**: a config that omits `[[registries]]` entirely
behaves exactly as before — `[options].default_registry`, the environment
variable `GRIM_DEFAULT_REGISTRY`, and the `--registry` flag still drive the
single-registry path. The two approaches do not mix: when any `[[registries]]`
entry is declared, `[options].default_registry` is ignored for browse purposes
(the `default = true` entry, or first entry, takes its role).

### Qualified references {#qualified-references}

When registries have aliases, a reference can be qualified with
`alias/repo[:tag]` to expand the alias to its configured URL. For example,
with the config above:

```sh
grim add acme/code-review:1.2
# expands to: grim add ghcr.io/acme/code-review:1.2

grim add internal/lint-rules:stable
# expands to: grim add registry.corp.example/team/lint-rules:stable
```

The qualified form uses a slash separator (`alias/repo`), not a colon —
`alias:repo` would be ambiguous with `repo:tag`. A reference whose leading
`/`-segment does not match any alias is treated as a multi-segment
repository path under the primary registry, exactly as without aliases
configured.

Short references with no alias and no explicit registry still expand
against the primary (or only) registry, unchanged from the single-registry
behavior.

## `grimoire.lock`

The lockfile pins every declared tag to an exact digest and records the
[scope's](./concepts.md#scopes) declaration hash so drift is detectable. It is
generated by [`grim lock`](./commands.md#lock), `grim add`, and the
[TUI's](./commands.md#tui) install action; treat it as machine-owned and
commit it alongside `grimoire.toml`:

```toml
[metadata]
lock_version = 1
generated_by = "grim 0.1.0"

[[skill]]
name = "code-review"
pinned = "ghcr.io/acme/code-review@sha256:…"

[[rule]]
name = "rust-style"
pinned = "ghcr.io/acme/rust-style@sha256:…"

[[agent]]
name = "code-reviewer"
pinned = "ghcr.io/acme/code-reviewer@sha256:…"
```

A member that came from a [bundle](./concepts.md#bundles) additionally carries
`bundle` and `bundle_tag` fields recording its origin; a directly-declared entry
omits them, so a bundle-free lock is byte-identical to one written before
bundles existed. A member that **several** declared bundles contributed (an
agreeing overlap) records every contributor in a `bundles` sub-table array
(`[[skill.bundles]]` rows with `repo` and `tag`) instead of the single pair —
removing one bundle then only strips its provenance entry, and the member
stays locked until the last contributing bundle is removed. The same
compatibility holds for agents: an agent-free lock carries no `[[agent]]`
array at all and is byte-identical to one written before agents existed.

A lock with declared bundles also caches each bundle's expansion result in a
`[[bundle]]` section — binding name, `repo`, `tag`, the resolved manifest
digest, and the member list as `[[bundle.member]]` rows:

```toml
[[bundle]]
name = "starter-pack"
repo = "ghcr.io/acme/bundles/starter-pack"
tag = "1"
pinned = "ghcr.io/acme/bundles/starter-pack@sha256:…"

[[bundle.member]]
kind = "skill"
name = "code-reviewer"
id = "ghcr.io/acme/code-reviewer:1"
```

This cache is what lets `grim remove` and `grim uninstall` work **offline**
on the *effective* declaration: before applying an edit they compute the set
of artifacts the declaration implies before and after, drop only what no
remaining declaration holds, and keep everything else. A bundle-free lock
carries no `[[bundle]]` section at all.

## Editor schema support {#editor-schema}

Both author-facing files ship a published [JSON Schema](https://json-schema.org/),
so an editor can autocomplete keys and flag a mistyped table name the moment
you save — instead of surfacing the error at the next `grim` run. The schemas
are generated from grim's own parser, so they accept exactly what grim accepts.

| File | Schema URL |
|------|------------|
| `grimoire.toml` | `https://michael-herwig.github.io/grimoire/schemas/grimoire-config.schema.json` |
| `publish.toml` | `https://michael-herwig.github.io/grimoire/schemas/grim-publish.schema.json` |

[Taplo](https://taplo.tamasfe.dev/) and the
[Even Better TOML](https://marketplace.visualstudio.com/items?itemName=tamasfe.even-better-toml)
VS Code extension bind a file to its schema through a first-line directive:

```toml
#:schema https://michael-herwig.github.io/grimoire/schemas/grimoire-config.schema.json
```

To regenerate or inspect a schema locally, use [`grim schema`](./commands.md#schema):
`grim schema --kind config` prints the `grimoire.toml` schema and
`grim schema --kind publish` prints the `publish.toml` one.

## Scopes on disk

A **project** config is the `grimoire.toml` discovered from the working
directory. The **global** config lives at `$GRIM_HOME/grimoire.toml` and is
selected with `--global`. See [Concepts](./concepts.md#scopes) for when each
applies.

## Environment variables

| Variable | Purpose | Default |
|----------|---------|---------|
| `GRIM_HOME` | Root data directory (cache, global config, global install state at `$GRIM_HOME/state/global.json`). Project install state lives at `<workspace>/.grimoire/state.json`, not here. | `~/.grimoire` |
| `GRIM_DEFAULT_REGISTRY` | Default registry for short references. | unset |
| `GRIM_OFFLINE` | Disable all network access (same as `--offline`). | `false` |
| `GRIM_INSECURE_REGISTRIES` | Comma-separated registries reachable over plain HTTP — for local or in-cluster registries without TLS. | unset |
| `DOCKER_CONFIG` | Directory holding the Docker-compatible `config.json` that [`grim login`](./authentication.md) reads and writes. | `~/.docker` |

By default Grimoire resolves floating tags fresh from the registry, then caches
the result, so a floating tag never serves a stale pin. Pass `--offline` (or set
`GRIM_OFFLINE`) to work from the cache alone and fail rather than reach the
network.

A command-line flag always wins. For the registry, the environment variable
wins over the config options: the registry resolves as `--registry`, then
`GRIM_DEFAULT_REGISTRY`, then the project config's `default_registry` option,
then the global config's, and finally the built-in default `grim.ocx.sh`
when nothing is configured anywhere. The `--offline` toggle has no
config-file counterpart — the flag or its `GRIM_OFFLINE` variable applies.

## Data layout

The resolved-artifact content store, the catalog cache that
[`grim search`](./commands.md#search) and the [TUI](./commands.md#tui) read, and
the **global** install state (`$GRIM_HOME/state/global.json`) all live under
`GRIM_HOME`. Keeping cache and global state under one directory means installs
can use atomic, same-filesystem operations.

**Project install state** is separate: it lives at
`<workspace>/.grimoire/state.json`, co-located with `grimoire.toml`. The
workspace directory is the key, so two projects sharing the same `GRIM_HOME`
volume cannot collide. Grim writes a self-managed `.grimoire/.gitignore`
(contents: `*`) the first time it creates the `.grimoire/` directory, so the
state file is kept out of version control without touching your root
`.gitignore`.
