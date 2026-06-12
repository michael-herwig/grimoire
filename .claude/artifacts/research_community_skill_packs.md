# Research: Community Norms for Published AI Skill/Rule/Agent Packs

**Date:** 2026-06-12
**Method:** Deep-research fan-out (5 parallel agents) over actual repo files via
GitHub API / raw.githubusercontent.com, followed by adversarial spot-verification
of load-bearing quotes against primary sources. All line/byte counts below were
measured from raw file contents, not estimated from READMEs.

---

## Executive Summary

1. **SKILL.md length norms are real and observed.** Across all 17 skills in
   `anthropics/skills`: min 32 / median 254 / max 590 lines. 16 of 17 respect the
   repo's own "<500 lines" rule. The de-facto shape is: thin skills (≤130 lines)
   that point to scripts, mid-size workflow guides (230–360), and a few dense
   knowledge skills (375–590). Superpowers skills run 100–400 lines with a
   stricter house target (<500 *words* for most skills).
2. **Frontmatter in the wild is minimal**: `name` + `description` only, sometimes
   `license`. Nobody in the two flagship repos uses `allowed-tools`, `version`,
   or `compatibility` — versioning lives in plugin manifests, not skills.
3. **The description is the product.** The dominant convention is *capability
   sentence + "Use when…" trigger sentence*, third person, keyword-stuffed
   (synonyms, file extensions, error messages), often with explicit negative
   scope ("Do NOT use for…"). Anthropic officially recommends "pushy"
   descriptions because Claude undertriggers; superpowers goes further:
   description = triggering conditions ONLY, never a workflow summary (a summary
   becomes a shortcut Claude takes instead of reading the skill body).
4. **CLI-teaching skills are a weak spot in the ecosystem.** Every published
   CLI skill examined (helmfile, kubectl/gh/terraform packs, ffmpeg, EKS, gh)
   teaches raw inline commands; **none** defers to `--help`, and **none**
   instructs the agent to verify the installed tool version before trusting
   documented flags. Drift handling tops out at a frontmatter `last_updated` +
   `doc_source` pair. The only "--help first" doctrine found anywhere is
   Anthropic's webapp-testing — aimed at its own *bundled* scripts (black-box
   pattern), not external CLIs. This is an open niche, not a solved norm.
5. **Quality separates on evaluation evidence and maintenance.** Excellent packs
   (obra/superpowers, wshobson/agents) test their skills (in-repo trigger/behavior
   test suites, RED-GREEN-REFACTOR for docs), version semantically, ship plugin
   marketplace metadata, and push daily/weekly. Mediocre packs are
   template-stamped 100+-agent rosters with no evals, vague descriptions, and
   silent staleness. The ecosystem converged on **plugins/marketplaces** as the
   distribution unit and SKILL.md as a cross-vendor open standard
   (agentskills.io, adopted by ~40 clients).

---

## Detailed Findings

### 1. Structure norms in the wild

#### anthropics/skills (official reference; branch `main`)

Repo layout: flat `skills/` dir (17 skills), `.claude-plugin/marketplace.json`
(3 plugin bundles: `document-skills`, `example-skills`, `claude-api`),
`template/SKILL.md` (6-line starter), `spec/` (now a pointer to
agentskills.io/specification). No per-skill READMEs anywhere — the convention is
**SKILL.md + LICENSE.txt + resource dirs only**.

Measured SKILL.md length distribution (all 17 skills):

| Bucket | Skills | Examples |
|---|---|---|
| ≤130 lines (thin pointer skills) | 7 | internal-comms 32, webapp-testing 95, canvas-design 129 |
| 230–360 (workflow guides) | 6 | pptx 232, mcp-builder 236, pdf 314, claude-api 356 |
| 375–590 (dense knowledge) | 4 | doc-coauthoring 375, skill-creator 485, **docx 590 (only one over the 500-line rule)** |

Min 32 / median 254 / mean ~238 / max 590 lines; mean ~1,776 words.

