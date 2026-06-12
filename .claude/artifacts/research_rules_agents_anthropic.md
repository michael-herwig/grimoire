# Research: Claude Code Rules/Memory (CLAUDE.md, .claude/rules) and Subagents — Authoring Best Practices

- **Date**: 2026-06-12
- **Method**: deep-research workflow — 5 parallel search agents (official docs, subagents/hooks docs, Anthropic engineering posts, community practice, 2025–2026 changelog delta), followed by direct primary-source verification of contested claims against `code.claude.com/docs` and the official changelog.
- **Claude Code version at research time**: v2.1.175 (changelog head).
- **Confidence labels**: [verified] = fetched primary source directly during this research; [high] = verbatim-quoted primary source via research agent; [medium] = credible secondary/practitioner claim, not independently reproduced.

---

## Executive Summary

1. **`.claude/rules/` is now an official, documented feature.** Project rules live in `.claude/rules/*.md` (discovered recursively, symlinks supported); rules *without* `paths:` frontmatter load at launch with the same priority as `.claude/CLAUDE.md`; rules *with* a `paths:` glob list load only when Claude **reads** a matching file — explicitly "not on every tool use". User-level rules in `~/.claude/rules/` load before project rules. Grimoire's catalog-plus-path-scoped-rules architecture matches the official mechanism. [verified]
2. **Official size budget exists and is concrete: target under 200 lines per CLAUDE.md file.** Longer files "consume more context and reduce adherence". `@`-imports are organizational only — imported files still load at launch and do **not** reduce context; path-scoped rules and skills are the sanctioned mechanisms for actually reducing always-loaded context. Import recursion depth is **4 hops** (older lore said 5). [verified]
3. **CLAUDE.md is advisory, not enforced** — it is delivered as a *user message after the system prompt*, and the docs say so explicitly. The official boundary: hooks for anything that "must happen every time" (deterministic), CLAUDE.md/rules for always-relevant facts and conventions, skills for on-demand procedures (body loads only when invoked). [verified]
4. **Subagents = context isolation + tool restriction + cheaper models.** Only `name` and `description` are required frontmatter; `tools` inherits everything when omitted (so minimize deliberately); `model` defaults to `inherit` with aliases `sonnet`/`opus`/`haiku`/`fable`. Delegation is driven by the `description` field — "use proactively" phrasing is officially recommended. Anthropic's orchestration guidance: delegate with explicit objective/output-format/tool-guidance/boundaries; don't multi-agent tasks with tight inter-step dependencies (most coding tasks). [verified]
5. **2025–2026 changed the lore substantially**: rules dir shipped ~v2.0.64 (Dec 2025), auto memory shipped v2.1.59 (Feb 2026), skills absorbed custom commands, the `#` memory shortcut is gone from docs, CLAUDE.local.md is *not* deprecated, and — bleeding edge — changelog v2.1.172 enables nested subagent spawning (5 levels) while the docs page still says subagents cannot spawn subagents.

---

## Detailed Findings

### 1. Rules & memory: structure, hierarchy, imports, path scoping

#### Hierarchy and load order [verified]

Source: https://code.claude.com/docs/en/memory

| Scope | Location | Notes |
|---|---|---|
| Managed policy | `/Library/Application Support/ClaudeCode/CLAUDE.md` (macOS), `/etc/claude-code/CLAUDE.md` (Linux/WSL), `C:\Program Files\ClaudeCode\CLAUDE.md` (Windows) | Cannot be excluded by users; `claudeMd` key in `managed-settings.json` is an alternative |
| User | `~/.claude/CLAUDE.md` | All projects |
| Project | `./CLAUDE.md` **or** `./.claude/CLAUDE.md` | Team-shared via VCS |
| Local | `./CLAUDE.local.md` | Personal, gitignore it; **not deprecated** (current docs list it as a first-class scope) |

