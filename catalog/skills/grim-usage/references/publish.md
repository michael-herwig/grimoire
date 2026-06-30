# Publishing Workflow

You loaded this file because you are turning a local skill, rule, agent,
or bundle into a published OCI artifact — the build → dry-run → release
loop, version tagging, and registry authentication.

Contents: [Build, Then Release](#build-then-release) ·
[Cascade Tags](#cascade-tags) · [Immutability](#immutability) ·
[Scripted Publishing](#scripted-publishing) · [Bundles](#bundles) ·
[Catalog Metadata](#catalog-metadata) · [Authentication](#authentication)

Flags shown here are grim 0.6.x; confirm with `grim <cmd> --help` before
relying on one.

## Build, Then Release

The publishing loop has three steps, each catching mistakes earlier than
the next:

```sh
grim build ./code-review                                        # validate + pack, no push
grim release ./code-review ghcr.io/acme/code-review:1.2.3 --dry-run  # print the push plan
grim release ./code-review ghcr.io/acme/code-review:1.2.3       # validate, pack, push
```

`grim build <path>` detects the kind from the path — a directory packs
as a **skill**, a `.md` file as a **rule**, a `.toml` file as a
**bundle**. An **agent** always needs `--kind agent`: its `.md` shape is
indistinguishable from a rule and grim never guesses from content. This
is the most common publishing mistake — see
[troubleshooting.md](troubleshooting.md).

`--dry-run` prints every tag the release would move and the digest each
would point at, without touching the registry. Make it a habit before
any version release.

## Cascade Tags

Releasing a full semver version moves the floating tags consumers track.
`grim release … :1.2.3` pushes `1.2.3` **and** moves `1.2`, `1`, and
`latest` to the same digest — that is what lets a consumer who declared
`:1` pick up `1.2.3` with a plain `grim update`.

A non-version tag (`canary`, `edge`) publishes only that exact tag, no
cascade. A reference with no tag at all is an error.

## Immutability

An exact-version tag is immutable by default: if `1.2.3` already exists
and points at different bytes, the release refuses (exit 65) rather than
rewrite history. Pass `--force` only when you deliberately mean to move
it. Floating tags (`1.2`, `1`, `latest`) move freely on every cascade —
that is their job.

## Scripted Publishing

For multi-package repositories, `grim publish` supersedes the manual
release loop below. It reads a `publish.toml` manifest, validates every
entry before touching the registry, and releases each package in a fixed
kind order — see [Manifest-Driven Batch Publishing](#manifest-driven-batch-publishing).
The manual loop remains useful for edge cases: per-package divergent flags,
or environments where `grim publish` is not available.

Publishing several packages from one repository? Keep their versions in
a reviewed manifest file (versions change only via commits, so the repo
records exactly what was published), then blanket-rerun the release for
every package with `--skip-existing` (conflicts with `--force`):

```sh
grim release ./skills/code-review reg.example.com/skills/code-review:1.3.0 --skip-existing
```

An already-published version is a success no-op — nothing pushes, no
tags move — so only the packages whose version you bumped go out. The
maintenance loop becomes: change content, bump that package's version,
rerun the whole publish. Two rules keep it sound: bump on every content
change (an unbumped change is silently never published), and release
bundle members before the bundle that references them.

## Manifest-Driven Batch Publishing

`grim publish` is the built-in command for multi-package repositories. It
reads a `publish.toml` manifest that declares every package with a `registry`
and per-entry `version`, validates the whole set before any push, then
releases each entry in a fixed kind order: skills first, then rules, then
agents, then bundles — alphabetical within each kind. Bundle members always
land before the bundles that reference them.

The manifest format uses per-entry sub-tables keyed by name. A minimal
example:

```toml
registry = "grim.ocx.sh"

[skills.code-review]
version = "1.2.0"

[bundles.dev-stack]
version = "0.3.0"
pin = true          # bundle-only: freeze floating member tags to digests
```

Key behaviors — confirmed invariants, not subject to minor-release drift:

- **Skip-existing by default.** An already-published exact version is a
  success no-op. Only bumped versions push. Use `--force` to move an
  existing exact-version tag instead (the two modes are mutually exclusive).
- **Fail-fast.** The first failing entry stops the batch. The report shows
  all completed entries plus the failed one. Re-run with `--only <name>` to
  resume from a specific entry.
- **`pin = true` is bundle-only.** Setting it on a skill, rule, or agent
  entry is a validation error (exit 65).
- **Namespace overrides.** By default an entry publishes to
  `{registry}/{kind-subdir}/{name}`. A manifest-level `repository_prefix`
  replaces the `{kind-subdir}` segment (`{prefix}/{name}`); a per-entry
  `repository` is used verbatim (name not appended) and wins over the prefix.
  Needed for registries that require a group/project path, e.g. GitLab.
  Full schema and charset rules: [Batch publishing with a manifest][batch-publish].

Common flags — confirm current spelling with `grim publish --help`:

```sh
grim publish --dry-run           # plan without pushing
grim publish --only code-review  # publish one entry
grim publish --tag canary        # movable tag, semver rejected
grim publish --manifest staging/publish.toml  # alternate manifest
```

See [Batch publishing with a manifest][batch-publish] for the full schema,
source layout conventions, and disambiguation from bundle TOML files.

## Editor schema support {#editor-schema}

`grim schema --kind config|publish` prints a JSON Schema for `grimoire.toml`
or `publish.toml` (generated from grim's own parser, so it accepts exactly
what grim accepts). The same schemas are published to the docs site; adding a
`#:schema` directive on the first line of a TOML file gives a supporting editor
(Taplo, Even Better TOML) autocomplete and typo-flagging. Confirm the flags
with `grim schema --help`; see the [Editor schema support][editor-schema] docs
for the hosted URLs.

## Bundles

A bundle is a small `.toml` whose `[skills]` / `[rules]` / `[agents]`
tables list members by reference — the same shape as a `grimoire.toml`.
Build and release it like any artifact:

```sh
grim build ./python-stack.toml
grim release ./python-stack.toml ghcr.io/acme/python-stack:1.0.0 --pin
```

**Publish members before the bundle.** A bundle holds references to
already-published artifacts; consumers resolve those members at lock
time, so a member that is not on the registry yet breaks every consumer.

By default member tags stay floating and each consumer's `grim lock`
re-resolves them fresh. `--pin` instead freezes every floating member to
a digest at release time, making the bundle reproducible on its own —
the stronger guarantee for air-gapped or tunneled networks. Re-run the
pinned release to roll members forward.

## Catalog Metadata

Four optional fields make an artifact findable in `grim search` and the
TUI: `summary`, `keywords`, `description`, `repository`. A fifth field,
`deprecated` (grim 0.6.x), retires a package *without* unpublishing it —
a non-empty notice keeps it resolving and installing while grim flags it
in `grim search`, the TUI, and on `grim add`; an empty or whitespace
value means not deprecated. You author them all in the source file
itself, so a release always publishes what the file says. Two invariants
hold for every kind:

- `keywords` is a single comma-separated **string** (`rust,lint`), never
  a YAML/TOML list — an OCI annotation value is a string.
- `repository` must be an `https://` URL; anything else fails the
  release with exit 65.

*Where* the fields live differs by kind (skill/agent: the frontmatter
`metadata` map; rule: top-level frontmatter; bundle: top-level TOML) —
see [the per-kind examples][metadata].

## Git Provenance

`build`, `release`, and `publish` take an opt-in `--git` flag that stamps the
publishing commit (revision, commit date, and the `origin` remote) onto the
manifest as standard OCI annotations, surfaced in the TUI detail pane and
`grim search --format json`. It is off by default so an ordinary re-release
stays idempotent; with `--git` a re-release from a different commit changes
the digest. A repo with no `origin` (or no HTTPS-resolvable remote) still
succeeds — revision and commit date are stamped and the source is just
omitted; only a non-git path or a missing `git` fails (exit 65). Confirm the
flag with `grim release --help` and see the [publishing guide][publishing]
for the trade-off.

## Authentication

grim reads and writes the same Docker-compatible credential store your
container tooling uses, so a prior `docker login` is already enough.
To log in with grim itself:

```sh
grim login ghcr.io -u alice                              # interactive prompt
echo "$GITHUB_TOKEN" | grim login ghcr.io -u alice --password-stdin
grim logout ghcr.io                                      # idempotent, exits 0
```

There is intentionally **no** `--password <value>` flag — a secret on
the command line leaks through the process list and shell history.

Credentials land in `$DOCKER_CONFIG/config.json` (default
`~/.docker/config.json`): a configured credential helper if present,
else grim *refuses* a plaintext write unless you opt in with
`--allow-insecure-store` (base64, not encryption; file mode `0600`).
A wrong password surfaces on the next pull or push, not at login time.

The CI recipe — headless runner, no keychain, per-job isolation:

```sh
export DOCKER_CONFIG="$RUNNER_TEMP/docker"
echo "$REGISTRY_TOKEN" | grim login "$REGISTRY" -u "$REGISTRY_USER" \
  --password-stdin --allow-insecure-store
grim release ./code-review "$REGISTRY/acme/code-review:1.2.3"
grim logout "$REGISTRY"
```

With no positional registry, `login`/`logout` resolve `--registry`, then
`GRIM_DEFAULT_REGISTRY` — confirm with `grim login --help`.

## Further Reading

- [Publishing][publishing] — the full workflow: support directories,
  per-kind metadata, dry runs, bundle pinning.
- [Authentication][auth] — credential resolution, storage tiers, CI.
- [Command reference: build, release, login, logout][commands].

[publishing]: https://michael-herwig.github.io/grimoire/publishing.html
[metadata]: https://michael-herwig.github.io/grimoire/publishing.html#metadata
[batch-publish]: https://michael-herwig.github.io/grimoire/publishing.html#batch-publish
[editor-schema]: https://michael-herwig.github.io/grimoire/configuration.html#editor-schema
[auth]: https://michael-herwig.github.io/grimoire/authentication.html
[commands]: https://michael-herwig.github.io/grimoire/commands.html#build
