# Registries, Scopes, and Targets

You loaded this file because you need to resolve which registry a short
reference hits, which scope a command edits, which AI clients an install
lands in, how offline mode behaves, or how to search a catalog.

Contents: [Registry Resolution](#registry-resolution) ·
[Multiple Registries](#multiple-registries) ·
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
3. project config `[options].default_registry`
4. global config `[options].default_registry`
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
the array is present it replaces the single-registry path: `grim search`
and the MCP server browse **all declared registries at once** instead of
one. The TUI currently browses a single registry and does not yet consume
`[[registries]]` — multi-registry TUI support is planned for a later
release.

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
deduped, first occurrence wins. An explicit `--registry` flag still
collapses the browse to exactly that one registry regardless of what is
declared.

A config with no `[[registries]]` behaves exactly as before — the
`[options].default_registry` / `GRIM_DEFAULT_REGISTRY` / `--registry` /
built-in fallback chain still applies (see [Registry Resolution](#registry-resolution)).
Confirm with `grim --help` and `grim search --help`.

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
Pin `[options].clients` when you want deterministic targets in CI.

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

`grim search [query]` matches a case-insensitive substring against each
catalog entry's repository, summary, description, and keywords; an empty
query lists the whole catalog. When `[[registries]]` are configured, all
of them are browsed and flattened into one table. The catalog is cached
under `$GRIM_HOME` — pass `--refresh` to rebuild it from the registry,
`--registry` to collapse the browse to exactly that one registry. Plain
output shows the one-line summary (truncated to the terminal); piped
output and `--format json` keep the full description, and JSON adds a
`repository` URL field for tooling.

```sh
grim search review
grim search --refresh --registry ghcr.io/acme --format json
```

`grim tui` browses a registry's catalog interactively: kind-grouped list,
live install state, multi-select with batch install/update/delete, and a
detail pane per entry. It browses a single registry (the effective default
registry from the precedence chain — flag, env, global config, or the
built-in fallback); multi-registry browse is planned for a future release.
When the active scope has no `grimoire.toml` yet it offers to create one
before starting via popup dialogs (the registry input is pre-filled with
the effective default registry and the accepted value is persisted as
`default_registry`; cancelling closes the TUI). Its install, update, and
delete actions go through the same seams as
`grim add`/`install`/`uninstall`. Press `?` inside for the full key map.

`grim mcp` runs a local [Model Context Protocol][mcp-spec] server over
STDIO. An AI agent host such as [Claude Code][claude-code] connects to it
and can call two read tools:

| Tool | What it returns |
|------|-----------------|
| `grim_search` | Same JSON as `grim search --format json`, over the configured registries (no registry override). Args: `query?`, `refresh?` |
| `grim_status` | Same JSON as `grim status --format json` for the fixed scope |

The server is read-only by default; `--allow-writes` reserves the gate
for mutating tools in a future release. The scope (`--global` or
`--config <path>`) is fixed at startup — tool calls cannot redirect it.
Diagnostics go to stderr; stdout is the JSON-RPC channel. Register it
in a project `.mcp.json`:

```json
{ "mcpServers": { "grimoire": { "command": "grim", "args": ["mcp"] } } }
```

Confirm current flags with `grim mcp --help`.

## Further Reading

- [Concepts: scopes][scopes], [clients][clients], and
  [online-by-default][online] — the semantics behind each section above.
- [Configuration][envvars] — environment variables, `[[registries]]`
  schema, precedence rules, data layout under `GRIM_HOME`.
- [Command reference: search][search], [tui][tui], and [mcp][mcp].

[scopes]: https://michael-herwig.github.io/grimoire/concepts.html#scopes
[clients]: https://michael-herwig.github.io/grimoire/concepts.html#clients
[online]: https://michael-herwig.github.io/grimoire/concepts.html#online-by-default-offline-on-demand
[envvars]: https://michael-herwig.github.io/grimoire/configuration.html#environment-variables
[search]: https://michael-herwig.github.io/grimoire/commands.html#search
[tui]: https://michael-herwig.github.io/grimoire/commands.html#tui
[mcp]: https://michael-herwig.github.io/grimoire/commands.html#mcp
[mcp-spec]: https://spec.modelcontextprotocol.io/
[claude-code]: https://docs.anthropic.com/en/docs/claude-code
