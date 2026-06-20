# ADR: Registry default-source deduplication — `[[registries]]` as the one source of truth

<!--
Architecture Decision Record (MADR-flavored).
Owner: Architect (/architect). Handoff to: Builder (/builder), QA (/qa-engineer).
-->

## Metadata

**Status:** Proposed
**Date:** 2026-06-20
**Deciders:** Maintainer (Michael Herwig); Architect worker (draft)
**Beads Issue:** N/A
**Related PRD:** N/A
**Tech Strategy Alignment:**
- [x] Decision follows Golden Path in `.claude/rules/product-tech-strategy.md` (Rust 2024, no new deps)
- [x] No deviation
**Domain Tags:** api, data, config
**Supersedes:** N/A (refines `adr_multi_registry_mcp.md`)
**Superseded By:** N/A

## Context

`grimoire.toml` (project and global scope) has **two** ways to declare a
default registry for short-identifier expansion:

1. **Legacy single string** — `[options].default_registry = "url"`
   (`ConfigOptions::default_registry: Option<String>`,
   `src/config/declaration.rs:82-87`).
2. **Registry array** — `[[registries]]` of `RegistryConfig { alias, url,
   default }` (`src/config/declaration.rs:99-122`), added by
   `adr_multi_registry_mcp.md` to support multi-registry browse + qualified
   `alias/repo` references.

These overlap: the array's `default = true` entry **is** the default
registry. The current resolver
(`resolve_registries`, `src/config/registry_resolve.rs:52-97`) treats the
array as authoritative when present and **silently ignores**
`[options].default_registry` (the doc comment at lines 49-51 and the inline
fold at 86-91 fold the legacy field only when `[[registries]]` is empty).
Verified precedence, top to bottom:

1. forced single (`--registry` flag / `$GRIM_DEFAULT_REGISTRY`) →
   collapses to exactly one (`registry_resolve.rs:60-67`);
2. `[[registries]]` (project then global, deduped by url) — authoritative,
   `normalize_primary` picks the first `default = true` else first entry
   (`registry_resolve.rs:69-84`, `99-107`);
3. legacy `default_registry` chain (project > global > fallback
   `grim.ocx.sh`) only when no `[[registries]]` exist anywhere
   (`registry_resolve.rs:86-96`).

The footgun: a config carrying **both** fields silently honors the array
and discards `default_registry` with no warning. Worse, the two **writers**
(`grim init` at `src/command/init.rs:78-86` and the TUI init-dialog, which
delegates to `init::run` via `prompt_init`,
`src/command/tui.rs:203-214`) only ever emit `[options].default_registry`
— never `[[registries]]`. So fresh configs are written in the legacy shape
that the multi-registry resolver de-emphasizes.

**Out of scope (confirmed):** GitLab `repository_prefix` and per-entry
`repository` live in `publish.toml` (`src/command/publish.rs`), are
publish-only, and have no field in `grimoire.toml`. The runtime overrides
`--registry` and `$GRIM_DEFAULT_REGISTRY` are not config redundancy; they
stay exactly as-is.

## Decision Drivers

- One source of truth for "the default registry" — eliminate the silent
  ignore footgun.
- Back-compat: existing configs with `[options].default_registry` must keep
  working without user action.
- Minimal blast radius: `resolve_registries` already resolves correctly; the
  real gap is the **writers** plus a missing validation.
- KISS / YAGNI: do not add deprecation-warning infrastructure or a migration
  command unless the maintainer asks for it.
- Avoid a downgrade trap (a user who upgrades, writes the new shape, then
  runs an older `grim`).

## Considered Options

### Option A: `[[registries]]` + `default = true` is the one source of truth (CHOSEN)

**Description:** Migrate both writers (`grim init`, TUI init-dialog) to emit
a single `[[registries]]` entry with `default = true` instead of
`[options].default_registry`. Keep **reading** `[options].default_registry`
indefinitely for back-compat (the existing fold in `resolve_registries`
stays). Add parse-time validation: at most one `default = true` across the
array, and `default = true` requires a non-empty `url` (already guaranteed
by the empty-url check). Never auto-rewrite a user's file; the legacy field
is only superseded the next time a writer creates the array.

