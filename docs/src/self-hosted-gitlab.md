# Self-Hosted GitLab Setup

Everything grim does on github.com works on a corporate GitLab instance:
private package index with auto-merge, publishing pipelines, and grim
itself installed from an internal mirror. This page is the operator
walkthrough for standing all of it up.

The setup has three independent pieces, in the order you need them:

1. **grim binaries** reach your runners from a release mirror.
2. **The index** — a fork of the default index repo, validated and
   auto-merged by GitLab CI.
3. **Publishers** — projects announcing their packages to that index
   with zero forge configuration.

GitHub Enterprise instances follow the same model — the forge-specific
notes are called out inline.

## Mirror grim Releases {#mirror}

Corporate runners usually cannot reach github.com. Both the
[GitLab CI/CD components][gl-components] and the
[setup-grimoire GitHub Action][setup-grimoire] take a release-mirror
base URL, so grim installs from wherever you can host files:

```yaml
include:
  - component: $CI_SERVER_FQDN/ci/grimoire-components/setup@1.1.0
    inputs:
      version: v0.7.0            # pin exactly — see below
      release_base_url: https://artifacts.example.com/grim/releases
      release_auth_header: "PRIVATE-TOKEN: $MIRROR_TOKEN"
```

The mirror must serve `<base>/download/<tag>/<asset>` plus the `.sha256`
sidecar next to every archive — mirror the GitHub release assets
verbatim (any raw HTTP store works: Artifactory, Nexus, a GitLab generic
package registry URL). The `latest` shortcut resolves through GitHub's
`<base>/latest/download/` redirect, which mirrors typically don't
implement — always pin an exact `vX.Y.Z`.

One more mirror is needed for the components themselves:
`include: component:` only resolves components hosted on the **same**
GitLab instance. Import or [pull-mirror][gl-mirror] the
[components project][gl-components] — **including its release tags**,
component versions resolve from tags — into your instance and include it
from there (the `$CI_SERVER_FQDN/ci/grimoire-components/…` path above).

> **GitHub Enterprise**: the same two inputs exist on the
> [setup-grimoire action][setup-grimoire] (`release-base-url`,
> `release-auth-header`). GHE without GitHub Connect cannot resolve
> marketplace actions — fork the action repo into your org and reference
> it as `uses: corp-org/setup-grimoire@v1`.

## Fork the Index {#index}

Import [grimoire-rs/index][index-repo] into your instance (e.g.
`platform/index`). The repo ships CI for **both** forges — GitHub
workflows under `.github/` and a `.gitlab-ci.yml` — so the import works
as-is; the foreign CI files stay inert (delete `.github/` if you like).

Then configure the project:

1. **Protect the default branch** (Maintainers + the bot may merge).
2. Create a [group access token][gl-gat] (role Maintainer, scope `api`)
   on the group that owns the index — or a project access token on the
   index project. `api` scope is required: the validation job merges
   MRs and reads the members API, which the job token cannot do
   (`CI_JOB_TOKEN` has no MR-merge or members endpoints).
3. Store it as the **masked** CI/CD variable `GRIM_INDEX_BOT_TOKEN` on
   the index project.

Optional variables:

| Variable | Effect |
|---|---|
| `GRIM_INDEX_MIN_ACCESS_LEVEL` | Membership threshold for auto-merge (default `30` = Developer) |
| `GRIM_INDEX_REGISTRY_AUTH` | `user:token` for the reachability check against a **private** container registry (the anonymous token dance fails there) |

### What auto-merges {#index-automerge}

The `validate-mr` job applies the
[same gate](./package-index.md#spec-validation) as the public index,
with GitLab identities:

1. Only `index/<your-host>/<namespace>/<pkg>/metadata.json` paths
   changed. `<namespace>` is the full group path — nested groups
   (`platform/ai`) work naturally.
2. The MR author owns each namespace: it is their username, or a group
   they are a member of (inherited membership counts) at
   `GRIM_INDEX_MIN_ACCESS_LEVEL` or above.
3. `owner.login` matches the namespace and `owner.id` matches the
   GitLab **namespace id** (live API lookup).
4. Schema valid; `ref` lists at least one tag on its registry.

On success the job requests merge-when-pipeline-succeeds; anything else
falls to manual review. Merges to the default branch trigger the `pages`
job, which compiles and serves `all.json` via GitLab Pages.

### Security model {#index-security}

The trust boundary is **project membership**. MR pipelines execute the
source branch's CI config, so any member who can push a branch could
read a CI variable — hence: keep the index project private, mask the
token, protect the default branch, and treat membership as the
publishing permission. Fork MRs are safer by default: GitLab does not
expose the project's CI variables to fork pipelines. The validate job
additionally re-checks out `scripts/` from the trusted target branch and
treats the MR tree as data only. For the strictest setup, pin the
[CI/CD configuration file][gl-external-ci] to an external project so MRs
cannot alter the executed pipeline at all.

## Announce from Publishing Projects {#announce}

A publishing project on the same instance needs exactly two `[announce]`
values — everything else auto-detects from the CI environment because
the index host equals `CI_SERVER_HOST`:

```toml
# publish.toml
registry = "registry.example.com"
repository_prefix = "platform/skills"

[announce]
repository = "https://gitlab.example.com/platform/index.git"
namespace  = "platform"          # full group path
```

```yaml
include:
  - component: $CI_SERVER_FQDN/ci/grimoire-components/publish@1.1.0
    inputs:
      announce: true
      announce_token: $INDEX_ANNOUNCE_TOKEN
```

The token becomes `GRIM_ANNOUNCE_TOKEN` for grim (always wins) and the
git credential for the push. Use a **group access token** of the owning
group: its bot user is a real group member, so the ordinary
membership check auto-merges its MRs — no validator allowlist needed.
(A central bot announcing for *other* groups' namespaces goes into the
`TRUSTED_BOTS` dict in the index fork's `scripts/validate_mr.py`.)

grim resolves the owner id via the API automatically; pin it with
`owner_id = <namespace id>` (from
`GET /api/v4/namespaces/<url-encoded-path>`) for hermetic runs.

Cross-forge announcing — e.g. a GitLab pipeline announcing to the
**public** GitHub index — inherits nothing from the CI environment (the
hosts differ, by design). Set `announce_token` to a GitHub token and the
github.com convention does the rest; the full resolution chains are in
[Announcing Packages](./package-index.md#announcing).

## Consume the Index {#consume}

Consumers on developer machines and CI point a registry entry at the
index repo — the git transport is the corporate path, private repos work
through ambient git credentials (credential helper, ssh agent; grim
never prompts):

```toml
[[registries]]
alias = "platform"
oci   = "registry.example.com/platform/skills"
index = "https://gitlab.example.com/platform/index.git"
```

GitLab Pages serving `all.json` also works as an `index =` locator, but
only when the Pages site is public — grim's HTTP transport does not
carry Pages access-control cookies. Prefer the git transport on private
instances. Details: [Consuming an Index](./package-index.md#consuming).

<!-- external -->
[gl-components]: https://gitlab.com/grimoire-rs/components
[gl-mirror]: https://docs.gitlab.com/user/project/repository/mirror/pull/
[gl-gat]: https://docs.gitlab.com/user/group/settings/group_access_tokens/
[gl-external-ci]: https://docs.gitlab.com/ci/pipelines/settings/#specify-a-custom-cicd-configuration-file

<!-- grimoire -->
[index-repo]: https://github.com/grimoire-rs/index
[setup-grimoire]: https://github.com/grimoire-rs/setup-grimoire
