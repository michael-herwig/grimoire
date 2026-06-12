# Publishing Workflow

You loaded this file because you are turning a local skill, rule, agent,
or bundle into a published OCI artifact — the build → dry-run → release
loop, version tagging, and registry authentication.

Contents: [Build, Then Release](#build-then-release) ·
[Cascade Tags](#cascade-tags) · [Immutability](#immutability) ·
[Bundles](#bundles) · [Catalog Metadata](#catalog-metadata) ·
[Authentication](#authentication)

Flags shown here are grim 0.4.x; confirm with `grim <cmd> --help` before
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
it. For scripted blanket publishing, `--skip-existing` (conflicts with
`--force`) makes an already-published version a success no-op instead —
only bumped versions push. Floating tags (`1.2`, `1`, `latest`) move
freely on every cascade — that is their job.

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
TUI: `summary`, `keywords`, `description`, `repository`. You author them
in the source file itself, so a release always publishes what the file
says. Two invariants hold for every kind:

- `keywords` is a single comma-separated **string** (`rust,lint`), never
  a YAML/TOML list — an OCI annotation value is a string.
- `repository` must be an `https://` URL; anything else fails the
  release with exit 65.

*Where* the fields live differs by kind (skill/agent: the frontmatter
`metadata` map; rule: top-level frontmatter; bundle: top-level TOML) —
see [the per-kind examples][metadata].

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
[auth]: https://michael-herwig.github.io/grimoire/authentication.html
[commands]: https://michael-herwig.github.io/grimoire/commands.html#build
