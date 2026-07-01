# Research: In-Repo Distillation Map for First-Party Catalog Packages

- **Date:** 2026-06-12
- **Scope:** classify in-repo source material for three publishable skills:
  `grim-usage`, `ai-config-authoring`, `grim-authoring`
- **Method:** full read of `.claude/rules/meta-ai-config.md`, `.claude/rules.md`,
  `claude_mechanisms` templates, `test_ai_config.py` (structure + key tests),
  all `docs/src/*.md`, `src/skill/*.rs`, `src/error.rs`, `src/cli/exit_code.rs`,
  `src/oci/{bundle,release,annotations}.rs`, exemplar skills (`architect`,
  `commit`, `swarm-plan`)

---

## 1. → `ai-config-authoring` (vendor-neutral authoring craft)

Legend: **[DG]** = needs de-Grimoire-ing (strip repo paths, Grimoire names,
repo infrastructure refs) before publication. Items without [DG] are already
project-neutral in substance.

### Budgets & context economy

| Item | Source | Notes |
|---|---|---|
| Context-budget table: CLAUDE.md <200 lines; rules <200 lines each; skill descriptions ≤2% of context window total; SKILL.md body <500 lines; hooks zero context cost; subagents isolated context | `meta-ai-config.md` § "Core Principle: Context Budget" | [DG] drop `CLAUDE.md`-as-filename framing where the consumer's client differs; present as "always-loaded memory file" |
| "Every line competes for attention; bloated config = ignored instructions" framing | same section | clean |
| Decision tree "where does this instruction belong?" (every-session? → path-scoped rule vs memory file; else manual-action skill vs auto-trigger skill vs hook) | `meta-ai-config.md` § decision tree | [DG] remove `disable-model-invocation` Claude-only field name from the tree or mark vendor-specific |
| Tightened practical budget: SKILL.md ≤200 lines with `references/` progressive disclosure; exceptions need documented justification | `test_ai_config.py::test_skill_body_budget` (lines ~1444-1468) | the 200 vs 500 split (hard cap vs working budget) is a good two-tier recommendation |

### Description rules (the #1 discovery factor)

| Item | Source | Notes |
|---|---|---|
| Description = #1 discovery factor; write trigger phrasing ("Contextual Signal Only": describe *when to invoke*, never the workflow) | `meta-ai-config.md` § Skills bullet 1 | [DG] drop pointer to `adr_ai_config_skill_description_csopolicy.md` (artifact not even on disk anymore) |
| Forbidden verbs in descriptions: `dispatches, runs, iterates, orchestrates, performs, executes, handles` — they make the model treat the description as the workflow and skip the body | `meta-ai-config.md` § Skills; regex in `test_ai_config.py` `_FORBIDDEN_VERB_RE` (line 1152, hyphen-aware word boundaries) | clean; the hyphen-boundary regex detail is worth carrying into a structural-test appendix |
| Front-load discriminating keywords (truncation cuts from the end); max 1024 chars; single-line only (block scalars truncate in naive parsers and concatenate in loaders) | `meta-ai-config.md` § Skills + `test_skill_descriptions_are_cso_compliant` docstring | clean |
| GOOD/BAD description examples ("API design skill. Use when designing REST APIs…" vs "Helps with APIs") | `.claude/templates/claude_mechanisms/skill.template.md` comment block | clean |
| Too many auto-trigger skills exhaust the description budget → skills silently excluded | `meta-ai-config.md` Anti-pattern #6 | clean |

### Progressive disclosure & layout

| Item | Source | Notes |
|---|---|---|
| SKILL.md = overview + workflow; reference material in sibling files loaded on demand | `meta-ai-config.md` § Skills; `skill.template.md` "Supporting files" comment | clean |
| Flat layout: skills discovered at `skills/<name>/SKILL.md` exactly; category subdirectories silently break discovery | `meta-ai-config.md` § Skills bullet "No category subdirectories" + Anti-pattern #9; `test_all_skills_at_flat_layout`, `test_no_category_directories_under_skills` | [DG] keep the principle, drop `.claude/`-specific path and Grimoire test names |
| `name` must equal directory name; lowercase+hyphens, ≤64 chars | `skill.template.md`; enforced by `test_skill_dir_matches_frontmatter_name` — and identically by grim itself (`src/skill/skill_name.rs`) | clean; nice cross-link: the agentskills rule grim enforces is the same |
| Exemplar skill structure: frontmatter → role line → numbered process → methodology → constraints (see `architect/SKILL.md`); action skills tiny + `disable-model-invocation` (see `commit`) | `.claude/skills/architect/`, `commit/`, `swarm-plan/` | structure only; [DG] all content (worker names, task commands) |

