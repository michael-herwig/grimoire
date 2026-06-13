# CLAUDE.md

This file guides Claude Code (claude.ai/code) when working in this repo.

## What is Grimoire

Grimoire is an OCI-backed package manager for AI skills and rules — a CLI
to install, maintain, and publish AI-agent configuration (skills, rules,
prompts) distributed through standard OCI registries. The binary is named
`grim`; the Rust crate/package is `grimoire`.

> **Status: provisional.** This is early scaffolding. The product vision
> will be fleshed out by the maintainer. Treat product/architecture docs
> as a sensible placeholder, not a finalized contract.

## Current State

Early stage: a single binary crate with `src/main.rs` as the only source
file today. No stable API, CLI, or config yet — for refactors, expect to
delete and start over.

## Project Identity

Full product vision, target users, and positioning →
[`product-context.md`](./.claude/rules/product-context.md). Consult when
reasoning about project direction, scope trade-offs, ADR motivation, or
research framing. Canonical product context — keep current (update
protocol at the bottom of that file).

## Rule Catalog

Before planning, research, or an architectural decision, scan "By concern"
in the catalog below. Auto-loaded rules (via path globs) fire when editing
matching files; the catalog covers cases needing guidance *before* a file
is open.

@.claude/rules.md

## Build & Development Commands

