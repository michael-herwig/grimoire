# Configuration

Grimoire keeps configuration in two small files and a handful of environment
variables. Settings (`[options]`, `[options.tui]`) and named registries
(`[[registries]]`) are managed through [`grim config`][grim-config]; declarations
(`[skills]`, `[rules]`, `[agents]`, `[bundles]`) stay under [`grim add`][grim-add]
and [`grim remove`][grim-remove]. You can also hand-edit either file directly,
but note that **any `grim` write — `grim config`, `grim add`, `grim remove` — uses a
lossy serializer: comments and the `#:schema` directive are removed** on every
write.

## `grimoire.toml`

The declaration file. An `[options]` table holds defaults, and `[skills]` /
`[rules]` / `[agents]` map each binding name to a reference:

```toml
#:schema https://grimoire.rs/schemas/grimoire-config.schema.json
[[registries]]
oci = "ghcr.io/acme"
default = true

[options]
clients = ["claude", "opencode"]

[skills]
code-review = "ghcr.io/acme/code-review:1"
commit-helper = "ghcr.io/acme/commit-helper:1"

[rules]
rust-style = "ghcr.io/acme/rust-style:2"

[agents]
code-reviewer = "ghcr.io/acme/code-reviewer:1"
```

The `[[registries]]` entry with `default = true` sets the primary registry short references expand against; `clients` selects which
[AI clients](./concepts.md#clients) `grim install` and `grim update` materialize
into. It accepts a TOML array of client names (`claude`, `opencode`, `copilot`);
when absent, the **detected** clients for the scope are targeted — every client
whose vendor directory or marker is present — falling back to all clients when
none are detected. Unknown keys are rejected on parse, so a typo surfaces
immediately rather than silently doing nothing.

### `[options.tui]` {#options-tui}

The optional `[options.tui]` sub-table tunes the interactive catalog browser
launched by [`grim tui`][grim-tui]. All three fields are opt-in —
an absent `[options.tui]` leaves the TUI at its built-in defaults.

```toml
[options.tui]
default_view = "tree"
group_by_type = true
tree_separators = ["/", "-"]
```

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `default_view` | `"flat"` or `"tree"` | `"flat"` | The view mode the browser opens in. `"tree"` starts in the collapsible grouped tree; `"flat"` starts in the plain list. An unrecognised value is a config parse error — the enum is strict. The runtime `t` key still toggles between modes ephemerally; the config is never auto-rewritten. |
| `group_by_type` | boolean | `false` | When `true`, inserts an extra type-level group — `skill`, `rule`, `agent`, or `bundle` — between the registry root and the repository path segments in tree view. Has no effect in flat mode. |
| `tree_separators` | array of single-character strings | (absent or `[]`) | The characters on which a repository path is split into nested tree groups. Omitting the field (or setting it to `[]`) leaves the array empty in the config file; at runtime, an empty array normalizes to `["/"]`. Add `"-"` to split on hyphens as well, so `code-review` becomes `code` → `review`. Each entry must be exactly one character; empty or multi-character entries are a parse error. |

Configuration parse errors — including an unrecognised `default_view` value or an invalid `tree_separators` entry — exit 78 (`EX_CONFIG`).

The registry host is always the tree root. When the browsed registry matches
the configured default registry, the host node is elided from the display
so leaf names stay short.

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
[`grim search`](./commands.md#search), the [MCP server](./commands.md#mcp), and
the [TUI](./commands.md#tui) — `grim tui` browses all declared registries, one
collapsible root per registry. An explicit `--registry` flag still collapses the
browse to exactly the registries it names — repeatable and comma-separated
(`--registry a,b`) for several at once. `GRIM_DEFAULT_REGISTRY` does **not**
collapse the browse set — it is the short-id resolution default and only
applies as the single-registry fallback when no `[[registries]]` array is
declared.

Each entry declares **exactly one** source locator (`oci` or `index`)
plus two optional fields:

| Field | Required | Description |
|-------|----------|-------------|
| `oci` | one of `oci`/`index` | Plain OCI registry ref — host and optional namespace, e.g. `ghcr.io/acme`. Same form as `[options].default_registry`. Lists packages via the OCI `_catalog` endpoint. The pre-0.7.0 key `url` is still accepted as a parse-time alias, so existing configs keep working; new writes use `oci`. |
| `index` | one of `oci`/`index` | A [package index](./package-index.md) locator: an `http(s)://` static base or a git repository (`git+…`, `ssh://`, `git@…`, or ending in `.git`). Replaces the `_catalog` listing; index entries carry their own registry refs. Mutually exclusive with `oci` — setting both is a parse error (exit 78). |
| `alias` | no | Short name for use in [qualified references](#qualified-references). Must be unique across the array. The TUI uses the alias as the display label in the flat list's Registry column and as the tree registry-root row label; entries without an alias fall back to the raw locator. |
| `default` | no | Marks this entry as the primary registry short identifiers expand against. At most one entry may set it; when none do, the first entry is primary. |

```toml
#:schema https://grimoire.rs/schemas/grimoire-config.schema.json
[[registries]]
alias = "acme"
oci = "ghcr.io/acme"
default = true

[[registries]]
alias = "internal"
oci = "registry.corp.example/team"
```

The same `[[registries]]` array can appear in the global config
(`$GRIM_HOME/grimoire.toml`). Project entries take precedence over global
entries; duplicate locators are deduped, first occurrence wins.

**Backward compatibility**: a config that omits `[[registries]]` entirely
behaves exactly as before — `[options].default_registry`, the environment
variable `GRIM_DEFAULT_REGISTRY`, and the `--registry` flag still drive the
single-registry path. The two approaches do not mix: when any `[[registries]]`
entry is declared, `[options].default_registry` is ignored for browse purposes
(the `default = true` entry, or first entry, takes its role). The field is still
read for back-compat and never destroyed on re-serialize, but `grim init` now
writes the `[[registries]]` shape for new configs — `[options].default_registry`
is deprecated for new writes.

**Known limitation**: `grim login` / `grim logout` with no positional argument
or `--registry` flag resolve the registry from the `--registry` flag,
`GRIM_DEFAULT_REGISTRY`, and the built-in default only — they do not consult
`[[registries]]`. Pass the registry explicitly (`grim login ghcr.io/acme`) when
your config uses `[[registries]]`-only.

**At-most-one `default = true`**: declaring two `[[registries]]` entries with
`default = true` is a parse error (exit 78). When none set it, the first entry
is the primary.

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

### Registry compatibility {#registry-compatibility}

`grim search` and the TUI browse a registry's catalog through the
host-level OCI `_catalog` endpoint. Not all registries expose it —
multi-tenant SaaS registries such as [GitHub Container Registry][ghcr]
and the [GitLab Container Registry][gitlab-registry] gate the endpoint
for namespace-privacy reasons. When a registry does not support
`_catalog`, a browse comes back empty.

An empty browse result on these registries is **expected behavior, not
an error**. Install, add, release, and publish work through explicit
references and are unaffected — every registry in the table below
supports explicit-reference operations.

To browse packages hosted on a `_catalog`-gated registry, use a
[package index](./package-index.md) entry (`index = …`) instead of a
registry `url` — the index lists the packages; the registry only serves
them.

| Registry | `_catalog` browse (`grim search`, TUI) | Explicit-ref ops (install / add / release / publish) |
|---|---|---|
| `registry:2` (local) | yes | yes |
| [Zot][zot] | yes | yes |
| [Harbor][harbor] | yes | yes |
| [GitHub Container Registry (GHCR)][ghcr] | no | yes |
| [Docker Hub][dockerhub] | no | yes |
| [GitLab Container Registry (SaaS)][gitlab-registry] | no | yes |

When an online browse comes back empty, grim prints a hint pointing to
this section so you can confirm whether the registry supports `_catalog`.

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
| `grimoire.toml` | `https://grimoire.rs/schemas/grimoire-config.schema.json` |
| `publish.toml` | `https://grimoire.rs/schemas/grim-publish.schema.json` |

[Taplo](https://taplo.tamasfe.dev/) and the
[Even Better TOML](https://marketplace.visualstudio.com/items?itemName=tamasfe.even-better-toml)
VS Code extension bind a file to its schema through a first-line directive:

```toml
#:schema https://grimoire.rs/schemas/grimoire-config.schema.json
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
| `GRIM_ANNOUNCE_TOKEN` | Forge API token for [`grim publish --announce`](./package-index.md#announcing) — always wins over CI-provided tokens. Sent as an API header only, never logged. | unset |
| `DOCKER_CONFIG` | Directory holding the Docker-compatible `config.json` that [`grim login`](./authentication.md) reads and writes. | `~/.docker` |

Announce additionally reads the standard CI variables (`GITHUB_ACTIONS`,
`GITHUB_SERVER_URL`, `GITHUB_API_URL`, `GITHUB_REPOSITORY_OWNER`,
`GH_TOKEN`/`GITHUB_TOKEN`; `GITLAB_CI`, `CI_SERVER_HOST`, `CI_API_V4_URL`,
`CI_PROJECT_NAMESPACE`, `GITLAB_TOKEN`) — only when the CI server host
equals the announce target host. See
[Announcing Packages](./package-index.md#announcing).

By default Grimoire resolves floating tags fresh from the registry, then caches
the result, so a floating tag never serves a stale pin. Pass `--offline` (or set
`GRIM_OFFLINE`) to work from the cache alone and fail rather than reach the
network.

A command-line flag always wins. Registry resolution operates on two separate
precedences depending on context:

**Browse-set** (what `grim search`, the TUI, and `grim mcp` browse): `--registry`
flag → project `[[registries]]` → global `[[registries]]` → single default
(`GRIM_DEFAULT_REGISTRY` → project `[options].default_registry` → global
`[options].default_registry` → built-in `https://index.grimoire.rs`, the
public [package index](./package-index.md)). The single-default tier
applies only when no `[[registries]]` array is declared anywhere. Only the
`--registry` flag collapses browse — to exactly the registries it names
(repeatable / comma-separated); `GRIM_DEFAULT_REGISTRY` does
not restrict the browse set when `[[registries]]` is configured.

**Short-id resolution** (expanding a bare `name:tag` to a full registry URL):
`--registry` flag → `GRIM_DEFAULT_REGISTRY` → project `[options].default_registry`
(or the primary entry of project `[[registries]]`) → global config → built-in
`ghcr.io/grimoire-rs`. Index sources never expand short ids — with an
index-only browse set the push-side fallback applies.

The `--offline` toggle has no config-file counterpart — the flag or its `GRIM_OFFLINE` variable applies.

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

<!-- internal -->
[grim-tui]: ./commands.md#tui
[grim-config]: ./commands.md#config
[grim-add]: ./commands.md#add
[grim-remove]: ./commands.md#remove

<!-- external -->
[ghcr]: https://docs.github.com/en/packages/working-with-a-github-packages-registry/working-with-the-container-registry
[gitlab-registry]: https://docs.gitlab.com/ee/user/packages/container_registry/
[zot]: https://zotregistry.dev/
[harbor]: https://goharbor.io/
[dockerhub]: https://hub.docker.com/
