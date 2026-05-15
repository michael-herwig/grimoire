# Contributing to Grimoire

## Prerequisites

- **Rust** (edition 2024) — install via [rustup](https://rustup.rs/)
- **[task](https://taskfile.dev)** — primary task runner (`brew install go-task` or see docs)
- **[uv](https://docs.astral.sh/uv/)** — Python toolchain for acceptance tests

## Layout

Single binary crate:

| Path | Purpose |
|------|---------|
| `src/` | The `grim` CLI (clap-based) |
| `test/` | Python (pytest) black-box acceptance suite |
| `.claude/` | AI-assisted development config (rules, skills, agents, hooks) |
| `taskfiles/` | Task automation modules |

## Building

```sh
cargo check                  # fast syntax/type check
cargo build                  # debug build
cargo build --release        # release `grim` binary
```

## Running Tests

**Unit tests:**

```sh
cargo nextest run
```

**Acceptance tests:**

```sh
task test              # build binary, run pytest suite
task test:quick        # skip binary rebuild
task test:parallel     # run tests in parallel with pytest-xdist
```

Acceptance tests live in `test/` and exercise the built `grim` binary.

## Code Style

```sh
cargo fmt              # format (max_width=120, see rustfmt.toml)
cargo clippy --all-targets
```

Format before every commit. CI enforces both.

## Commit Conventions

All commits must follow [Conventional Commits](https://www.conventionalcommits.org/):

```
feat: add publish command
fix: handle missing manifest
refactor: extract registry client
ci: add verify step to release workflow
```

Scopes are optional. cocogitto validates commit messages in CI.

## Branch Model

- Branch from `main` — never commit directly to `main`.
- Keep commits atomic and complete — no WIP commits on shared branches.

## Before Submitting

```sh
task verify    # fmt check + clippy + build + unit tests + acceptance tests
```

All checks must pass before opening a pull request.
