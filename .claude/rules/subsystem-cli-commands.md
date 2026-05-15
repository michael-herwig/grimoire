---
paths:
  - src/**
---

# Grimoire CLI Commands — Quick Reference

Concise index of `grim` CLI commands. Implementation lives under
`src/command/` — read the source for return types, call sites, and report
column formats.

> **Status: provisional.** The CLI is not implemented yet (only
> `src/main.rs` exists). The command surface below is illustrative of the
> intended shape, not a description of shipped behavior. Update this file
> as commands land — keep it a faithful index, never speculative.

## Command Surface (illustrative)

| Command | Purpose |
|---------|---------|
| `grim install <ref>` | Fetch and install an AI-config artifact (skill/rule set) |
| `grim list` | List installed artifacts |
| `grim update [<ref>]` | Pull newer versions |
| `grim remove <ref>` | Uninstall an artifact |
| `grim publish <path> <ref>` | Push an artifact to a registry |
| `grim version` | Print the compiled version |

Global flags (illustrative): `--offline`, `--remote`, `--format json`.

## Conventions (apply as commands land)

- **One file per subcommand** under `src/command/`.
- **Typed identifiers**: parse user-supplied references into domain types
  early; the rest of the command works on typed values, not raw strings.
- **Report actual results**: a command reports what happened, not an echo
  of its input. Operations return enough data to build accurate output.
- **Exit codes**: follow `quality-rust-exit_codes.md` — usage errors,
  data errors, and I/O errors map to distinct, documented codes.
- **Output**: structured data goes through the shared output trait so
  `--format json` and the plain table render from one source.

## Cross-References

- `subsystem-cli.md` — CLI shell structure and clap usage
- `subsystem-cli-api.md` — output / report data layer patterns
- `quality-rust-exit_codes.md` — exit code design