Frontmatter universe across the whole repo: **only `name`, `description`,
`license`** ever appear. No `allowed-tools`, `version`, `metadata`, or
`compatibility` (skill-creator documents `compatibility` as an option, but no
skill uses it — verified across all 17 frontmatters).

Canonical anatomy per skill-creator
(<https://raw.githubusercontent.com/anthropics/skills/main/skills/skill-creator/SKILL.md>):
`scripts/` (executable code for deterministic/repetitive tasks), `references/`
(docs loaded into context as needed), `assets/` (files used in output). In
practice even Anthropic deviates: mcp-builder uses `reference/` (singular),
webapp-testing uses `examples/`, pdf keeps `reference.md`/`forms.md` at the
skill root, skill-creator adds `agents/`. Naming: skill dirs lowercase
kebab-case matching frontmatter `name` exactly; supporting files snake_case.

Authoring rules embedded in skill-creator (verified verbatim):

- Progressive disclosure: *"1. Metadata (name + description) - Always in context
  (~100 words) 2. SKILL.md body - In context whenever skill triggers (<500 lines
  ideal) 3. Bundled resources - As needed (unlimited, scripts can execute
  without loading)"*
- *"Keep SKILL.md under 500 lines; if you're approaching this limit, add an
  additional layer of hierarchy along with clear pointers about where the model
  using the skill should go next."*
- *"Reference files clearly from SKILL.md with guidance on when to read them"*;
  TOC for reference files >300 lines.
- Style: *"Prefer using the imperative form"*; *"If you find yourself writing
  ALWAYS or NEVER in all caps, or using super rigid structures, that's a yellow
  flag."* (Note: superpowers deliberately violates this with Iron Laws — a real
  philosophical split between the two flagship packs.)
- Script-bundling test: if 3 test runs all make the agent write the same
  `create_docx.py`, bundle it in `scripts/` and tell the skill to use it.

Linking styles observed: relative links with explicit when-to-read guidance
("Read `agents/comparator.md` for the details"), mcp-builder's load-phase
annotations ("Load During Phase 4"), and webapp-testing's black-box doctrine
(verified verbatim): *"Always run scripts with `--help` first to see usage. DO
NOT read the source until you try running the script first… They exist to be
called directly as black-box scripts rather than ingested into your context
window."*

Licensing: per-skill, two tiers — Apache 2.0 for 12 example skills, a short
proprietary license for the 4 production document skills (docx/pdf/pptx/xlsx,
README: "source-available, not open source"). No repo-wide LICENSE.

#### obra/superpowers (flagship community pack, v5.1.0)

<https://github.com/obra/superpowers> — 14 process skills, flat under `skills/`,
MIT. Frontmatter even more minimal than Anthropic's: **only `name` +
`description`** on every skill. Structure highlights:

- **Hook-bootstrapped meta-skill**: a SessionStart hook cats
  `skills/using-superpowers/SKILL.md` into every session wrapped in
  `<EXTREMELY_IMPORTANT>`; everything else is discovered via descriptions.
