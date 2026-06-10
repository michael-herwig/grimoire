# Command Reference

Every command follows the same shape: parse references into typed values, run
the operation, and report what actually happened. Structured output renders as
an aligned table by default or as JSON with `--format json`, so the same
command serves humans and scripts.

Run `grim <command> --help` for the authoritative, always-current flag list.

## Global options

These apply to every subcommand:

| Flag | Effect |
|------|--------|
| `--format <plain\|json>` | Output format for structured results (default `plain`). |
| `--global` | Operate on the global scope instead of the discovered project. |
| `--config <path>` | Use an explicit project config file. |
| `--registry <ref>` | Default registry for short identifiers. |
| `--offline` | Disable all network access; work from the cache only and fail rather than reach a registry. |
| `--log-level <level>` | Override the tracing log level (`warn`, `info`, `debug`). |

## The lifecycle commands

| Command | Purpose |
|---------|---------|
| [`grim init`](#init) | Create a fresh `grimoire.toml`. |
| [`grim add`](#add) | Declare a skill/rule and lock it. |
| [`grim lock`](#lock) | Resolve declared floating tags to pinned digests. |
| [`grim install`](#install) | Materialize the locked artifacts into your AI client(s). |
| [`grim update`](#update) | Re-resolve floating tags and re-materialize changes. |
| [`grim status`](#status) | Report the state of every declared artifact. |
| [`grim remove`](#remove) | Undeclare an artifact (config + lock only). |
| [`grim uninstall`](#uninstall) | Fully remove an artifact (files + record + config). |
| [`grim search`](#search) | Search the registry catalog. |
| [`grim tui`](#tui) | Browse the catalog interactively. |
| [`grim build`](#build) | Validate and pack a local artifact. |
| [`grim release`](#release) | Validate, pack, and push an artifact. |
| [`grim login`](#login) | Authenticate to a registry and store the credential. |
| [`grim logout`](#logout) | Remove a stored registry credential. |

## grim init {#init}

Writes a fresh `grimoire.toml` in the current directory. `--registry <ref>`
seeds the `default_registry` option; `--global` creates the global config at
`$GRIM_HOME/grimoire.toml` instead of a project-local one.

```sh
grim init --registry ghcr.io/acme
```

## grim add {#add}

`grim add [--kind <skill|rule|bundle>] [--name <name>] <reference>` declares a
skill, rule, or bundle and immediately pins it in the lock. `<reference>` is the
only required argument — `registry/repo:tag` or `registry/repo@sha256:…`.

When `--kind` is omitted, the kind is inferred from the artifact's OCI
`artifactType` (`application/vnd.grimoire.<kind>.v1`) set at release time. When
`--name` is omitted, the binding name defaults to the reference's last path
segment. If the kind cannot be inferred (for example, a non-Grimoire image),
`add` errors and asks you to supply `--kind` explicitly.

```sh
grim add ghcr.io/acme/code-review:1
grim add --kind rule --name rust-style ghcr.io/acme/rust-style:2
grim add --kind bundle ghcr.io/acme/python-stack:1
```

Adding a [bundle](./concepts.md#bundles) declares it in `[bundles]` and expands
its members into the lock. `grim remove bundle <name>` undeclares the bundle and
drops the members it contributed.

## grim lock {#lock}

Resolves the floating tags declared in `grimoire.toml` to concrete digests and
writes `grimoire.lock`. Run it after editing the config by hand; `grim add`
already locks what it declares.

## grim install {#install}

Materializes every locked artifact into your AI clients' configuration
directories. `--client <list>` selects AI clients (`claude`, `opencode`,
`copilot`, comma-separated), overriding the config `clients` option (default
`["claude"]`). `--force` overwrites a locally modified artifact instead of
refusing it.

```sh
grim install
grim install --client claude,copilot
```

## grim update {#update}

`grim update [names…]` re-resolves floating tags, rolls the lock forward, and
re-materializes only what changed. With no names it updates everything; pass
binding names to scope it. Shares `--client` and `--force` with install.

```sh
grim update
grim update code-review rust-style
```

Because update reconciles the workspace to the freshly-resolved lock, it also
**prunes** artifacts that have dropped out of the lock — most often a
[bundle](./concepts.md#bundles) member that the bundle stopped including. A
clean, unmodified orphan is deleted (files and install record) and reported with
the `removed` action. An orphan you have edited locally is **kept** and reported
as `kept-modified`, so an accidental bundle change never silently discards your
work; re-run with `--force` to prune it anyway. This mirrors the install
integrity gate, where a locally modified artifact is refused rather than
overwritten without `--force`.

Pruning happens only on `update`. `grim install` materializes the current lock
but never deletes — like [`grim remove`](#remove), it leaves files on disk.

## grim status {#status}

Reports each declared artifact's state — installed, outdated, locally modified,
integrity-missing, or not installed. The `Source` column shows each artifact's
[provenance](./concepts.md#bundles): `direct` or the bundle it came from. Pair
with `--format json` to drive automation.

## grim remove {#remove}

`grim remove <kind> <name>` undeclares an artifact from `grimoire.toml` and the
lock. It leaves already-installed files on disk — use
[`grim uninstall`](#uninstall) to remove those too.

## grim uninstall {#uninstall}

`grim uninstall <kind> <name>` is the full inverse of install: it deletes the
materialized files, drops the install record, and undeclares the artifact from
the config and lock. The interactive TUI's delete action reuses the same seam.

## grim search {#search}

`grim search [query]` searches the registry catalog by case-insensitive
substring against repository, description, and keywords; an empty query lists
the whole catalog. `--refresh` forces a catalog rebuild; `--registry <ref>`
chooses which registry to search.

```sh
grim search review
grim search --refresh --registry ghcr.io/acme
```

## grim tui {#tui}

`grim tui` opens an interactive browser over a registry's catalog. It groups
entries into a collapsible tree by registry and path, shows live install state
in colour, and supports multi-select with batch install, update, and delete.
Press `?` in the TUI for the full key map; highlights are `t` to toggle the
tree, `v` to pick a version, `g` to switch scope, and `space` to mark rows.

```sh
grim tui --registry ghcr.io/acme
```

## grim build {#build}

`grim build <path>` validates and packs a local skill directory, rule `.md`
file, or bundle `.toml` file without pushing it — a dry run for authors.
`--kind <skill|rule|bundle>` forces the artifact kind instead of auto-detecting
it from the path.

## grim release {#release}

`grim release <path> <reference>` validates, packs, and pushes an artifact.
A full semver reference (e.g. `1.2.3`) applies cascade tags — `1.2.3`, `1.2`,
`1`, and `latest` are all moved. A non-version tag (e.g. `canary`, `edge`)
publishes only that exact tag with no cascade. A reference with no tag at all
is an error. `--dry-run` prints the push plan without pushing; `--force`
moves an existing exact-version tag that points at a different digest. A
`.toml` path publishes a [bundle](./concepts.md#bundles); `--pin` then freezes
its floating members to digests. See [Publishing](./publishing.md) for the full
workflow.

```sh
grim release ./code-review ghcr.io/acme/code-review:1.2.3 --dry-run
grim release ./python-stack.toml ghcr.io/acme/python-stack:1.0.0 --pin
```

## grim login {#login}

`grim login [registry]` authenticates to a registry and stores the credential
in the Docker-compatible credential store, so later pulls and pushes reuse it.
Pass the username with `-u`/`--username` (prompted on a terminal when omitted)
and the password via `--password-stdin` or a hidden terminal prompt — there is
no `--password <value>` flag, by design. `--allow-insecure-store` permits a
base64 plaintext entry when no credential helper is configured. With no
positional `registry`, it resolves `--registry`, then `default_registry`, then
`GRIM_DEFAULT_REGISTRY`. See [Authentication](./authentication.md) for storage
details.

```sh
echo "$TOKEN" | grim login ghcr.io -u alice --password-stdin
```

## grim logout {#logout}

`grim logout [registry]` removes a stored credential. It is idempotent —
logging out when nothing is stored exits `0` — and resolves the registry the
same way [`grim login`](#login) does.

```sh
grim logout ghcr.io
```
