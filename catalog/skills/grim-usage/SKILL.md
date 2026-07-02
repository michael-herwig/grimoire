---
name: grim-usage
description: Drive the grim CLI — the OCI package manager for AI skills, rules, agents, and bundles. Use when installing, updating, searching, or publishing AI-config artifacts with grim; when composing grim init, config, add, lock, install, update, status, search, tui, mcp, build, release, publish, login, or logout commands; when configuring settings, multiple registries, or qualified alias/repo references; or when resolving registries, project vs global scope, client targets, or offline mode.
license: Apache-2.0
compatibility: grim>=0.6
metadata:
  summary: How to use the grim CLI end to end
  keywords: grim,grimoire,cli,oci,registry,install,update,publish,skills,rules,agents,bundles,mcp,multi-registry
  repository: https://github.com/grimoire-rs/grimoire
---

# Grim Usage

Grimoire (binary: `grim`) is a package manager for AI-agent configuration.
It distributes four artifact kinds — **skills**, **rules**, **agents**, and
**bundles** — through any standard OCI registry (GHCR, Docker Hub, a private
Distribution), with lockfile-pinned installs into AI clients such as Claude
Code, OpenCode, and GitHub Copilot.

## Verify Before Acting

Before composing any non-trivial grim command:

1. Run `grim --version`. This guide is written against grim 0.6.x; on a
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
| `grim config` | Read/write `grimoire.toml` settings and registries | [registries](references/registries.md) |
| `grim add` | Declare an artifact and pin it in the lock | [consume](references/consume.md) |
| `grim lock` | Resolve floating tags to digests | [consume](references/consume.md) |
| `grim install` | Materialize the lock into AI clients | [consume](references/consume.md) |
| `grim update` | Re-resolve, re-materialize, prune | [consume](references/consume.md) |
| `grim status` | Report each declared artifact's state | [consume](references/consume.md) |
| `grim remove` / `uninstall` | Undeclare vs full inverse of install | [consume](references/consume.md) |
| `grim search` / `tui` | Browse your declared registries' catalogs | [registries](references/registries.md) |
| `grim mcp` | Run a local STDIO MCP server for AI agent integration | [registries](references/registries.md) |
| `grim build` | Validate and pack locally, no push | [publish](references/publish.md) |
| `grim release` | Validate, pack, push with cascade tags | [publish](references/publish.md) |
| `grim publish` | Batch-release packages from a `publish.toml` manifest | [publish](references/publish.md) |
| `grim login` / `logout` | Manage registry credentials | [publish](references/publish.md) |
| `grim schema` | Emit the JSON Schema for `grimoire.toml` / `publish.toml` | [publish](references/publish.md) |

> **Deprecation (0.6.x):** a publisher can retire a package without
> unpublishing it; `add`, `status`, `search`, and `tui` then flag it as
> deprecated (an `add` of a deprecated reference still succeeds). This is
> runtime output, not a flag — see [Publishing][publishing]; `grim <cmd>
> --help` does not list it.
>
> **Git provenance (0.6.x):** `build`, `release`, and `publish` can embed
> the publishing commit, date, and origin as OCI annotations via opt-in
> `--git`; confirm with `grim release --help`.

## Reference Syntax

An artifact is named `registry/repository:tag` (a floating tag — `:1`
follows the newest `1.x` release) or `registry/repository@sha256:…` (an
immutable digest). A bare reference defaults to `:latest`. A short
reference with no registry resolves against the default registry —
`--registry` flag, then `GRIM_DEFAULT_REGISTRY`, then config, then the
built-in fallback registry `ghcr.io/grimoire-rs`; full
precedence in [references/registries.md](references/registries.md). Browsing
with nothing configured (`grim search`, `grim tui`, `grim mcp`) falls back
to the public package index at `https://index.grimoire.rs` instead — see
[references/registries.md](references/registries.md#multiple-registries).

When a config declares `[[registries]]` with aliases, a **qualified
reference** `alias/repo[:tag]` expands the alias to its configured URL —
for example `acme/code-review:1` becomes `ghcr.io/acme/code-review:1`
when `acme` is aliased to `ghcr.io/acme`. Full details and the
multi-registry browse behavior in
[references/registries.md](references/registries.md).

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

[commands]: https://grimoire.rs/commands.html
[concepts]: https://grimoire.rs/concepts.html
[config]: https://grimoire.rs/configuration.html
[publishing]: https://grimoire.rs/publishing.html
[auth]: https://grimoire.rs/authentication.html

---

Verified against grim 0.6.2.
