# Registries, Scopes, and Targets

You loaded this file because you need to resolve which registry a short
reference hits, which scope a command edits, which AI clients an install
lands in, how offline mode behaves, or how to search a catalog.

Contents: [Registry Resolution](#registry-resolution) ·
[Multiple Registries](#multiple-registries) ·
[Managing Config](#managing-config) ·
[Qualified References](#qualified-references) ·
[Scopes](#scopes) · [Client Targets](#client-targets) ·
[Offline Mode](#offline-mode) · [Search, TUI, and MCP](#search-tui-and-mcp)

## Registry Resolution

A fully qualified reference (`ghcr.io/acme/code-review:1`) needs no
resolution. A short reference (`code-review:1`) is expanded against the
default registry, resolved with this precedence — first present value
wins:

1. `--registry` flag
2. `GRIM_DEFAULT_REGISTRY` environment variable
3. project config `[[registries]]` primary (or legacy `[options].default_registry` when no `[[registries]]` declared)
4. global config `[[registries]]` primary (or legacy `[options].default_registry`)
5. the built-in default `grim.ocx.sh` (applies only when nothing above
   is set)

Whatever default applied, the expanded reference is persisted **fully
qualified** in `grimoire.toml` and the lock — so a config never depends
on the environment that wrote it.

One exception: `grim login` / `grim logout` resolve their registry from
the positional argument, then `--registry`, then `GRIM_DEFAULT_REGISTRY`
— config files are not consulted on the login path. Confirm with
`grim login --help`.

Environment variables that matter here (full table:
[Configuration][envvars]):

| Variable | Purpose |
|---|---|
| `GRIM_HOME` | Data root: cache, catalog, global config (default `~/.grimoire`) |
| `GRIM_DEFAULT_REGISTRY` | Default registry for short references |
| `GRIM_OFFLINE` | Same as `--offline` |
| `GRIM_INSECURE_REGISTRIES` | Comma-separated plain-HTTP registries (local/in-cluster) |
| `DOCKER_CONFIG` | Directory of the Docker-compatible credential `config.json` |

## Multiple Registries {#multiple-registries}

When a project draws from more than one registry, declare them in a
`[[registries]]` array in `grimoire.toml` (or the global config). When
the array is present it replaces the single-registry path: `grim search`,
`grim tui`, and the MCP server browse **all declared registries at once**
instead of one. In the TUI each registry becomes its own collapsible tree
root, with the registry prefix shown only when more than one registry
resolves.

Each entry:

| Field | Required | Purpose |
|-------|----------|---------|
| `url` | yes | Registry host and optional namespace — same form as `[options].default_registry` |
| `alias` | no | Short name for qualified `alias/repo` references |
| `default` | no | Marks the primary registry for short-id expansion; first entry is primary when none set it |

```toml
[[registries]]
alias = "acme"
url = "ghcr.io/acme"
default = true

[[registries]]
alias = "internal"
url = "registry.corp.example/team"
```

Project entries take precedence over global entries; duplicate URLs are
deduped, first occurrence wins.

Browse-set precedence (what `grim search`, `grim tui`, and `grim mcp`
browse):

1. `--registry` flag — collapses browse to exactly the registries it names.
   Repeatable and comma-separated (`--registry a,b` or `--registry a
   --registry b`); the first value is the primary (short-id default).
2. `[[registries]]` (project, then global) — authoritative when present;
   `GRIM_DEFAULT_REGISTRY` does **not** collapse or restrict this set.
3. Single-default fallback (no `[[registries]]` declared): `GRIM_DEFAULT_REGISTRY`
   → project `[options].default_registry` → global `[options].default_registry`
   → built-in `grim.ocx.sh`.

A config with no `[[registries]]` behaves exactly as before — the
`[options].default_registry` / `GRIM_DEFAULT_REGISTRY` / `--registry` /
built-in fallback chain still applies (see [Registry Resolution](#registry-resolution)).
Confirm with `grim --help` and `grim search --help`.

## Managing Config {#managing-config}

`grim config` (0.6.2+) reads and writes `grimoire.toml`, modeled on `git
config`, so you rarely hand-edit the file. It covers **settings**
(`[options]`, `[options.tui]`) and **named registries** (`[[registries]]`) —
but **not declarations** (`[skills]` / `[rules]` / `[agents]` / `[bundles]`),
which stay under `grim add` / `grim remove` because those must re-resolve the
lock on every change.

- **Settings** use dotted keys — `grim config get|set|unset <key>` and
  `grim config list`:

  ```sh
  grim config set   options.clients claude,opencode
  grim config set   options.tui.default_view tree
  grim config get   options.clients          # bare value on one line; exit 1 if unset
  grim config list                           # every explicitly-set key in this scope
  ```

- **Registries** use lifecycle verbs under `grim config registry`:

  ```sh
  grim config registry add acme --url ghcr.io/acme   # create an entry (--url required)
  grim config registry use acme                       # set default, clearing all others atomically
  grim config registry list                           # all entries in this scope
  grim config registry rm  acme
  ```

  `registry use` is the correct way to change the default registry.

Scope follows the usual rule — project by default, `--global` for the global
config, `--config <path>` for an explicit file; each invocation reads or
writes exactly one scope (never merged). `get` prints the bare value so it
scripts cleanly (`$(grim config get options.clients)`), exiting `1` when the
key is valid but unset. Add `--format json` to any subcommand for
machine-readable output.

**Every grim write is lossy**: comments and the `#:schema` directive are
stripped from `grimoire.toml` on any `grim config` / `grim add` / `grim
remove`. The full dotted-key list, JSON shapes, and exit codes live in the
[command reference][config-cmd] — never memorize them; confirm with `grim
config --help`.

## Qualified References {#qualified-references}

A `[[registries]]` alias enables the `alias/repo[:tag]` qualified form:

```sh
# with alias "acme" → "ghcr.io/acme"
grim add acme/code-review:1.2
# expands to: ghcr.io/acme/code-review:1.2

# with alias "internal" → "registry.corp.example/team"
grim add internal/lint-rules:stable
# expands to: registry.corp.example/team/lint-rules:stable
```

The separator is `/`, not `:` — the colon form (`alias:repo`) is not
treated as a qualified reference because it is indistinguishable from a
bare `repo:tag`. A leading segment that does not match any configured alias
is treated as a repository path component under the primary registry:
`acme/x:1` where `acme` is not an alias expands to
`<primary-registry>/acme/x:1`.

Short references (no `/`-prefix alias, no explicit registry) still expand
against the primary registry unchanged.

## Scopes

grim works in two scopes. The **project** scope is the `grimoire.toml`
discovered upward from the working directory — per-repository config
beside the code. The **global** scope is a single config at
`$GRIM_HOME/grimoire.toml` for artifacts you want everywhere.

Commands operate on the discovered project by default; `--global`
switches to the global scope (and `grim init --global` creates it).
Global-scope installs land in each client's *native* user-level
directory (for example `~/.claude/skills/`), so clients find them with
no extra configuration. The TUI flips scope at runtime with `g`.

## Client Targets

An installed artifact lands in a **client target**: `claude`,
`opencode`, or `copilot`, each receiving the artifact in its native
layout. `grim install` and `grim update` choose targets by precedence:

1. `--client <list>` flag (comma-separated: `--client claude,copilot`)
2. config `[options].clients` (TOML array of client names)
3. auto-detection — every client whose marker exists for the active
   scope (e.g. a `.claude/` directory in the project)
4. fallback to **all** clients when nothing is detected, so an install
   never silently targets zero clients or prefers one

The detected set is recomputed each run, never written back to config.
Pin `[options].clients` when you want deterministic targets in CI — set it
with `grim config set options.clients claude,opencode` (see [Managing
Config](#managing-config)).

## Offline Mode

grim is **online by default**: every floating-tag lookup resolves fresh
against the registry, and the result is cached write-through. A floating
tag therefore never serves a stale pin, and there is no "cache first"
mode to surprise you.

`--offline` (or `GRIM_OFFLINE`) flips to **cache-only**: all network
access is forbidden, and an operation that would need the registry fails
with exit 81 instead of silently degrading. Use it in sealed CI or
air-gapped networks. Warm the cache first with a normal online run:

```sh
grim lock              # online: resolve + cache everything declared
grim install --offline # later: cache-only, no network
```

The flag or env var are the only switches — there is no config-file
counterpart for offline.

## Search, TUI, and MCP {#search-tui-and-mcp}

`grim search [query]` splits the query on whitespace and ANDs the terms —
each term substring-matches (case-insensitive) any of an entry's kind,
repository, summary, description, or keywords. A bare kind keyword
(`skill`/`rule`/`bundle`, singular or plural) filters by kind instead of
matching as text; an empty query lists the whole catalog. Confirm the
match fields and kind-filter keywords with `grim search --help`. When
`[[registries]]` are configured, all
of them are browsed and flattened into one table. The catalog is cached
under `$GRIM_HOME` — pass `--refresh` to rebuild it from the registry,
`--registry` to collapse the browse to exactly the registries it names
(repeatable / comma-separated for several at once). Plain
output shows the one-line summary (truncated to the terminal); piped
output and `--format json` keep the full description, and JSON adds a
`repository` URL field for tooling.

```sh
grim search review
grim search --refresh --registry ghcr.io/acme --format json
```

A package the publisher has marked deprecated is flagged in both
`grim search` output (a `deprecated` marker on the entry, and a
`deprecated` field under `--format json`, which the `grim_search` MCP
tool inherits) and the TUI (a yellow `⚠` on the entry, with the notice
in the detail pane), so you can avoid pinning it.

`grim tui` browses your declared registries' catalogs interactively: kind-grouped list,
live install state, multi-select with batch install/update/delete, and a
detail pane per entry. Press `t` to toggle between the flat list and a
grouped collapsible tree view; the tree's opening mode and path-splitting
characters are configurable via `[options.tui]` in `grimoire.toml`
(`default_view`, `group_by_type`, `tree_separators` — set them with `grim
config set options.tui.<key>`, see [Managing Config](#managing-config)).
When `[[registries]]`
are configured, the TUI browses all of them, one collapsible root per
registry; with exactly one it elides that root. A `--registry` flag collapses
the browse to exactly the registries it names (repeatable /
comma-separated). `GRIM_DEFAULT_REGISTRY` does **not**
collapse the browse set — it is only the short-id resolution default and the
single-registry fallback when no `[[registries]]` is declared. When the active
scope has no `grimoire.toml` yet it offers to create one before starting via
popup dialogs (the registry input is pre-filled with the effective default
registry and the accepted value is persisted as a `[[registries]]` entry with
`default = true`; cancelling closes the TUI). Its install, update, and delete
actions go through the same seams as `grim add`/`install`/`uninstall`. Press
`?` inside for the full key map.

`grim mcp` runs a local [Model Context Protocol][mcp-spec] server over
STDIO. An AI agent host such as [Claude Code][claude-code] connects to it
and can call two read tools:

| Tool | What it returns |
|------|-----------------|
| `grim_search` | Same JSON as `grim search --format json`, over the configured registries (no registry override). Args: `query?`, `refresh?` |
| `grim_status` | Same JSON as `grim status --format json` for the fixed scope |

The server is read-only by default; `--allow-writes` enables the mutating
tools (`add` / `install` / `update` / `uninstall`) against the server's
fixed scope — leave it off for a browse-only server. Confirm the current
tool set with `grim mcp --help`. The scope (`--global` or
`--config <path>`) is fixed at startup — tool calls cannot redirect it.
Diagnostics go to stderr; stdout is the JSON-RPC channel. Register it
in a project `.mcp.json`:

```json
{ "mcpServers": { "grimoire": { "command": "grim", "args": ["mcp"] } } }
```

Confirm current flags with `grim mcp --help`.

> **Registry note**: catalog browse (`grim search` / TUI) depends on
> the registry exposing the `_catalog` endpoint. Registries such as GHCR,
> Docker Hub, and the GitLab Container Registry (SaaS) gate this endpoint
> — an empty browse result there is expected, not an error. Explicit-ref
> operations (install, add, release, publish) work on all registries. See
> [Registry compatibility][registry-compat] for the full table.

## Further Reading

- [Concepts: scopes][scopes], [clients][clients], and
  [online-by-default][online] — the semantics behind each section above.
- [Configuration][envvars] — environment variables, `[[registries]]`
  schema, precedence rules, data layout under `GRIM_HOME`.
- [Command reference: search][search], [tui][tui], and [mcp][mcp].

[scopes]: https://grimoire.rs/concepts.html#scopes
[clients]: https://grimoire.rs/concepts.html#clients
[online]: https://grimoire.rs/concepts.html#online-by-default-offline-on-demand
[envvars]: https://grimoire.rs/configuration.html#environment-variables
[registry-compat]: https://grimoire.rs/configuration.html#registry-compatibility
[config-cmd]: https://grimoire.rs/commands.html#config
[search]: https://grimoire.rs/commands.html#search
[tui]: https://grimoire.rs/commands.html#tui
[mcp]: https://grimoire.rs/commands.html#mcp
[mcp-spec]: https://spec.modelcontextprotocol.io/
[claude-code]: https://docs.anthropic.com/en/docs/claude-code
