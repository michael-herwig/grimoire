# Research: Anthropic Agent Skills — Authoring Best Practices & Design Considerations

**Date:** 2026-06-12 · **Scope:** Official Anthropic docs (platform.claude.com, code.claude.com), engineering blog, agentskills.io open standard, anthropics/skills repo, "Complete Guide to Building Skills for Claude" PDF (Jan 2026).

---

## Executive Summary

- A skill is a directory whose entrypoint is `SKILL.md` (YAML frontmatter + Markdown body). The open standard (agentskills.io) requires `name` + `description`; optional `license`, `compatibility`, `metadata`, `allowed-tools`. Claude Code layers ~13 extra frontmatter fields on top (`when_to_use`, `disable-model-invocation`, `context: fork`, `paths`, `hooks`, `model`, `effort`, …).
- Hard limits: `name` ≤ 64 chars (lowercase/digits/hyphens, must match directory name); `description` ≤ 1024 chars, non-empty, no XML tags; keep `SKILL.md` body **under 500 lines / <5k tokens**; metadata costs ~100 tokens per skill at startup.
- Descriptions are the *only* always-loaded content and drive triggering. Official formula: **what the skill does + when to use it**, third person, with concrete trigger keywords ("Use when working with PDF files or when the user mentions PDFs, forms, …"). Anthropic's skill-creator advises making descriptions "a little bit pushy" to counter undertriggering.
- Progressive disclosure has three levels: metadata (always) → SKILL.md body (on trigger) → bundled files (on demand, "effectively unlimited"). SKILL.md should act as a table of contents; reference files **one level deep only**, each with its own TOC if >100 lines.
- Conventional subdirectories from the spec: `scripts/` (executed, never loaded into context), `references/` (docs read on demand), `assets/` (templates/fonts/icons used in output).
- Anthropic's eval guidance: **build evaluations before writing documentation**, ≥3 scenarios, baseline without the skill, iterate via a "Claude A authors / Claude B tests" loop, and test on every model tier you target.
- 2025–2026 changes older community lore misses: commands merged into skills, the open standard + `skills-ref` validator, dynamic context injection (`` !`cmd` ``), skill-listing character budgets (1% of context window, 1,536-char per-entry cap), compaction re-attachment budgets (5k per skill / 25k total), `paths`-scoped activation, and forked-subagent execution.

---

## Detailed Findings

### 1. Canonical skill anatomy

**Minimum unit.** A skill is a directory containing `SKILL.md`: YAML frontmatter between `---` markers, then Markdown instructions. The directory name is the skill's identity — the spec requires `name` to **match the parent directory name**, and in Claude Code the directory name becomes the `/command` you type. (https://agentskills.io/specification, https://code.claude.com/docs/en/skills)

**Frontmatter — open standard (agentskills.io/specification):**

| Field | Required | Constraints |
|---|---|---|
| `name` | Yes | 1–64 chars; lowercase `a-z`, `0-9`, hyphens only; no leading/trailing/consecutive hyphens; must match directory name |
| `description` | Yes | 1–1024 chars, non-empty; what it does + when to use it |
| `license` | No | License name or pointer to bundled license file |
| `compatibility` | No | ≤500 chars; environment requirements (product, packages, network). "Most skills do not need" it |
| `metadata` | No | Arbitrary string→string map (author, version, …) |
| `allowed-tools` | No | Space-separated pre-approved tools; **experimental**, support varies by implementation |

