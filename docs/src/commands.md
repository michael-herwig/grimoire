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
| `--registry <ref>` | Registry for short identifiers and the browse set. Repeatable / comma-separated (`--registry a,b`); the first value is the default. |
| `--offline` | Disable all network access; work from the cache only and fail rather than reach a registry. |
| `--log-level <level>` | Override the tracing log level (`warn`, `info`, `debug`). |

## The lifecycle commands

| Command | Purpose |
|---------|---------|
| [`grim init`](#init) | Create a fresh `grimoire.toml`. |
| [`grim config`](#config) | Read and write `grimoire.toml` settings and registries. |
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
| [`grim mcp`](#mcp) | Run a local STDIO MCP server for AI agent integration. |

## grim init {#init}

Writes a fresh `grimoire.toml` in the current directory. `--registry <ref>`
seeds the default browse source as a `[[registries]]` entry with
`default = true` — the locator's shape picks the key: an index-shaped value
(`http(s)://`, `git+…`, `ssh://`, `git@…`, `….git`) is written as
`index = …`, anything else as a plain OCI `oci = …`. Without the flag, a
set `GRIM_DEFAULT_REGISTRY` is snapshotted the same way (the built-in
defaults are never written — they keep floating with the binary).
`--global` creates the global config at `$GRIM_HOME/grimoire.toml`
instead of a project-local one.

```sh
grim init --registry ghcr.io/acme
```

## grim config {#config}

`grim config` reads and writes `grimoire.toml`, modeled on [`git config`][git-config]. Before it existed, querying a setting or scripting a config change required hand-editing TOML and relying on the next command run to catch typos.

The command covers two areas of the file: **settings** (the `[options]` and `[options.tui]` tables) and **named registries** (the `[[registries]]` array). Declarations — the `[skills]`, `[rules]`, `[agents]`, and `[bundles]` tables — remain under [`grim add`](#add) and [`grim remove`](#remove), which must re-resolve the lockfile on every change.

Scope follows the same rule as every config-aware command: without a flag, `grim config` discovers and edits the project `grimoire.toml` by walking up from the working directory; `--global` targets `$GRIM_HOME/grimoire.toml`; `--config <path>` selects an explicit project file.

Every write re-runs registry validation before touching the file, so the at-most-one-`default` constraint and alias rules always hold. The serializer is shared with [`grim add`](#add) and [`grim remove`](#remove) — **comments and the `#:schema` directive are not preserved on any write**.

### Settings {#config-settings}

Four verbs operate on dotted keys:

```sh
grim config get   options.clients
grim config set   options.clients claude,opencode
grim config unset options.tui.default_view
grim config list
```

`get` prints the bare value on a single line with no key name or table header, so `$(grim config get options.clients)` works directly in shell. A valid-but-unset key exits `1` with no stdout — the same contract as [`git config`][git-config]: `grim config get options.clients || echo default`. An unknown key (typo or unsupported leaf) exits `64` without reading the config.

`set` and `unset` print a one-row confirmation table with `Action`, `Key`, `Value`, and `Scope` columns.

`list` shows every explicitly-set key and value for the active scope — keys at their default or absent values are omitted. Each invocation reads from exactly one scope, so origin is implicit in the scope flag used. Scopes are never merged: `grim config --global list` shows only global values, project `list` shows only project values.

The supported dotted keys are:

| Key | Value type | Notes |
|-----|------------|-------|
| `options.clients` | comma-separated client names | e.g. `claude,opencode`. Empty string clears the list. |
| `options.default_registry` | string | Legacy field — prefer `grim config registry use` for new configs. |
| `options.tui.default_view` | `flat` or `tree` | Other values exit `65`. |
| `options.tui.group_by_type` | `true` or `false` | `false` is the default; setting it to `false` removes the key, so a subsequent `get` exits 1 (consistent with `list`, which omits default values). |
| `options.tui.tree_separators` | comma-separated single-character strings | Each character must be non-control and non-whitespace; other values exit `65`. |
| `registry.<alias>.oci` | string | The registry entry must already exist. Mutually exclusive with `index` (setting it on an index entry exits `65`); unsettable only when `index` is set — else use `grim config registry rm <alias>`. The pre-0.7.0 field name `url` is accepted as an alias. |
| `registry.<alias>.index` | string | A [package-index](./package-index.md) locator (`http(s)://` base or git repository). Mutually exclusive with `oci` (same rules mirrored); a locator matching neither transport exits `65`. |
| `registry.<alias>.default` | `true` or `false` | Setting to `true` clears all other entries' `default` flag, the same as `grim config registry use`. |

Registry dotted keys require the entry to already exist — only `grim config registry add` creates entries. Passing `registry.<alias>` without a trailing field to `unset` removes the whole entry, equivalent to `grim config registry rm <alias>`.

### Registry lifecycle {#config-registry}

`grim config registry` manages the `[[registries]]` array through dedicated lifecycle verbs:

```sh
grim config registry add  acme --oci ghcr.io/acme
grim config registry add  acme --oci ghcr.io/acme --default
grim config registry add  hub  --index https://index.grimoire.rs
grim config registry use  acme     # mark as default; clears the prior default
grim config registry show acme     # print one registry's fields
grim config registry rm   acme
grim config registry list
```

`registry add` requires exactly one of `--oci` / `--index` — a registry
entry lists via the OCI `_catalog` endpoint, an index entry lists from a
[package index](./package-index.md). (`--url` remains a hidden alias for
`--oci` from before 0.7.0.) Adding an alias that already exists
exits `64` — update the locator with `grim config set
registry.<alias>.oci <new-ref>`, or remove and re-add.

`registry use` is the correct way to change the default registry. It sets the target entry's `default` flag and clears the flag on all others in one atomic write. Dotted `grim config set registry.<alias>.default true` routes through the same logic.

`registry list` shows all `[[registries]]` entries in the scope. Entries without an alias (locator-only entries hand-authored before aliases were introduced) appear with an empty `Alias` cell and are **not addressable by dotted key** — assign them an alias to manage them with `grim config`.

### JSON output {#config-json}

Add `--format json` to any subcommand for machine-readable output. The shapes are:

| Subcommand | JSON shape |
|-----------|------------|
| `get` (value set) | `{"key":"…","value":"…","set":true,"scope":"project"\|"global"}` |
| `get` (unset, exits 1) | `{"key":"…","value":null,"set":false,"scope":"project"\|"global"}` |
| `set` / `unset` / `registry add`, `rm`, `use` | `{"action":"…","key":"…","value":string or null,"scope":"…"}` |
| `list` | array of `{"key":"…","value":"…"}` |
| `registry list` | array of `{"alias":string or null,"url"\|"index":"…","default":bool}` |
| `registry show` | `{"alias":"…","url"\|"index":"…","default":bool}` |

The `action` field in write confirmations takes one of: `set`, `unset`, `registry-added`, `registry-removed`, `registry-default`. The `scope` field is `project` or `global`.

### Exit codes {#config-exit-codes}

| Situation | Code |
|-----------|------|
| Success | `0` |
| `get` of a valid-but-unset key (no stdout) | `1` |
| Unknown key name / missing or duplicate alias / bad subcommand args | `64` |
| Invalid value (bad enum, non-boolean, bad separator character) | `65` |
| Write or lock I/O failure | `74` |
| Concurrent write that can't acquire the config lock | `75` |
| Config file parse failure | `78` |
| Explicit `--config <path>` not found, or required config absent | `79` |

## grim add {#add}

`grim add [--kind <skill|rule|agent|bundle>] [--name <name>] <reference>`
declares a skill, rule, [agent](./agents.md), or bundle and immediately pins it
in the lock. `<reference>` is the only required argument —
`registry/repo:tag` or `registry/repo@sha256:…`.

When `--kind` is omitted, the kind is inferred from the artifact's
`com.grimoire.kind` manifest annotation set at release time (artifacts
published by older grim are still typed from their legacy `artifactType`). When
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

If the reference is [deprecated](./publishing.md#metadata-deprecated), `add`
prints the publisher's notice on stderr and still completes the add.

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
query lists the whole catalog. When `[[registries]]` are configured, all
of them are browsed and the results are flattened into one table.
`--refresh` forces a catalog rebuild; `--registry <ref>` collapses the
browse to exactly the registries it names — repeatable and comma-separated
(`--registry a,b` or `--registry a --registry b`), first value is primary.
`GRIM_DEFAULT_REGISTRY` is only the
short-id resolution default — it does not restrict the browse set when
`[[registries]]` is configured.

The plain table shows each entry's short summary (`com.grimoire.summary`),
falling back to the description when no summary is set. On an interactive
terminal that column is truncated to fit the width; piped output and
`--format json` keep the full description. The JSON output also carries a
`repository` field — the artifact's authored
[repository URL](./publishing.md#metadata-repository), or `null` when the
artifact has none.

A [deprecated](./publishing.md#metadata-deprecated) entry is flagged in the
`Status` cell with a comma-suffixed `deprecated` (e.g. `installed,deprecated`),
and JSON carries the notice in a `deprecated` field (`null` when the artifact
is not deprecated).

```sh
grim search review
grim search --refresh --registry ghcr.io/acme
```

## grim tui {#tui}

`grim tui` opens an interactive browser over your declared registries'
catalogs. It shows the catalog with live install state in colour, toggling
between a flat kind-grouped list and a collapsible tree (press `t`). When
more than one registry is configured, the flat list adds a leading **Registry**
column showing the configured alias (or the raw URL when no alias was set), and
the Repo cell is shortened to the registry-relative path so names stay readable.
It supports multi-select with batch install, update, and delete. Press `?` in the TUI
for the full key map; highlights are `t` to toggle tree/flat view, `v` to
pick a version, `o` to open the selected entry's repository URL in the
browser, `g` to switch scope, and `space` to mark rows.

**Tree view** — pressing `t` switches the catalog between flat list mode and
a collapsible tree grouped by registry host and repository path. In tree mode:

| Key | Action |
|-----|--------|
| `t` | Toggle between flat list and tree view. |
| `→` | Expand the selected group (reveal its children). Tree mode only. |
| `←` | Collapse the selected group. On an already-collapsed group or on a leaf entry, jump to the parent group instead (ARIA-style navigation). Tree mode only. |
| `Enter` on a group | Fold or unfold the group (same as `→`/`←` toggle); on a leaf entry, open the detail pane as usual. |
| `space` on a group | Mark every descendant leaf in the subtree. The group's mark glyph turns filled (`▣`) when all descendants are marked. |
| `i` / `u` / `d` on a group | Install, update, or uninstall every leaf in the subtree (when no other rows are individually marked). Batch behavior follows the same selection precedence as the flat view. |

Each group row shows a rollup glyph reflecting the worst install state of
its descendants — `↑` when any descendant is outdated, `✱` when any is
locally modified, and so on — so a collapsed tree still surfaces what needs
attention.

**Compact namespaces** — a run of namespace segments that never branches
collapses into one row whose label is the joined path, the same idea as [VS
Code's "compact folders"][vscode-compact] folding `a/b/c` when each level
holds a single child. The join merges namespace groups into each other only —
never a namespace into the package row directly below it — and stops where the
path branches, so a registry holding only `acme/team/skills/lint` and
`acme/team/skills/fmt` shows `acme/team/skills` as one group above the `lint`
and `fmt` leaves. A registry root always keeps its own row.

**Bundle member expansion** — when the selected row is a bundle leaf, pressing
`→` (or `Enter`) reveals its members as indented child rows badged
`(via bundle)`. Member rows are read-only: they reflect what a bundle
declares, derived from the registry (or the lock snapshot when offline).
Bundle members cannot be individually marked, installed, or uninstalled from
the tree — use the parent bundle row for batch operations.

An active search (started with `/`) reveals matching entries even when their
parent group is collapsed — the tree stays navigable in search mode and does
not force a switch to flat view.

Three config fields under `[options.tui]` in `grimoire.toml` let you set
the opening view mode and control how paths are split into groups. See
[`[options.tui]`][options-tui] for the full reference.

Like `grim search`, the TUI browses **every** registry declared in
`[[registries]]`, grouping entries under one collapsible root per registry.
When exactly one registry resolves, its root prefix is elided to keep names
short; with several, the roots are ordered by resolution precedence, and a
registry that is empty or offline still appears as an empty `0/0` root so the
full configured set stays visible. An explicit `--registry` flag collapses the
browse to exactly the registries it names — repeatable and comma-separated
for several at once. `GRIM_DEFAULT_REGISTRY` is only the
short-id resolution default — it does not collapse the browse set when
`[[registries]]` is configured; in that case both `grim search` and `grim tui`
browse all declared registries regardless of whether the env var is set.

When the active scope has no `grimoire.toml` yet, the TUI offers to create
one before starting, as popup dialogs: confirm the init, then accept or
edit the browse source. The input is pre-filled with the effective browse
primary — the `--registry` flag, then the configured `[[registries]]`
primary / legacy default chain, then the built-in fallback **index**
`https://index.grimoire.rs` — and the accepted value is persisted as a
`[[registries]]` entry with `default = true` in the new config, keyed
`index` or `oci` by the locator's shape (clearing the input seeds
nothing). Cancelling closes the TUI.

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
needs `--kind agent` — a bare `.md` packs as a rule. `--git` embeds
[git provenance](./publishing.md#git-provenance) (commit revision, commit
date, and the `origin` remote) so the preflight reflects what a release would
stamp.

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
digests. `--git` embeds [git provenance](./publishing.md#git-provenance)
(commit revision, date, and `origin` remote) as OCI annotations; it is
off by default so an ordinary re-release stays idempotent. See
[Publishing](./publishing.md) for the full workflow.

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
`--git` embeds [git provenance](./publishing.md#git-provenance) on every
published entry (forwarded to each `release`); a non-git path fails (65).
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

## grim mcp {#mcp}

`grim mcp` runs a local [Model Context Protocol][mcp-spec] server over
STDIO. An AI agent host — [Claude Code][claude-code], [OpenCode][opencode],
or any [MCP][mcp-spec]-compatible client — connects to it over stdin/stdout
and gains structured access to Grimoire's catalog and install state without
running shell commands.

The server is **read-only by default**. Mutating tools (add, install,
update, uninstall) are gated behind `--allow-writes` and are not yet
registered; the flag reserves the gate for a later release.

The install **scope is fixed at server start**: `--global` operates on the
global scope; `--config <path>` points at a specific project config.
Individual tool calls cannot redirect the scope.

Because stdout carries the [JSON-RPC][json-rpc] channel, the server writes
no diagnostic output there — all tracing goes to stderr. The server shuts
down when the client closes stdin (EOF).

| Flag | Effect |
|------|--------|
| `--allow-writes` | Enable mutating tools when they land (currently no-op — server is read-only). |
| `--global` | Fix the scope to the global config for the server's lifetime. |
| `--config <path>` | Use an explicit project config (scope resolution for status tools). |

**Tools exposed today:**

| Tool | Description | Equivalent CLI |
|------|-------------|----------------|
| `grim_search` | Browse/search the configured registries (no registry override — the configured set is the boundary). Args: `query?`, `refresh?`. | `grim search --format json` |
| `grim_status` | Install status of every declared artifact in the fixed scope. | `grim status --format json` |

The JSON payload each tool returns is identical to the `--format json`
output of the corresponding command — one source of truth for both the CLI
and the MCP surface.

**Registering with Claude Code** — add to `.mcp.json` in the project root
(or register globally via `claude mcp add`):

```json
{
  "mcpServers": {
    "grimoire": {
      "command": "grim",
      "args": ["mcp"]
    }
  }
}
```

Pass `--global` to the `args` array when you want the server to operate on
the global scope rather than the discovered project:

```json
{
  "mcpServers": {
    "grimoire": {
      "command": "grim",
      "args": ["mcp", "--global"]
    }
  }
}
```

<!-- internal -->
[global-options]: #global-options
[options-tui]: ./configuration.md#options-tui

<!-- external -->
[git-config]: https://git-scm.com/docs/git-config
[vscode-compact]: https://code.visualstudio.com/docs/getstarted/userinterface
[mcp-spec]: https://spec.modelcontextprotocol.io/
[claude-code]: https://docs.anthropic.com/en/docs/claude-code
[opencode]: https://opencode.ai/
[json-rpc]: https://www.jsonrpc.org/specification
