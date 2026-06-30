# ADR: `grim config` — a git-style config command for settings and registries

<!--
Architecture Decision Record (MADR-flavored).
Owner: Architect (/architect). Handoff to: /swarm-plan, /swarm-execute, /swarm-review.
-->

## Metadata

**Status:** Accepted
**Date:** 2026-06-30
**Deciders:** Maintainer (Michael Herwig); Architect (/architect)
**Beads Issue:** N/A
**Related PRD:** N/A
**Tech Strategy Alignment:**
- [x] Follows Golden Path in `product-tech-strategy.md` (Rust 2024, clap, Tokio). One new
      crate proposed (`toml_edit`) is **deferred** — v1 reuses the existing hand-rolled writer.
- [x] No deviation.
**Domain Tags:** api, config, cli
**Supersedes:** N/A (builds on `adr_multi_registry_mcp.md`, `adr_registry_default_dedup.md`)
**Superseded By:** N/A

## Context

`grimoire.toml` (project scope) and `$GRIM_HOME/grimoire.toml` (global scope) hold all
user-facing configuration. Today there is **no CLI to read or edit it** — users hand-edit
TOML. The only programmatic writers are `grim init` (creates the file) and `grim add` /
`grim remove` (round-trip it while editing declarations). There is no `grim config`, no
`grim registry`.

The maintainer wants a `git config`-style command so settings and registries can be
queried and mutated from the CLI — crucially, so **migration scripts** can adjust user
config deterministically without a TOML parser in bash.

Two distinct kinds of content live in the file:

1. **Settings** — `[options]` (`default_registry` *(legacy)*, `clients`, `[options.tui]`
   with `default_view` / `group_by_type` / `tree_separators`).
2. **Named registries** — the `[[registries]]` array (`url`, optional `alias`, `default`).

A third kind — `[skills]` / `[rules]` / `[agents]` / `[bundles]` **declarations** — is
explicitly **out of scope** (see Decision Drivers): those are owned by
`add` / `remove` / `install` / `lock`, which re-resolve the lockfile on every edit.

### Confirmed current state (file:line evidence)

- Data model: `src/config/declaration.rs` — `ConfigOptions` (82–104), `TuiOptions`
  (42–62), `DefaultView` (27–33), `RegistryConfig` (113–130). `RegistryConfig.alias` is
  `Option<String>`; `url` required; `default: bool`, at most one `true`.
- Containers: `ProjectConfig` (`src/config/project_config.rs:27–35`), `GlobalConfig`
  (`src/config/global_config.rs:19–27`) — identical field layout, shared parser
  (`GlobalConfig::from_toml_str` → `ProjectConfig::from_toml_str`).
- Validation: `validate_registries` (`src/config/project_config.rs:184–252`) — alias
  uniqueness, no `/`, no surrounding whitespace, non-empty url. **At-most-one-default is
  enforced** (per `adr_registry_default_dedup.md`).
- The **only** `grimoire.toml` write seam: `write_config` (`src/command/add.rs:319–408`),
  `pub(crate)`, **manual string construction** (no `toml::to_string` anywhere), via
  `store::atomic_write::atomic_write`. Comments and the `#:schema` directive are **already
  dropped** on every `add`/`remove` re-serialize today.
- RMW safety: `ConfigFileLock` advisory flock (`src/command/add.rs:76–79`).
- Scope + path resolution: `command::scope_resolution::resolve(ctx, global, config_path)`
  (`src/command/scope_resolution.rs`). Two scopes, **never merged**.
- CLI: clap derive, one file per command under `src/command/`; handler returns
  `(Report, ExitCode)`; dispatch in `src/app.rs`; output via `Printable` + `print_table`
  (`src/cli/printer.rs`). Reference report: `src/api/login_report.rs`.
- Exit codes (`src/cli/exit_code.rs`): `Success=0`, `Failure=1`, `UsageError=64`,
  `DataError=65`, `IoError=74`, `ConfigError=78`, `NotFound=79`.

## Decision Drivers

- **Migration-script friendly** — explicit, composable verbs; `--format json`; stable,
  documented exit codes; idempotent where it can be.
- **Match the maintainer's mental model** — `git config`-style dotted keys, `--global`
  flag, registries addressed like git remotes (`registry.<alias>.url`).