| Pros | Cons |
|------|------|
| Single canonical shape going forward; writers and resolver agree | Two readable shapes coexist for a long time (legacy + array) |
| Resolver unchanged — lowest-risk core | A config written by new `grim` is silently ignored by a *pre-array* `grim` (downgrade), though that binary predates the feature anyway |
| Back-compat read path already exists and is tested | `grim init` output shape changes (acceptance tests must update) |
| At-most-one-default validation closes a real ambiguity | — |

### Option B: Pointer model — `default_registry` names an alias in `[[registries]]`

**Description:** Redefine `[options].default_registry` to hold an **alias**
that must resolve to a `[[registries]]` entry; drop the per-entry `default`
flag. The single string becomes a pointer into the array.

| Pros | Cons |
|------|------|
| One place expresses "which is primary" (the pointer) | Breaking re-interpretation: today the field is a *url*, not an alias |
| No boolean-flag duplication | Requires every legacy url-valued config to be rewritten or specially handled |
| | Two coupled fields must stay consistent (pointer + array) — new validation surface |
| | Larger resolver change; higher risk against a working seam |

### Option C: Keep both fields, only fix the footgun (warn on conflict)

**Description:** Leave writers and schema as-is. Add a warning (or hard
error) when both `[options].default_registry` and `[[registries]]` are
present so the silent ignore becomes visible.

| Pros | Cons |
|------|------|
| Smallest code change | Does not deduplicate — both shapes remain first-class |
| No writer churn, no test churn | Requires deprecation-warning infrastructure that does not exist today |
| | Fresh configs still written in legacy shape — the divergence persists |

## Decision Outcome

**Chosen Option:** Option A.

**Rationale:** The resolver is already correct and well-tested; the actual
defect is that writers emit the de-emphasized shape and the parser tolerates
an ambiguous `default = true` count. Option A fixes exactly those two seams
with no change to the load-bearing `resolve_registries` function, preserves
the existing back-compat read path verbatim, and avoids re-defining a field's
meaning (Option B) or perpetuating the divergence (Option C). It spends zero
innovation tokens (no new infra) and is reversible per-writer.

### Consequences

**Positive:**
- New configs use one canonical shape; what the writer emits is what the
  resolver treats as authoritative.
- The "both fields present, legacy silently ignored" footgun is no longer
  *produced* by grim; for hand-authored configs it is now detectable via the
  optional warning (see Deprecation UX).
- At-most-one-default validation removes a silent normalization (today two
  `default = true` entries parse and the first silently wins).

**Negative:**
- `grim init` and TUI-init output shape changes — three acceptance tests in
  `test_init.py` assert the legacy string and must be updated.
- Two readable shapes coexist indefinitely (acceptable: the legacy field is
  cheap to keep reading).

**Risks:** see Risks section in the structured output / table below.

## Technical Details

### Schema changes

`src/config/declaration.rs`:
- `RegistryConfig` unchanged structurally. Tighten the `default` doc comment
  to read "exactly one entry MAY set it; setting it on two entries is a parse
  error" (today it says "at most one … when none do, the first entry is
  primary" — keep the none-set behavior, but the multi-set case moves from
  tolerated to rejected).
- `ConfigOptions::default_registry` stays `Option<String>` and stays
  readable. Add a doc-comment note: "Deprecated for new writes — grim now
  emits `[[registries]]` with `default = true`. Still read for back-compat;
  ignored for browse when `[[registries]]` is present." Do **not** add
  `#[deprecated]` (it is a serde field, not a callable; the attribute adds no
  value and risks lint noise).
- No `serde` rename/alias. No version bump (on-disk shape is additive-read).

### Resolve-precedence changes

`src/config/registry_resolve.rs`: **none** to `resolve_registries`,
`normalize_primary`, `primary_registry`, `resolve_reference`. The three-tier
precedence (forced → array → legacy chain) is unchanged. `command.rs`
seams (`resolve_default_registry`, `registries_for_scope`,
`global_config_default`, `global_config_registries`) are unchanged.

The one decision to record: **`normalize_primary` stays** as the
resolution-time defensive net (it tolerates a multi-default in-memory set and
keeps the first). Parse-time validation is the user-facing gate; normalize is
belt-and-suspenders for programmatically constructed sets.

