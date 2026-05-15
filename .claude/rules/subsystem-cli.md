---
paths:
  - src/**
---

# CLI Subsystem

Thin CLI shell built on clap. One file per subcommand under
`src/command/`. Structured output goes through a shared output trait so
plain-text and `--format json` render from one source.

> **Status: provisional.** Only `src/main.rs` exists today. This file
> records the intended structure and conventions, not shipped behavior.

## Design Rationale

Keep the CLI thin: argument parsing, a per-invocation context, and
delegation to operations. Business logic lives in plain modules so it is
testable without the CLI. A single context struct is initialized lazily
once per invocation — do not build clients/state that the command will not
use.

## Command Pattern

Every command follows the same flow:

1. **Parse + transform** — parse user references into typed identifiers
2. **Run the operation** — delegate to a domain function
3. **Build report data** — from the operation's return values, never from
   the raw CLI args alone
4. **Render** — emit via the shared output trait (plain or JSON)

## Output Layer Rules

- **Single table**: each plain renderer makes exactly one table call
- **Static headers**: use `&str` arrays, never `format!()` for headers
- **Typed enums**: status values are enums with `Display` + `Serialize`,
  not raw strings
- **Report actual results**: build data from operation return values
- **Preserve input order**: zip results with original identifiers

See `subsystem-cli-api.md` for the full output-layer contract and
`subsystem-cli-commands.md` for the command index.

## Configuration Forwarding

Any code that spawns a subprocess must apply the running process's parsed
configuration to the child environment through one dedicated function —
that function is the sole path that lands `GRIM_*` keys on a child env.
The parsed config is authoritative; never rely on ambient parent-shell
exports. Resolution-affecting flags (offline / remote / config / index)
propagate via env and must be documented in the env-var reference;
presentation flags (log-level / format / color) must NOT propagate.

## Quality Gate

During the review-fix loop, run `task rust:verify` — not the full
`task verify`. Full `task verify` is the final gate before commit.