The Claude platform adds two validation rules on top: no XML tags in `name`/`description`, and `name` cannot contain the reserved words "anthropic" or "claude". (https://platform.claude.com/docs/en/agents-and-tools/agent-skills/overview#skill-structure)

**Frontmatter — Claude Code extensions** (all optional; `description` "recommended", defaults to first paragraph of body if omitted): `name` (display label only — command name comes from the directory), `description`, `when_to_use` (extra trigger context appended to description), `argument-hint`, `arguments` (named positional args), `disable-model-invocation` (manual-only `/command`), `user-invocable: false` (Claude-only background knowledge), `allowed-tools` (pre-approves, does **not** restrict), `disallowed-tools`, `model`, `effort`, `context: fork` (+ `agent` to pick the subagent type), `hooks` (skill-scoped lifecycle hooks), `paths` (glob-scoped activation), `shell`. (https://code.claude.com/docs/en/skills#frontmatter-reference)

**Size limits.** Three-level token economics, stated identically in the overview and the spec (https://platform.claude.com/docs/en/agents-and-tools/agent-skills/overview#how-skills-work):

| Level | When loaded | Budget |
|---|---|---|
| 1: Metadata (`name` + `description`) | Always, at startup | ~100 tokens per skill |
| 2: SKILL.md body | When triggered | < 5k tokens; "Keep SKILL.md under 500 lines" |
| 3: Bundled resources/scripts | As needed | "Effectively unlimited" — zero cost until read; scripts never enter context, only their output |

**Directory conventions.** Spec-blessed optional subdirectories (https://agentskills.io/specification#optional-directories):
- `scripts/` — executable code for deterministic/repetitive operations; self-contained or with documented dependencies, helpful error messages.
- `references/` — documentation loaded on demand (`REFERENCE.md`, `FORMS.md`, domain files like `finance.md`); keep each file focused.
- `assets/` — static resources used in *output*: templates, icons, fonts, schemas, lookup tables.

The anthropics/skills `skill-creator` adds: name files descriptively (`form_validation_rules.md`, not `doc2.md`); organize `references/` by domain/variant (`aws.md`, `gcp.md`, `azure.md`); **no README.md** — `SKILL.md` is the sole entrypoint (skill-creator + "Complete Guide" PDF, Ch. 2). (https://github.com/anthropics/skills)

**Where skills live (Claude Code).** Enterprise (managed settings) > Personal `~/.claude/skills/` > Project `.claude/skills/` (precedence on name collision); plugins use a `plugin:skill` namespace and cannot collide. Project skills are also discovered from parent directories up to the repo root and from nested `.claude/skills/` (monorepos), and from `--add-dir` directories. (https://code.claude.com/docs/en/skills#where-skills-live)

**Validation/packaging.** `skills-ref validate ./my-skill` (reference library from the agentskills GitHub org) checks frontmatter and naming; anthropics/skills ships a `package_skill` script producing a `.skill` archive. (https://agentskills.io/specification#validation, https://github.com/anthropics/skills)

### 2. Description craft

The description is the trigger surface: "Claude uses it to choose the right Skill from potentially 100+ available Skills." All official guidance from https://platform.claude.com/docs/en/agents-and-tools/agent-skills/best-practices#writing-effective-descriptions unless noted:

- **Formula: what + when.** "Include both what the Skill does and when Claude should use it." Canonical example: `Extract text and tables from PDF files, fill forms, merge documents. Use when working with PDF files or when the user mentions PDFs, forms, or document extraction.` — the "Use when…" sentence carrying explicit user-utterance keywords is the official pattern.
- **Third person, always.** "The description is injected into the system prompt, and inconsistent point-of-view can cause discovery problems." Good: "Processes Excel files and generates reports." Avoid: "I can help you…" / "You can use this to…".
- **Specific keywords, no vagueness.** Include terms users would actually say (file extensions, product names, verbs). Named bad examples: "Helps with documents", "Processes data", "Does stuff with files".
- **Length:** ≤1024 chars (validation limit). In Claude Code, the combined `description` + `when_to_use` text is truncated at **1,536 characters** in the skill listing — "put the key use case first." (https://code.claude.com/docs/en/skills#frontmatter-reference)
- **What loads always vs on demand:** only `name` + `description` are pre-loaded into the system prompt; the body loads on trigger. So "when to use" information belongs in the description, not the body — the body is read *after* the trigger decision (skill-creator guidance, https://github.com/anthropics/skills).
- **Tune for trigger rate.** skill-creator: make descriptions "a little bit 'pushy'" if undertriggering. The Jan-2026 PDF guide adds diagnostics: undertriggering → add keywords/technical synonyms; overtriggering → narrow scope, add *negative triggers* ("not for X — use the Y skill instead"), or set `disable-model-invocation: true`. (Complete Guide PDF, Ch. 3 & 5)
- **Claude Code listing budget (2026).** Skill descriptions share a character budget of 1% of the model's context window; on overflow, least-invoked skills lose their descriptions first. `/doctor` reports overflow; `skillListingBudgetFraction`, `SLASH_COMMAND_TOOL_CHAR_BUDGET`, `maxSkillDescriptionChars`, and `skillOverrides: "name-only"` tune it. Practical consequence: front-load discriminating keywords in the first sentence. (https://code.claude.com/docs/en/skills#skill-descriptions-are-cut-short)
- The engineering blog frames metadata as "the first level of progressive disclosure: just enough information for Claude to know when each skill should be used" and tells authors to iterate on name/description based on observed trigger behavior. (https://www.anthropic.com/engineering/equipping-agents-for-the-real-world-with-agent-skills)

### 3. Progressive disclosure

Official mental model: "Like a well-organized manual that starts with a table of contents, then specific chapters, and finally a detailed appendix." (https://www.anthropic.com/engineering/equipping-agents-for-the-real-world-with-agent-skills)

- **Root file as index.** "SKILL.md serves as an overview that points Claude to detailed materials as needed, like a table of contents in an onboarding guide." (https://platform.claude.com/docs/en/agents-and-tools/agent-skills/best-practices#progressive-disclosure-patterns)
- **When to split:** keep the body under 500 lines; "split content into separate files when approaching this limit." skill-creator phrases it as "add an additional layer of hierarchy" with clear pointers.
- **Three named patterns** (best-practices page): (1) *High-level guide with references* — quick start inline, `**Form filling**: See [FORMS.md](FORMS.md)`; (2) *Domain-specific organization* — `reference/finance.md`, `reference/sales.md` so a sales question never loads finance schemas; can include `grep` hints for searching reference files; (3) *Conditional details* — basic path inline, "**For tracked changes**: See [REDLINING.md](REDLINING.md)".
- **How to instruct on-demand loading:** reference each supporting file from SKILL.md *with what it contains and when to load it* ("For complete API details, see reference.md"). Make execution intent explicit: "Run `analyze_form.py` to extract fields" (execute) vs "See `analyze_form.py` for the algorithm" (read). (https://code.claude.com/docs/en/skills#add-supporting-files, best-practices page)
- **One level deep, no chains.** "Keep references one level deep from SKILL.md." With nested chains Claude "might use commands like `head -100` to preview content," getting incomplete information. Reference files >100 lines need a table of contents at the top so partial reads still reveal scope (skill-creator says >300 lines; the stricter docs figure is 100).
- **Relative paths from skill root, forward slashes only**; `${CLAUDE_SKILL_DIR}` resolves bundled script paths in Claude Code regardless of install level.
- **Lifecycle caveat (Claude Code):** once invoked, skill content enters the conversation and is *not* re-read on later turns — "write guidance that should apply throughout a task as standing instructions rather than one-time steps." After auto-compaction, each skill's most recent invocation is re-attached keeping its first 5,000 tokens, under a combined 25,000-token budget (most-recent-first) — another reason to front-load critical rules. (https://code.claude.com/docs/en/skills#skill-content-lifecycle)

### 4. Design considerations

**When a skill vs other mechanisms:**
- *vs CLAUDE.md/memory:* "Create a skill when you keep pasting the same instructions, checklist, or multi-step procedure into chat, or when a section of CLAUDE.md has grown into a procedure rather than a fact." Skills cost nothing until used; CLAUDE.md is always loaded. (https://code.claude.com/docs/en/skills)
- *vs prompts:* "Unlike prompts (conversation-level instructions for one-off tasks), Skills load on-demand and eliminate the need to repeatedly provide the same guidance." (overview)
- *vs MCP:* complementary, not competing — the Jan-2026 PDF's kitchen analogy: "MCP provides the professional kitchen [connectivity, tools, data]; Skills provide the recipes [how to use them well]." MCP = what Claude *can* do, skills = *how* Claude should do it. Always use fully qualified `ServerName:tool_name` references inside skills. (Complete Guide PDF Ch. 1; best-practices #mcp-tool-references)
- *vs subagents (Claude Code):* `context: fork` + `agent:` runs the skill body as a subagent's task prompt (needs explicit instructions, not bare guidelines); inversely, subagents can preload skills as reference material. (https://code.claude.com/docs/en/skills#run-skills-in-a-subagent)
- *Invocation control:* `disable-model-invocation: true` for side-effectful workflows (deploy, commit — "You don't want Claude deciding to deploy because your code looks ready"); `user-invocable: false` for background knowledge that isn't a meaningful user action.
- *Degrees of freedom* (best-practices): match specificity to fragility — high freedom (heuristic text instructions) for context-dependent work like code review; medium (pseudocode/templates with parameters); low (exact scripts, "Do not modify the command") for fragile sequences like migrations. Analogy: open field vs narrow bridge with cliffs.

**Anti-patterns Anthropic names explicitly** (best-practices page unless noted):
- Verbose explanation of things Claude already knows — "Default assumption: Claude is already very smart"; "The context window is a public good"; challenge every paragraph's token cost.
- Vague descriptions; vague names (`helper`, `utils`, `tools`, `documents`); inconsistent naming patterns across a collection (prefer gerunds: `processing-pdfs`).
- First/second-person descriptions (discovery problems).
- Time-sensitive content ("before August 2025 use the old API") — use a collapsible "Old patterns" section instead.
- Inconsistent terminology (mixing "field"/"box"/"element").
- Too many options — provide one default with an escape hatch, not "pypdf, or pdfplumber, or PyMuPDF, or…".
- Deeply nested reference chains; Windows-style paths.
- Scripts that "punt" errors to Claude instead of handling them; magic "voodoo constants" without justification (Ousterhout's law: "If you don't know the right value, how will Claude determine it?").
- Assuming packages are installed — state install commands; verify availability per surface (API container has no network/runtime installs; Claude Code skills should install locally, not globally).
- Skill body content that surprises users relative to the stated description (skill-creator security stance).

**Testing & evaluation:**
- "**Create evaluations BEFORE writing extensive documentation.**" Evaluation-driven development: identify gaps by running Claude without the skill → build three scenarios → baseline → write minimal instructions to pass → iterate. Example eval record: `{"skills": [...], "query": ..., "files": [...], "expected_behavior": [...]}`. No built-in runner exists — "Evaluations are your source of truth." (best-practices #evaluation-and-iteration)
- **Claude A / Claude B loop:** author and refine with one Claude instance, test with a fresh instance, feed observed failures back ("Claude B forgot to filter test accounts — is the rule prominent enough?"). Claude understands the SKILL.md format natively; no special meta-prompt needed.
- **Observe navigation:** unexpected read order → unintuitive structure; never-read bundled file → unnecessary or badly signaled; repeatedly read file → promote into SKILL.md.
- **Test on every target model** (Haiku may need more guidance; Opus needs less); test triggering both ways — should-trigger phrasings *and* should-not-trigger neighbors (Complete Guide PDF Ch. 3).
- **Quality patterns to bake in:** workflow checklists Claude copies and ticks off; feedback loops (run validator → fix → repeat); plan-validate-execute with verifiable intermediate artifacts (e.g., a `changes.json` validated before applying batch edits) for destructive/batch/high-stakes operations; verbose validator error messages so Claude can self-correct.
- **Security:** treat skills like installed software; audit every bundled file; skills fetching external URLs are the highest-risk category; in Claude Code, project-skill `allowed-tools` only takes effect after workspace trust. (overview #security-considerations; code.claude.com #pre-approve-tools-for-a-skill)

### 5. New in 2025–2026 (what older community knowledge misses)

- **Commands merged into skills** (Claude Code): `.claude/commands/deploy.md` ≡ `.claude/skills/deploy/SKILL.md`; skills are the superset (supporting files, invocation control, auto-loading). (https://code.claude.com/docs/en/skills)
- **Agent Skills open standard** at agentskills.io (multi-tool portability) with new spec-level fields `license`, `compatibility`, `metadata`, plus the `skills-ref` validator and stricter name rules (no consecutive hyphens; name must match directory).
- **Claude Code frontmatter expansion:** `when_to_use`, `paths` (glob-scoped auto-activation), `context: fork` + `agent`, skill-scoped `hooks`, `model`/`effort` overrides, `disallowed-tools`, `arguments`/`argument-hint`, `shell`.
- **String substitutions:** `$ARGUMENTS`, `$ARGUMENTS[N]`/`$N`, named `$arg`, `${CLAUDE_SKILL_DIR}` (portable script paths), `${CLAUDE_SESSION_ID}`, `${CLAUDE_EFFORT}`.
- **Dynamic context injection:** `` !`command` `` and ```` ```! ```` blocks execute *before* Claude sees the content (preprocessing, output inlined); policy off-switch `disableSkillShellExecution`.
- **Skill-listing budget mechanics** (v2.1.129, May 2026): 1% of context window for descriptions, least-invoked dropped first, per-entry 1,536-char cap, `/doctor` diagnostics, `skillOverrides` four-state visibility (`on`/`name-only`/`user-invocable-only`/`off`).
- **Compaction re-attachment:** first 5k tokens per invoked skill, 25k combined budget — large skills can silently degrade after compaction; re-invoke to restore.
- **Live change detection** (edits to SKILL.md apply mid-session) and monorepo discovery (parent + nested `.claude/skills/`, `--add-dir` exception).
- **Bundled meta-skills:** `/run`, `/verify`, `/run-skill-generator` (records a project's launch recipe as a generated skill — skills authored by agents in practice), matching the engineering blog's stated direction: "we hope to enable agents to create, edit, and evaluate Skills on their own."
- **Distribution reality (Jan 2026 PDF):** no cross-surface sync (claude.ai uploads ≠ API uploads ≠ Claude Code filesystem); claude.ai has no centralized org management; API skills are workspace-wide via `/v1/skills`; Claude Code shares via git/plugins/managed settings; surface-selection matrix (interactive → claude.ai/Code; programmatic/scale → API).

---

## Canonical Further-Reading Links

- https://agentskills.io/specification — the open-standard format spec: exact frontmatter constraints, `scripts/`/`references/`/`assets/` conventions, validation tooling.
- https://platform.claude.com/docs/en/agents-and-tools/agent-skills/best-practices — Anthropic's authoring bible: descriptions, degrees of freedom, disclosure patterns, anti-patterns, eval-first workflow, author checklist.
- https://platform.claude.com/docs/en/agents-and-tools/agent-skills/overview — conceptual model: three loading levels, token costs, runtime architecture, security, surface constraints.
- https://code.claude.com/docs/en/skills — Claude Code reference: full frontmatter table, invocation control, content lifecycle/compaction, listing budgets, subagent/fork patterns, dynamic context injection.
- https://www.anthropic.com/engineering/equipping-agents-for-the-real-world-with-agent-skills — the launch engineering post (Oct 2025): design rationale, scripts-as-tools-and-docs, future of agent-authored skills.
- https://github.com/anthropics/skills — official examples + `skill-creator` meta-skill + template + packaging script; installable via `/plugin marketplace add anthropics/skills`.
- https://resources.anthropic.com/hubfs/The-Complete-Guide-to-Building-Skill-for-Claude.pdf — Jan-2026 long-form guide: MCP-vs-skills kitchen analogy, trigger diagnostics, testing matrix, distribution model.
- https://www.anthropic.com/engineering/effective-context-engineering-for-ai-agents — sibling post: why progressive disclosure / context-as-public-good underpins skill design.
- https://www.anthropic.com/engineering/writing-tools-for-agents — companion guidance for the tool layer skills orchestrate; description-prompting transfers directly.

## Durable Search Terms

- `anthropic agent skills authoring best practices SKILL.md`
- `agentskills.io specification frontmatter`
- `site:code.claude.com skills frontmatter reference`
- `anthropic engineering "agent skills"`
- `anthropics/skills skill-creator github`
- `claude skill description triggering undertriggering overtriggering`
- `claude skills progressive disclosure 500 lines`
- `claude code skill listing budget skillOverrides /doctor`
- `agent skills evaluation-driven development baseline`

## Sources

| Source | URL | Accessed |
|---|---|---|
| Claude Code: Extend Claude with skills | https://code.claude.com/docs/en/skills | 2026-06-12 |
| Agent Skills overview (platform docs) | https://platform.claude.com/docs/en/agents-and-tools/agent-skills/overview | 2026-06-12 |
| Skill authoring best practices (platform docs) | https://platform.claude.com/docs/en/agents-and-tools/agent-skills/best-practices | 2026-06-12 |
| Engineering blog: Equipping agents for the real world with Agent Skills (2025-10-16) | https://www.anthropic.com/engineering/equipping-agents-for-the-real-world-with-agent-skills | 2026-06-12 |
| Agent Skills open standard — Specification | https://agentskills.io/specification | 2026-06-12 |
| anthropics/skills repo (incl. skill-creator) | https://github.com/anthropics/skills | 2026-06-12 |
| The Complete Guide to Building Skills for Claude (PDF, dated "January 2026" internally) | https://resources.anthropic.com/hubfs/The-Complete-Guide-to-Building-Skill-for-Claude.pdf | 2026-06-12 |
| Anthropic engineering index (post inventory) | https://www.anthropic.com/engineering | 2026-06-12 |
| Web search corroboration: skill budget v2.1.129 behavior | https://claudefa.st/blog/guide/mechanics/skill-listing-budget | 2026-06-12 |
| Web search corroboration: triggering failures community analysis | https://dev.to/lizechengnet/why-claude-code-skills-dont-trigger-and-how-to-fix-them-in-2026-o7h | 2026-06-12 |