- House style: GraphViz `dot` flowcharts ("Claude is particularly good at
  following processes written in dot" — Superpowers 4 post), "Iron Law" blocks,
  **rationalization tables** (Excuse → Reality), **Red Flags** sections,
  `<Good>`/`<Bad>` paired examples.
- Cross-linking convention (verified verbatim from writing-skills): use
  plugin-qualified names — `**REQUIRED SUB-SKILL:** Use
  superpowers:test-driven-development` — and never `@`-paths: *"`@` syntax
  force-loads files immediately, consuming 200k+ context before you need
  them."*
- Skill testing as a first-class practice:
  `skills/writing-skills/testing-skills-with-subagents.md` (384 lines) — *"Writing
  skills IS Test-Driven Development applied to process documentation"*: run
  pressure scenarios WITHOUT the skill (RED, capture rationalizations verbatim),
  write the skill against those failures (GREEN), close loopholes (REFACTOR).
  Plus executable harnesses in `tests/` (skill-triggering assertions, behavioral
  tests that plant real bugs and assert the reviewer catches them).
- Distribution: `.claude-plugin/plugin.json` (semver 5.1.0) + per-harness
  manifests (Codex, Cursor, Gemini, OpenCode) synced from one repo; production
  marketplace is a separate repo (`obra/superpowers-marketplace`); 1,180-line
  human-written RELEASE-NOTES.md. Legacy `commands/` and `agents/` dirs were
  **deliberately removed** in v5.1.0 — skills absorbed both roles.

### 2. CLI-teaching skills: patterns and drift handling

Eight CLI skills examined at file level. Spectrum of findings:

| Skill | Tool | Lines | Inline cmds | --help deferral | Drift handling | Troubleshooting |
|---|---|---|---|---|---|---|
| [helmfile/helmfile skills/helmfile](https://github.com/helmfile/helmfile/blob/main/skills/helmfile/SKILL.md) | helmfile | 698 | 32 + flag tables | none | dated prose "Status" section ("as of May 2025") — itself rots | yes, "Common Issues" prose list |
| [Melvynx/cli-skills](https://github.com/Melvynx/cli-skills) (kubectl/gh/terraform) | 26 CLIs | 98–131 each | 36–46 table rows each | none | **none** (`--version` only as install check) | absent |
| [rendi-api/ffmpeg-cheatsheet](https://github.com/rendi-api/ffmpeg-cheatsheet/blob/main/skills/ffmpeg/SKILL.md) | ffmpeg | 135 | 11 full invocations | defers to bundled `references/command-patterns.md`, not the tool | none | "Gotchas" section (pitfall → explanation) |
| [itsmostafa/aws-agent-skills skills/eks](https://github.com/itsmostafa/aws-agent-skills/blob/main/skills/eks/SKILL.md) | aws/eksctl/kubectl | 392 | 31 | passive doc links only | **frontmatter `last_updated: "2026-01-07"` + `doc_source:` URL** — strongest found; yet body still hardcodes `--addon-version` pins | best found: symptom headings + diagnostic command blocks |
| [Dimillian/Skills github](https://github.com/Dimillian/Skills/blob/main/github/SKILL.md) | gh | 68 | ~10, as task recipes | none | none | none |
| [mauromedda/agent-toolkit terraform](https://github.com/mauromedda/agent-toolkit/blob/main/skills/terraform/SKILL.md) | terraform | 642 | many | none | deprecation row in anti-pattern table ("`terraform taint` → Deprecated, use `-replace`") | partial |

Cross-cutting conclusions (each a finding, including the negatives):

1. **Wrap vs teach**: every CLI skill found *teaches* raw commands; none *wraps*
   the CLI in bundled scripts. The wrap pattern (webapp-testing) is used for
   context economy around bundled helpers, not drift insulation around external
   tools.
2. **`--help` deferral is essentially absent** in the wild. Zero instances of
   "run `<tool> --help` before using these flags" across all eight skills.
3. **No skill anywhere instructs version verification before trusting documented
   flags.** `tool --version` appears only as an install check. The strongest
   drift practices observed: frontmatter `last_updated`/`doc_source` (EKS),
   deprecation rows (mauromedda), dated status prose (helmfile).
4. Two viable content shapes: **exhaustive cheatsheet tables** (Melvynx — high
   token cost, high coverage) vs **task recipes/workflows** (Dimillian's
   4-step "Debugging a CI Failure": `gh pr checks` → `gh run list` →
   `gh run view` → `gh run view --log-failed`) — the recipe form is half the
   lines and closer to how agents actually work. ffmpeg's "decision rules, not
   just commands" is the best judgment/recipe balance found.
5. Community consensus on staleness exists but lives outside skills: Joost de
   Valk, *"An agent skill without a version check is cached documentation.
   Useful until it's wrong"*
   (<https://joost.blog/self-updating-agent-skills/>) — his fix is a
   SessionStart hook running `npx skills update` outside the context window.
   Stale skills fail *silently*: output stays plausible while quality drifts.

### 3. Description/keyword conventions for discoverability

**Mechanism (confirmed against official docs):** only name + description are
preloaded (~100 tokens/skill) into the system prompt; the body loads on trigger;
resources load on demand
(<https://platform.claude.com/docs/en/agents-and-tools/agent-skills/overview>).
Claude Code truncates combined description text at **1,536 chars** in the skill
listing (<https://code.claude.com/docs/en/skills>).

**Hard constraints** (platform docs + agentskills.io spec): `name` ≤64 chars,
lowercase alphanumeric + hyphens, no leading/trailing/consecutive hyphens, must
match the parent directory name, reserved words "anthropic"/"claude" banned;
`description` non-empty, ≤1024 chars, no XML tags.

**Conventions observed across real packs:**

- Dominant shape: *capability sentence(s) + "Use when …" trigger sentence*,
  third person. Official good example: `Extract text and tables from PDF files,
  fill forms, merge documents. Use when working with PDF files or when the user
  mentions PDFs, forms, or document extraction.`
- **Keyword stuffing is sanctioned practice** for production skills: the docx
  description enumerates trigger phrases ("'Word doc', 'word document',
  '.docx'"), deliverable synonyms ("'report', 'memo', 'letter'"), and ends with
  negative scope: *"Do NOT use for PDFs, spreadsheets, Google Docs…"*
- Official guidance says make descriptions *"a little bit 'pushy'"* because
  Claude undertriggers (skill-creator, verified verbatim).
- **Superpowers' stricter refinement (CSO — "Claude Search Optimization")**,
  verified verbatim from writing-skills: *"Description = When to Use, NOT What
  the Skill Does… Do NOT summarize the skill's process or workflow in the
  description."* Empirical basis: *"A description saying 'code review between
  tasks' caused Claude to do ONE review, even though the skill's flowchart
  clearly showed TWO reviews."* The trap: workflow-summarizing descriptions
  become a shortcut Claude takes instead of reading the body. Jesse Vincent's
  blog confirms the experimental origin ("Skills for Claude!", 2025-10-16).
- Naming: gerund verb-first recommended by both camps (`processing-pdfs`,
  `writing-skills`); avoid `helper`/`utils`/`tools`.
- Version numbers in descriptions are rare; versioning lives in plugin
  manifests.
- Anti-patterns in the wild: missing when-clause (even official webapp-testing),
  over-broad triggers ("any web application"), spec-violating uppercase names,
  vague one-liners ("Helps with documents" — the docs' own bad example).

### 4. Quality signals: excellent vs mediocre packs

**Signals that separate the top packs** (each verified at source):

1. **Evaluation evidence** — the scarcest signal. Official best-practices doc:
   "Create evaluations BEFORE writing extensive documentation." skill-creator
   ships an eval harness (`run_eval.py`, benchmark variance analysis);
   superpowers ships `tests/` suites asserting trigger behavior and review
   quality. No 100+-agent roster examined carries per-agent evals.
2. **Progressive disclosure discipline** — <500-line bodies, one level of
   reference hierarchy, TOCs for big references, scripts as black boxes.
3. **Description quality** per §3.
4. **Scripts for deterministic ops** — "Solve, don't punt"; steipete's
   migration from prose rules (`agent-rules`) to executable scripts+skills
   (`agent-scripts`) is field evidence of the same lesson.
5. **Maintenance cadence + honest deprecation** — superpowers/wshobson push
   daily-weekly; steipete's README honestly points to its successor; zombie
   packs (iannuttall/claude-agents, frozen July 2025 on pre-skills frontmatter)
   keep accumulating installs from awesome-list links.
6. **Real distribution mechanics** — plugin marketplace manifests with
   per-plugin license/version/author beat "copy this file into your project."

**Held up as excellent:**

- **obra/superpowers** (<https://github.com/obra/superpowers>) — the only pack
  treating skills as *tested artifacts*: TDD-for-docs methodology, in-repo test
  harnesses, semver, multi-harness packaging, daily maintenance.
- **wshobson/agents** (<https://github.com/wshobson/agents>) — best-in-class
  distribution engineering: 84 granular per-topic plugins ("optimized for
  granular installation and minimal token usage"), full marketplace metadata,
  third-party contribution model.

**Mediocre patterns:**

- Template-stamped agent rosters — recurring scaffold text across unrelated
  agents (e.g. "Query context manager for…" boilerplate in
  [VoltAgent backend-developer.md](https://github.com/VoltAgent/awesome-claude-code-subagents/blob/main/categories/01-core-development/backend-developer.md));
  volume before evaluation.
- Persona-flattery rules with fake scoping (`globs: **/*` + "you are a genius
  at reasoning") — legacy `*-cursorrules-prompt-file.mdc` conversions in
  awesome-cursorrules.
- Silent staleness without deprecation pointers.
- Note: superpowers v5.1.0 release notes cite a **94% PR rejection rate** driven
  by AI-generated slop — curation pressure is now a defining ecosystem force
  (hesreallyhim renamed his list from "awesome-" to "a-list-of-" because
  curation at volume was infeasible).

### 5. Rule packs / agent packs: notable collections and conventions

- **Agent packs**: one `.md` per agent with frontmatter `name`, `description`
  ("Use this agent when…"), optional `tools:` allowlist and `model:`
  (`inherit` ages better than pinned model names). wshobson namespaces agent
  names by plugin (`backend-development-backend-architect`); VoltAgent
  organizes by numbered category dirs (`categories/01-core-development/…`)
  exposed as per-category marketplace plugins.
- **Rule packs**: PatrickJS/awesome-cursorrules (40k stars, CC0) — 257 flat
  `.mdc` files, frontmatter `description`/`globs`/`alwaysApply`, install =
  copy-paste into `.cursor/rules/`. steipete/agent-rules — dual-purpose
  `.mdc`/commands, now superseded by steipete/agent-scripts (scripts + hooks +
  skills + AGENTS.md). **Distributed CLAUDE.md/rule collections remain
  fragmented; the ecosystem converged on plugins as the shareable unit
  instead.** Path-glob-scoped rules (like Grimoire's `paths:` frontmatter) are
  a community convention, not a documented cross-pack standard.
- **Plugins as the distribution vehicle**
  (<https://code.claude.com/docs/en/plugins>): `.claude-plugin/plugin.json`
  (name = skill namespace; omitted version → git SHA), components at plugin
  *root* (`skills/`, `agents/`, `commands/`, `hooks/`, `.mcp.json`);
  marketplace = repo with `.claude-plugin/marketplace.json`. Two Anthropic-run
  marketplaces: `claude-plugins-official` (curated) and
  `anthropics/claude-plugins-community` (reviewed, pinned to commit SHAs,
  `claude plugin validate` required).
- **The 2026 structural shift**: SKILL.md is now the cross-vendor **Agent
  Skills open standard** (<https://agentskills.io/specification>, repo
  `agentskills/agentskills`, validator `skills-ref validate`), adopted by ~40
  clients (Cursor, Copilot/VS Code, Gemini CLI, Codex, OpenCode, Goose, Amp,
  Roo Code…). Spec adds optional `license`, `compatibility`, `metadata` (string
  map for author/version), `allowed-tools` (experimental). There is **no
  official central public registry**; third-party indexes (skillsmp.com,
  claiming ~1.6M scraped SKILL.md files — unverified) impose no metadata beyond
  the standard frontmatter. This decentralized, registry-less state is the gap
  Grimoire targets.

### Verification notes

- Spot-verified verbatim against raw files: skill-creator's 500-line rule +
  "pushy" guidance + 3-level loading; superpowers' CSO section, one-vs-two
  reviews anecdote, @-link warning, word targets; webapp-testing's
  `--help`-first black-box doctrine. All confirmed.
- Star counts (anthropics/skills ~149.6k; obra/superpowers ~225k) were returned
  consistently by GitHub API across multiple independent agent runs but not
  cross-checked against the web UI; treat magnitudes as indicative.
- Marked unverified throughout: skillsmp.com volume claims, Chat2AnyLLM's
  10,149-skill count, ComposioHQ's "1000+" claim, secondary blog coverage seen
  only via search snippets.

---

## Patterns to Adopt / Patterns to Avoid

### Adopt

1. **SKILL.md ≤500 lines, ideally far less**; one level of reference hierarchy;
   every reference file linked with explicit when-to-read guidance; TOC for
   references >300 lines.
2. **Frontmatter discipline**: `name` (kebab-case, = dir name, gerund
   verb-first) + `description` only, plus `license` when publishing. Put
   version/author in plugin/marketplace metadata, not skill frontmatter.
3. **Description formula**: third person, capability + "Use when…" + concrete
   trigger keywords (synonyms, file extensions, command names, error strings) +
   negative scope ("Do NOT use for…"). Pushy beats modest; triggering
   conditions beat workflow summaries (CSO rule — never summarize the process).
4. **For CLI-teaching skills specifically** (where the field is weak — chance to
   set the norm): task recipes over exhaustive flag tables; a verify-version
   protocol ("run `tool --version`; these instructions assume ≥X.Y; on mismatch
   prefer `tool <cmd> --help` over documented flags") — *nobody does this yet
   and the staleness failure mode is silent*; frontmatter
   `last_updated`/`doc_source` (EKS pattern); deprecation rows in anti-pattern
   tables; symptom-first troubleshooting with diagnostic command blocks.
5. **Bundle scripts for deterministic ops** and mark them black-box ("run with
   `--help` first, don't read the source"). Apply the 3-run test: if agents
   keep writing the same helper, ship it.
6. **Test skills like code**: baseline runs without the skill, pressure
   scenarios capturing rationalizations verbatim, trigger-assertion tests in
   CI (superpowers model); evals before documentation (Anthropic model).
7. **Cross-link skills by qualified name** ("Use pack:skill-name"), never
   force-loading file links.
8. **Honest lifecycle signals**: semver + release notes; if a pack is done,
   README points to the successor.

### Avoid

1. Workflow summaries in descriptions (agents follow the summary and skip the
   body), missing when-clauses, over-broad triggers, vague one-liners.
2. Hardcoded version pins inside command examples without a re-resolve
   instruction (EKS's `--addon-version v1.11.1-eksbuild.4` rot pattern), and
   dated prose status sections as the *only* drift defense.
3. Exhaustive flag-table cheatsheets that duplicate `--help` output — pure
   token tax that drifts.
4. Template-stamped bodies at scale, persona flattery, fake glob scoping
   (`globs: **/*`).
5. Per-skill READMEs (nobody ships them), `commands/`+`agents/` duplication of
   what skills can express (superpowers deleted both), pinned `model:` names
   in agent frontmatter (use `inherit`).
6. Silent abandonment; shipping without license metadata.

---

## Canonical Further-Reading Links (annotated)

- <https://platform.claude.com/docs/en/agents-and-tools/agent-skills/best-practices>
  — the official authoring bible: evals-first, third-person descriptions,
  "context window is a public good", degrees-of-freedom matching, multi-model
  testing checklist.
- <https://platform.claude.com/docs/en/agents-and-tools/agent-skills/overview> —
  the 3-level loading model with token budgets; name/description constraints.
- <https://agentskills.io/specification> — the cross-vendor SKILL.md standard;
  the durable home of format rules (anthropics/skills `spec/` now just points
  here). Validator: `skills-ref validate`.
- <https://raw.githubusercontent.com/anthropics/skills/main/skills/skill-creator/SKILL.md>
  — canonical skill anatomy + eval harness; the 500-line rule and "pushy"
  description guidance live here.
- <https://raw.githubusercontent.com/obra/superpowers/main/skills/writing-skills/SKILL.md>
  — CSO doctrine, TDD-for-skills, cross-linking rules; the sharpest community
  thinking on discoverability.
- <https://blog.fsck.com/2025/10/16/skills-for-claude/> and
  <https://blog.fsck.com/2025/12/18/superpowers-4/> — empirical origin of
  "Use when"-only descriptions and the observation that Claude "wings it" when
  descriptions reveal too much.
- <https://code.claude.com/docs/en/plugins> and
  <https://code.claude.com/docs/en/plugin-marketplaces> — plugin/marketplace
  manifest schemas; how packs are actually distributed.
- <https://www.anthropic.com/engineering/equipping-agents-for-the-real-world-with-agent-skills>
  — Anthropic engineering rationale for progressive disclosure.
- <https://joost.blog/self-updating-agent-skills/> — the skill-staleness
  problem statement ("cached documentation") and a hook-based update fix.

## Durable Search Terms

- `agent skills specification site:agentskills.io`
- `skill authoring best practices site:platform.claude.com`
- `"SKILL.md" filename code search github` (GitHub code search: `path:**/SKILL.md <tool>`)
- `Claude Search Optimization skill description "use when"`
- `claude code plugin marketplace.json schema`
- `awesome-claude-skills` / `awesome-claude-code` (lists churn; search fresh)
- `superpowers obra release notes` / `blog.fsck.com superpowers`
- `self-updating agent skills stale drift`
- `skills-ref validate agentskills`
- `claude-plugins-community submission requirements`

## Sources

Primary (files examined raw):

- https://github.com/anthropics/skills — all 17 SKILL.md frontmatters + bodies
  measured; close reads: skill-creator, mcp-builder, docx, webapp-testing, pdf;
  `.claude-plugin/marketplace.json`, `template/SKILL.md`
- https://github.com/obra/superpowers — full tree (147 blobs); close reads:
  writing-skills, test-driven-development, systematic-debugging, brainstorming,
  writing-plans, subagent-driven-development, requesting-code-review,
  using-superpowers; hooks/, tests/, plugin manifests, RELEASE-NOTES.md
- CLI skills: helmfile/helmfile, Melvynx/cli-skills, rendi-api/ffmpeg-cheatsheet,
  itsmostafa/aws-agent-skills, Dimillian/Skills, tldraw/tldraw (skills/pr),
  mauromedda/agent-toolkit
- Agent/rule packs: wshobson/agents, VoltAgent/awesome-claude-code-subagents,
  iannuttall/claude-agents, PatrickJS/awesome-cursorrules, steipete/agent-rules
  → steipete/agent-scripts, trailofbits/skills
- Awesome lists: hesreallyhim/awesome-claude-code, ComposioHQ/, travisvn/,
  BehiSecc/, Chat2AnyLLM/, karanb192/awesome-claude-skills

Official docs:

- https://platform.claude.com/docs/en/agents-and-tools/agent-skills/overview
- https://platform.claude.com/docs/en/agents-and-tools/agent-skills/best-practices
- https://code.claude.com/docs/en/skills, /plugins, /plugin-marketplaces
- https://agentskills.io/specification
- https://www.anthropic.com/engineering/equipping-agents-for-the-real-world-with-agent-skills

Secondary:

- https://blog.fsck.com/2025/10/09/superpowers/, /2025/10/16/skills-for-claude/,
  /2025/12/18/superpowers-4/, /2026/03/09/superpowers-5/
- https://joost.blog/self-updating-agent-skills/
- https://github.com/anthropics/claude-plugins-community
- skillsmp.com and marketplace-comparison coverage (volume claims unverified)