### Writer migrations

1. `src/command/init.rs` `render_config()` (lines 78-86): when a registry is
   present, emit
   ```toml
   [[registries]]
   url = "<reg>"
   default = true
   ```
   instead of `[options].default_registry`. When no registry, emit no
   `[[registries]]` and no `[options]` (preserve
   `test_init_without_any_registry_omits_options`). `snapshot_registry`
   (lines 71-73) is unchanged — it still resolves explicit > flag > env. The
   `InitArgs.registry` doc/help string ("Seed `[options].default_registry`")
   updates to "Seed the default registry as a `[[registries]]` entry".
2. `src/command/tui.rs` `prompt_init` (lines 203-214): **no change** — it
   passes the registry string to `init::run`, which now emits the array. The
   doc comment block (lines 167-177) that says "snapshots it into
   `[options].default_registry`" updates to name `[[registries]]`.
3. `src/tui/init_dialog.rs`: **no change** to the state machine — it still
   collects one registry string. Update the `InitDialogOutcome::Confirmed`
   doc comment (lines 64-69) wording from "seeding `[options].default_registry`"
   to "seeding the default `[[registries]]` entry".
4. `src/command/add.rs` `write_config` (lines 281-370): **no migration of the
   legacy field.** Keep emitting `[options].default_registry` verbatim when
   `options.default_registry.is_some()` (line 294-295) so a re-serialized
   legacy config does not lose data on `add`/`remove`/TUI-edit. The
   `[[registries]]` preservation loop (lines 334-344) already round-trips the
   array. This is the deliberate "stop creating new legacy fields, never
   destroy existing ones" stance — no auto-migration on write (see Open
   Questions for the alternative).

### Validation rules (parse-time, `validate_registries`, `project_config.rs:185-242`)

Add after the per-entry loop:
- **At most one default:** count entries with `default == true`; if `> 1`,
  return `ConfigErrorKind::RegistryInvalid { reason: "at most one
  [[registries]] entry may set default = true" }`. Applies to both scopes
  (shared parser via `from_toml_str` → `GlobalConfig` reuses it, confirmed
  `global_config.rs:31-38, 67-71`).
- **Default implies usable url:** already covered — the empty-url check runs
  first for every entry, so a `default = true` entry necessarily has a
  non-empty url.

Existing checks (empty url, empty/whitespace/`/`/control alias, duplicate
alias) are unchanged.

### Data model (on-disk shapes, all still readable)

```
# Canonical (what grim now writes):
[[registries]]
url = "ghcr.io/acme"
default = true

# Legacy (still read; never written fresh; preserved on re-serialize):
[options]
default_registry = "ghcr.io/acme"

# Mixed (hand-authored; array wins, legacy ignored for browse; optional warn):
[options]
default_registry = "ghcr.io/acme"   # ignored
[[registries]]
url = "registry.corp/team"
default = true
```

## Back-compat & migration strategy

- **Read path:** unchanged. `[options].default_registry` is folded by
  `resolve_registries` exactly as today (legacy chain when no array).
- **Write path:** writers stop *creating* the legacy field; `write_config`
  *preserves* an existing one (no destructive auto-migration). This avoids the
  downgrade trap and surprising diffs.
- **No on-disk version bump**, no `serde` alias, no migration command in v1.
- **Mixed configs:** continue to resolve deterministically (array wins). The
  only behavior change a user can hit is the new at-most-one-default
  rejection, which only fires on a genuinely ambiguous hand-authored array.

## Deprecation UX

Recommended v1 stance (pending maintainer confirmation — see Open Questions):
- **Silent read** of `[options].default_registry` (no warning) to avoid
  warning-fatigue in test suites / scripts and because grim has **no
  deprecation-warning infrastructure today** (would be net-new surface).
- **Documentation-led deprecation:** update `docs/src/configuration.md`
  (the `default_registry` rows at lines 15, 29, 95, 116-119, 281) to mark
  `[options].default_registry` as legacy and point to `[[registries]] +
  default = true`. Add a CHANGELOG note.
