# The Package Index

Most OCI registries cannot answer the question *"what packages exist?"*
The `_catalog` endpoint that grim's browse surfaces (`search`, the TUI,
MCP) rely on is gated or absent on [GHCR][ghcr], [GitLab SaaS][gitlab-reg],
and [Docker Hub][dockerhub]. A **package index** fills that gap: a small,
decentralized directory of package pointers that grim reads instead of
`_catalog`.

Grimoire is decentralized by design. Anyone can host an index (a git
repository or a folder of static files), and any OCI registry can host
the packages it points to. The happy path is the default index at
[index.grimoire.rs][index-site], maintained at
[grimoire-rs/index][index-repo] on GitHub — but nothing in grim is
hard-wired to it.

> **Phone book, not catalog.** The index stores *pointers* — name, kind,
> OCI ref, description, ownership. It never stores versions. grim
> resolves tags live from the registry at install time, so a stale index
> can never serve a stale version.

## Consuming an Index {#consuming}

A [`[[registries]]`](./configuration.md) entry declares **exactly one**
of `url` / `index`:

```toml
# grimoire.toml
[[registries]]
alias = "hub"
index = "https://index.grimoire.rs"   # package index (browse source)
default = true

[[registries]]
alias = "corp"
oci = "registry.corp.example/team"    # plain OCI registry (_catalog)
```

`oci` and `index` are mutually exclusive because they answer the same
question differently: an `oci` entry lists what *that registry* holds via
`_catalog`; an `index` entry lists whatever the index points to — its
entries carry their own fully-qualified registry refs and may span many
registries.

Two transports, chosen by the locator's shape:

| Locator shape | Transport |
|---|---|
| `http://…`, `https://…` | Static files — grim fetches `<base>/all.json` |
| `git+…`, `ssh://…`, `git@…`, or ending in `.git` | Git — grim shallow-clones and walks `index/**/metadata.json` |

Both transports share the regular catalog machinery: the per-source
cache under `$GRIM_HOME/catalog/`, the 1-hour TTL, `--refresh`, and
offline degradation (`--offline` serves the cached listing and never
touches the network).

CLI equivalent of the config above:

```console
$ grim config registry add hub --index https://index.grimoire.rs --default
$ grim config registry add corp --oci registry.corp.example/team
```

## Index Specification (v1) {#spec}

This section is normative for index producers and consumers.

### Repository Layout {#spec-layout}

```
index/
  github.com/<namespace>/          # namespace = GitHub identity
    <package>/
      metadata.json                # one pointer per package
scripts/                           # (optional) build/validation tooling
```

- `<namespace>` is a GitHub login or organization name, lowercase as
  registered.
- `<package>` is the package name and MUST equal the `name` field in the
  contained `metadata.json`.
- Top-level namespaces without the `github.com/` prefix are *reserved*
  (vanity namespaces; maintainer-approved).

### `metadata.json` {#spec-metadata}

```json
{
  "schema": 1,
  "name": "grim-usage",
  "kind": "skill",
  "ref": "ghcr.io/grimoire-rs/skills/grim-usage",
  "description": "Drive the grim CLI — install, update, search, publish.",
  "repository": "https://github.com/grimoire-rs/grimoire",
  "owner": { "github": "grimoire-rs", "id": 298895348 }
}
```

| Field | Type | Required | Constraints |
|---|---|---|---|
| `schema` | integer | yes | Metadata schema version. This document specifies `1`. Consumers MUST skip entries with an unknown `schema` (forward compatibility). |
| `name` | string | yes | Package name. MUST equal the directory name containing the file. |
| `kind` | string | yes | One of `skill`, `rule`, `agent`, `bundle`. |
| `ref` | string | yes | Fully-qualified OCI reference **without a tag**: `registry-host[/namespace]/repository`. MUST contain at least one `/`. MUST NOT carry a tag or digest — versions are resolved live. |
| `description` | string | yes | One line, shown by `grim search` and the TUI. |
| `repository` | string | no | Source repository URL. Consumers keep it only with an `https://` prefix. |
| `owner.github` | string | yes | GitHub login owning the namespace. MUST match the namespace directory (case-insensitive). |
| `owner.id` | integer | yes | The account's numeric GitHub ID. Immutable — logins can be deleted and re-registered by someone else; the ID cannot. Validation compares it against the live API. |

Unknown additional fields MUST be tolerated by consumers (additive
schema evolution without a version bump).

### Compiled Artifacts {#spec-compiled}

A statically-served index publishes the compiled form:

| Path | Content |
|---|---|
| `/all.json` | Every package, one JSON array. Each element is the `metadata.json` object plus a derived `namespace` field (e.g. `"github.com/grimoire-rs"`). |
| `/index/<namespace…>/<package>/metadata.json` | Path-addressable copy of each pointer. |

`all.json` is the only endpoint grim's HTTP transport requires. The
path-addressable copies allow cheap single-package lookups by any
consumer without downloading the full set.

The git transport skips compilation entirely: grim walks the
`index/**/metadata.json` tree of the clone, so a plain git repository
with the layout above *is already a fully functional index*.

### Namespaces and Ownership {#spec-namespaces}

Namespaces are GitHub identities. There is no reservation step: the
first accepted pull request under `index/github.com/<login>/` creates
the namespace. A namespace can only be modified by:

- pull requests authored by `<login>`, or
- pull requests authored by a **public member** of the `<login>`
  organization.

### Validation and Auto-Merge {#spec-validation}

The default index auto-merges announcement PRs when **all** of the
following hold (anything else falls to manual maintainer review):

1. Only `index/github.com/<ns>/<pkg>/metadata.json` paths changed.
2. `<ns>` is the PR author's login, or an org the author publicly
   belongs to.
3. `owner.github` matches `<ns>` and `owner.id` matches the account's
   numeric GitHub ID (live API lookup — spoof-proof against login
   recycling).
4. Every changed file passes the schema above.
5. `ref` is *reachable*: the registry lists at least one tag
   anonymously. Publish before you announce.

Deletions inside your own namespace pass the same ownership check and
auto-merge too.

## Announcing Packages {#announcing}

Publish first (the packages must be pullable), then announce:

```console
$ grim publish --announce
```

`--announce` writes/updates your `metadata.json` pointers in a clone of
the index repository and opens a pull request (GitHub) or pushes a
branch (any other git host). The target repository is configurable:

```toml
# publish.toml
registry = "ghcr.io"

[announce]
repository = "https://github.com/grimoire-rs/index"  # default
namespace = "your-login"                              # default: your gh login
```

Point `[announce] repository` at any index repository — including a
private company index on GitLab — to announce there instead.

Announcing straight from a pipeline (GitHub Actions or GitLab CI, with
the token wiring each forge needs) is covered in
[Publishing from CI](./ci.md).

## Hosting Your Own Index {#self-hosting}

Any of the following is a complete, working index:

### A Plain Git Repository {#self-hosting-git}

Simplest — works everywhere. Create a repository with the layout above,
on GitHub, [GitLab][gitlab], or any git host. Done. Consumers configure:

```toml
[[registries]]
alias = "team"
index = "https://gitlab.com/your-group/index.git"
```

Private repositories work through ambient git credentials (credential
helper or ssh agent) — grim never prompts.

### Static Files {#self-hosting-static}

Fastest for consumers. Compile `all.json` (see [`scripts/build.py`][build-py]
in the default index for a ~50-line reference) and serve the `dist/`
folder from [GitHub Pages][gh-pages], [GitLab Pages][gl-pages], or any
webserver:

```toml
[[registries]]
alias = "team"
index = "https://index.your-domain.example"
```

### Fork the Default Index {#self-hosting-fork}

Fork [grimoire-rs/index][index-repo] to inherit the layout, the build
script, the Pages deployment, and the PR validation / auto-merge
workflow in one step.

## Relationship to Registries {#registries}

The index and the registry are independent axes:

| | Default | Self-hosted |
|---|---|---|
| **Packages (OCI)** | `ghcr.io/…` (any public registry) | [Zot][zot], [Harbor][harbor], GitLab registry, … |
| **Discovery (index)** | `index.grimoire.rs` | git repo or static files anywhere |

Mix freely: a public index can point at private registries (consumers
authenticate via [`grim login`](./authentication.md)), and a private
index can point at public packages.

<!-- external -->
[ghcr]: https://docs.github.com/en/packages/working-with-a-github-packages-registry/working-with-the-container-registry
[gitlab-reg]: https://docs.gitlab.com/ee/user/packages/container_registry/
[dockerhub]: https://hub.docker.com/
[gitlab]: https://gitlab.com/
[gh-pages]: https://pages.github.com/
[gl-pages]: https://docs.gitlab.com/ee/user/project/pages/
[zot]: https://zotregistry.dev/
[harbor]: https://goharbor.io/

<!-- grimoire -->
[index-site]: https://index.grimoire.rs
[index-repo]: https://github.com/grimoire-rs/index
[build-py]: https://github.com/grimoire-rs/index/blob/main/scripts/build.py