- **One umbrella** — the maintainer wants "one config subcommand, maybe with nested
  subcommands." Everything in `grimoire.toml` reachable under `grim config`.
- **Don't break invariants** — at-most-one-default and registry validation must hold on
  every write; reuse the existing validated model, never write raw TOML blind.
- **Minimal blast radius / KISS / YAGNI** — reuse `write_config`, `scope_resolution`,
  `ConfigFileLock`, `validate_registries`; add no infra not required for v1.
- **Separation of concerns** — declarations stay with `add`/`remove` (lock coupling);
  auth stays with `login`/`logout` (docker config, not `grimoire.toml`).

## Industry Context & Research

Full synthesis lives inline in this session's research; summary of the seven tools
surveyed (git, cargo, gh, npm, docker, kubectl, aws):

- **Explicit `get`/`set`/`unset`/`list` verbs have won.** git itself deprecated its
  implicit positional form (`git config key [value]`) in 2.41 for `git config get|set`.
  Every modern CLI (gh, npm, aws, pixi, uv, mise) uses explicit verbs.
- **Named-object lists split two ways**: git's dotted subsection (`remote.<name>.url`) vs
  dedicated subcommands (docker `context create/use/ls/rm`, kubectl `*-context`). Dedicated
  verbs win on `--help` discoverability; dotted keys win on script composability.
- **"Set the default" is a verb**: docker `context use`, kubectl `use-context`. Clean and
  discoverable; encapsulates the "clear the previous default" invariant.
- **`cargo` has no stable `config get`** (tracking issue open since 2021) — a cautionary
  tale: without CLI introspection, users grep TOML by hand. We will not repeat that.
- **Scope flags**: git's `--global`/`--local`/`--system`. grim already uses `--global`
  with an implicit project default — we keep that, not introduce `--local`.

**Key insight:** the maintainer's two stated wishes (git dotted keys *and* "one config
subcommand with nested subcommands") are best served by a **hybrid under one umbrella** —
git-style dotted `get`/`set` as the universal scriptable core, plus a thin nested
`config registry` verb group for the operations dotted keys handle badly (notably
set-default).

## Considered Options

### Option A: Pure dotted-key, git-style (no registry subcommands)

`grim config get|set|unset|list <dotted.key>` covers everything, including registries via
`registry.<alias>.url` / `.default`.

| Pros | Cons |
|------|------|
| Smallest surface; one mental model | "Set default" is awkward — must clear the prior `default=true` to keep at-most-one |
| Maximally scriptable / uniform | No `--help` enumeration of registry fields; user must know key names |
| Mirrors maintainer's `registry.name` phrasing | `unset registry.<alias>` to delete a whole entry reads oddly |

### Option B: Hybrid — dotted scalars + **nested** `config registry` verbs (CHOSEN)

`grim config get|set|unset|list` for scalars and registry *fields*; `grim config registry
add|rm|use|show|list` for registry lifecycle. All under one `config` umbrella.

| Pros | Cons |
|------|------|
| Honors "one config subcommand with nested subcommands" | Larger surface than A |
| `config registry use <alias>` cleanly encapsulates at-most-one-default | Mild overlap: a registry url is reachable via both `set registry.x.url` and `registry add` |
| Each registry verb gets its own `--help` (discoverable) | — |
| Dotted `get`/`set` still available for migration scripts (uniformity) | — |
| Maps directly to clap `#[command(subcommand)]` nesting | — |

### Option C: Hybrid — dotted scalars + **top-level** `grim registry` verbs

Like B, but registries are a sibling of `grim login` (`grim registry add …`), not nested.

| Pros | Cons |
|------|------|
| Shorter to type; mirrors docker `context` / `gh auth` | Splits the config surface into two top-level nouns |
| Registries treated as their own concept | Contradicts maintainer's explicit "one config subcommand" steer |
| — | Registries *are* `grimoire.toml` content — unlike `login` (docker config) |

## Decision Outcome

**Chosen: Option B** — hybrid surface, registry verbs **nested under `grim config`**, with
**explicit get/set verbs** (no implicit positional form), scoped to **settings +
registries only**. (All three confirmed with the maintainer.)

