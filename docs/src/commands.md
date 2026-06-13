# Command Reference

Every command follows the same shape: parse references into typed values, run
the operation, and report what actually happened. Structured output renders as
an aligned table by default or as JSON with `--format json`, so the same
command serves humans and scripts.

Run `grim <command> --help` for the authoritative, always-current flag list.

## Global options {#global-options}

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
| [`grim add`](#add) | Declare a skill/rule/agent and lock it. |
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
| [`grim publish`](#publish) | Validate and batch-release all packages from a manifest. |
| [`grim login`](#login) | Authenticate to a registry and store the credential. |
| [`grim logout`](#logout) | Remove a stored registry credential. |
| [`grim schema`](#schema) | Print the JSON Schema for `grimoire.toml` or `publish.toml`. |

## grim init {#init}

Writes a fresh `grimoire.toml` in the current directory. `--registry <ref>`
seeds the `default_registry` option; without the flag, a set
`GRIM_DEFAULT_REGISTRY` is snapshotted into the option instead (the built-in
default registry is never written — it keeps floating with the binary).
`--global` creates the global config at `$GRIM_HOME/grimoire.toml` instead
of a project-local one.

```sh
grim init --registry ghcr.io/acme
```

## grim add {#add}

`grim add [--kind <skill|rule|agent|bundle>] [--name <name>] <reference>`
declares a skill, rule, [agent](./agents.md), or bundle and immediately pins it
in the lock. `<reference>` is the only required argument —
`registry/repo:tag` or `registry/repo@sha256:…`.

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
drops the members it contributed — a member another still-declared bundle also
contributes only loses this bundle's provenance entry and stays locked.

## grim lock {#lock}

Resolves the floating tags declared in `grimoire.toml` to concrete digests and
writes `grimoire.lock`. Run it after editing the config by hand; `grim add`
already locks what it declares.

## grim install {#install}

Materializes every locked artifact into your AI clients' configuration
directories. `--client <list>` selects AI clients (`claude`, `opencode`,
`copilot`, comma-separated), overriding the config `clients` option. When
neither selects a client, the **detected** clients for the scope are
targeted — every client whose vendor directory or marker is present —
falling back to all clients when none are detected. `--force` overwrites a
locally modified artifact instead of refusing it.

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

Removal acts on the **effective** declaration, fully offline: the lock entry
is dropped only when no remaining declaration holds the artifact. Removing a
direct declaration while a declared bundle still names the artifact at the
*same* identifier keeps the entry — its provenance flips to the bundle. If
the bundle names it at a *different* identifier, the correct pin cannot be
derived offline: the entry is dropped, the lock is left stale, and grim tells
you to run [`grim lock`](#lock) — never a silently incomplete fresh lock.

## grim uninstall {#uninstall}

`grim uninstall <kind> <name>` is the full inverse of install: it deletes the
materialized files, drops the install record, and undeclares the artifact from
the config and lock. The interactive TUI's delete action reuses the same seam.

The lock follows the same effective-declaration rule as
[`grim remove`](#remove): when a declared bundle still names the artifact at
the same identifier, the files are deleted (that is what you asked for) but
the lock entry survives via the bundle — the next `grim install`
rematerializes it.

## grim search {#search}

`grim search [query]` searches the registry catalog by case-insensitive
substring against repository, summary, description, and keywords; an empty
query lists the whole catalog. `--refresh` forces a catalog rebuild;
`--registry <ref>` chooses which registry to search.

The plain table shows each entry's short summary (`com.grimoire.summary`),
falling back to the description when no summary is set. On an interactive
terminal that column is truncated to fit the width; piped output and
`--format json` keep the full description. The JSON output also carries a
`repository` field — the artifact's authored
[repository URL](./publishing.md#metadata-repository), or `null` when the
artifact has none.

```sh
grim search review
grim search --refresh --registry ghcr.io/acme
```

## grim tui {#tui}

`grim tui` opens an interactive browser over a registry's catalog. It shows
a flat, kind-grouped list with live install state in colour, and supports
multi-select with batch install, update, and delete. Press `?` in the TUI
for the full key map; highlights are `v` to pick a version, `o` to open
the selected entry's repository URL in the browser, `g` to switch scope,
and `space` to mark rows.

When the active scope has no `grimoire.toml` yet, the TUI offers to create
one before starting, as popup dialogs: confirm the init, then accept or
edit the registry to seed `default_registry` with. The input is pre-filled
with the effective default — the `--registry` flag, then
`GRIM_DEFAULT_REGISTRY`, then the global config, then the built-in
`grim.ocx.sh` fallback — and the accepted value is persisted in the new
config (clearing the input seeds nothing). Cancelling closes the TUI.

`enter` opens the detail pane for the selected row: the centered artifact
reference, its `Summary:` and `Description:` sections, and a `Metadata:`
block with the keywords and the
[repository URL](./publishing.md#metadata-repository) (version and install
status stay on the catalog row). While the pane is open, `↑`/`↓` (or
`j`/`k`) scroll it instead of moving the selection; `esc` returns to the
list. `pgup`/`pgdn` scroll the pane from any mode — no need to open it
first. Scrolling is clamped at both ends: it saturates at the top and
stops when the content's last line reaches the pane's bottom edge.

A TUI install or update goes through the same seams as the commands: it
declares the entry in the active scope's `grimoire.toml` and relocks it (like
[`grim add`](#add)), then materializes just that artifact (like
[`grim install`](#install)). Delete is the full inverse via the
[`grim uninstall`](#uninstall) seam. Installing a version older than the
registry's latest flips the row to `outdated` right after the install
completes.

A bundle row works the same way at the bundle level. Install declares it
under `[bundles]`, expands it into its members (like
`grim add --kind bundle`), and materializes exactly those members; the row's
state aggregates the member states. Delete removes the member files and
records, evicts the members from the lock, and undeclares the bundle. A
member shared with another still-declared bundle is spared: its files stay
on disk and its lock entry only loses the deleted bundle's provenance.

```sh
grim tui --registry ghcr.io/acme
```

## grim build {#build}

`grim build <path>` validates and packs a local skill directory, rule `.md`
file, [agent](./agents.md) `.md` file, or bundle `.toml` file without pushing
it — a dry run for authors. `--kind <skill|rule|agent|bundle>` forces the
artifact kind instead of auto-detecting it from the path. An agent always
needs `--kind agent` — a bare `.md` packs as a rule.

## grim release {#release}

`grim release <path> <reference>` validates, packs, and pushes an artifact.
A full semver reference (e.g. `1.2.3`) applies cascade tags — `1.2.3`, `1.2`,
`1`, and `latest` are all moved. A non-version tag (e.g. `canary`, `edge`)
publishes only that exact tag with no cascade. A reference with no tag at all
is an error. `--dry-run` prints the push plan without pushing; `--force`
moves an existing exact-version tag that points at a different digest;
`--skip-existing` (conflicts with `--force`) turns a release whose
exact-version tag already exists into a success no-op that pushes nothing —
for manifest-driven publishers that re-run blanket releases and only want
bumped versions pushed. A `.toml` path publishes a
[bundle](./concepts.md#bundles); `--pin` then freezes its floating members to
digests. See [Publishing](./publishing.md) for the full workflow.

Pointing `grim release` at a `publish.toml` (a file with a top-level
`registry` key) produces a hint to use `grim publish` instead. The mirror
also holds: pointing `grim publish` at a bundle TOML (flat `name = "reference"`
entries) produces a hint to use `grim release --kind bundle`.

```sh
grim release ./code-review ghcr.io/acme/code-review:1.2.3 --dry-run
grim release ./python-stack.toml ghcr.io/acme/python-stack:1.0.0 --pin
```

## grim publish {#publish}

`grim publish` reads a `publish.toml` manifest and releases every declared
package in kind order (skills → rules → agents → bundles, alphabetical
within kind). It validates the whole manifest before any push, then
composes [`grim release`](#release) per entry.

The default behavior skips entries whose exact-version tag already exists,
making the command idempotent: re-running after a partial failure pushes only
the remaining entries. Pass `--force` to move existing exact-version tags
instead. The two modes are mutually exclusive.

`--dry-run` validates the manifest and prints the full push plan without
touching the registry. `--only <name>` (repeatable) filters to a single
entry; a name absent from the manifest exits 65. `--tag <tag>` overrides
the published tag with a movable channel tag (e.g. `canary`); semver values
are rejected with exit 65, keeping all semver releases in the manifest. A
channel tag always moves on re-publish — no skip, no `--force` needed.
`--manifest <path>` selects a manifest other than the default `./publish.toml`.
The [global `--registry` flag][global-options] overrides the manifest's
`registry` value for staging runs or acceptance tests without editing the file.
`GRIM_DEFAULT_REGISTRY` and the config-file `default_registry` do **not**
override the manifest — the manifest's `registry` field is explicit input, and
only the flag tier wins.

Exit codes from the release path propagate per entry. Validation failures
exit 65 (data error). The report renders for all completed entries plus
the first failed entry; re-run with `--only` for surgical recovery.

```sh
grim publish --dry-run
grim publish
grim publish --only grim-usage
grim publish --tag canary
```

See [Batch publishing with a manifest](./publishing.md#batch-publish) for
the manifest schema, source layout conventions, and disambiguation from
bundle files.

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

## grim schema {#schema}

`grim schema --kind <config|publish>` prints a [JSON
Schema](https://json-schema.org/) for one of the two author-facing TOML files
to stdout. `--kind config` describes `grimoire.toml`; `--kind publish`
describes `publish.toml`. The schema is generated from grim's own parser, so it
accepts exactly what grim accepts.

```sh
grim schema --kind config > grimoire-config.schema.json
grim schema --kind publish | jq .title
```

The same schemas are published to the docs site; see [Editor schema
support](./configuration.md#editor-schema) for the hosted URLs and the
`#:schema` directive that wires an editor up to them.

<!-- internal -->
[global-options]: #global-options
