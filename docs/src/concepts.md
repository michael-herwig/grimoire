# Concepts

Grimoire borrows its mental model from package managers you already use, then
swaps the transport for an [OCI registry][oci]. This page covers the handful of
ideas that make the [commands](./commands.md) feel obvious.

## Skills and rules

Grimoire distributes two kinds of artifact. A **skill** is a directory — a
`SKILL.md` plus any supporting scripts or references — that teaches an agent a
capability. A **rule** is a single Markdown file that states a standard or
constraint the agent should always follow.

Both are declared the same way and travel through the same pipeline; the only
difference is shape on disk (a folder versus one file) and the `kind` argument
(`skill` or `rule`) you pass to commands like [`grim add`](./commands.md#add).

## Artifacts as OCI content

Under the hood every skill or rule is packed into an OCI artifact and addressed
by content digest, exactly like a container image layer. Identical content is
stored once and is immutable: a `sha256:…` digest always names the same bytes.

This is why Grimoire needs no server of its own. Any registry that speaks the
[distribution spec][oci] — [GHCR][ghcr], [Docker Hub][hub], a private
[Distribution][dist] — is a complete backend.

## References, tags, and digests

You name an artifact with a reference: `registry/repository:tag`, or
`registry/repository@sha256:…` for an exact digest. A **floating tag** such as
`:1` points at the newest `1.x` release and moves over time; a **digest** never
moves.

You declare floating tags for convenience and let Grimoire pin them to digests
for reproducibility — which is the job of the lock.

## The lock

`grimoire.lock` records the exact digest each declared tag resolved to, so an
install is byte-for-byte reproducible until you deliberately upgrade.
[`grim lock`](./commands.md#lock) resolves the floating tags in
`grimoire.toml`; [`grim update`](./commands.md#update) re-resolves them and
rolls the pins forward when a newer version appears behind the same tag.

The lock also stores a hash of the declaration it was generated from, so
Grimoire can tell when `grimoire.toml` has drifted ahead of the lock.

## Scopes

Grimoire works in two scopes. The **project** scope is the `grimoire.toml`
discovered from the current directory — per-repository configuration that lives
beside your code. The **global** scope is a single config under `$GRIM_HOME`
for artifacts you want everywhere.

Most commands operate on the discovered project by default and switch to the
global scope with `--global`. The [TUI](./commands.md#tui) can flip between the
two at runtime.

## Editors

An installed artifact has to land somewhere the agent reads. Grimoire calls
that destination an **editor target** and ships three: [Claude Code][claude],
[opencode][opencode], and [GitHub Copilot][copilot]. The same skill is
transformed into each editor's native layout on install.

[`grim install`](./commands.md#install) writes to the target named by the
`editor` option in your config, defaulting to `claude`; `--target` overrides it
and accepts a comma-separated list to install into several editors at once.

## The catalog

[`grim search`](./commands.md#search) and the [TUI](./commands.md#tui) read a
**catalog** — an index of the artifacts a registry offers, cached locally under
`$GRIM_HOME` so repeat browsing is fast and works offline. Pass `--refresh` to
rebuild it from the registry.

## Online by default, offline on demand

By default Grimoire is **online**: every floating-tag lookup resolves fresh
against the registry, and the resolved digest is cached as a write-through so
later offline runs still work. A floating tag therefore never serves a stale
pin — there is no "use the cache first" mode to surprise you, and no `--remote`
flag to remember.

Pass `--offline` to flip to **cache-only**: Grimoire forbids all network access
and fails rather than touch a registry — useful in sealed CI or an air-gapped
network. Warm the cache with a normal online `grim lock` (or `grim update`)
before going offline. `--offline` has an environment-variable equivalent
described in [Configuration](./configuration.md).

<!-- external -->
[oci]: https://github.com/opencontainers/distribution-spec
[ghcr]: https://docs.github.com/en/packages/working-with-a-github-packages-registry/working-with-the-container-registry
[hub]: https://hub.docker.com
[dist]: https://distribution.github.io/distribution/
[claude]: https://docs.anthropic.com/en/docs/claude-code/overview
[opencode]: https://opencode.ai
[copilot]: https://github.com/features/copilot
