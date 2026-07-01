# Consumer Lifecycle

You loaded this file because you are installing, updating, or removing
artifacts in a project with grim — the init → add → lock → install →
update loop and the two files it maintains.

Contents: [The Loop](#the-loop) · [The Two Files](#the-two-files) ·
[Declaring](#declaring) · [Installing](#installing) ·
[Updating](#updating) · [Inspecting](#inspecting) ·
[Removing](#removing) · [Bundles](#bundles)

Flags shown here are grim 0.6.x; confirm with `grim <cmd> --help` before
relying on one.

## The Loop

A complete first session, start to installed:

```sh
grim init --registry ghcr.io/acme        # write grimoire.toml
grim add ghcr.io/acme/code-review:1      # declare + pin in the lock
grim install                             # materialize into AI clients
grim status                              # confirm what landed
```

From then on the steady state is `grim update` to roll floating tags
forward and `grim status` to see where you stand.

## The Two Files

`grimoire.toml` is the declaration: an optional `[[registries]]` array (the
canonical way to set a default registry via a `default = true` entry, or
`grim config registry use <alias>`), an `[options]` table for other defaults
(`clients`, `[options.tui]` for the interactive browser), and `[skills]` /
`[rules]` / `[agents]` / `[bundles]` tables mapping a binding name to a
reference. Manage settings and registries with `grim config` (0.6.2+) rather
than by hand — see [registries.md](registries.md#managing-config); it never
touches the declaration tables, which stay under `grim add` / `grim remove`.
You may still edit by hand (run `grim lock` afterwards), but note that any
grim write strips comments and the `#:schema` directive.

`grimoire.lock` pins every declared tag to an exact digest and records a
hash of the declaration it came from, so drift is detectable. It is
machine-owned — never edit it, but **do commit it** beside
`grimoire.toml` so installs are reproducible for everyone. Full shape of
both files: [Configuration][config-toml].

## Declaring

`grim add <reference>` declares an artifact and immediately pins it in
the lock. The reference is the only required argument:

```sh
grim add ghcr.io/acme/code-review:1
grim add --kind rule --name rust-style ghcr.io/acme/rust-style:2
grim add --kind bundle ghcr.io/acme/python-stack:1
```

- `--kind` (skill, rule, agent, bundle) is normally inferred from the
  artifact's OCI `artifactType`, set at release time. If grim cannot
  infer it (a non-Grimoire image), `add` errors and asks for `--kind`.
- `--name` defaults to the reference's last path segment.

If the reference is deprecated, `add` prints the publisher's deprecation
notice on stderr and still completes the add — treat it as a prompt to
look for a successor, not a failure.

`grim lock` re-resolves the floating tags declared in `grimoire.toml`
and rewrites the lock. You need it only after hand-editing the config —
`grim add` already locks what it declares.

## Installing

`grim install` materializes every locked artifact into your AI clients'
configuration directories. It writes into the clients selected by
`--client`, the config's `clients` option, or auto-detection (details in
[registries.md](registries.md)):

```sh
grim install
grim install --client claude,copilot
```

Install never deletes anything, and it refuses to overwrite an artifact
you have modified locally — pass `--force` to overwrite deliberately.
See [troubleshooting.md](troubleshooting.md) for the integrity gate.

## Updating

`grim update [names…]` re-resolves floating tags, rolls the lock
forward, and re-materializes only what changed. With no names it updates
everything; pass binding names to scope it:

```sh
grim update
grim update code-review rust-style
```

Update is also the only command that **prunes**: an artifact that
dropped out of the lock (most often a bundle member the bundle stopped
including) is deleted and reported as `removed` — unless you edited it
locally, in which case it is kept and reported as `kept-modified` until
you re-run with `--force`. Your local edits are never silently
discarded.

## Inspecting

`grim status` reports each declared artifact's state — installed,
outdated, locally modified, integrity-missing, or not installed. The
`Source` column shows provenance: `direct`, or the bundle the artifact
came from. Pair with `--format json` to drive automation.

## Removing

Two commands with deliberately different depths:

| Command | Config + lock | Installed files |
|---|---|---|
| `grim remove <kind> <name>` | undeclared | left on disk |
| `grim uninstall <kind> <name>` | undeclared | deleted, record dropped |

`remove` only undeclares; `uninstall` is the full inverse of install.
Both act on the **effective** declaration, fully offline: if a declared
bundle still names the artifact at the same identifier, the lock entry
survives via the bundle (and `uninstall` still deletes the files — the
next `grim install` rematerializes them). When the surviving bundle
binds a *different* identifier, grim drops the entry, leaves the lock
stale, and tells you to run `grim lock` — never a silently wrong pin.

## Bundles

A bundle is a published, curated set of members. Declare it once and it
**expands** into its member skills, rules, and agents at lock time, each
pinned like a direct declaration and tagged with the bundle as its
provenance:

```sh
grim add --kind bundle ghcr.io/acme/python-stack:1
grim install
```

Membership tracks the published bundle: a new bundle version that adds a
member expands it on the next `grim lock`; one that drops a member
removes it from the lock, and `grim update` prunes its files (subject to
`kept-modified` above).

Conflicts on the same `(kind, name)` slot resolve deterministically: a
direct declaration always wins over any bundle (the override mechanism),
agreeing bundles coalesce, and disagreeing bundles **fail closed** with
a conflict error at lock time — declare the member directly to pick a
winner. `grim remove bundle <name>` undeclares the bundle and drops only
the members no other declaration still holds.

## Further Reading

- [Quickstart][quickstart] — the same loop as a guided walk.
- [Command reference][commands] — per-command pages with current flags.
- [Concepts: the lock][lock] and [bundles][bundles] — semantics in full.
- [Configuration][config-toml] — `grimoire.toml` and `grimoire.lock`
  shape, scopes on disk.

[quickstart]: https://grimoire.rs/quickstart.html
[commands]: https://grimoire.rs/commands.html
[lock]: https://grimoire.rs/concepts.html#the-lock
[bundles]: https://grimoire.rs/concepts.html#bundles
[config-toml]: https://grimoire.rs/configuration.html