**Rationale:** B is the only option that satisfies both stated wishes simultaneously — git
dotted-key ergonomics *and* one discoverable umbrella — while keeping registry invariants
in one validated place (`config registry use` for set-default). Explicit verbs are the
settled industry standard and the best fit for migration scripts (unambiguous, each verb
self-documents). Nesting (B over C) follows the maintainer's preference and the fact that
registries are literally `grimoire.toml` content, whereas auth (`login`/`logout`) writes a
*different* file (docker config) and rightly stays top-level. Declarations stay out because
editing them without lockfile re-resolution produces config/lock drift — `add`/`remove`
own that coupling.

### Consequences

**Positive:**
- First CLI path to read/write config; closes the "grep the TOML" gap cargo still has.
- Migration scripts get deterministic, JSON-capable, exit-code-stable primitives.
- Registry lifecycle (`add`/`rm`/`use`) is discoverable and invariant-safe.
- Zero new runtime dependency in v1 (reuses `write_config`).

**Negative:**
- New command surface to document and test (`docs/src/commands.md`, `configuration.md`,
  catalog skills `grim-usage` / `grim-authoring` drift review per `catalog/README.md`).
- v1 inherits the existing **lossy re-serialize** (drops comments + `#:schema`). This is
  the *current* behavior of `add`/`remove`, so it is no new regression — but a `config`
  command invites frequent edits, raising the cost. See Risks.
- Mild surface overlap (dotted `set registry.x.url` vs `registry add`) — acceptable; git
  has the same overlap (`git config remote.x.url` vs `git remote add`).

**Risks:**
- *Comment/schema-directive loss on every write.* Mitigation: v1 documents it; the
  **upgrade path** is to migrate `write_config` to `toml_edit` (already the backing crate
  of the `toml` dep, so near-zero new surface) — this benefits `add`/`remove` too and is
  recorded as a follow-up, not v1 scope.
- *Writing an invalid registry set.* Mitigation: the command **constructs the new
  `Vec<RegistryConfig>` and runs `validate_registries` before calling `write_config`**
  (which itself does not validate). Non-negotiable — see Technical Details.
- *Concurrent writers.* Mitigation: acquire `ConfigFileLock` around every read-modify-write
  exactly as `add.rs` does.

## Technical Details

### Command surface

```
grim config get   <key>                 # print one value (plain: bare value; json: {key,value,scope})
grim config set   <key> <value>         # set/replace one value
grim config unset <key>                 # remove a key (clears option / deletes registry entry)
grim config list  [--show-origin]       # all effective key=value for the scope

grim config registry add  <alias> --url <url> [--default]
grim config registry rm   <alias>
grim config registry use  <alias>       # mark default; clears any prior default
grim config registry show <alias>       # one registry's fields
grim config registry list               # all registries (default marked)

# Scope (all subcommands):  --global  → $GRIM_HOME/grimoire.toml
#                           (absent)  → project grimoire.toml (walk-up) or --config <path>
# Output:  global --format json|plain  (existing global flag)
```

### Key namespace (dotted ↔ model)

| Dotted key | Model field | Type / value parsing |
|---|---|---|
| `options.clients` | `ConfigOptions.clients` | comma-separated list, e.g. `claude,opencode`; empty string ⇒ unset |
| `options.default_registry` | `ConfigOptions.default_registry` | string *(legacy; `get`/`set` allowed, but `registry use` is preferred and documented as the modern path)* |
| `options.tui.default_view` | `TuiOptions.default_view` | enum `flat` \| `tree` (invalid ⇒ `DataError 65`) |
| `options.tui.group_by_type` | `TuiOptions.group_by_type` | bool `true` \| `false` |
| `options.tui.tree_separators` | `TuiOptions.tree_separators` | comma-separated single chars |
| `registry.<alias>.url` | matching `RegistryConfig.url` | string |
| `registry.<alias>.default` | matching `RegistryConfig.default` | bool — **set true routes through the same clear-others logic as `registry use`** |

**Naming note:** dotted keys use **singular** `registry.<alias>.*` (git's `remote.<name>.*`
ergonomic), even though the TOML table is `[[registries]]`. The `<alias>` segment maps to
`RegistryConfig.alias`.

### Edge semantics (the load-bearing ones; finer cases for the design spec)

- **Unknown key name** (typo `optins.clients`, or an unsupported leaf) → `UsageError 64`,
  message lists valid roots.