- Load order is broadest → most specific: "a project instruction appears in context after a user instruction." Files **concatenate**; nothing overrides.
- Directory walk: ancestors of cwd load in full at launch (root-down ordering, closest-to-cwd last); **subdirectory** CLAUDE.md files load lazily "when Claude reads files in those subdirectories" — a native progressive-disclosure mechanism for monorepos.
- `CLAUDE.local.md` appends after `CLAUDE.md` within each directory.
- `claudeMdExcludes` (any settings layer, glob over absolute paths) skips irrelevant monorepo files; managed CLAUDE.md is exempt.
- AGENTS.md: Claude Code reads `CLAUDE.md`, not `AGENTS.md`; official pattern is `@AGENTS.md` import or symlink.
- Delivery detail: "CLAUDE.md content is delivered as a user message after the system prompt, not as part of the system prompt itself... there's no guarantee of strict compliance." For true system-prompt placement use `--append-system-prompt`.
- Project-root CLAUDE.md survives `/compact` (re-read and re-injected); nested CLAUDE.md files are not re-injected until next matching read.

#### Size and writing guidance [verified]

Source: https://code.claude.com/docs/en/memory and https://code.claude.com/docs/en/best-practices

- **"Size: target under 200 lines per CLAUDE.md file. Longer files consume more context and reduce adherence."** Overflow remedies, in order: path-scoped rules, then `@`-imports for organization only.
- Structure with headers/bullets ("organized sections are easier to follow than dense paragraphs"); specificity that is *verifiable* ("Use 2-space indentation" not "Format code properly"); consistency ("if two rules contradict each other, Claude may pick one arbitrarily").
- When to add: same mistake twice, review catches something Claude should have known, you re-type a correction across sessions. Multi-step procedures or area-specific content belong in a skill or path-scoped rule instead.
- Best-practices doc include/exclude table — include: commands Claude can't guess, style rules differing from defaults, repo etiquette, env quirks, gotchas; exclude: anything readable from code, standard conventions, detailed API docs ("link to docs instead"), frequently changing info, file-by-file codebase descriptions. (https://code.claude.com/docs/en/best-practices) [high]
- "Keep it concise. For each line, ask: 'Would removing this cause Claude to make mistakes?' If not, cut it. Bloated CLAUDE.md files cause Claude to ignore your actual instructions!" (best-practices) [high]
- Tuning: "Treat CLAUDE.md like code: review it when things go wrong, prune it regularly, and test changes by observing whether Claude's behavior actually shifts"; emphasis tokens ("IMPORTANT", "YOU MUST") improve adherence. [high]
- Block-level HTML comments (`<!-- -->`) are **stripped** before injection — free maintainer notes; preserved inside code blocks. [verified]

#### Import syntax [verified]

Source: https://code.claude.com/docs/en/memory

- `@path/to/file` anywhere in a CLAUDE.md; relative paths resolve relative to the *containing file*; absolute and `~/` paths allowed (`@~/.claude/my-project-instructions.md` is the documented per-user-cross-worktree pattern).
- **Max recursion depth: four hops** (older community material said five — current docs say "a maximum depth of four hops").
- External imports trigger a one-time approval dialog; declining disables them permanently for that project.
- **Imports do not save context**: "Splitting into `@path` imports helps organization but does not reduce context, since imported files load at launch."

#### `.claude/rules/` mechanics [verified]

Source: https://code.claude.com/docs/en/memory ("Organize rules with .claude/rules/")

- Project rules: `.claude/rules/*.md`, one topic per file, discovered **recursively** (subdirectories like `frontend/` fine); **symlinks supported** (dirs or files; circular symlinks handled).
- No `paths:` frontmatter → loaded at launch, "same priority as `.claude/CLAUDE.md`", applies to all files.
- With `paths:` frontmatter (YAML list of globs; brace expansion like `src/**/*.{ts,tsx}` supported) → "Path-scoped rules trigger when Claude **reads** files matching the pattern, not on every tool use."
- User-level rules: `~/.claude/rules/`, loaded before project rules ("giving project rules higher priority").
- Official rules-vs-skills boundary: "Rules load into context every session or when matching files are opened. For task-specific instructions that don't need to be in context all the time, use skills instead."
- `InstructionsLoaded` hook (v2.1.69+) logs exactly which instruction files loaded and why — the official debugging tool for path-scoped rules. [verified]