- If the maintainer wants a signal: emit a **once-per-process** stderr line
  (gated so `--format json` stdout is never polluted) when both fields are
  present in the same file — the "silently ignored" case is the only one
  worth flagging. This is a small follow-up, not required for the core change.

## Test plan

### Unit (Rust, inline `#[cfg(test)]`)

- `registry_resolve.rs`: keep all existing tests (they prove the resolver is
  unchanged). Add `both_fields_present_array_wins_legacy_ignored` documenting
  the intended precedence with both a legacy default and a non-empty array.
- `project_config.rs`:
  - `registries_two_defaults_rejected` — two `default = true` → `RegistryInvalid`.
  - `registries_single_default_accepted` — one `default = true` parses.
  - `registries_no_default_accepted` — zero `default` parses (resolver promotes first).
- `global_config.rs`: `global_registries_two_defaults_rejected` (parser is
  shared, but lock the contract for the global scope explicitly).
- `init.rs`:
  - `render_includes_registries_array_when_present` — body contains
    `[[registries]]`, `url = "..."`, `default = true`; no `default_registry =`.
  - `render_omits_registries_without_registry` — no `[[registries]]`, no
    `[options]`, parses back as empty (keep existing assertion shape).
  - `render_output_parses_and_resolves_primary` — parse the rendered body,
    run `resolve_registries`, assert `primary_registry` equals the seeded url.
- `add.rs`: add `write_config_preserves_legacy_default_registry` — a config
  with `default_registry` set and no array re-serializes with the legacy field
  intact (guards the no-destructive-migration stance). Existing
  `write_config_preserves_registries_array` stays.

### Acceptance (pytest, `test/tests/`)

- `test_init.py`: **update** `test_init_with_registry_seeds_options`,
  `test_init_snapshots_env_default_registry`,
  `test_init_explicit_registry_beats_env` to assert the `[[registries]]` /
  `default = true` shape instead of `default_registry = "..."`. Keep
  `test_init_without_any_registry_omits_options` (still valid — nothing
  emitted). Add `test_init_registry_resolves_for_add` — `grim init --registry
  X` then a short-id `add` resolves against `X` (end-to-end primary works).
- `test_default_registry.py`: add `test_legacy_default_registry_still_resolves`
  — a hand-written `[options].default_registry` config resolves short ids
  (back-compat lock). Add `test_both_fields_array_wins` — config with both,
  array's url is the one used.
- `test_registries.py`: add `test_two_defaults_rejected` — config with two
  `default = true` exits with config error (65/78), error text mentions
  "default".

### Gate

`task rust:verify` during the loop; `task test:parallel` for acceptance;
`task verify` before commit. `task claude:tests` only if `.claude/` changes.

## Implementation plan

1. [ ] `validate_registries`: add at-most-one-default check (+ unit tests, both scopes).
2. [ ] `init.rs` `render_config`: emit `[[registries]]` + `default = true`; update help/doc strings (+ unit tests).
3. [ ] TUI doc-comment wording updates (`tui.rs`, `init_dialog.rs`) — no behavior change.
4. [ ] `add.rs`: add the legacy-preservation regression test (no code change to write_config).
5. [ ] Update `test_init.py` acceptance tests to the new shape; add new acceptance tests.
6. [ ] Update `docs/src/configuration.md` + CHANGELOG to mark legacy field deprecated.
7. [ ] (Optional, if maintainer approves) once-per-process stderr warning when both fields coexist.
8. [ ] `task verify`.

## Validation

- [ ] `resolve_registries` test suite unchanged and green (proves core untouched).
- [ ] New parse-time rejection covered both scopes.
- [ ] Writer output parses back and resolves to the seeded primary.
- [ ] Legacy read path still resolves (acceptance).

## Links

- [adr_multi_registry_mcp.md](./adr_multi_registry_mcp.md) — established `[[registries]]` + `resolve_registries`
- `src/config/registry_resolve.rs`, `src/config/declaration.rs`, `src/config/project_config.rs`
- `src/command/init.rs`, `src/command/tui.rs`, `src/tui/init_dialog.rs`, `src/command/add.rs`

---

## Changelog