### Activation-layer model & catalog/index pattern

| Item | Source | Notes |
|---|---|---|
| Three activation layers: path-scoped rule (fires while editing matching file) vs skill (LLM matches description to task) vs catalog (read-on-demand map during planning) — conflating them = dead rules or context bloat | `meta-ai-config.md` § "Three Activation Layers" | [DG] strip Grimoire example column |
| The gap the catalog closes: path-scoped rules can't fire during planning (no file open); skills require the model to already know they exist | same section | clean — this is the strongest piece of original analysis in the repo |
| Catalog structure: "By concern", "By language", "By subsystem", "By auto-load path" tables; globals footer; declared-overlap table | `.claude/rules.md` (whole file as worked example) | [DG] heavy — keep table shapes, replace all Grimoire rows with generic examples |
| Catalog parity protocol: any rule add/remove/rename updates the catalog in the same commit; drift caught by structural tests | `.claude/rules.md` § "How to update"; `meta-ai-config.md` Anti-pattern #11 | [DG] drop test names |
| Declared path-scope overlaps: every pair of rules sharing a glob must be an intentional, documented group | `.claude/rules.md` § "Declared Path-Scope Overlaps"; `test_path_overlaps_declared_or_absent` | [DG] |

### Rule authoring

| Item | Source | Notes |
|---|---|---|
| `paths:` frontmatter for scoped rules, omit for global; minimize globals (attention cost every session) | `meta-ai-config.md` § Rules; § "Current Global Rules" | clean |
| Rule body structure: types → invariants → gotchas → cross-refs | `meta-ai-config.md` § Rules | clean |
| Dead-glob detection: after renames, verify `paths:` still match ≥1 file | `meta-ai-config.md` § Rules + Anti-pattern #7; `test_all_rule_globs_match_files` | clean |
| Don't scope rules to `src/**/*`-style catch-alls — that's just another always-on file | Anti-pattern #2; `test_subsystem_globs_not_catch_all_rs` | clean |
| Shareable-rule discipline: a rule meant for reuse across repos must contain zero project-specific types/modules/paths — and that is *testable* (forbidden-strings scan) | `meta-ai-config.md` § Rules + Anti-pattern #10; `test_shareable_rules_no_grimoire_leak` + `_GRIMOIRE_FORBIDDEN_STRINGS` | this exact pattern is the de-Grimoire-ing enforcement mechanism for the published packages themselves — promote to a first-class section |

### Agents & hooks (brief, vendor-flavored)

| Item | Source | Notes |
|---|---|---|
| Agent model tiering: cheap model for exploration, mid for implementation/review, top for architecture; narrowest `tools:` for role | `meta-ai-config.md` § Agents; `agent.template.md` | clean (genericize model names) |
| "Minimal anchored preamble + catalog pointer" agent pattern — ≤5 block-tier anchors each citing source rule, instead of duplicating whole checklists (drift-prone) | `meta-ai-config.md` § Agents | [DG] |
| Hooks = the only deterministic, zero-context enforcement layer; exit 0 proceed / exit 2 block+stderr; PreToolUse for blocking, PostToolUse for logging (never exit non-zero) | `meta-ai-config.md` § Hooks | Claude-Code-specific mechanics — keep in a clearly-labelled vendor note |
| `disable-model-invocation: true` on action skills with side effects (commit/deploy/release); keep an explicit per-skill intent table so flips are caught | `meta-ai-config.md` § Skills + Anti-pattern #5; `_EXPECTED_DISABLE_MODEL_INVOCATION` table (test line ~1159) | vendor-specific field, general principle |
| Field-name trap: subagents use `tools:`, commands/skills use `allowed-tools:` | `agent.template.md` / `command.template.md` NOTE comments | vendor note |