- **`get` of a valid-but-unset key** → exit `1` (`Failure`), no stdout (git-compatible,
  script-friendly: `grim config get … || default`). JSON mode emits `{"key":…,"set":false}`.
- **Registry alias not found** on `get`/`show`/`rm`/`use`/`set registry.x.*` →
  `UsageError 64` ("no registry 'x'; add it with `grim config registry add`").
- **Dotted `set registry.<alias>.<field>` requires the entry to already exist.** Creation
  is `registry add` only — keeps the url-required + validation invariant in one path and
  avoids half-built entries. (`unset registry.<alias>` with no field deletes the entry =
  `registry rm`.)
- **`registry add` of an existing alias** → `UsageError 64` (use dotted `set` or `rm`+`add`
  to change). Migration scripts wanting idempotent upsert use dotted `set registry.x.url`.
- **`registry use` / `set …default true`** sets the target `default=true` and clears it on
  all others (enforces at-most-one before write).
- **Aliasless registries** (hand-authored url-only entries) are shown in `registry list`
  (labelled by url) but are **not addressable by dotted key**; give them an alias to manage
  them. Documented limitation.

### Architecture (placement)

```
src/command/config.rs          NEW  — ConfigArgs { #[command(subcommand)] ConfigCommand }
                                      ConfigCommand::{Get,Set,Unset,List,Registry(RegistryArgs)}
                                      RegistryArgs { #[command(subcommand)] RegistryCommand }
                                      pub async fn run(ctx, args) -> Result<(report, ExitCode)>
src/command.rs                  EDIT — pub mod config;
src/main.rs                     EDIT — Command::Config(ConfigArgs)
src/app.rs                      EDIT — dispatch arm → command::config::run → render
src/api/config_report.rs        NEW  — ConfigGetReport, ConfigListReport, ConfigSetReport,
                                      RegistryListReport, RegistryShowReport (Printable each)
src/api.rs                      EDIT — pub mod config_report; re-exports
```

### Reuse seams (do NOT reinvent)

- **Write:** build a mutated `ConfigOptions` + `Vec<RegistryConfig>` + (unchanged)
  `DesiredSet`, then call `crate::command::add::write_config(path, &options, &registries,
  &set)`. It already does `atomic_write` and preserves declarations + the array.
- **Validate before write:** call `validate_registries(&registries, &path)` (make it
  `pub(crate)` if not already) so every mutation re-checks alias/url/at-most-one-default.
- **Scope + path:** `scope_resolution::resolve(ctx, global, config_path)`.
- **Lock:** `ConfigFileLock::try_acquire(&config_path)` for the whole RMW, as in `add.rs`.
- **Load:** project via `ProjectConfig::discover`/`load_from_path`; global via
  `GlobalConfig::load(ctx.paths().global_config())`.

### Output layer (per `subsystem-cli-api.md`)

- `get`: plain prints the **bare value** (no key) for `$(grim config get …)` ergonomics;
  JSON prints `{key,value,scope}`. One `print_table` is not used for the single bare value
  — `get` writes the raw string + newline (it is value-only, the gh-style script contract).
- `list` / `registry list`: exactly one `print_table` call; `--show-origin` adds an Origin
  column (project|global|file path), not a second table.
- Status/marker columns are typed enums with `Display` + `Serialize`, never raw strings
  (e.g. a `Default` marker column for `registry list`).

### Exit-code map (per `quality-rust-exit_codes.md`)

| Situation | Code |
|---|---|
| Success | `Success 0` |
| `get` of unset key | `Failure 1` (no stdout) |
| Unknown key name / bad subcommand args / alias not found / duplicate alias on add | `UsageError 64` |
| Invalid value format (bad enum, non-bool, bad separator char) | `DataError 65` |
| Config file parse failure (corrupt existing file) | `ConfigError 78` |
| `--config <path>` not found / project config required but absent | `NotFound 79` |
| Write / lock I/O failure | `IoError 74` |

## Back-compat & migration strategy

- Reading: unchanged. Legacy `[options].default_registry` still honored by the resolver;
  `config get options.default_registry` reads it; `config registry list` shows
  `[[registries]]`.