| Date | Author | Change |
|------|--------|--------|
| 2026-06-20 | Architect worker | Initial draft (Option A chosen) |

---

## Completeness Review

*Added by reviewer worker, 2026-06-20. Scope: adversarial check of all cited symbols, writer coverage, migration strategy, resolution-consumer coverage, and validation correctness against HEAD source.*

### Verified claims — evidence-backed

**Resolver (`src/config/registry_resolve.rs`):**
- `resolve_registries` at lines 52-97: verified — exact function signature, three-tier precedence (forced→array→legacy), line numbers confirmed.
- `normalize_primary` at lines 99-107: verified — enforces exactly one `is_default` by position (first `is_default=true` wins, else promotes index 0).
- `primary_registry` at lines 109-118: verified — returns `is_default` entry's url or first entry.
- `resolve_reference` at lines 120-143: verified — `alias/repo` substitution via `split_once('/')`.

**Schema (`src/config/declaration.rs`):**
- `ConfigOptions::default_registry: Option<String>` at lines 86-87: verified.
- `RegistryConfig` struct at lines 106-122: verified — `alias: Option<String>`, `url: String`, `default: bool` with `skip_serializing_if = "std::ops::Not::not"` (omits `default = false` on write).
- `#[serde(deny_unknown_fields)]` on both: verified.

**`validate_registries` (`src/config/project_config.rs` lines 185-242):**
- Verified checks: empty url, empty alias, leading/trailing whitespace alias, alias containing `/`, alias with control chars, duplicate alias.
- **CONFIRMED GAP**: The comment at lines 181-184 explicitly states "multiple `default = true` entries are tolerated here (the first wins); only structurally ambiguous input…is rejected." At-most-one-default validation does NOT exist yet. The ADR's proposed addition is correctly stated.

**Writers (`src/command/init.rs` lines 78-86):**
- `render_config` at lines 78-86: verified — emits `[options]\ndefault_registry = "{reg}"\n` only. Does NOT emit `[[registries]]`. Confirmed.
- `snapshot_registry` at lines 71-73: verified — `explicit.or_else(|| ctx.registry_flag()).or_else(|| ctx.registry_env())`.
- `InitArgs.registry` help string at line 32-34: current text is "Seed `[options].default_registry` with this value." — ADR correctly notes this needs updating.

**`write_config` (`src/command/add.rs` lines 281-370):**
- Verified: lines 290-296 emit `[options].default_registry` when `options.default_registry.is_some()`. Lines 334-344 re-serialize `[[registries]]` verbatim. No migration on write. Confirmed as the ADR states.

**TUI path (`src/command/tui.rs` lines 183-217 → `src/tui/init_dialog.rs`):**
- `prompt_init` at lines 183-217: verified — calls `crate::tui::init_dialog::run`, gets `InitDialogOutcome::Confirmed { registry }`, passes to `crate::command::init::run`. Delegates to the same `render_config`.
- `InitDialogOutcome::Confirmed` doc comment at lines 64-69 in `init_dialog.rs`: verified — says "seeding `[options].default_registry`". Needs wording update per ADR (no behavior change).
- **UI string at `init_dialog.rs` line 210**: `"seeded as [options].default_registry in {}"` — this is a rendered TUI string visible to users, not just a doc comment. The ADR's Technical Details section mentions updating doc-comment wording but does NOT mention this user-visible rendered string. This is a **gap**.

**`GlobalConfig` (`src/config/global_config.rs`):**
- `GlobalConfig::from_toml_str` at lines 31-38 delegates to `ProjectConfig::from_toml_str`. Verified — shared parser path, validation is identical for both scopes. ADR claim confirmed.
- `GlobalConfig::load` at lines 50-77 calls `ProjectConfig::from_toml_str` at line 67. Confirmed.

