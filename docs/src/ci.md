# Publishing from CI

Publishing by hand works until the second contributor bumps a version and
forgets to run `grim publish`. The natural home for publishing is CI: every
merge to the default branch (or every tag) re-publishes the manifest, and
[skip-existing](./publishing.md#batch-publish) makes the re-run idempotent —
unchanged versions are no-ops, bumped versions push.

Grimoire ships first-party CI integrations for both major forges: a
[GitHub Action][setup-grimoire] and [GitLab CI/CD components][gl-components].
Both install the released `grim` binary (checksum-verified), and the GitLab
side adds a complete publish job as a one-line include. This page shows the
full setup for each — publishing to a registry, announcing to the
[package index](./package-index.md), and the tokens each step needs.

## GitHub Actions {#github-actions}

Two credentials are involved, and keeping them apart is the whole trick:

| Step | Credential | Why |
|---|---|---|
| `grim publish` to GHCR | `GITHUB_TOKEN` with `packages: write` | Registry push stays inside the repo's own permissions |
| `grim publish --announce` | A separate token that can push to the index repository | `GITHUB_TOKEN` is repo-scoped — it cannot open a PR on [grimoire-rs/index][index-repo] |

A minimal publish workflow:

```yaml
name: Publish
on:
  push:
    branches: [main]

permissions:
  contents: read
  packages: write

jobs:
  publish:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: grimoire-rs/setup-grimoire@v1
      - name: grim login
        env:
          REGISTRY_TOKEN: ${{ secrets.GITHUB_TOKEN }}
        run: |
          echo "$REGISTRY_TOKEN" | grim login ghcr.io -u "$GITHUB_ACTOR" \
            --password-stdin --allow-insecure-store
      - name: Publish
        env:
          GH_TOKEN: ${{ secrets.INDEX_ANNOUNCE_TOKEN }}
        run: grim publish --announce
```

`--announce` clones the index repository, writes your `metadata.json`
pointers, and opens the pull request via the [`gh` CLI][gh-cli]
(preinstalled on GitHub runners; it picks up `GH_TOKEN`). The announce
credential must be able to **push a branch** to the index repository:

- **Your own or your organization's index** — a fine-grained PAT or GitHub
  App installation token with `contents` + `pull-requests` write on the
  index repository. This is exactly how the [first-party catalog
  publishes][publish-catalog].
- **The public index, without write access** — point
  `[announce] repository` (or `--announce-repo`) at your **fork** of
  [grimoire-rs/index][index-repo] and open the pull request from the
  branch banner GitHub shows on the fork. The
  [auto-merge validation](./package-index.md#spec-validation) checks the
  PR author, so the PR must come from you either way.

Skipping `--announce` needs no extra token at all — publish is fully
self-contained on `GITHUB_TOKEN`.

## GitLab CI/CD {#gitlab}

On GitLab the same pipeline is a component include. The
[`grimoire-rs/components`][gl-components] catalog project provides two
components:

| Component | What it adds |
|---|---|
| `setup` | A hidden `.grim-setup` job that installs `grim` — see [Installation](./installation.md#gitlab-ci) |
| `publish` | A complete `grim-publish` job: install, `grim login`, `grim publish`, optional announce |

The publish component defaults to the **project's own GitLab container
registry** using the job token — zero secrets for the registry side:

```yaml
# .gitlab-ci.yml
include:
  - component: gitlab.com/grimoire-rs/components/publish@1.0.0
    inputs:
      stage: deploy
```

```toml
# publish.toml
registry = "registry.gitlab.com"
repository_prefix = "your-group/your-project"

[skills.my-skill]
version = "1.0.0"
```

The [GitLab container registry][gitlab-registry] requires every image to
live under a group-and-project path — `repository_prefix` handles that
(details: [Repository namespace](./publishing.md#batch-publish-namespace)).

> `include: component:` only resolves components hosted on the **same
> GitLab instance**. On self-managed GitLab, [mirror the components
> project][gl-mirror] into your instance first (or copy the two template
> files — they are self-contained).

### Announcing to the public index {#gitlab-announce-public}

Announcing from GitLab CI to the GitHub-hosted public index crosses
forges, so the job needs a GitHub token — the GitLab CI environment
deliberately contributes nothing when the index host differs from the CI
server host. Hand the token to the component and grim wires up both the
git push and the pull request (opened via the GitHub REST API — no `gh`
CLI involved):

```yaml
include:
  - component: gitlab.com/grimoire-rs/components/publish@1.0.0
    inputs:
      announce: true
      announce_token: $INDEX_ANNOUNCE_TOKEN   # masked CI/CD variable
```

The same write-access rule as on GitHub Actions applies: use a token that
can push to the index repository, or announce to your fork
(`announce_repo: https://github.com/<you>/index`) and open the PR from the
fork's branch banner.

### Announcing to a self-hosted index {#gitlab-announce-self-hosted}

A company index is [just a git repository](./package-index.md#self-hosting)
— host it on the same GitLab instance and announce with a [project access
token][gl-pat] (`api` scope, so grim can open the merge request through
the GitLab API; with `write_repository` only, grim falls back to git push
options and finally to the pushed branch):

```yaml
include:
  - component: gitlab.com/grimoire-rs/components/publish@1.0.0
    inputs:
      announce: true
      announce_repo: https://gitlab.example.com/platform/index.git
      announce_token: $INDEX_ANNOUNCE_TOKEN
```

Because the index host matches the CI server host, grim auto-detects the
GitLab forge and API from the CI environment — no `[announce] forge` or
`api_url` config needed. Re-announcing the same content is detected as
up-to-date; changed content force-updates the same deterministic
`announce/<ns>-<hash>` branch (and its open MR) instead of littering new
ones. The self-hosted index can run the same
[validation and auto-merge](./package-index.md#spec-validation) gate as
the public one — the index repo ships a `.gitlab-ci.yml` for exactly
that; setup in [Self-Hosted GitLab Setup](./self-hosted-gitlab.md).

Consumers then wire the index into their config as usual:

```toml
[[registries]]
alias = "platform"
index = "https://gitlab.example.com/platform/index.git"
```

See [Consuming an Index](./package-index.md#consuming) for the transports
and caching behavior.

<!-- external -->
[gh-cli]: https://cli.github.com/
[gitlab-registry]: https://docs.gitlab.com/ee/user/packages/container_registry/
[gl-components]: https://gitlab.com/grimoire-rs/components
[gl-mirror]: https://docs.gitlab.com/user/project/repository/mirror/
[gl-pat]: https://docs.gitlab.com/user/project/settings/project_access_tokens/
[index-repo]: https://github.com/grimoire-rs/index
[publish-catalog]: https://github.com/grimoire-rs/grimoire/blob/main/.github/workflows/publish-catalog.yml
[setup-grimoire]: https://github.com/grimoire-rs/setup-grimoire
