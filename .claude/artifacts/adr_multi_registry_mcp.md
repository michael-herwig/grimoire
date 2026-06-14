# ADR: Multiple registries, shared catalog core, and a local MCP server

## Metadata

**Status:** Accepted
**Date:** 2026-06-15
**Deciders:** Michael Herwig (maintainer)
**Tech Strategy Alignment:**
- [x] Decision follows Golden Path in `.claude/rules/product-tech-strategy.md`
      (Rust 2024 + Tokio; the one new dependency — `rmcp` — is the official
      MCP SDK and is Tokio/serde/schemars-native, sitting directly on the
      existing async golden path)
**Domain Tags:** api, integration, infrastructure, tui
**Supersedes:** N/A

## Context

`grim` browses exactly one registry per invocation. `grim search` and the
TUI each resolve a single registry through the precedence chain
(`resolve_default_registry`), call `Catalog::load_or_refresh` against a
single `$GRIM_HOME/catalog.json`, load lock/state for badges, and project
rows — with the registry-resolution body copy-pasted between
`src/command/search.rs` and `src/command/tui.rs`. There is no programmatic
(non-CLI) interface for an AI agent to query or mutate Grimoire, no way to
configure more than one registry, and the TUI is a flat list.

Three capabilities are wanted together, and they share one substrate:

1. A **local STDIO MCP server** (`grim mcp`) so an AI agent can search,
   inspect, and (opt-in) mutate Grimoire directly.
2. **Multiple registries** configured globally and per-project, that work
   across every interface.
3. A **TUI tree** with registries as collapsible root nodes.

The hard constraint: the catalog/search/registry logic must be implemented
**once** and shared across `search` / `tui` / `mcp`, and the on-disk cache
must be **safe to refresh from concurrent processes** — a long-lived MCP
server will run alongside ad-hoc CLI and TUI invocations against the same
`$GRIM_HOME`.

This ADR records the one-way-door decisions (on-disk cache layout, config
schema, reference syntax, SDK adoption). The full design spec is
`~/.claude/plans/i-want-to-add-drifting-hammock.md`.

## Decision Drivers

- **DRY across interfaces** — one catalog seam, not three copies.
- **Backward compatibility** — existing single-`default_registry` configs
  and the disposable `catalog.json` cache must not break.
- **Concurrency safety** — no corruption and no thundering-herd network
  refreshes when N processes share `$GRIM_HOME`.
- **Security** — an MCP server runs with the user's privileges; mutation is
  real filesystem change.
- **Boring technology** — spend at most one innovation token (the MCP SDK);
  reuse existing primitives (atomic writes, advisory flock, JoinSet,
  ratatui) everywhere else.

## Decision

### 1. Shared catalog service (the "once")

A new `src/catalog/catalog_service.rs` exposes a single `load_catalog(...)`
seam returning `CatalogResults { groups: Vec<CatalogGroup> }`, each group
registry-tagged with already-filtered, already-badged rows. It fans out one
coordinated per-registry refresh on a `tokio::task::JoinSet`, re-sorts by
input order (determinism rule), filters via `SearchQuery` once, and applies
`derive_badge` once. `search` and the MCP read tools consume it. The TUI keeps
its richer per-row state (`IntegrityMissing`, worst-of bundle) layered locally
— no other front-end needs it (YAGNI), so it is not hoisted into the shared
layer.

**Shipped subset vs. deferred (Workstream E):** The `load_catalog` seam is
consumed by `grim search` and the MCP `grim_search` tool only. The TUI still
browses a single registry via `Catalog::load_or_refresh_coordinated` directly
(`src/tui/app.rs`) and does not consume `load_catalog` or `[[registries]]`.
TUI multi-registry browse and the `VisibleRow` collapsible registry-tree are
deferred to a follow-up workstream.

### 2. Multiple registries — additive `[[registries]]`

Config gains an optional `[[registries]]` array of `RegistryConfig { alias:
Option<String>, url: String, default: bool }` in both project and global
`grimoire.toml`, alongside the existing single `default_registry`. With
`#[serde(default)]`, every current config parses to an empty vec — fully
backward compatible. A new `resolve_registries(...)` list resolver orders
them (`--registry`/env forced front → project `[[registries]]` → global
`[[registries]]` → legacy `default_registry` folded in **only if no
`[[registries]]` exist** → built-in fallback only when otherwise empty),
deduped by url. The single-default precedence chain
(`resolve_default_registry`) is kept verbatim for commands that need one
registry. `ResolvedScope` carries the registry list the same way it carries
`options`.

### 3. Qualified references use the `alias/repo` form (never `alias:repo`)

A new `resolve_reference(input, registries, default_registry) -> Identifier`
seam sits above `Identifier`. If `input` has an explicit registry → parse as
today. Else, if the first `/`-segment matches a configured **alias** →
substitute that alias's url as the registry. Else → the existing
`parse_with_default_registry` path. The `/` form is collision-safe:
`Identifier::parse` already **rejects** bare `alias/repo` with
`MissingRegistry`, so alias substitution only rescues inputs that fail
today. The colon form `alias:repo` is **rejected** because it collides with
the existing `repo:tag` syntax. Short ids still resolve to the default
registry (deterministic); there is no cross-registry fallback for ambiguous
short ids.

### 4. Per-registry cache layout + advisory refresh coordination

`$GRIM_HOME/catalog.json` is replaced by `$GRIM_HOME/catalog/<hash>.json`,
one file per registry (`<hash>` = first 16 hex of SHA-256 of the registry
url). The on-disk `CatalogFile` format is **unchanged** (still
`CatalogVersion::V1`, still self-keyed by `registry`, so the in-file guard
defends against hash collisions). No migration: the catalog is a disposable
TTL cache — `ensure_layout` best-effort reaps a stale legacy `catalog.json`
and the cold-miss path rebuilds (online) or serves empty (offline).

