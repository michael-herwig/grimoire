# Publishing Skills and Rules

Consuming artifacts is only half of Grimoire. The other half is producing them:
turning a local skill directory or rule file into a versioned OCI artifact that
others can [`grim add`](./commands.md#add).

## Author locally

A **skill** is a directory containing a `SKILL.md` and any supporting files; a
**rule** is a Markdown file, optionally with a
[sibling support directory](#rule-support-dir); an
[**agent**](./agents.md) is a Markdown file defining a delegatable assistant;
a [**bundle**](./concepts.md#bundles) is a `.toml` file listing members.
Grimoire detects which one you mean from the path — a directory packs as a
skill, a `.md` file as a rule, a `.toml` file as a bundle — and `--kind`
overrides the guess when you need to. An agent **requires** `--kind agent`:
its `.md` shape is indistinguishable from a rule, and grim never guesses from
content (see [Agent Artifacts](./agents.md#publishing)).

### Rules with a support directory {#rule-support-dir}

An index rule often references extra context — examples, a schema, a script —
that does not belong inside the rule body. Put those in a folder beside the
rule that shares its stem, and Grimoire packs both into the one artifact:

```
rules/
  my-rule.md        # the index you pass to build/release
  my-rule/          # optional support directory, same stem
    examples.md
    schema.json
```

You still point [`grim build`](./commands.md#build) and
[`grim release`](./commands.md#release) at the index `.md` file — the sibling
directory is discovered automatically when it exists:

```sh
grim release ./my-rule.md ghcr.io/acme/my-rule:1.0.0
```

Every file under `my-rule/` rides along in the same layer and installs beside
the index (`.claude/rules/my-rule.md` + `.claude/rules/my-rule/…`), so the
index's relative links resolve on the consumer. Support files are copied
verbatim for every [client](./concepts.md#clients) — only the index is ever
transformed. A rule with no support directory packs to exactly the single
`my-rule.md` it always did.

## Catalog metadata {#metadata}

[`grim search`](./commands.md#search) and the [TUI](./commands.md#tui) list
every match in a table. To make a result legible and findable, an artifact
carries five pieces of catalog metadata, all optional:

| Field | Annotation | Purpose |
|-------|-----------|---------|
| `summary` | `com.grimoire.summary` | One-line blurb shown in the catalog (preferred over the description). |
| `keywords` | `com.grimoire.keywords` | Comma-separated terms that search matches. |
| `description` | `org.opencontainers.image.description` | The full description. |
| `repository` | `org.opencontainers.image.source` | HTTPS URL of the artifact's source repository ([details](#metadata-repository)). |
| `deprecated` | `com.grimoire.deprecated` | A deprecation notice; marks the package deprecated and flags it everywhere ([details](#metadata-deprecated)). |

`grim search` shows the `summary` in place of the `description`, truncated to
fit the terminal; the full description stays in `--format json` and in piped
output. Search matches the repository, summary, description, **and** keywords,
so a query hits regardless of which one carries the term. Omit `summary` and
the catalog falls back to the description.

You author this metadata in the source file, so a `grim release` always
publishes whatever the file currently says — no separate flags to remember.
Where it lives differs by kind.

### In a skill {#metadata-skill}

A skill puts catalog metadata under the `metadata` map of its `SKILL.md`
frontmatter (the map the [Agent Skills](https://docs.claude.com/en/docs/agents-and-tools/agent-skills/overview)
format defines), separate from the top-level `description`:

```yaml
# code-review/SKILL.md
---
name: code-review
description: A thorough multi-pass reviewer that checks correctness, security, and style across the whole diff.
metadata:
  summary: Multi-pass code reviewer
  keywords: review,quality
  repository: https://github.com/acme/code-review
---
```

### In a rule {#metadata-rule}

A rule has no `description` field — that is derived from the body's first
heading or paragraph. `summary` and `keywords` sit at the top level of its
frontmatter:

```yaml
# rust-style.md
---
paths: ["**/*.rs"]
summary: Idiomatic Rust style rules
keywords: rust,lint
repository: https://github.com/acme/rust-style
---
# Rust Style
…
```

### In an agent {#metadata-agent}

An agent authors catalog metadata in its `metadata` map, like a skill; the
required `description` doubles as the full catalog description:

```yaml
# code-reviewer.md
---
name: code-reviewer
description: Reviews diffs for correctness, security, and style.
metadata:
  summary: Multi-pass diff reviewer
  keywords: review,quality
  repository: https://github.com/acme/code-reviewer
---
```

### In a bundle {#metadata-bundle}

A [bundle](#bundles) sets the same keys at the top level of its `.toml`, above
the member tables. Here `description` overrides the otherwise-automatic
`grimoire bundle of N members`:

```toml
# python-stack.toml
summary = "Python dev stack"
keywords = "python,lint,test"
description = "Skills and rules for Python work"
repository = "https://github.com/acme/python-stack"

[skills]
code-review = "ghcr.io/acme/code-review:1"
[rules]
rust-style = "ghcr.io/acme/rust-style:2"
```

### Keywords are a string {#metadata-keywords}

`keywords` is always a single comma-separated string — in every kind — because
an OCI annotation value is itself a string. A YAML or TOML list is **not**
accepted; write `keywords: rust,lint`, not `keywords: [rust, lint]`.

### Repository URL {#metadata-repository}

`repository` links a published artifact back to the source repository it
came from. The value must be an `https://` URL (GitHub, GitLab, or any
forge) — a `git@…` or `http://` value fails the release with exit 65, the
same hard gate that guards [vendor metadata](./vendor-metadata.md#publish-validation).
The URL must **not** carry embedded credentials: an authored
`https://token@host/owner/repo` fails the release with exit 65 rather than
publishing the secret in the manifest. (grim never strips an *authored*
credential silently — only a git-derived `origin` remote is sanitized.)

On the wire it travels as the standard `org.opencontainers.image.source`
annotation, so registries that honor the key link the package to its
repository. When no `repository` is authored, grim keeps its previous
behavior and stamps the tagless release reference there instead. The
[TUI](./commands.md#tui) shows the URL in the detail pane and opens it
with the `o` key; `grim search --format json` exposes it as the
`repository` field.

### Deprecating a package {#metadata-deprecated}

`deprecated` retires a package without unpublishing it. Author a short
notice — ideally naming the replacement — and the package keeps resolving
and installing, but grim flags it at every point a consumer might reach for
it. The notice is the message; an empty or whitespace-only value means *not*
deprecated, so no annotation is emitted.

```yaml
# code-review/SKILL.md (skill / agent: under the metadata map)
metadata:
  deprecated: use acme/code-review-2 instead
```

```yaml
# rust-style.md (rule: top-level, like summary)
deprecated: superseded by rust-style-2
```

```toml
# python-stack.toml (bundle: top-level)
deprecated = "migrate to python-stack-2"
```

Because the notice rides the `com.grimoire.deprecated` annotation on the
manifest, every surface reads it back without unpacking the artifact:

- [`grim search`](./commands.md#search) appends a comma-suffixed `deprecated`
  to the result's `Status` cell (e.g. `installed,deprecated`) and exposes the
  message as a `deprecated` field in `--format json`.
- The [TUI](./commands.md#tui) appends a yellow `⚠ deprecated` after the
  install-status label in the `Status` column (explained in the legend) and
  shows the full notice in the detail pane.
- [`grim add`](./commands.md#add) prints the notice on stderr when you
  acquire a deprecated reference (the add still succeeds).

A re-release with the notice removed clears the deprecation — the annotation
simply stops being emitted.

## Validate before you push

[`grim build`](./commands.md#build) validates and packs an artifact **without**
pushing it. Run it while iterating to catch a malformed skill before anyone
else sees it:

```sh
grim build ./code-review
grim build ./rust-style.md --kind rule
grim build ./code-reviewer.md --kind agent
```

## Release

[`grim release`](./commands.md#release) validates, packs, and pushes to a
registry in one step. Give it the source path and the release reference:

```sh
grim release ./code-review ghcr.io/acme/code-review:1.2.3
```

### Cascade tags

A release does more than push one tag. From a `1.2.3` version it also moves the
**floating** tags that consumers track — `1`, `1.2`, and `latest` — to the new
digest. That is what lets a consumer who declared `:1` pick up `1.2.3` with a
plain [`grim update`](./commands.md#update).

### Dry runs and overwrites

Preview the exact push plan — every tag and the digest each will point at —
without touching the registry:

```sh
grim release ./code-review ghcr.io/acme/code-review:1.2.3 --dry-run
```

An exact-version tag is immutable by default: if `1.2.3` already exists and
points at different bytes, the release refuses rather than rewrite history.
Pass `--force` only when you deliberately mean to move it.

## Git provenance {#git-provenance}

A published artifact rarely records which commit it was built from. Without
that link, tracing a registry tag back to the source — for an audit, a
rebuild, or a "why did this change" investigation — means guessing from
timestamps.

The opt-in `--git` flag closes that gap. Pass it to `grim build`,
`grim release`, or `grim publish` and grim reads the artifact's git working
tree and stamps three standard OCI annotations onto the manifest:

| Annotation | Value |
|---|---|
| `org.opencontainers.image.revision` | the `HEAD` commit SHA, suffixed `-dirty` when tracked files differ from `HEAD` |
| `org.opencontainers.image.created` | the commit date (RFC3339) — the *commit's* date, not a build clock |
| `org.opencontainers.image.source` | the `origin` remote, normalized to an `https://` URL — **conditional**: the git-derived URL is **not used** when you authored a [`repository`](#metadata-repository) value (the authored URL wins) or the repo has no HTTPS-resolvable remote. The annotation itself is still emitted from the usual fallback (authored `repository`, else the tagless release reference) |

```sh
grim release ./code-review ghcr.io/acme/code-review:1.2.3 --git
```

The git remote only fills `source` when you did **not** author a
[`repository`](#metadata-repository) value — an authored URL always wins, so
the two never collide. Any credentials embedded in the remote
(`https://token@host/...`) are stripped before the URL is written, so a token
in your `origin` URL never reaches the annotation. A path that is not inside a
git repository (or a host with no `git`) fails the release with exit 65 rather
than silently dropping the provenance you asked for.

There is a third outcome between those two. A repository with **no `origin`
remote** — or one whose remote does not resolve to an HTTPS URL (an SSH-only
host grim cannot rewrite, a `file://` remote, a bare local path) — is **not**
an error: `revision` and `created` are still stamped, and `source` is simply
omitted (falling back to whatever the authored `repository` or the tagless
release reference supplies). Only an absent repository or a missing `git`
fails.

### Why it is opt-in {#git-idempotent}

By default a re-release of identical content produces the same manifest
digest, so re-running a release is a harmless no-op (the
[overwrite guard](#dry-runs-and-overwrites) recognizes it). Embedding the
commit ties the digest to that commit: a re-release from a *different* commit
now changes the digest and is refused unless you pass `--force`. That is the
correct behavior — the provenance genuinely changed — but it is why git
provenance is something you ask for, not a silent default. The commit *date*
(not a wall-clock build time) keeps a re-release from the **same** commit
fully idempotent.

Every read surface shows the provenance back: the [TUI](./commands.md#tui)
detail pane adds `Revision:` and `Created:` rows, and
[`grim search --format json`](./commands.md#search) exposes `revision` and
`created` fields.

## Publishing bundles {#bundles}

A [bundle](./concepts.md#bundles) groups skills, rules, and
[agents](./agents.md) so consumers declare one reference instead of a dozen.
You author it as a small TOML file whose `[skills]`/`[rules]`/`[agents]`
tables list the members — the same shape as a `grimoire.toml`:

```toml
# python-stack.toml
[skills]
code-review = "ghcr.io/acme/code-review:1"

[rules]
rust-style = "ghcr.io/acme/rust-style:2"

[agents]
code-reviewer = "ghcr.io/acme/code-reviewer:1"
```

[`grim build`](./commands.md#build) validates it (a `.toml` path packs as a
bundle), and [`grim release`](./commands.md#release) pushes it with the same
cascade tags as any other artifact:

```sh
grim build ./python-stack.toml
grim release ./python-stack.toml ghcr.io/acme/python-stack:1.0.0
```

### Floating or pinned members {#pin}

By default the bundle stores its members exactly as written — floating tags stay
floating, and each consumer's [`grim lock`](./commands.md#lock) re-resolves them
fresh. Add `--pin` to resolve every floating member to a digest at release time
and freeze it into the published bundle:

```sh
grim release ./python-stack.toml ghcr.io/acme/python-stack:1.0.0 --pin
```

A pinned bundle is reproducible on its own: it always expands to the exact same
member digests, even on an air-gapped or tunneled network that cannot re-resolve
a tag. Re-run the release (a cron job tracking `:stable`, say) to roll the
pinned members forward.

## Batch publishing with a manifest {#batch-publish}

When a repository contains more than one package, releasing them one by one
with `grim release` means maintaining a shell script (or CI job) that
re-invents version tracking, ordering, and idempotent re-runs. That is a
generic capability dressed as project-specific tooling.

`grim publish` is the built-in alternative: it reads a `publish.toml`
manifest, validates the whole set before touching the registry, then
releases each entry in a fixed order.

### The publish.toml format {#batch-publish-manifest}

A manifest has one required top-level field — `registry` — and up to four
kind tables. Each table entry is a sub-table keyed by name with a required
`version` field:

```toml
#:schema https://grimoire.rs/schemas/grim-publish.schema.json
registry = "ghcr.io"              # required; overridden by --registry

[skills.grim-usage]
version = "0.1.1"                  # required, strict X.Y.Z

[rules.custom-rule]
version = "0.2.0"
path = "shared/custom-rule.md"     # optional — overrides the conventional path

[agents.helper]
version = "0.1.0"

[bundles.grim-essentials]
version = "0.1.0"
pin = true                         # optional, bundle entries only; default false
```

The `registry` value is a plain host (e.g. `ghcr.io`, `localhost:5000`), not a
full reference. All entries in the manifest publish to the same registry.

Entry names must start with a character in `[a-z0-9]` and contain only
`[a-z0-9._-]` in the remainder. Uppercase letters, slashes, and `..`
components are all rejected at validation time (exit 65) — they would
produce an invalid OCI repository segment or a path traversal hazard. Unknown
fields in the manifest or in any entry sub-table are a hard parse error
(`deny_unknown_fields`): a typo like `versions` instead of `version` exits
immediately rather than silently using a default.

The first line above is a [Taplo](https://taplo.tamasfe.dev/) /
[Even Better TOML](https://marketplace.visualstudio.com/items?itemName=tamasfe.even-better-toml)
`#:schema` directive that binds the manifest to its published [JSON
Schema](https://grimoire.rs/schemas/grim-publish.schema.json),
so a supporting editor autocompletes keys and flags a typo before you ever run
`grim publish`. The schema is generated from grim's own manifest parser — see
[Editor schema support](./configuration.md#editor-schema) for both schema URLs
and [`grim schema`](./commands.md#schema) to print one locally.

### Repository namespace {#batch-publish-namespace}

By default, each entry pushes to `{kind-subdir}/{name}` under the
manifest's registry — a skill named `hearth` publishes to
`registry/skills/hearth`. Most self-hosted or single-user registries
work fine with this convention. Multi-tenant SaaS registries — such as
the [GitLab Container Registry][gitlab-registry] — require every image
to live under a group-and-project path, making the default layout
inaccessible.

Two optional fields let you replace the `{kind-subdir}` segment with an
arbitrary namespace path, so a publish manifest can target any registry
layout.

**Manifest-level `repository_prefix`** — a string applied to every entry
that does not set its own `repository`. The published repository becomes
`{repository_prefix}/{name}`; the prefix replaces the conventional
`{kind.subdir()}` segment. Registry-relative, no tag.

**Per-entry `repository`** — a string inside a `[skills.<name>]`,
`[rules.<name>]`, `[agents.<name>]`, or `[bundles.<name>]` sub-table.
The value is used verbatim as the full repository path; the entry name is
**not** appended — the same way `grim release` uses the repository portion
of its positional `registry/repo:version` reference verbatim. Wins over
`repository_prefix` when both are set.

Resolution precedence per entry (highest first):

1. per-entry `repository` (full path, name not appended)
2. manifest `repository_prefix` → `{prefix}/{name}`
3. default → `{kind.subdir()}/{name}` (unchanged backward-compatible behavior)

```toml
#:schema https://grimoire.rs/schemas/grim-publish.schema.json
registry = "registry.gitlab.com"
repository_prefix = "durzn-technology/hearth/skill"

[skills.hearth]
version = "0.2.0"
# publishes to: registry.gitlab.com/durzn-technology/hearth/skill/hearth

[skills.other-skill]
version = "0.1.0"
repository = "durzn-technology/hearth/skill/other-skill"
# per-entry form — identical effect for this entry, wins over repository_prefix
```

The reporter's working example: registry `registry.gitlab.com`, prefix
`durzn-technology/hearth/skill`, skill `hearth` → resolves to
`registry.gitlab.com/durzn-technology/hearth/skill/hearth`.

**Charset rules** — each `/`-separated segment of both fields must match
the OCI name grammar: runs of `[a-z0-9]` joined by a single `.` or `_`, a
double `__`, or a run of `-`, with no leading, trailing, or doubled
separator. A leading or trailing `/`, empty `//` segments, `.` or `..`
segments, an embedded `:`, uppercase, and a path longer than 255 characters
are all rejected at manifest validation time with exit 65 (data error). An
invalid prefix or repository aborts the whole manifest before any push.

A manifest with neither field is unchanged: `ghcr.io/skills/grim-usage`
style paths are the default and remain fully backward compatible.

### Conventional source layout {#batch-publish-layout}

When `path` is omitted, grim derives the source path from the entry name and
kind, relative to the manifest's directory:

| Kind | Conventional path |
|------|-------------------|
| skill | `skills/{name}/` |
| rule | `rules/{name}.md` |
| agent | `agents/{name}.md` |
| bundle | `bundles/{name}.toml` |

The `path` field overrides this convention for entries whose source lives
elsewhere.

### Kind ordering {#batch-publish-ordering}

Entries publish in a fixed kind order — skills, then rules, then agents, then
bundles — alphabetical within each kind. Bundle entries land last by design:
a bundle holds references to already-published members, and consumers resolve
those members at lock time. Publishing a bundle before its members would
produce a bundle that references artifacts that do not yet exist.

### Skip-existing default and --force {#batch-publish-skip-existing}

By default, `grim publish` skips any entry whose exact-version tag already
exists on the registry — the push is a success no-op and nothing moves. This
makes the command safe to re-run from the top: only entries whose version was
bumped in the manifest since the last run actually push anything.

`--force` replaces the default with the opposite behavior: it moves an
existing exact-version tag that points at a different digest. The two modes
are mutually exclusive — `--force` and skip-existing cannot be combined.

`--force` also cannot be combined with `--tag`: a channel-tag run always
moves the tag, so `--force` would be redundant — passing both is rejected
as a usage error.

### Flags {#batch-publish-flags}

| Flag | Description |
|------|-------------|
| `--manifest <path>` | Manifest file to read (default: `./publish.toml`). |
| `--dry-run` | Validate and plan without pushing. Prints what would be pushed. |
| `--force` | Move existing exact-version tags instead of skipping them. Cannot be combined with `--tag`. |
| `--only <name>` | Publish only the named entry (repeatable). A name absent from the manifest exits 65. |
| `--tag <tag>` | Override the published tag with a movable channel tag (e.g. `canary`). Must be non-semver — semver values exit 65, keeping all semver releases in the manifest where the repo can track them. A channel tag always moves: re-publishing with `--tag` overwrites the existing tag without skipping and without `--force`. |
| `--registry <ref>` | The [global `--registry` flag][global-options] overrides the manifest's `registry` value for this run. `GRIM_DEFAULT_REGISTRY` and the config-file `default_registry` do **not** override the manifest — `registry` is explicit input, like a fully-qualified reference. Only the flag tier wins. |

### Validation and fail-fast {#batch-publish-validation}

`grim publish` validates the whole manifest before any push: every `version`
must be strict `X.Y.Z` semver, every source path must exist, and `pin = true`
is rejected on non-bundle entries (exit 65 for each). Only after the full
manifest passes does the first network call happen.

Two additional conditions exit 65 at validation time:

- **Empty manifest** — a manifest that declares no entries in any kind table
  exits 65 with "no packages declared in manifest". Grim treats this as a
  likely wrong-file mistake rather than a valid no-op.
- **Oversized manifest** — a manifest file larger than 64 KiB is rejected
  before parsing. This is an unconditional limit, not a warning.

During the release run the command is fail-fast: the first failing entry
stops the batch. The report still renders — completed entries show their
status (`pushed`, `skipped`, or `dry-run`), the failed entry shows `failed`,
and remaining entries are unreported. Because skip-existing is the default,
re-running from the top after a fix pushes only what is left.

### Example run {#batch-publish-example}

```sh
# Preview the full publish plan — zero writes
grim publish --dry-run

# Release everything in publish.toml, skip already-published versions
grim publish

# Release only one package
grim publish --only grim-usage

# Push a movable canary tag (manifest versions untouched)
grim publish --tag canary
```

### Manifest vs bundle disambiguation {#batch-publish-disambiguation}

A `publish.toml` and a bundle `.toml` are structurally different: a manifest
has a top-level `registry` string and per-entry sub-tables with `version`; a
bundle has flat `name = "reference"` strings in its kind tables. The schemas
are disjoint and each parser rejects the other's input.

If you point `grim publish` at a bundle file, the command detects the shape
and reports: "this looks like a bundle source file; use `grim release --kind
bundle`". If you point `grim release` at a publish manifest, the bundle
reader returns the mirror hint. Neither silently misparses the other's format.

## Authenticate {#authenticate}

Grimoire pushes over standard OCI, so it reuses your existing registry
credentials — the same login your container tooling uses. Authenticate once
with your registry (for example, `docker login` against [GitHub Container
Registry][ghcr]) and `grim release` inherits it.

<!-- external -->
[ghcr]: https://docs.github.com/en/packages/working-with-a-github-packages-registry/working-with-the-container-registry
[gitlab-registry]: https://docs.gitlab.com/ee/user/packages/container_registry/

<!-- internal -->
[global-options]: ./commands.md#global-options