**Known sharp edges** (community-confirmed bugs, relevant to Grimoire's heavy rules usage):
- Path-scoped rules are injected on matching **Read**, but *not* when Claude **Writes/creates** a matching file — GitHub issue #23478, closed "not planned" (Feb 2026). https://github.com/anthropics/claude-code/issues/23478 [high]
- `paths:` frontmatter in **user-level** `~/.claude/rules/` was reported silently ignored — GitHub issue #21858, open as of early 2026. https://github.com/anthropics/claude-code/issues/21858 [high]
- Useful practitioner framing: "Scoped rules are deterministic in application. CLAUDE.md is deterministic in expectation." https://joseparreogarcia.substack.com/p/how-claude-code-rules-actually-work [medium]

### 2. Subagents: schema, delegation, tool/model selection, orchestration

#### File format and frontmatter [verified]

Source: https://code.claude.com/docs/en/sub-agents

- Locations and precedence (highest first): managed settings → `--agents` CLI flag → `.claude/agents/` (project) → `~/.claude/agents/` (user) → plugin `agents/` dir. Scanned recursively; identity comes from `name`, not filename; duplicate names within a scope: one silently wins.
- **Only `name` and `description` are required.** Full field set: `name`, `description`, `tools`, `disallowedTools`, `model`, `permissionMode`, `maxTurns`, `skills`, `mcpServers`, `hooks`, `memory`, `background`, `effort`, `isolation`, `color`, `initialPrompt`. Markdown body = the subagent's system prompt (it does *not* get the full Claude Code system prompt).
- `tools`: **inherits all tools if omitted** — tool minimization must be explicit. `disallowedTools` is applied first, then `tools` resolves against the remainder. Session-state tools (`Agent`, `AskUserQuestion`, plan-mode tools, etc.) are never available to subagents.
- `model`: `sonnet` | `opus` | `haiku` | `fable` | full model ID | `inherit`; **defaults to `inherit`**. Resolution: `CLAUDE_CODE_SUBAGENT_MODEL` env > per-invocation parameter > frontmatter > main model. Official cost guidance: "Control costs by routing tasks to faster, cheaper models like Haiku" (the built-in Explore agent runs Haiku).
- `memory: user|project|local` gives a subagent persistent cross-session memory; `isolation: worktree` gives an isolated git worktree (auto-cleaned if unchanged); `permissionMode` is overridden when parent uses `bypassPermissions`/`acceptEdits`/auto. Plugin-shipped subagents ignore `hooks`, `mcpServers`, `permissionMode` for security.
- Edits on disk require session restart; `/agents` UI changes apply immediately.

#### Delegation [verified]

- "Claude uses each subagent's description to decide when to delegate tasks... To encourage proactive delegation, include phrases like 'use proactively' in your subagent's description field." Official examples: "Use immediately after writing or modifying code", "Use proactively when encountering any issues."
- Explicit invocation escalation: natural language naming → `@agent-<name>` mention (guarantees the agent, Claude still writes the task prompt) → `--agent <name>` (replaces the main thread's system prompt/tools/model entirely).
- Subagents start with a **fresh, isolated context**: no conversation history, no already-read files; they receive their system prompt, a delegation message Claude composes, the CLAUDE.md/memory hierarchy (except Explore/Plan), git status, and any preloaded `skills`. Forks (`/fork`, v2.1.117+) are the exception — they inherit parent context and reuse its prompt cache.
- Official "use the main conversation when": latency matters ("Subagents start fresh and may need time to gather context") and when the task needs shared context.

#### Orchestration patterns and anti-patterns

- Orchestrator-worker is Anthropic's canonical multi-agent pattern; token budget is the dominant performance driver ("token usage by itself explains 80% of the variance"), and multi-agent ran ~**15× more tokens than chat** — use only where the value justifies it. https://www.anthropic.com/engineering/multi-agent-research-system [high]
- Delegation prompts need four elements: **objective, output format, tool guidance, task boundaries** — vague prompts caused duplicated work ("2 others duplicated work investigating current 2025 supply chains") and over-spawning ("spawning 50 subagents for simple queries"). [high]
- When NOT to multi-agent: "most coding tasks involve fewer truly parallelizable tasks than research"; domains with many inter-agent dependencies fit poorly. [high]
- Context-engineering framing: subagents exist for *context isolation* — each "returns only a condensed, distilled summary of its work (often 1,000–2,000 tokens)". https://www.anthropic.com/engineering/effective-context-engineering-for-ai-agents [high]
- Nesting: docs say "Subagents cannot spawn other subagents. If your workflow requires nested delegation, use Skills or chain subagents from the main conversation" [verified], **but** changelog v2.1.172 says "Sub-agents can now spawn their own sub-agents (up to 5 levels deep)" [verified] — a live docs/changelog divergence as of 2026-06-12; treat single-level as the conservative design assumption.
- Reviewer-agent caveat (official): "A reviewer prompted to find gaps will usually report some, even when the work is sound... Chasing every finding leads to over-engineering." https://code.claude.com/docs/en/best-practices [high]
- Practitioner counterweight (Armin Ronacher): parallel subagents mixing reads and writes "create chaos"; per-task clear instructions "outperform elaborate pre-written prompts"; sub-tool "dead ends" are fixed by a shared filesystem as common data store. https://lucumr.pocoo.org/2025/7/30/things-that-didnt-work/ , https://lucumr.pocoo.org/2025/11/21/agents-are-hard/ [medium–high]

### 3. Hooks vs rules vs skills boundary

- Official definition: hooks "provide deterministic control over Claude Code's behavior, ensuring certain actions always happen rather than relying on the LLM to choose to run them." https://code.claude.com/docs/en/hooks-guide [verified]
- The memory doc draws the line explicitly: CLAUDE.md/auto-memory are "context, not enforced configuration. To block an action regardless of what Claude decides, use a PreToolUse hook instead"; and "If the instruction is something that must run at a specific point, such as before every commit or after each file edit, write it as a hook instead." [verified]
- Inverse rule: "For static context that does not require a script, use CLAUDE.md instead." (hooks-guide) [high]
- Mechanics worth knowing: exit code **2** blocks (PreToolUse blocks the call; Stop prevents stopping); exit 1 is non-blocking; JSON output supports `permissionDecision`, `updatedInput`, `additionalContext`; matchers are exact/pipe lists or JS regex; five handler types (`command`, `http`, `mcp_tool`, `prompt`, `agent`); prompt/agent hooks are **probabilistic — don't use them for safety boundaries**; the `if` filter is best-effort, so hard allow/deny belongs in the permission system, not a hook. https://code.claude.com/docs/en/hooks [high]
- Skills boundary: skill name+description always loaded; body loads on invocation — "Unlike CLAUDE.md content, a skill's body loads only when it's used, so long reference material costs almost nothing until you need it." Skills run in main context by default; `context: fork` runs them in a subagent. `allowed-tools` *grants permission without prompting*, it does not restrict availability. https://code.claude.com/docs/en/skills [high]
- Decision heuristic synthesis: **rules/CLAUDE.md** = always-true facts and conventions (advisory); **path-scoped rules** = area-specific conventions (load on read of matching files); **skills** = on-demand procedures/workflows; **hooks** = anything with a 100%-compliance requirement (format-on-write, block dangerous commands, gate stop); **permissions/managed settings** = hard security boundaries.
- Community calibration: one practitioner measured ~70% compliance for a "never run rm -rf" CLAUDE.md instruction vs 100% with a PreToolUse hook (https://www.dotzlaw.com/insights/claude-hooks/) [medium]; "A system prompt is a request. A hook is a guarantee" and the always-blocking-Stop-hook infinite loop anti-pattern (https://hidekazu-konishi.com/entry/claude_code_hooks_complete_guide.html) [medium-high]; Ronacher found 2025-era hooks limited and used PATH interceptors instead [medium — predates later hook expansion].

### 4. Context engineering: keep always-loaded content small

- "Context... must be treated as a finite resource with diminishing marginal returns"; LLMs have an "attention budget" (n² pairwise attention). System prompts should sit at the "right altitude" — "a minimal set of information that fully outlines your expected behavior", structured with headings/XML. https://www.anthropic.com/engineering/effective-context-engineering-for-ai-agents [high]
- Just-in-time retrieval over pre-loading: "maintain lightweight identifiers (file paths, stored queries, web links...) and use these references to dynamically load data into context at runtime" — the official endorsement of **index/pointer patterns** like a rule catalog. [high]
- Progressive disclosure is the design principle behind skills: name+description pre-loaded (~tens of tokens each), SKILL.md on demand, bundled files only when needed. https://www.anthropic.com/engineering/equipping-agents-for-the-real-world-with-agent-skills [high]
- Official skill-authoring limits: SKILL.md body under ~500 lines; keep file references **one level deep** from SKILL.md — nested reference chains cause partial reads (`head -100`-style truncation). https://platform.claude.com/docs/en/agents-and-tools/agent-skills/best-practices [high]
- Native on-demand mechanisms ranked by cost: subdirectory CLAUDE.md (lazy), path-scoped rules (load on read-match), skills (load on invocation), subagents (separate window entirely). Imports are *not* on this list — they load eagerly. [verified]
- Compaction: customize via CLAUDE.md ("When compacting, always preserve the full list of modified files...") — official pattern from best-practices. [high]
- Community size calibration (consistent direction, varying numbers): HumanLayer — frontier models follow ~150–200 instructions, Claude Code's system prompt already uses ~50; recommend <300 lines, their own file <60 (https://www.humanlayer.dev/blog/writing-a-good-claude-md) [medium-high]; prefer `file:line` pointers over pasted code snippets (they go stale) [medium-high]; an index CLAUDE.md needs an activation nudge — "IMPORTANT: Before starting any task, identify which docs below are relevant and read them first" (https://alexop.dev/posts/stop-bloating-your-claude-md-progressive-disclosure-ai-coding-tools/) [medium]; Vercel evals reportedly saw skills never auto-invoked in 56% of cases, arguing for explicit pointers for must-read material [medium, secondary citation].

### 5. What changed 2025–2026 vs older community lore

| Old lore (2024 – mid-2025) | Current state (June 2026) | Source |
|---|---|---|
| "Rules directories are a Cursor thing; Claude Code only has CLAUDE.md" | `.claude/rules/` + `paths:` globs official, shipped ~v2.0.64 (Dec 2025) | https://code.claude.com/docs/en/memory [verified]; https://paddo.dev/blog/claude-rules-path-specific-native/ [medium] |
| Import depth "5 hops" | **4 hops** in current docs | memory doc [verified] |
| "Use `#` to quickly add memories" | `#` shortcut absent from current docs; replaced by `/memory`, asking Claude, or auto memory | memory doc [verified] |
| "CLAUDE.local.md is deprecated, use imports" | Not deprecated — first-class Local scope row; imports recommended only for cross-worktree sharing | memory doc [verified]; https://github.com/anthropics/claude-code/issues/2394 |
| "Split CLAUDE.md with @imports to save context" | Imports never saved context; docs now say so explicitly | memory doc [verified] |
| No official size number | "Target under 200 lines per CLAUDE.md file" | memory doc [verified] |
| Memory = only what you write | **Auto memory** (v2.1.59, Feb 2026): Claude-maintained `MEMORY.md` index (first 200 lines/25KB loaded) + on-demand topic files, per-repo at `~/.claude/projects/<project>/memory/` | memory doc [verified] |
| Slash commands ≠ skills | "Custom commands have been merged into skills"; `.claude/commands/` still works, skills preferred | https://code.claude.com/docs/en/skills [high] |
| Subagent frontmatter = name/description/tools/model | 16 fields incl. `memory`, `isolation: worktree`, `hooks`, `skills` preloading, `effort`, `initialPrompt`; hooks-in-frontmatter since 2.1.0 (Jan 2026) | sub-agents doc [verified] |
| "Subagents can never nest" | Changelog v2.1.172: nesting up to 5 levels; docs page not yet updated — in flux | changelog [verified] vs sub-agents doc [verified] |
| Hooks = 6 events (June 2025, v1.0.59) | Large event set incl. `InstructionsLoaded` (v2.1.69), `PostToolBatch`, `SubagentStart/Stop`, compact events; 5 handler types incl. prompt/agent hooks | https://code.claude.com/docs/en/hooks [high] |
| Docs at docs.anthropic.com | Claude Code docs canonical at **code.claude.com/docs**; engineering best-practices post folded into docs (anthropic.com/engineering/claude-code-best-practices redirects) | [verified by fetch behavior] |
| Skills are Claude-only | Agent Skills is an open standard (agentskills.io) since Dec 18, 2025 | skills doc [high]; https://claude.com/blog/skills |

Timeline anchor points: hooks ~v1.0.59 (Jun 2025); subagents `/agents` ~v1.0.60 (Jul 2025); Claude Code 2.0 + skills v2.0.22 (Oct 16, 2025); plugins beta (Oct 2025); rules v2.0.64 (Dec 11, 2025); 2.1.0 (Jan 7, 2026 — hooks in agent/skill frontmatter); auto memory v2.1.59 (Feb 26, 2026); nested subagents v2.1.172 (Jun 2026). Pre-Dec-2025 blog posts predate the rules dir entirely; pre-Oct-2025 posts predate skills.

---

## Canonical Further-Reading Links (annotated, embeddable)

Official docs (canonical, maintained):
- https://code.claude.com/docs/en/memory — **The** reference for CLAUDE.md, `.claude/rules/`, `paths:` scoping, imports, auto memory, `/memory`, `claudeMdExcludes`. Contains the under-200-lines guidance and the advisory-vs-enforced framing.
- https://code.claude.com/docs/en/sub-agents — Full subagent frontmatter schema, precedence, delegation mechanics, built-in agents, forks, worktree isolation, persistent subagent memory.
- https://code.claude.com/docs/en/hooks — Hooks reference: all events, matchers, exit-code and JSON-output semantics, handler types.
- https://code.claude.com/docs/en/hooks-guide — Hooks how-to; source of the "deterministic control... rather than relying on the LLM" boundary statement.
- https://code.claude.com/docs/en/skills — Skills: SKILL.md format, invocation control, `context: fork`, commands-merged-into-skills note.
- https://code.claude.com/docs/en/best-practices — Successor to the engineering best-practices post; include/exclude table, "would removing this cause mistakes?" test, subagent and compaction tips.
- https://platform.claude.com/docs/en/agents-and-tools/agent-skills/best-practices — Skill authoring: <500-line bodies, one-level-deep references, description writing.
- https://github.com/anthropics/claude-code/blob/main/CHANGELOG.md — Ground truth for feature timing; check before trusting any dated community claim.

Anthropic engineering/blog (conceptual foundations):
- https://www.anthropic.com/engineering/effective-context-engineering-for-ai-agents — Attention budget, right-altitude prompts, just-in-time retrieval, compaction, subagent isolation. The theory behind every "keep it small" rule.
- https://www.anthropic.com/engineering/multi-agent-research-system — Orchestrator-worker pattern, delegation prompt anatomy, 15× token cost, when multi-agent is wrong.
- https://www.anthropic.com/engineering/equipping-agents-for-the-real-world-with-agent-skills — Progressive disclosure three-level model.
- https://claude.com/blog/building-agents-with-the-claude-agent-sdk — Subagents for parallelization + context isolation in SDK terms.
- https://www.anthropic.com/engineering/writing-tools-for-agents — Tool descriptions as a prompt-engineering surface (applies to subagent descriptions too).

Practitioner (high-signal, opinionated):
- https://www.humanlayer.dev/blog/writing-a-good-claude-md — Instruction-count budget argument; <300 lines; pointers over copies.
- https://alexop.dev/posts/stop-bloating-your-claude-md-progressive-disclosure-ai-coding-tools/ — CLAUDE.md-as-index with explicit read-first directive; cites Vercel skill-activation evals.
- https://lucumr.pocoo.org/2025/11/21/agents-are-hard/ and https://lucumr.pocoo.org/2025/7/30/things-that-didnt-work/ — Armin Ronacher's negative results: automation disengagement, parallel write chaos, dead ends.
- https://hidekazu-konishi.com/entry/claude_code_hooks_complete_guide.html — Hook lifecycle semantics; "a hook is a guarantee"; Stop-hook loop anti-pattern.
- https://github.com/anthropics/claude-code/issues/23478 and https://github.com/anthropics/claude-code/issues/21858 — Known `paths:` rule gaps (no trigger on Write; user-level paths ignored).

## Durable Search Terms

- `claude code memory CLAUDE.md site:code.claude.com`
- `claude code ".claude/rules" paths frontmatter`
- `claude code subagents frontmatter model tools site:code.claude.com`
- `claude code hooks deterministic PreToolUse exit code 2`
- `anthropic effective context engineering attention budget`
- `anthropic multi-agent research system orchestrator worker`
- `agent skills progressive disclosure SKILL.md`
- `anthropics/claude-code CHANGELOG rules OR agents OR skills`
- `CLAUDE.md size lines adherence best practices`
- `claude code hooks vs CLAUDE.md instructions compliance`

## Sources

Primary (fetched/verified 2026-06-12):
1. https://code.claude.com/docs/en/memory [verified in full]
2. https://code.claude.com/docs/en/sub-agents [verified in full]
3. https://code.claude.com/docs/en/hooks-guide [verified, intro + setup]
4. https://raw.githubusercontent.com/anthropics/claude-code/main/CHANGELOG.md [verified: v2.1.175 head; v2.1.172 nesting entry]
5. https://code.claude.com/docs/en/hooks [agent-fetched, verbatim quotes]
6. https://code.claude.com/docs/en/skills [agent-fetched, verbatim quotes]
7. https://code.claude.com/docs/en/best-practices [agent-fetched, verbatim quotes]
8. https://platform.claude.com/docs/en/agents-and-tools/agent-skills/best-practices [agent-fetched]

Anthropic engineering/blog:
9. https://www.anthropic.com/engineering/effective-context-engineering-for-ai-agents
10. https://www.anthropic.com/engineering/multi-agent-research-system
11. https://www.anthropic.com/engineering/equipping-agents-for-the-real-world-with-agent-skills (Dec 18, 2025)
12. https://claude.com/blog/building-agents-with-the-claude-agent-sdk (Jan 28, 2026)
13. https://www.anthropic.com/engineering/writing-tools-for-agents
14. https://claude.com/blog/skills (Oct 16, 2025; updated Dec 18, 2025)

Community / secondary:
15. https://www.humanlayer.dev/blog/writing-a-good-claude-md
16. https://www.humanlayer.dev/blog/stop-claude-from-ignoring-your-claude-md
17. https://alexop.dev/posts/stop-bloating-your-claude-md-progressive-disclosure-ai-coding-tools/
18. https://www.dotzlaw.com/insights/claude-hooks/
19. https://hidekazu-konishi.com/entry/claude_code_hooks_complete_guide.html
20. https://joseparreogarcia.substack.com/p/how-claude-code-rules-actually-work
21. https://lucumr.pocoo.org/2025/6/12/agentic-coding/ , https://lucumr.pocoo.org/2025/7/30/things-that-didnt-work/ , https://lucumr.pocoo.org/2025/11/21/agents-are-hard/
22. https://simonwillison.net/2025/Oct/16/claude-skills/
23. https://paddo.dev/blog/claude-rules-path-specific-native/ (rules launch, v2.0.64)
24. https://github.com/anthropics/claude-code/issues/23478 , https://github.com/anthropics/claude-code/issues/21858 , https://github.com/anthropics/claude-code/issues/2394
25. https://boringbot.substack.com/p/claude-code-skills-subagents-hooks
26. https://www.richsnapp.com/article/2025/10-05-context-management-with-subagents-in-claude-code
27. https://dev.to/oikon/reflections-of-claude-code-from-changelog-833 (Dec 30, 2025)
28. https://techcrunch.com/2025/10/20/anthropic-brings-claude-code-to-the-web/