### Anti-patterns (consolidated)

From `meta-ai-config.md` § Anti-Patterns — generalize #1–#10 (over-long globals, catch-all globs, duplicated content across layers, verbose SKILL.md, missing invocation gate on action skills, description-budget exhaustion, dead globs, config drift vs code, category subdirs, project leakage into shareable artifacts). **Exclude** #12 (`triggers:` — repo-local routing hook). #11 (catalog drift) keep as part of catalog pattern.

### Structural-test ideas (appendix material)

From `test_ai_config.py` (1840 lines) — the *ideas*, never the file: dead-glob scan; line budgets (memory file, rule, skill body) with justified-exception lists; cross-reference resolution (every markdown link in config resolves); skill-dir == frontmatter-name; flat-layout check; forbidden-verb regex on descriptions; total description budget vs context cap; per-skill invocation-gate intent table; forbidden-strings scan for shareable artifacts; "test the contract, not the content" principle (`meta-ai-config.md` § Structural Validation Tests). **[DG]** all of it — examples must use neutral paths.

### Caution — stale template content

`rule.template.md` claims "Rules are auto-loaded for ALL files… NO path-based conditional loading", contradicting `meta-ai-config.md` and actual repo usage of `paths:` frontmatter. Do **not** distill that comment; treat `meta-ai-config.md` as authoritative.

---

## 2. → `grim-usage` / `grim-authoring` distillation map

Docs site base: `https://grimoire.rs/` (mdBook; pages =
`docs/src/*.md` → `*.html`). Anchors marked *(auto)* are mdBook-slugified
headings; all others are explicit `{#…}` ids in the source and stable.

### grim-usage (use the grim CLI)