**`command.rs` seams:**
- `resolve_default_registry` at lines 70-81: verified — returns `String`, uses `ctx.registry_flag().or_else(||ctx.registry_env()).or(project_default).or(global_default).unwrap_or(FALLBACK_REGISTRY)`. This does NOT call `resolve_registries` — it is the simpler single-default chain used by `login`/`logout` and as the fallback seam for `tui.rs:resolve_registry`.
- `global_config_default` at lines 91-101: verified — reads `cfg.options.default_registry`. This is the per-seam that currently reads ONLY `[options].default_registry` from global config. When the global config is migrated to `[[registries]]`, this function will silently return `None` and callers fall through to `FALLBACK_REGISTRY` — unless global config still has `default_registry`. **This is a back-compat hole if users set the global config's default registry via the TUI init-dialog before this change.** The ADR does not cover this case explicitly.
- `registries_for_scope` at lines 130-144: verified — assembles full browse set including both tiers.
- `global_config_registries` at lines 111-121: verified — reads `cfg.registries`.

**`FALLBACK_REGISTRY` constant:** at line 56, value `"grim.ocx.sh"`. Verified.

**Acceptance tests (`test/tests/test_init.py`):**
- `test_init_with_registry_seeds_options` at line 22: asserts `'default_registry = "ghcr.io/acme"' in body`. Will break when `render_config` migrates. Confirmed.
- `test_init_snapshots_env_default_registry` at line 30: asserts `'default_registry = "snap.example"' in body`. Will break. Confirmed.
- `test_init_explicit_registry_beats_env` at line 40: asserts `'default_registry = "flag.example"' in body`. Will break. Confirmed.
- `test_init_without_any_registry_omits_options` at line 50: asserts `"[options]" not in body` and `"default_registry" not in body`. Remains valid after migration (when no registry given, nothing is emitted). Confirmed safe.

---

### Gaps found

**G1 — User-visible TUI string not in scope (actionable).**
`src/tui/init_dialog.rs` line 210 renders `"seeded as [options].default_registry in {}"` as a user-facing popup string (not just a doc comment). The ADR covers updating doc-comment wording in `InitDialogOutcome::Confirmed` (lines 64-69) but omits this rendered string. After migration, the TUI popup will show incorrect wording to users setting up a new config. Must be updated alongside the doc-comment wording.

**G2 — `global_config_default` does not fall through to `[[registries]]` (actionable).**
`command::global_config_default` (lines 91-101) reads only `cfg.options.default_registry`. If a user migrates their global config by running the TUI init-dialog (which after Option A will write `[[registries]]` with `default=true` and no `default_registry`), then `global_config_default` returns `None`. The `resolve_default_registry` call in `tui.rs::resolve_registry` (line 239) would then fall through to `FALLBACK_REGISTRY`, silently losing the user's global registry setting. The `registries_for_scope` path handles this correctly (uses `global_config_registries`), but `resolve_default_registry` is the fallback path used when no `[[registries]]` exists at project scope. This is a real behavioral hole for the mixed-scope case: global has `[[registries]]` only, project scope has no `[[registries]]`. The ADR does not address this. Resolution: either (a) `global_config_default` also falls back to the primary of `global_config_registries` when `default_registry` is `None`, or (b) document as a known limitation (global registry only works for browse, not single-default resolution, unless written with `default_registry`).

**G3 — No test covering "both fields present" before this change (actionable).**
The plan adds `both_fields_present_array_wins_legacy_ignored` as a new unit test, but this test does not exist yet. Without it, the current "array wins" behavior is undocumented in the test suite and could silently regress. This is a gap in the pre-migration test baseline, not just an addition.

**G4 — `add.rs` write path with mixed config (actionable).**
When `write_config` re-serializes a config that has BOTH `options.default_registry` and `[[registries]]`, it writes both back. After migration, a user who: (1) had legacy config, (2) manually adds a `[[registries]]` entry, (3) runs `grim add`, will end up with both fields in the file. The resolution (array wins at runtime) is correct but the footgun persists in the written file. The ADR acknowledges this under back-compat but does not add a test that explicitly documents this specific round-trip scenario. The unit test `write_config_preserves_legacy_default_registry` covers the legacy-only case but not the mixed case.