- Writing: reuses `write_config` — same lossy-but-safe behavior as today's `add`/`remove`
  (preserves structured content + the legacy field, drops comments). No on-disk version
  bump, no schema change.
- `toml_edit` comment-preserving writer is a **separate, later** change (benefits all
  writers); explicitly out of v1 scope.

## Test plan (outline; /qa-engineer + /swarm-execute Specify stage own the detail)

### Unit (Rust, inline `#[cfg(test)]`)
- Key parsing: each dotted key ↔ field; unknown key → 64; bad enum/bool → 65.
- Registry ops: `add` (validates, rejects dup), `rm` (deletes; missing alias → 64),
  `use`/`set default true` (clears prior default; at-most-one holds), `set
  registry.x.url` on missing alias → 64.
- Write path constructs a set that **passes `validate_registries`** before `write_config`;
  round-trip parse-back equals the intended model.
- Report types: plain (`print_table` one call; `get` bare value) + JSON shapes.

### Acceptance (pytest, `test/tests/`)
- `test_config.py` (new): `set`→`get` round-trips for `options.clients`,
  `options.tui.default_view`, `options.default_registry` at both scopes (`--global` and
  project); `unset`; `list` / `list --show-origin`; JSON output shape; exit codes (unset
  get → 1, unknown key → 64, bad enum → 65).
- `test_config_registry.py` (new): `registry add`/`list`/`show`/`use`/`rm`; at-most-one
  default after `use`; dup-add → 64; missing-alias ops → 64; a project then `--global`
  registry add resolves end-to-end (short-id `add` against it).
- Concurrency smoke: two `config set` under lock don't corrupt the file.

### Gate
`task rust:verify` in the dev loop; `task test:parallel` for acceptance; `task verify`
before commit; `task claude:tests` + `task catalog:verify` for the docs/skill drift.

## Implementation plan (high-level; /swarm-plan expands)

1. [ ] `ConfigArgs` + nested `ConfigCommand` / `RegistryCommand` (clap derive) in
   `src/command/config.rs`; wire `command.rs`, `main.rs`, `app.rs`.
2. [ ] Dotted-key ↔ model mapping + value parsers (clients list, tui enum/bool/separators,
   registry fields). Unknown-key / bad-value error mapping.
3. [ ] Read path: `get` / `list` / `registry list|show` from the resolved scope.
4. [ ] Write path: load → lock → mutate model → `validate_registries` → `write_config`.
   `set` / `unset` / `registry add|rm|use`.
5. [ ] `src/api/config_report.rs` report types + `Printable` impls; render arms in `app.rs`.
6. [ ] Unit tests (step-by-step with each stage); acceptance suites.
7. [ ] Docs: `docs/src/commands.md` (new `config` section), `docs/src/configuration.md`
   (point hand-edit guidance at `grim config`); CHANGELOG.
8. [ ] Catalog drift review: `grim-usage` / `grim-authoring` skills (`catalog/README.md`);
   `task catalog:verify`.
9. [ ] `task verify`.

## Validation

- [ ] Every mutation re-runs `validate_registries`; no path writes an invalid set.
- [ ] At-most-one-default holds after `use` / `set default true` (unit + acceptance).
- [ ] `--global` vs project scope writes the correct file; scopes never merge.
- [ ] JSON output is machine-parseable; exit codes match the map (migration-script
  contract).
- [ ] `task verify` + `task catalog:verify` green.

## Links

- [adr_multi_registry_mcp.md](./adr_multi_registry_mcp.md) — `[[registries]]` + `resolve_registries`
- [adr_registry_default_dedup.md](./adr_registry_default_dedup.md) — at-most-one-default; `write_config` preservation
- `src/config/declaration.rs`, `src/config/project_config.rs`, `src/config/scope.rs`
- `src/command/add.rs` (`write_config`), `src/command/scope_resolution.rs`, `src/command/init.rs`
- `src/api/login_report.rs`, `src/cli/printer.rs`, `src/cli/exit_code.rs`, `src/app.rs`
- `docs/src/configuration.md`, `docs/src/commands.md`, `catalog/README.md`

---

## Changelog

| Date | Author | Change |
|------|--------|--------|
| 2026-06-30 | Architect (/architect) | Initial draft; Option B chosen (nested registry verbs, explicit get/set, settings+registries scope) — all three confirmed with maintainer |
