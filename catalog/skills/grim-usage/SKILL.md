---
name: grim-usage
description: Drive the grim CLI — the OCI package manager for AI skills, rules, agents, and bundles. Use when installing, updating, searching, or publishing AI-config artifacts with grim; when composing grim init, add, lock, install, update, status, search, build, release, publish, login, or logout commands; or when resolving registries, project vs global scope, client targets, or offline mode.
license: Apache-2.0
compatibility: grim>=0.4
metadata:
  summary: How to use the grim CLI end to end
  keywords: grim,grimoire,cli,oci,registry,install,update,publish,skills,rules,agents,bundles
  repository: https://github.com/michael-herwig/grimoire
---

# Grim Usage

Grimoire (binary: `grim`) is a package manager for AI-agent configuration.
It distributes four artifact kinds — **skills**, **rules**, **agents**, and
**bundles** — through any standard OCI registry (GHCR, Docker Hub, a private
Distribution), with lockfile-pinned installs into AI clients such as Claude
Code, OpenCode, and GitHub Copilot.

## Verify Before Acting

Before composing any non-trivial grim command:

1. Run `grim --version`. This guide is written against grim 0.4.x; on a
   different minor, treat every flag mentioned here as a hypothesis.
2. Run `grim <command> --help` before using flags you have not verified
   this session — it is the authoritative, always-current flag list.
3. On any conflict between this skill and live `--help` output, **trust
   `--help`**. It ships with the binary; this guide can lag.

These pages teach workflows and semantics, never exhaustive flags. The
full reference is `--help` plus the docs site linked below.

## Command Map

| Command | Purpose | Details |
|---|---|---|
| `grim init` | Create a fresh `grimoire.toml` | [consume](references/consume.md) |
| `grim add` | Declare an artifact and pin it in the lock | [consume](references/consume.md) |
| `grim lock` | Resolve floating tags to digests | [consume](references/consume.md) |
| `grim install` | Materialize the lock into AI clients | [consume](references/consume.md) |
| `grim update` | Re-resolve, re-materialize, prune | [consume](references/consume.md) |
| `grim status` | Report each declared artifact's state | [consume](references/consume.md) |
| `grim remove` / `uninstall` | Undeclare vs full inverse of install | [consume](references/consume.md) |
| `grim search` / `tui` | Browse a registry's catalog | [registries](references/registries.md) |
| `grim build` | Validate and pack locally, no push | [publish](references/publish.md) |
| `grim release` | Validate, pack, push with cascade tags | [publish](references/publish.md) |
| `grim publish` | Batch-release packages from a `publish.toml` manifest | [publish](references/publish.md) |
| `grim login` / `logout` | Manage registry credentials | [publish](references/publish.md) |
| `grim schema` | Emit the JSON Schema for `grimoire.toml` / `publish.toml` | [publish](references/publish.md) |

## Reference Syntax

An artifact is named `registry/repository:tag` (a floating tag — `:1`
follows the newest `1.x` release) or `registry/repository@sha256:…` (an
immutable digest). A bare reference defaults to `:latest`. A short
reference with no registry resolves against the default registry —
`--registry` flag, then `GRIM_DEFAULT_REGISTRY`, then config, then the
built-in default `grim.ocx.sh`; full
precedence in [references/registries.md](references/registries.md).

## Routing Table

| Read... | ...when |
|---|---|
| [references/consume.md](references/consume.md) | Installing, updating, or removing artifacts in a project |
| [references/publish.md](references/publish.md) | Building, releasing, tagging, or logging in to publish |
| [references/registries.md](references/registries.md) | Resolving registries, scopes, client targets, offline mode, or searching |
| [references/troubleshooting.md](references/troubleshooting.md) | A grim command failed — exit codes, integrity gates, common causes |
| [references/updating.md](references/updating.md) | Maintaining this skill itself against newer grim releases |

## Further Reading

- [Command reference][commands] — every command with current flags.
- [Concepts][concepts] — kinds, references, the lock, bundles, scopes,
  clients.
- [Configuration][config] — `grimoire.toml`, `grimoire.lock`, environment
  variables.
- [Publishing][publishing] — the author-to-release workflow.
- [Authentication][auth] — credential store, login/logout, CI recipes.

[commands]: https://michael-herwig.github.io/grimoire/commands.html
[concepts]: https://michael-herwig.github.io/grimoire/concepts.html
[config]: https://michael-herwig.github.io/grimoire/configuration.html
[publishing]: https://michael-herwig.github.io/grimoire/publishing.html
[auth]: https://michael-herwig.github.io/grimoire/authentication.html

---

Verified against grim 0.4.x.