| Proposed reference file | Distills | Anchor list |
|---|---|---|
| `SKILL.md` (overview + mental model) | `concepts.md` (kinds, OCI backing, refs/tags/digests, lock, scopes, clients, catalog, online-by-default); `introduction.md` | `concepts.html` *(auto: #skills-rules-and-agents, #artifacts-as-oci-content, #references-tags-and-digests, #the-lock, #scopes, #the-catalog)*, `#bundles`, `#clients`, `#rule-support-dir`; `introduction.html` *(auto)* |
| `lifecycle.md` | `commands.md` lifecycle: init → add → lock → install → update → status → remove vs uninstall; update pruning + `kept-modified`; effective-declaration semantics | `commands.html#init`, `#add`, `#lock`, `#install`, `#update`, `#status`, `#remove`, `#uninstall`; `quickstart.html` *(auto: #1-create-a-project-config … )*, `#5-upgrade` |
| `bundles.md` | `concepts.md` bundles (expansion, provenance, membership tracking, conflict policy fail-closed, floating vs pinned); `configuration.md` `[bundles]` + lock `[[bundle]]` cache | `concepts.html#bundles`, `#bundle-membership`, `#bundle-conflicts`, `#bundle-pinning`; `configuration.html` *(auto: #grimoiretoml, #grimoirelock)* |
| `config.md` | `configuration.md`: `grimoire.toml` shape, `grimoire.lock` (machine-owned, commit it), scopes on disk, env vars (`GRIM_HOME`, `GRIM_DEFAULT_REGISTRY`, `GRIM_OFFLINE`, `GRIM_INSECURE_REGISTRIES`, `DOCKER_CONFIG`), precedence flag > config option > env | `configuration.html` *(auto: #grimoiretoml, #grimoirelock, #scopes-on-disk, #environment-variables, #data-layout)* |
| `auth.md` | `authentication.md`: docker-config credential resolution, login/logout flow, `--password-stdin` (no `--password` by design), `--allow-insecure-store`, helper precedence, CI recipe | `authentication.html#resolving`, `#login`, `#logout`, `#store`, `#ci`; `commands.html#login`, `#logout` |
| `discovery.md` | `commands.md` search + TUI; catalog cache, `--refresh`, summary-vs-description display, JSON `repository` field; TUI key highlights | `commands.html#search`, `#tui`; `publishing.html#metadata-repository` |
| `offline.md` | `concepts.md` online-by-default / `--offline` cache-only; warm-cache workflow | `concepts.html` *(auto: #online-by-default-offline-on-demand)*; `configuration.html` *(auto: #environment-variables)* |
| `troubleshooting.md` | exit-code classes (§5 below) + common failure → code → fix table; install integrity gate / `--force`; drift on generated files | `vendor-metadata.html#drift`; `commands.html#install`, `#update` |
| `install-targets.md` (optional) | client selection (`--client`, `clients` option, default `["claude"]`), per-client install locations project + global, env overrides | `vendor-metadata.html#discovery-locations`; `agents.html#locations`; `concepts.html#clients` |

### grim-authoring (artifact schemas & pitfalls)

| Proposed reference file | Distills | Anchor list |
|---|---|---|
| `SKILL.md` (kind chooser + build/release workflow) | `artifacts.md` four-kinds table + kind inference (`--kind agent` mandatory); `publishing.md` build-then-release, dry-run, cascade tags, `--force`, `--pin` | `artifacts.html#kinds`, `#names`; `publishing.html` *(auto: #validate-before-you-push, #release, #cascade-tags? → auto: #cascade-tags, #dry-runs-and-overwrites)*, `#bundles`, `#pin`; `commands.html#build`, `#release` |
| `skills-schema.md` | `artifacts.md` skill table + examples; name/description constraints; `metadata` map string-only contract | `artifacts.html#skills`, `#skill-example-minimal`, `#skill-example-full`, `#names` |
| `rules-schema.md` | `artifacts.md` rule table (frontmatter fully optional; top-level `summary`/`keywords` asymmetry vs skills); support directory | `artifacts.html#rules`, `#rule-example-minimal`, `#rule-example-scoped`, `#rule-example-vendor`; `publishing.html#rule-support-dir`; `concepts.html#rule-support-dir` |
| `agents-schema.md` | `agents.md`: required frontmatter, name == stem, common fields, override precedence, emit matrix, limitations (no object-valued fields, no support dir, no model translation) | `agents.html#format`, `#common-fields`, `#override-precedence`, `#emit-matrix`, `#publishing`, `#limitations` |
| `bundles-schema.md` | `artifacts.md` bundle table (member tables, 512-member / 512 KiB caps, no nesting); `publishing.md` bundle publishing + pinning | `artifacts.html#bundles`, `#bundle-example`; `publishing.html#bundles`, `#pin`; `concepts.html#bundle-pinning` |
| `vendor-metadata.md` | `vendor-metadata.md` whole page: common-vs-unique rule, projection semantics table, all four key registries, empty registries = typo guard, unified universal render, publish-time validation | `vendor-metadata.html#why-metadata`, `#common-vs-unique`, `#projection-semantics`, `#claude-registry`, `#agent-overrides`, `#claude-agent-registry`, `#opencode-agent-registry`, `#copilot-agent-registry`, `#empty-registries`, `#rule-keys`, `#publish-validation`, `#migration` |
| `catalog-metadata.md` | `publishing.md` catalog metadata per kind (where `summary`/`keywords`/`repository` live differs by kind!), keywords-is-a-string rule, repository HTTPS rule; `artifacts.md` annotations table | `publishing.html#metadata`, `#metadata-skill`, `#metadata-rule`, `#metadata-agent`, `#metadata-bundle`, `#metadata-keywords`, `#metadata-repository`; `artifacts.html#annotations`, `#vendor-extensions` |
| `pitfalls.md` (per-kind tables) | §4 below | cite `vendor-metadata.html#publish-validation` and `artifacts.html#names` |

Note the deliberate split: **grim-usage** never explains frontmatter schemas;
**grim-authoring** never explains lifecycle commands beyond `build`/`release`;
both link the docs site rather than duplicating long tables (the registries
tables are the one justified duplication — they ARE the pitfalls source).

---

## 3. → EXCLUDE (Grimoire-repo-internal; must not leak)

| Item | Where it lives | Why excluded |
|---|---|---|
| Worktree names/branches (`goat`, `evelynn`, `sion`, `soraka`) | `CLAUDE.md` § Workflow | repo workflow |
| Task runner specifics (`task verify`, `rust:verify`, `shell:verify`, `claude:tests`, Taskfile tree) | `CLAUDE.md`, `subsystem-taskfiles.md` | repo build infra |
| Swarm workflow + worker agents (`swarm-plan/execute/review`, `worker-*`, tiers, Review-Fix Loop) | `workflow-swarm.md`, swarm skills | repo orchestration |
| Plan Status Protocol, `current_plan.md`, subplan parent-stack, `/next` | `meta-ai-config.md` § Plan Status Protocol | repo state machine |
| Cross-Session Learnings Store (JSONL schema, TTL decay, hooks) | `meta-ai-config.md` § Learnings Store | repo infra |
| `triggers:` frontmatter + UserPromptSubmit routing hook contract | `meta-ai-config.md` § Skills; `TestPromptRoutingTriggers` | non-standard, repo-local hook |
| Research Protocol (worker-explorer/researcher spawning, `research_*.md` persistence) | `meta-ai-config.md` § Research Protocol | repo process |
| Hook implementations (`post_tool_use_tracker.py`, `stop_validator.py`, PEP 723/`uv run` conventions, `config_reminder` tables) | `meta-ai-config.md` § Hooks / Staleness Detection | repo scripts (the generic exit-0/2 semantics survive into §1) |
| Commit/branch/landing protocol (`/commit`, `/finalize`, Conventional Commits policy, "never push") | `workflow-git.md`, CLAUDE.md | repo git policy |
| GitHub context checks, issue/PR templates, workflow-intent router | `workflow-intent.md`, `workflow-github.md` | repo process |
| Grimoire subsystem rules content (`subsystem-cli*.md`, `subsystem-file-structure.md`, `subsystem-tests.md`, `subsystem-ci.md`), `arch-principles.md`, ADR index | `.claude/rules/` | Grimoire codebase internals (overlapping *product* facts must be sourced from `docs/src`, never from these) |
| `quality-*.md` rule contents (Rust/Python/Bash standards, exit-code rule file) | `.claude/rules/` | Michael's cross-repo code standards, out of scope for all three skills (exit-code *facts* for grim come from `src/`, not the rule) |
| Structural test file itself (`test_ai_config.py` constants, `_GRIMOIRE_FORBIDDEN_STRINGS`, expected-skill tables) | `.claude/tests/` | repo enforcement; only the test *ideas* travel (§1) |
| Product positioning / target users / "provisional" status framing | `product-context.md` | internal strategy; published skills state facts, not positioning |
| `GRIM_*` env facts must come from `docs/src/configuration.md`, not CLAUDE.md's env table (CLAUDE.md mixes in repo-dev detail like `OPENCODE_CONFIG` nuances + detection internals) | `CLAUDE.md` § Environment Variables | partially stale-prone duplicate of docs |
| Persona skill roster (`/architect`, `/builder`, …) and rule-catalog rows | `.claude/rules.md` | repo-specific instances (the *pattern* travels, §1) |

---

## 4. Validation pitfalls inventory (hard errors — exit 65 unless noted)

Seed material for per-kind pitfalls tables in `grim-authoring/pitfalls.md`.

### All kinds — names (`src/skill/skill_name.rs`)

| Pitfall | Error |
|---|---|
| Name empty | `skill name is empty` |
| Name >64 chars | `…exceeds 64 characters` |
| Chars outside `[a-z0-9-]` (uppercase, `_`, `.`, space) | `…must contain only lowercase letters, digits, and hyphens` |
| Leading/trailing hyphen | `…must not start or end with a hyphen` |
| Consecutive hyphens (`a--b`) | `…must not contain consecutive hyphens` |

### Skills (`src/skill/skill_package.rs::validate_skill_dir`, `skill_frontmatter.rs`, `skill_description.rs`)

| Pitfall | Error kind |
|---|---|
| Directory has no `SKILL.md` | `MissingSkillMd` |
| No leading `---` fence, or fence never closed | `MissingFrontmatter` |
| Malformed YAML; missing `name` or `description` | `FrontmatterParse` |
| Frontmatter `name` ≠ directory name | `NameMismatch` |
| Description empty/whitespace-only or >1024 chars | rejected in deserialize → `FrontmatterParse` |
| Directory name itself invalid as a name | `NameInvalid` |

Non-errors that surprise: unknown top-level keys are *preserved* (forward
compat), but legacy Claude fields at top level (e.g. `user-invocable: true`)
emit a migration-nudge **warning** (`docs/src/vendor-metadata.md#migration`).

### Rules (`validate_rule_file`, `rule_frontmatter.rs`)

| Pitfall | Error kind |
|---|---|
| File stem violates name rules (`Bad_Name.md`) | `NameInvalid` |
| Fence present but YAML malformed | `FrontmatterParse` |
| Vendor key authored top-level instead of in `metadata` (`copilot.exclude-agent:` at top level) | **warning**, key not projected (`vendor-metadata.md#rule-authoring-example`) |

Bare `.md` with no fence = valid (empty frontmatter, body = whole doc).
Inversion pitfall: `summary`/`keywords` are **top-level** in rules, but inside
`metadata` for skills/agents (`artifacts.md#rules`).

### Agents (`validate_agent_file`, `agent_frontmatter.rs`)

| Pitfall | Error kind |
|---|---|
| No frontmatter at all (frontmatter is REQUIRED for agents) | `MissingFrontmatter` |
| Missing `description` (or `name`) | `FrontmatterParse` |
| `name` ≠ file stem | `NameMismatch` |
| Forgetting `--kind agent` at build/release | not an error — silently packs as a **rule**; grim warns when a rule carries both `name`+`description` (`agents.md#publishing`) |
| Sibling dir sharing the stem | silently **ignored** (agents have no support dir) |

### Vendor metadata (all kinds; `SkillErrorKind::MetadataInvalid`; `vendor-metadata.md#projection-semantics`, registry tables)

| Pitfall | Outcome |
|---|---|
| Known `<vendor>.<field>` with bad literal — bool not `"true"/"false"`, enum outside its closed set, non-base-10 integer, non-finite float | hard error: publish fails 65; install fails `MaterializeFailed` |
| Closed enum sets to memorize: `claude.effort`/agent `claude.effort` ∈ low\|medium\|high\|xhigh\|max; `claude.context` ∈ fork; `claude.shell` ∈ bash\|powershell; `claude.permission-mode` ∈ default\|acceptEdits\|auto\|dontAsk\|bypassPermissions\|plan; `claude.memory` ∈ user\|project\|local; `claude.isolation` ∈ worktree; `claude.color` ∈ red\|blue\|green\|yellow\|purple\|orange\|pink\|cyan; `opencode.mode` ∈ primary\|subagent\|all; `copilot.exclude-agent` ∈ code-review\|cloud-agent | hard error 65 on any other value |
| Unknown key in own namespace (typo: `claude.efort`) | **warning** + drop (typo guard) — silent data loss if warning ignored |
| Any `opencode.*`/`copilot.*` key on a **skill** | always unknown (registries empty) → warn + drop |
| Foreign-namespace key | dropped silently (by design) |
| Object-valued vendor fields (`hooks`, `mcpServers`, OpenCode `permission`, Copilot `mcp-servers`) | not authorable at all — `metadata` is string-valued |
| Namespaced key colliding with same-name top-level key | namespaced wins + warning (migration case) |
| Vendor key shadowing common field (`claude.model` vs `model` on agents) | silent override — documented escape hatch, NOT an error |

### Catalog metadata / annotations (`src/oci/annotations.rs`; `publishing.md`)

| Pitfall | Outcome |
|---|---|
| `repository` not `https://` (e.g. `git@…`, `http://`) | hard error 65: `invalid value '…' for metadata key 'repository': expected an https:// URL` |
| `keywords` as YAML/TOML list instead of comma string | not accepted — must be a single string (`publishing.md#metadata-keywords`) |

### Bundles (`src/oci/bundle.rs`; `artifacts.md#bundles`)

| Pitfall | Outcome |
|---|---|
| Nested bundle member | invalid — members must be skill/rule/agent |
| >512 members (`MAX_BUNDLE_MEMBERS`) | parse-time error |
| Members document >512 KiB (`BUNDLE_LAYER_SIZE_LIMIT`) | rejected |
| Member ref not fully qualified | invalid reference (DataError) |
| Two declared bundles disagreeing on a member | `BundleConflict` → exit **78** ConfigError at lock time (consumer-side, fail-closed) |

### Release (`src/oci/release.rs` → all DataError 65)

| Pitfall | Error |
|---|---|
| Reference with no tag at all | `MissingTag` |
| Invalid version string | `InvalidVersion` |
| Exact-version tag exists pointing at different bytes, no `--force` | `TagExists` (immutability gate) |

---

## 5. Exit-code classes (`src/cli/exit_code.rs` + classifier `src/error.rs`)

For `grim-usage/troubleshooting.md`. Aligned with BSD `sysexits.h`; 79+ are
grim-specific extensions.

| Code | Name | Meaning / representative triggers (from `classify_*`) |
|---|---|---|
| 0 | Success | — |
| 1 | Failure | unclassified fall-through (locked by test) |
| 64 | UsageError | bad invocation; login input errors; `grim init` when config already exists |
| 65 | DataError | the authoring-validation class: bad identifiers/digests, skill/rule/agent validation (§4), bad vendor literals, non-HTTPS repository, release version/tag errors, integrity mismatch, malformed manifests/lock-stale, malformed docker config |
| 69 | Unavailable | registry unreachable, resolve timeout |
| 74 | IoError | filesystem read/write failures (non-permission), helper communication failure |
| 75 | TempFail | advisory lock held by another process (`Locked`); credential-helper timeout |
| 77 | NoPermission | `PermissionDenied` I/O anywhere in the chain |
| 78 | ConfigError | TOML parse failures (config/lock), unsupported versions, no registry resolvable, bundle conflict, unsupported client, credential helper not on PATH, no credential store |
| 79 | NotFound | tag/bundle not found, manifest/blob 404, config not discovered, lock missing |
| 80 | AuthError | registry auth failure, helper failed |
| 81 | OfflineBlocked | `--offline`/`GRIM_OFFLINE` blocked a network operation (deliberate policy, distinct from 69) |

Troubleshooting hook for scripts: `case $?` on these values is the supported
automation contract (`commands.md` + JSON output).

---

## Sources

- `/home/mherwig/dev/grimoire/.claude/rules/meta-ai-config.md`
- `/home/mherwig/dev/grimoire/.claude/rules.md`
- `/home/mherwig/dev/grimoire/.claude/templates/claude_mechanisms/skill.template.md`, `rule.template.md`, `agent.template.md`, `command.template.md`
- `/home/mherwig/dev/grimoire/.claude/tests/test_ai_config.py`
- `/home/mherwig/dev/grimoire/.claude/skills/architect/SKILL.md`, `commit/SKILL.md`, `swarm-plan/SKILL.md` (structure exemplars)
- `/home/mherwig/dev/grimoire/docs/src/SUMMARY.md`, `introduction.md`, `installation.md`, `quickstart.md`, `concepts.md`, `commands.md`, `configuration.md`, `authentication.md`, `publishing.md`, `agents.md`, `artifacts.md`, `vendor-metadata.md`
- `/home/mherwig/dev/grimoire/src/skill/skill_name.rs`, `skill_description.rs`, `skill_frontmatter.rs`, `rule_frontmatter.rs`, `agent_frontmatter.rs`, `skill_error.rs`, `skill_package.rs`
- `/home/mherwig/dev/grimoire/src/error.rs`, `src/cli/exit_code.rs`
- `/home/mherwig/dev/grimoire/src/oci/bundle.rs`, `release.rs`, `annotations.rs`
- ADRs skimmed: `adr_agent_artifact_kind.md`, `adr_tool_namespaced_metadata_rendering.md`, `adr_repository_annotation.md`, `adr_catalog_summary_annotation.md`, `adr_multifile_rules.md`, `adr_oci_artifact_type.md` (titles + decision lines; docs pages are the published-facing source of the same facts)