**Task runner**: [`task`](https://taskfile.dev) (Taskfile v3) is the
primary runner. **Always check `task --list` before inventing ad-hoc
commands.** Taskfiles are tree-structured: root (`taskfile.yml`),
subsystem dirs (`test/`, `.claude/`), and `taskfiles/*.taskfile.yml` for
cross-cutting concerns.

**Key workflows:**
```sh
task                           # fast check (format + clippy + cargo check)
task verify                    # full quality gate (lint, then build + tests)
task --force verify            # bypass caching — run everything
task rust:verify               # Rust-only gate
task shell:verify              # shell-only gate (shellcheck + shfmt)
task claude:tests              # AI config structural tests
```

**Cargo commands** (for finer control): `cargo check`, `cargo build
--release` (binary `grim`), `cargo fmt`, `cargo clippy`, `cargo test`.

**Always run `task verify` after implementation is done.** Always run
`cargo fmt` before commit. Subsystem verify tasks (`rust:verify`,
`shell:verify`, `claude:verify`) are AI dev-loop gates — run the subsystem
gate for the code changed; full `task verify` is the final gate before
commit. Conventions →
[subsystem-taskfiles.md](./.claude/rules/subsystem-taskfiles.md).

## Architecture

**Layout**: a single binary crate. All source under `src/`; binary `grim`;
crate/package `grimoire`. No workspace, no lib/CLI split. Acceptance tests
under `test/`. Rust edition 2024.

**Subsystem context** rules auto-load on matching files:

| Subsystem | Rule | Scope |
|-----------|------|-------|
| Storage / file structure | [subsystem-file-structure.md](./.claude/rules/subsystem-file-structure.md) | `src/**` |
| CLI shell | [subsystem-cli.md](./.claude/rules/subsystem-cli.md) | `src/**` |
| Acceptance tests (pytest, fixtures) | [subsystem-tests.md](./.claude/rules/subsystem-tests.md) | `test/**` |

**Read the relevant subsystem rule before working on code in that area.**

## Environment Variables

| Variable | Purpose | Default |
|---|---|---|
| `GRIM_HOME` | Root data directory (content store, catalog, global config, global-scope install state at `$GRIM_HOME/state/global.json`). Project-scope install state lives at `<workspace>/.grimoire/state.json`. Global-scope client output lands in vendor-native dirs — see subsystem-file-structure.md | `~/.grimoire` |
| `GRIM_DEFAULT_REGISTRY` | Default registry for short identifiers. Registry precedence: `--registry` flag > `GRIM_DEFAULT_REGISTRY` > project config `[options].default_registry` > global config > built-in fallback `grim.ocx.sh` | (unset) |
| `GRIM_OFFLINE` | Disable all network access (cache-only; default is always-fresh online resolution) | false |
| `DOCKER_CONFIG` | Directory holding the docker-compatible `config.json` read/written by `grim login`/`logout` (and the credential read path) | `~/.docker` |
| `OPENCODE_CONFIG` | OpenCode config file that grim edits for global-scope rule registration (vendor variable, honored read/write). When unset, grim falls back to `$XDG_CONFIG_HOME/opencode/opencode.json` (or `~/.config/opencode/opencode.json` if `XDG_CONFIG_HOME` is also unset). Config-file-only — no effect on skill/agent paths | (unset) |
| `CLAUDE_CONFIG_DIR`, `COPILOT_HOME`, `OPENCODE_CONFIG_DIR` | Vendor config-dir overrides (honored read-only). Global-scope installs follow them: `CLAUDE_CONFIG_DIR` replaces `~/.claude` (skills, rules, and agents), `COPILOT_HOME` replaces `~/.copilot` (skills and agents), `OPENCODE_CONFIG_DIR` is the preferred install target over the XDG default for OpenCode skills and agents (additive — OpenCode scans both). They also drive global-scope client *detection* — a client counts as present when its (possibly overridden) native root exists. Details → subsystem-file-structure.md | (unset) |

## First-Party Catalog

`catalog/` holds grim-publishable packages (skills `grim-usage`,
`ai-config-authoring`, `grim-authoring` + the `grim-essentials` bundle).
**CLI (`src/command/**`) or docs-page changes require a drift review of
these skills** — duty + procedure: [catalog/README.md](./catalog/README.md).
Hooks remind on matching edits; `task catalog:verify` gates CI.

## Deep Context

- `.claude/rules/product-context.md` — product vision and positioning
- `.claude/rules/arch-principles.md` — design principles (auto-loads on
  Rust files)

## Core Principles

Eight principles distill every rule, skill, and standard in the framework.
Follow them and everything else follows.

### 1. Understand First

Read before write. Grep before create. Never modify code not read — before
changing a function, grep all callers. Check what exists before building
new.

### 2. Prove It Works

Write tests for the use case first. Run them before commit. Every bug fix
gets a regression test. All quality gates must pass.

### 3. Keep It Safe

No secrets in code. Validate all external input. Least privilege
everywhere. Flag vulnerabilities immediately.

### 4. Keep It Simple

Small functions, single responsibility. No premature abstraction. Delete
dead code. Comments explain *why*, never *what*.

### 5. Don't Repeat Yourself

Check `.claude/skills/` before ad-hoc generation. Follow existing patterns.
Single source of truth for logic. Extract only when duplication is real.

### 6. Ship It

Work on a branch, never main. Commit iteratively. **Never push to remote**
— the human decides when to push. Push triggers CI, real cost.

### 7. Leave a Trail

Planning artifacts go in `./.claude/artifacts/`. Document architectural
decisions in ADRs. Name things so the next person understands.

### 8. Learn and Adapt

When you get user feedback or corrections, evaluate whether the insight
should persist as an AI config update (rules, skills, agents).

## Tech Stack

@.claude/rules/product-tech-strategy.md

## Workflow

**Worktrees**: Four git worktrees with fixed branch names:

| Directory | Branch |
|-----------|--------|
| `grimoire` | `goat` |
| `grim-evelynn` | `evelynn` |
| `grim-sion` | `sion` |
| `grim-soraka` | `soraka` |

**Commits**: Use [Conventional Commits](https://www.conventionalcommits.org/)
(e.g., `feat:`, `fix:`, `refactor:`, `ci:`, `chore:`). Scopes optional. No
`Co-Authored-By` trailers. Use `chore:` for AI settings, skills, CLAUDE.md,
and tooling files that should not appear in the changelog.

**Landing a feature**: When a feature is done, run `/finalize` to clean
branch history into a sequence of Conventional Commits ready to
fast-forward onto `main`. Two-phase model (`/commit` during dev,
`/finalize` before landing) →
[workflow-git.md](./.claude/rules/workflow-git.md).

**Planning flow**: ADR → Design Spec → Plan → Implementation. All planning
docs stored in `./.claude/artifacts/`; templates in
`./.claude/templates/artifacts/`.

## Skills & Personas

Persona skills (`/architect`, `/builder`, `/qa-engineer`,
`/security-auditor`, `/code-check`, `/swarm-plan`, `/swarm-execute`,
`/swarm-review`) and task skills live in `.claude/skills/`. Full map →
"Skills by task topic" in [.claude/rules.md](./.claude/rules.md). Check
`.claude/skills/` before ad-hoc generation.

## Starting Work

Every task starts with
[workflow-intent.md](./.claude/rules/workflow-intent.md) — classify work
(feature, bug fix, refactoring), check GitHub for related issues/PRs, then
follow the appropriate workflow. Also:
[workflow-feature.md](./.claude/rules/workflow-feature.md),
[workflow-bugfix.md](./.claude/rules/workflow-bugfix.md),
[workflow-refactor.md](./.claude/rules/workflow-refactor.md).