Concurrency is handled by generalizing the proven `ConfigFileLock` sidecar
flock (`src/lock/file_lock.rs`) into a reusable `AdvisoryFileLock` and
adding `Catalog::load_or_refresh_coordinated`. The refresh is double-checked
and anti-thundering-herd: a fresh cache serves with **no lock taken**; a
stale cache triggers a **non-blocking** `try_acquire` — the winner
re-checks then rebuilds, while a contender **serves stale** rather than
waiting. The advisory lock is an OS fd (not a `MutexGuard`), so holding it
across the async rebuild does not violate the across-`.await` rule. A
long-lived MCP process additionally invalidates its in-memory copy by
catalog-file `mtime`.

### 5. `grim mcp` uses the official `rmcp` SDK; read-only by default

`grim mcp` runs a local STDIO MCP server via `rmcp` 1.7 (official SDK;
Tokio/serde/schemars-native; the schemars major is already locked). It is
`Printable`-exempt (like `tui`/`schema`), returns `ExitCode` directly, and
shuts down cleanly on stdin EOF. The accepted flags are `--allow-writes`,
`--global`, and `--config <path>`. The install scope is fixed at server start
(no per-call scope redirection → no project→global escalation). stdout is
the JSON-RPC channel — diagnostics go to stderr via `tracing`.

**Shipped v1 subset:** Two read tools are always available: `grim_search`
and `grim_status`. Both map to existing domain seams and reuse the
`api/*_report.rs` serializers so MCP JSON equals `grim … --format json`.

**Deferred:** `grim_detail` and `grim_list_installed` are not implemented
in this branch. Write tools (`grim_add`, `grim_install`, `grim_update`,
`grim_uninstall`) are **not yet registered** — only the `--allow-writes`
flag and scope plumbing exist. They will be gated behind `--allow-writes`
and added in a later change; all writes will route through existing seams,
inheriting the path-anchor containment guard.

**Tool input boundary (registry allowlist).** `grim_search` exposes **no**
registry override: it always browses the server's configured registry set
(`[[registries]]` + fallback). The CLI's `--registry` (free choice of host)
is intentionally *not* surfaced to the agent — honoring an arbitrary
agent-supplied registry would let a prompt-injected agent point grim at an
unconfigured host (SSRF, CWE-918). The configured set is the security
boundary; a narrower or unconfigured registry is reachable only by editing
config, not by a tool call.

**Known limitation (deferred):** tool errors currently return the full
`anyhow` chain (`{err:#}`, which can include filesystem paths) to the MCP
client (CWE-209). Accepted for v1 — the server is local and runs as the same
user as the client, so the chain crosses no privilege boundary and aids
debugging. Revisit (trim to a top-level message) before write tools land,
when error surfaces widen.

## Consequences

**Positive**
- One catalog seam; the duplicated `resolve_registry` is deleted.
- Multi-registry browse/search works across `grim search` and the MCP `grim_search` tool (TUI multi-registry deferred to a follow-up).
- Concurrent processes coordinate cleanly: no corruption, no redundant
  registry walks, readers never block.
- An AI agent gets a safe, read-by-default programmatic interface; mutation
  is an explicit opt-in with a fixed scope and the existing containment
  guards.

**Negative / risks**
- New on-disk cache location. Mitigated: cache not state; legacy file
  reaped; cold-miss rebuilds. No migration, no version bump.
- One new dependency tree (`rmcp`). Accepted: it is the official SDK and the
  protocol surface is exactly the commodified work a library should own.
- Advisory locks on network/shared `GRIM_HOME` volumes are unreliable;
  atomic-write still prevents corruption, only herd-suppression weakens.
  Documented (same caveat class as shared-`GRIM_HOME` install state).
- Large surface delivered on one branch; decomposed into independently
  reviewable workstreams (config → cache → service → mcp/tui).

## Alternatives Considered

- **Hand-rolled JSON-RPC over stdio** instead of `rmcp` — owns the MCP
  handshake/framing/schema-gen, hundreds of lines tracking an evolving
  spec for zero user-visible benefit. Rejected: the boring choice is the
  maintained official SDK, not a bespoke protocol implementation.
- **`alias:repo` qualified syntax** — collides with `repo:tag`. Rejected in
  favor of the collision-safe `alias/repo` form.
- **Cross-registry fallback for short ids** (try A, then B) — non-
  deterministic, surprising auth prompts. Rejected: short ids resolve to the
  default registry; ambiguity is opt-out via a qualified `alias/repo` ref.
- **Promote the TUI's `ArtifactState` (5-state) into the shared service** —
  pushes a TUI-only concept into the core for one consumer. Rejected (YAGNI);
  the service emits `StatusBadge` and the TUI layers its richer state.
- **Single merged catalog file for all registries** — loses per-registry
  freshness/locking granularity and the natural tree grouping. Rejected for
  per-registry files.
- **MCP writes enabled by default** — largest attack surface for an
  autonomous agent. Rejected for read-default + `--allow-writes` gate.

## Links

- Design spec: `~/.claude/plans/i-want-to-add-drifting-hammock.md`
- [`adr_install_state_portability.md`](./adr_install_state_portability.md) —
  anchor-relative paths + the shared-`GRIM_HOME` concurrency stance reused here
- [`adr_repository_annotation.md`](./adr_repository_annotation.md) —
  `CatalogEntry` metadata surfaced by the shared catalog service

<!-- external -->
[rmcp]: https://github.com/modelcontextprotocol/rust-sdk
