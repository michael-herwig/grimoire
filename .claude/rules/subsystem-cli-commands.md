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
| `grim remove <kind> <name>` | Undeclare an artifact (config + lock only; files left on disk) |
| `grim uninstall <kind> <name>` | Full inverse of install: delete files, drop the install record, undeclare (config + lock). Shared seam reused by the TUI delete action |
| `grim release <path> <ref>` | Push a single artifact to a registry (validate, pack, push with cascade tags) |
| `grim publish` | Batch-release all packages declared in a `publish.toml` manifest; validates whole manifest before any push; fixed kind order (skills → rules → agents → bundles), skip-existing by default |
| `grim login [<registry>]` | Authenticate to a registry; store the credential via the docker-compatible credential store (helper or, with `--allow-insecure-store`, plaintext) |
| `grim logout [<registry>]` | Remove a stored registry credential (idempotent — exits 0 when nothing is stored) |
| `grim schema --kind <config\|publish>` | Print the JSON Schema for `grimoire.toml` or `publish.toml` to stdout (generated from the real parse structs); emits a document, not a `Printable` report |
| `grim version` | Print the compiled version |

Global flags (illustrative): `--offline`, `--remote`, `--format json`.

`login`/`logout` resolve the registry from the positional argument, else
`--registry` / the `default_registry` option / `GRIM_DEFAULT_REGISTRY`.
They read and write the docker config at `$DOCKER_CONFIG/config.json`
(default `~/.docker/config.json`) — the same file the credential read path
consults — so credentials round-trip with `docker login`.

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