**G5 — `resolve_login_registry` reads `ctx.default_registry()` not the config (deferred — human decision needed).**
`command::resolve_login_registry` (lines 44-51) calls `ctx.default_registry()` which returns the `--registry` flag or `$GRIM_DEFAULT_REGISTRY` env, but NOT any config-level registry. This means `grim login` and `grim logout` never consult `[[registries]]` or `[options].default_registry`. After Option A, a user who has only `[[registries]]` in their config and runs `grim login` with no flag/env will get `NoLoginRegistry`. This is pre-existing behavior, but the ADR claims the login path is "unchanged" without noting this footgun becomes more likely as configs migrate. Reason for deferral: whether `grim login` should consult `[[registries]]` is a design decision outside Option A scope — flagged for maintainer awareness.

---

### Unverified claims

**U1 — ADR cites `global_config.rs:31-38, 67-71` for the shared parser claim.** Verified against HEAD: `from_toml_str` delegates at lines 31-37 (not 38); `load` calls `from_toml_str` at line 67. Line numbers are off by one for the closing brace but the claim is substantively correct. Negligible.

**U2 — ADR cites `project_config.rs:181-184` for the "multiple defaults tolerated" comment.** Verified at HEAD: the comment is at lines 181-184. Confirmed exactly.

**U3 — ADR cites `tui.rs:203-214` for `prompt_init`.** Verified at HEAD: `prompt_init` is defined at line 183 and the body spans through line 217. The cited range (203-214) is mid-body, not the full function. Negligible impact on implementation, but the line reference in Technical Details is slightly inaccurate.

**U4 — ADR cites `init_dialog.rs:64-69` for `InitDialogOutcome::Confirmed` doc comment.** Verified at HEAD: `InitDialogOutcome::Confirmed` doc comment is at lines 64-69 ("seeding `[options].default_registry` with `registry` when present"). Confirmed exactly. However, the ADR says update it but does not mention the user-visible rendered string at line 210 (gap G1 above).

---

### Back-compat holes

**B1 — Global config written as `[[registries]]` after Option A is ignored by `global_config_default`.**
Described under G2. If a user runs `grim tui` on a machine with no project config and accepts the TUI init-dialog for the global scope, after Option A the global config will be written with `[[registries]]`. The `resolve_default_registry` fallback path (used by `add`, `release`, and `search` as a single-default) reads `global_config_default` which returns `None` for that global config. The multi-registry path (`registries_for_scope`) works correctly. The single-default resolution path silently degrades to `FALLBACK_REGISTRY`. This is a user-visible regression for any command using `resolve_default_registry` after global config migration.

**B2 — `InitArgs.registry` help text misleads implementers post-migration.**
After `render_config` is changed to emit `[[registries]]`, the `InitArgs` struct at `init.rs:32-34` still has help text "Seed `[options].default_registry` with this value." This is only user-visible in `grim init --help` output. The ADR mentions updating this in Technical Details (writer changes item 1) but it belongs in the implementation checklist (step 2) which currently says "update help/doc strings" — this is correct, just noting the exact field for implementer clarity.

**B3 — `test_init.py` assertions break immediately on merge.**
Three acceptance tests assert the legacy string literal `'default_registry = "..."'`. These must be updated in the same PR as the `render_config` change or CI will fail. The ADR correctly plans this under the test plan.

---

### Additional files to read (if refining the ADR further)

- `/home/mherwig/dev/grimoire/test/tests/test_default_registry.py` — verify what tests exist for the back-compat read path, to confirm no existing test already covers the "both fields" case.
- `/home/mherwig/dev/grimoire/test/tests/test_registries.py` — verify two-defaults-rejected scenario not already covered.
- `/home/mherwig/dev/grimoire/src/command/add.rs` lines 80-130 — verify `registries_for_scope` call site in `add` uses the correct precedence seam.

---

### Confidence

High on resolver, schema, and writer claims — all cited symbols confirmed at HEAD with exact line numbers. Medium on the global-config single-default fallback hole (G2/B1) — the code path is confirmed but the severity depends on how many users configure a global registry via TUI. The `resolve_login_registry` gap (G5) is pre-existing and outside Option A scope.

| | |
|---|---|
| **Actionable findings** | G1, G2, G3, G4 |
| **Deferred findings** | G5 (human design decision: should `grim login` consult `[[registries]]`?) |
| **Critical back-compat hole** | B1 — `global_config_default` returns `None` for a global config written by the new TUI init path; single-default callers silently fall back to `grim.ocx.sh` |

