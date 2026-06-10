# Concepts

Grimoire borrows its mental model from package managers you already use, then
swaps the transport for an [OCI registry][oci]. This page covers the handful of
ideas that make the [commands](./commands.md) feel obvious.

## Skills and rules

Grimoire distributes two kinds of artifact. A **skill** is a directory — a
`SKILL.md` plus any supporting scripts or references — that teaches an agent a
capability. A **rule** is a Markdown file that states a standard or constraint
the agent should always follow.

Both are declared the same way and travel through the same pipeline; the only
difference is shape on disk (a folder versus a file) and the `kind` argument
(`skill` or `rule`) you pass to commands like [`grim add`](./commands.md#add).

### Rules with a support directory {#rule-support-dir}

A rule is often an *index* that points at extra context — a worked example, a
JSON schema, a script — that would clutter the rule itself. The convention
across AI-config tooling is to put that next to the rule in a sibling folder
with the same name (`rules/my-rule.md` referencing `./my-rule/examples.md`).

A bare index whose links resolved to nothing would be useless, so a rule may
carry an optional **support directory**: a folder beside the index sharing its
stem (`my-rule.md` + `my-rule/`). Grimoire packs the index and every file under
that folder into the one artifact, and installs them beside each other so the
index's relative links resolve:

```
.claude/rules/my-rule.md
.claude/rules/my-rule/examples.md
.claude/rules/my-rule/schema.json
```

A rule with no support directory is unchanged — it remains the single
`my-rule.md` file. See [Publishing](./publishing.md#rule-support-dir) for how
the directory is packed.

## Artifacts as OCI content

Under the hood every skill or rule is packed into an OCI artifact and addressed
by content digest, exactly like a container image layer. Identical content is
stored once and is immutable: a `sha256:…` digest always names the same bytes.

Each artifact declares its kind through its OCI `artifactType` —
`application/vnd.grimoire.skill.v1`, `…rule.v1`, or `…bundle.v1` — rather than a
custom annotation. A registry, `grim`, or any OCI-aware tool can therefore tell
a Grimoire artifact apart from an ordinary container image without unpacking it.

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

## Bundles {#bundles}

Declaring the same dozen skills and rules in every repository does not scale.
Teams end up copying a block of `grimoire.toml` between projects, and when the
approved set changes someone has to chase down every copy.

A **bundle** is a curated set of members — skills and rules — published as its
own OCI artifact. You declare the bundle once in `[bundles]`, and on
[`grim lock`](./commands.md#lock) it **expands** into its members, which are
pinned into the lock exactly like a direct declaration. Update the published
bundle, re-lock, and every project that declares it moves together.

Each locked member records its **provenance** — `direct` for something you
declared yourself, or the bundle it came from — which [`grim status`](./commands.md#status)
surfaces so you always know why an artifact is installed.

### Adding and dropping members {#bundle-membership}

The bundle's member list is authoritative on every resolve, so membership tracks
the published bundle. When a new bundle version **adds** a member, the next
[`grim lock`](./commands.md#lock) expands it into the lock and the next install
materializes it. When a version **drops** a member, that member leaves the lock —
and [`grim update`](./commands.md#update) prunes its materialized files, unless
you have edited them locally, in which case it is kept until you re-run with
`--force`. This is the same reconciliation any artifact leaving the lock
receives; bundles just make it routine.

### Conflict policy {#bundle-conflicts}

Because a member is keyed by `(kind, name)`, two sources can name the same slot.
Grimoire resolves that deterministically:

- a **direct** `[skills]`/`[rules]` declaration always wins over any bundle —
  this is how you override a single member without forking the bundle;
- two bundles that name a member at the **same** identifier coalesce to one
  entry;
- two bundles that **disagree** fail closed: `grim lock` stops with a conflict
  error and asks you to declare the member directly to choose one.

Failing closed is deliberate. Silently picking a winner would let an unrelated
bundle bump change what a project installs without anyone noticing.

### Floating versus pinned members {#bundle-pinning}

A bundle's members can themselves be floating tags or exact digests. A floating
member is re-resolved fresh on every consumer `grim lock`, so reproducibility
comes from the consumer's own lock. Publishing with
[`grim release --pin`](./publishing.md#bundles) instead freezes every floating
member to a digest at publish time, so the bundle is reproducible on its own —
the stronger guarantee for air-gapped or tunneled networks that cannot
re-resolve a tag. See [Publishing](./publishing.md#bundles).

## Scopes

Grimoire works in two scopes. The **project** scope is the `grimoire.toml`
discovered from the current directory — per-repository configuration that lives
beside your code. The **global** scope is a single config under `$GRIM_HOME`
for artifacts you want everywhere.

Most commands operate on the discovered project by default and switch to the
global scope with `--global`. The [TUI](./commands.md#tui) can flip between the
two at runtime.

## Clients {#clients}

An installed artifact has to land somewhere the agent reads. Grimoire calls
that destination a **client target** and ships three: [Claude Code][claude],
[opencode][opencode], and [GitHub Copilot][copilot]. The same skill is
transformed into each client's native layout on install.

[`grim install`](./commands.md#install) writes to the targets listed in the
`clients` option in your config, defaulting to `["claude"]`; `--client`
overrides it and accepts a comma-separated list to install into several AI
clients at once.

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
