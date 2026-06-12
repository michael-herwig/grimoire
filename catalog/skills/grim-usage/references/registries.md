# Registries, Scopes, and Targets

You loaded this file because you need to resolve which registry a short
reference hits, which scope a command edits, which AI clients an install
lands in, how offline mode behaves, or how to search a catalog.

Contents: [Registry Resolution](#registry-resolution) ·
[Scopes](#scopes) · [Client Targets](#client-targets) ·
[Offline Mode](#offline-mode) · [Search and TUI](#search-and-tui)

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

## Search and TUI

`grim search [query]` matches a case-insensitive substring against each
catalog entry's repository, summary, description, and keywords; an empty
query lists the whole catalog. The catalog is cached under `$GRIM_HOME`
— pass `--refresh` to rebuild it from the registry, `--registry` to pick
which registry to search. Plain output shows the one-line summary
(truncated to the terminal); piped output and `--format json` keep the
full description, and JSON adds a `repository` URL field for tooling.

```sh
grim search review
grim search --refresh --registry ghcr.io/acme --format json
```

`grim tui` browses the same catalog interactively: kind-grouped list,
live install state, multi-select with batch install/update/delete, and a
detail pane per entry. When the active scope has no `grimoire.toml` yet
it offers to create one before starting (registry prompt pre-filled from
`GRIM_DEFAULT_REGISTRY`; cancelling closes the TUI). Its install, update, and delete actions go
through the same seams as `grim add`/`install`/`uninstall`, so nothing
the TUI does is special. Press `?` inside for the full key map rather
than memorizing keys from any guide.

## Further Reading

- [Concepts: scopes][scopes], [clients][clients], and
  [online-by-default][online] — the semantics behind each section above.
- [Configuration][envvars] — environment variables, precedence rules,
  data layout under `GRIM_HOME`.
- [Command reference: search][search] and [tui][tui].

[scopes]: https://michael-herwig.github.io/grimoire/concepts.html#scopes
[clients]: https://michael-herwig.github.io/grimoire/concepts.html#clients
[online]: https://michael-herwig.github.io/grimoire/concepts.html#online-by-default-offline-on-demand
[envvars]: https://michael-herwig.github.io/grimoire/configuration.html#environment-variables
[search]: https://michael-herwig.github.io/grimoire/commands.html#search
[tui]: https://michael-herwig.github.io/grimoire/commands.html#tui
