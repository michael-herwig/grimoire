---
paths:
  - src/**/*.rs
---

# Grimoire Architecture Principles

Auto-loads on every Rust file edit. Provides stable architectural context —
the "why" behind design. For dynamic discovery of current code state,
launch `worker-architecture-explorer`.

> **Status: provisional.** Grimoire is early scaffolding — a single binary
> crate at `src/` with `src/main.rs` as the only source file today. The
> guidance below is the intended shape, not a description of code that
> exists. Update this file as real structure lands; do not treat the
> module/type names below as already implemented.

## Crate Layout

Grimoire is a **single binary crate**:

- Crate / package name: `grimoire`
- Binary name: `grim`
- All source lives under `src/`. No workspace, no separate library crate,
  no lib/CLI split. Acceptance tests live under `test/`.

## Design Principles

These patterns are the intended backbone. Apply them as the codebase grows.

| Principle | Intent |
|-----------|--------|
| **Facade** | A single coordination point hides subsystem complexity from the CLI layer |
| **Strategy / trait dispatch** | Swappable implementations (e.g. local vs remote registry access) for testability |
| **Command pattern** | Uniform CLI flow: args → typed identifiers → operation → report data → output |
| **Three-layer errors** | Top-level error wraps domain errors wraps kinds, so batch operations can diagnose per item |
| **Option-based lookups** | "Not found" is `Option::None`, not an error, at the lookup layer |
| **Extension traits in a prelude** | Ergonomic helpers without polluting core types |
| **Builder pattern** | Fluent construction where there are many optional parameters |
| **Lazily-initialized context** | One init per invocation; avoid unused work |

## Intended Command Flow

```
CLI command (clap parse)
  → Context init (config, registry client, local store)
  → command/{name}.rs — transform args into typed identifiers
    → coordinate the operation (resolve, fetch, install, ...)
  → build report data from results
  → render to stdout (plain / JSON)
```

## ADR Index

Architecture decisions are recorded as `.claude/artifacts/adr_*.md`. Read
the relevant ADRs before making decisions in the same domain.

| ADR | Decision |
|-----|----------|
| [adr_oci_artifact_type.md](../artifacts/adr_oci_artifact_type.md) | Type artifacts with OCI `artifactType` + a Grimoire config media type per kind; retire the `com.grimoire.kind` annotation |
| [adr_multifile_rules.md](../artifacts/adr_multifile_rules.md) | A rule may carry an optional sibling support directory (`<name>/`) packed into the same single tar layer and installed beside the index `<name>.md`; wire contract unchanged, single-file rules unaffected, install record gains an optional `support_dir` |
| [adr_catalog_summary_annotation.md](../artifacts/adr_catalog_summary_annotation.md) | Add an optional `com.grimoire.summary` annotation, authored in-file for every kind (skill `metadata`, rule frontmatter, bundle `.toml`); keywords are string-only everywhere; `grim search` shows summary-or-description truncated to a terminal-width-clamped window (full when piped), keeps the full description in JSON, and search matches the summary too |

## Code Style Conventions

Project-wide conventions enforced by review:

| Convention | Rule | Deviation = Bug |
|------------|------|-----------------|
| **Type names** | Full descriptive names (`OperatingSystem`, `Architecture`), not abbreviations (`Os`, `Arch`) | Abbreviated type names |
| **Module structure** | One concept per file; named module files, no `mod.rs` | Monolithic files, `mod.rs` files |
| **Internal enum exhaustiveness** | Omit `#[non_exhaustive]` on internal non-error enums so matches stay total. The binary is the only consumer — no stable lib API. Error enums are exempt | `#[non_exhaustive]` on a closed internal enum |
| **Domain types over `String`** | Fields representing a domain concept (registry reference, digest, version, platform) use a dedicated type with `Serialize`/`Deserialize` round-tripping through canonical string form, not raw `String` | Stringly-typed domain field |

## Where Features Land

| Feature type | Location | Notes |
|--------------|----------|-------|
| New CLI command | `src/command/` | One file per command, follow the command pattern |
| New output format | `src/api/` | Implement the shared output trait |
| New acceptance test | `test/tests/test_*.py` | Use fixtures, maintain test isolation |

## Utility Discipline

**Before writing a small helper inside a module, check whether `std`,
`tokio`, or an existing crate-level utility already covers it.** A helper
reinvented in one module is wasted effort and a drift risk. If a new helper
is broadly applicable, place it in a shared `utility`/prelude module in the
same change rather than locally. Check `std` first, then existing utilities,
then invent.
