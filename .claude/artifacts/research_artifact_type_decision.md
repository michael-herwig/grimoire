# Research: When to Use Which AI-Config Artifact Type

<!--
Technology Landscape Research
Filename: artifacts/research_artifact_type_decision.md
Owner: Researcher (worker-researcher / deep-research harness)
Handoff to: Architect (/architect), meta-maintain-config, Grimoire product design
Purpose: Decision framework for choosing between always-on instructions,
path-scoped rules, skills, subagents, and hooks — across Claude Code,
OpenCode, and GitHub Copilot. Directly relevant to Grimoire's domain
(packaging and installing exactly these artifact types).
Artifacts decay — check dates before trusting findings.
-->

## Metadata

**Date:** 2026-06-12
**Domain:** AI-agent configuration (cross-vendor)
**Triggered by:** Need for a research-backed decision framework: skill vs rule vs subagent vs hook vs always-on instruction
**Expires:** 2026-12 (vendor docs move fast; re-verify activation/token mechanics)
**Method:** 5-angle web research fan-out (official Claude Code docs, Anthropic engineering + agentskills.io, OpenCode docs, GitHub Copilot docs, community analysis 2025–2026), followed by an adversarial verification pass on all community-sourced claims (13 spot-checks; corrections noted inline).

## Executive Summary

The five artifact types form a spectrum along two axes: **when content enters context** (always → on file match → on task match → on delegation → never) and **how reliably it applies** (deterministic → advisory).

1. **Always-on instruction files** (CLAUDE.md / AGENTS.md / copilot-instructions.md) pay full token cost every session and are *advisory*. Adherence measurably degrades with length — Anthropic's own guidance is a "deletion test" per line and <200 lines; HumanLayer measured ~150–200 instructions as the consistency ceiling. Reserve for content relevant to *every* task.
2. **Path/glob-scoped rules** are free until a matching file is touched. They are the right home for per-language/per-subsystem standards — but they are silent during planning (no file open) and do not transfer into spawned subagents. Vendors disagree fundamentally here: Claude Code loads lazily on file read; Copilot attaches on `applyTo` match; **OpenCode has no lazy equivalent** — its `instructions` globs resolve at startup and load always-on.
3. **Skills** are the on-demand knowledge vehicle and the only artifact with a cross-vendor open standard (agentskills.io, ~35 adopters including Copilot, OpenCode, Cursor, Codex, Gemini CLI). Cost: ~100 tokens of metadata per skill at startup; body (<5k tokens recommended) only on invocation. Their defining weakness is *probabilistic activation*: ~50% baseline auto-trigger with weak descriptions, 73% of audited community skills silently never fire, and 0% auto-activation inside Task-spawned subagents.
4. **Subagents** buy context isolation and parallelism at the price of amnesia: fresh context, no conversation history, summaries lossy by design, and token spend multiplies linearly with parallelism.
5. **Hooks** are the only *deterministic* mechanism — "CLAUDE.md instructions are advisory, hooks are deterministic" (Anthropic) — at zero context cost. But they are not a hard security boundary: Claude Code's `if:` filter fails open, blocked tools get routed around (`rm` → `perl -e 'unlink'`), and pipe mode (`claude -p`) skips hooks entirely.

**The one-line rule:** *put invariants in hooks, identity in the always-on file, file-local standards in scoped rules, occasional know-how in skills, and context-hungry or privilege-separated work in subagents.*

## The Decision Table

| | **Always-on instruction file**<br>(CLAUDE.md, AGENTS.md, copilot-instructions.md, unscoped rules) | **Path/glob-scoped rule**<br>(.claude/rules `paths:`, Copilot `.instructions.md` `applyTo`) | **On-demand skill**<br>(SKILL.md, Agent Skills standard) | **Subagent**<br>(.claude/agents, .opencode/agents, .github/agents) | **Hook**<br>(Claude hooks, OpenCode plugins, Copilot hooks) |
|---|---|---|---|---|---|
| **Activation model** | Unconditional at session start. Claude Code: injected as a *user message* after the system prompt; `@imports` expand at launch (no deferral). Copilot: auto-added to every chat request (not inline suggestions). OpenCode: upward traversal finds AGENTS.md *and* CLAUDE.md; `instructions` array merged in. [S1, S10, S15] | Fires on file match. Claude Code: "when Claude reads files matching the pattern, not on every tool use". Copilot: `applyTo` glob match on files in context, plus semantic description match. OpenCode: **no lazy mode** — globs resolve at startup, then always-on. [S1, S11, S16] | Two-stage. Metadata (name + description) preloaded for routing; body loads when model judges the task matches the description, or on explicit `/name`. Level-3 bundled files load only on access (3-level progressive disclosure). [S2, S6, S7, S13] | Model-delegated on `description` match ("use proactively" raises delegation rate) or explicit invocation (Agent tool, `@name` in OpenCode). Runs in a fresh, isolated context — no conversation history. [S3, S12, S14] | Deterministic, event-fired (PreToolUse/PostToolUse/SessionStart/Stop…). "Unlike CLAUDE.md instructions which are advisory, hooks are deterministic and guarantee the action happens." Never model-invoked. [S4, S5] |
| **Context cost (when tokens are spent)** | Full cost **every session, every turn** — a permanent attention tax. Splitting into `@imports` saves nothing. Auto-memory capped at 200 lines / 25 KB at load. [S1, S8] | **Zero until a matching file is touched**, then full rule body for the session. Cheapest way to carry per-area standards. [S1] | ~100 tokens/skill metadata at startup (agentskills.io spec); body <5k tokens recommended, paid only on invocation. Claude Code: once invoked, body persists for the session; after compaction re-attached at 5k/skill within a 25k shared budget. OpenCode: body returned as a `skill` tool result. [S6, S7, S2, S16] | Main context pays only the returned summary. The subagent burns its own budget — total spend goes *up*: "running ten agents in parallel uses quota ten times faster." [S9, S12, S22] | **Zero context cost** for the script itself; only injected output costs tokens (Claude Code caps hook output at 10,000 chars). [S4, S5] |
| **Best for** | Project identity, build/test commands, universal conventions — "only include things that apply broadly" (Anthropic). Cross-tool baseline via AGENTS.md. [S5, S20] | Language/subsystem standards needed *while editing those files* (e.g. `quality-rust.md` on `**/*.rs`); monorepo per-package conventions (AGENTS.md nearest-file in Copilot). [S1, S15] | Occasionally-relevant workflows and domain knowledge; procedures with bundled scripts/templates (executable code never enters context); anything that should be portable — only artifact with a ~35-adopter open standard. [S2, S6, S7] | Context-heavy research that would pollute the main session ("returns only a condensed, distilled summary"); parallel workstreams; least-privilege tool/permission sets; per-role model selection. [S8, S9, S3] | Invariants: format-on-save, lint gates, blocking writes to protected paths, audit logging, env injection. Anything that must happen 100% of the time without model judgment. [S4, S5, S21] |
| **Poor fit for** | Occasional workflows (constant tax for rare value); long reference material; anything needing *guaranteed* application — it is advisory and decays with length. | Guidance needed during planning/architecture before any file is open (needs a catalog/index workaround); enforcement; content for spawned subagents (rules don't transfer). [S19] | Must-always-apply invariants — activation is probabilistic (~50% baseline with weak descriptions); headless/`-p` automation; spawned-subagent contexts (0% auto-recall without explicit instruction). [S17, S18, S19] | Tasks needing the parent conversation's nuance (summaries are lossy); quick single lookups (spawn overhead); deeply interactive work. | Anything needing judgment or nuance; a *hard* security boundary — `if:` filters fail open, agents route around blocked tools ("whack-a-mole"), pipe mode skips hooks. Use the permission system for hard deny. [S4, S21] |
| **Vendor support** | **Claude Code:** CLAUDE.md hierarchy + imports + memory. **OpenCode:** AGENTS.md *and* CLAUDE.md discovered; remote `https://` instruction URLs. **Copilot:** copilot-instructions.md (all surfaces) + AGENTS.md/CLAUDE.md/GEMINI.md (cloud agent, CLI, VS Code). [S1, S10, S15] | **Claude Code:** `.claude/rules/*.md` with `paths:`. **Copilot:** `.github/instructions/*.instructions.md` with `applyTo` (gaps: not in VS Code/VS code review; not Eclipse chat). **OpenCode:** partial — always-on globs only. [S1, S11, S16] | All three, natively, via the Agent Skills standard. Copilot and OpenCode both auto-discover `.claude/skills/` for compatibility. Copilot GA: Dec 18, 2025. [S6, S13, S16] | **Claude Code:** `.claude/agents/*.md`, no nesting. **OpenCode:** primary agents + subagent child sessions, granular per-agent permissions. **Copilot:** `.github/agents/*.agent.md` (ex-chatmode; 30k-char prompt cap; subagent chaining off by default). [S3, S12, S14] | **Claude Code:** shell commands, exit-code protocol. **OpenCode:** JS/TS Bun plugins — can *cancel* tool calls by throwing and register new tools. **Copilot:** `.github/hooks/*.json` — command/http/prompt types; cloud-agent hooks must be on the default branch. [S4, S12, S14] |
| **Failure modes** | Adherence collapse with size: "If your CLAUDE.md is too long, Claude ignores half of it" (Anthropic); ~150–200-instruction ceiling (HumanLayer); referenced rule files dropped after compaction (issue #9796); LLM-*generated* context files reduced task success ~3% vs none while raising cost >20% (ETH Zurich). [S5, S17, S18, S23] | Dead globs silently never fire after renames; `applyTo` mismatch is "the primary reason instructions fail to load" (VS Code docs); invisible during planning; not inherited by subagents. [S11, S19] | Silent non-activation: 73% of 214 audited community skills never fire; vague descriptions ≈ coin-flip activation (20x better with explicit trigger conditions); ~15,000-char combined description budget silently truncates the skill list; compaction trims loaded bodies. [S17, S18] | Over-summarization loses cross-domain context; skills/rules don't auto-fire inside (0/20 tests); Claude Code subagents load at session start (file edits need restart) and cannot nest; linear cost multiplication. [S19, S22, S3] | Counter-intuitive exit codes (only exit 2 blocks; exit 1 proceeds); `if:` filter fails open on unparseable commands; tool-level blocking circumvented (`perl -e 'unlink'`); no hook protection in `claude -p`; hooks have no controlling terminal. [S4, S21] |

## Decision Heuristics

Ask these in order; the first decisive answer usually picks the type.

1. **Must it happen every time, with zero exceptions?** If yes and it is *mechanical* (no judgment needed) → **hook**. If yes but it needs judgment → one terse line in the **always-on file** (and accept it is advisory), optionally backed by a hook that checks the outcome. "A system prompt is a request. A hook is a guarantee." [S21, S5]
2. **Is it deterministic?** Formatting, linting, blocking paths, logging, env setup → **hook** (zero context cost). Anything requiring interpretation → prose artifact. [S4, S5]
3. **How often is it relevant?**
   - Every task → always-on file (then apply the deletion test per line). [S5]
   - Only when editing certain files → **path-scoped rule**. [S1]
   - Occasionally, by topic → **skill** (auto-invocable, description with explicit WHEN triggers). [S2, S18]
   - Only on explicit request / has side effects → skill with `disable-model-invocation: true` (slash-command style; in Claude Code, commands and skills are now one mechanism). [S2]
4. **Does it need to fire without being asked?** Always-on files and hooks fire unconditionally; scoped rules fire on file match; skills fire on *probabilistic* description match — never rely on a skill for something that must not be missed. [S17, S18]
5. **Does the work need isolation, parallelism, or different privileges/model?** → **subagent**. If it merely needs occasional knowledge, a skill is far cheaper. Remember: subagents don't inherit your session's rules or auto-activate skills — preload via the subagent's `skills:` field or explicit prompt instruction. [S3, S19]
6. **Is it knowledge or capability?** Prose someone must read → rule/skill. Logic a machine can run → hook, or a script *bundled inside* a skill (executable code never costs context). [S6, S7]
7. **Does it need to work across vendors?** → AGENTS.md for the baseline + Agent Skills for workflows; both are multi-vendor standards. Vendor-native rules/hooks are lock-in surfaces. [S10, S13, S20]
8. **Would removing it cause mistakes?** Anthropic's deletion test for every always-on line: "If not, cut it. Bloated CLAUDE.md files cause Claude to ignore your actual instructions!" [S5]

## Per-Vendor Notes (where the models disagree)

### Glob-scoped rules — three different semantics
- **Claude Code**: lazy — rule injects when a matching file is *read*; unscoped rules load at launch like CLAUDE.md. [S1]
- **Copilot**: lazy-ish — `applyTo` match against files in context, *plus* semantic matching of the rule's description to the task; file order not guaranteed; unsupported in some surfaces (VS Code code review). [S11]
- **OpenCode**: **not lazy at all** — `instructions` globs are a discovery mechanism; everything matched is "combined with your AGENTS.md files" at startup. Porting a Claude Code rule set to OpenCode converts scoped rules into always-on cost. OpenCode uniquely accepts remote `https://` instruction URLs. [S16]

### Skills — same standard, different plumbing
- **Claude Code**: SKILL.md body injected as a conversation message, persists all session; post-compaction re-attach budget 5k/skill, 25k total; `context: fork` runs a skill in an isolated subagent; description listing truncated (~1,536 chars/skill listing cap; ~15k combined budget). [S2, S17]
- **OpenCode**: skills surface as a native **`skill` tool** — names/descriptions live in the tool description; the body arrives as a tool *result*, not prompt injection. Six discovery paths including `.claude/skills/`. [S16]
- **Copilot**: dual activation (semantic + slash); three-level progressive loading; discovers `.github/skills/`, `.claude/skills/`, `.agents/skills/`; GA Dec 2025. [S13]

### Hooks — shell vs JS vs JSON
- **Claude Code**: shell commands; exit-code protocol (exit 2 blocks, exit 1 proceeds — counter-intuitive); cannot cancel an in-flight tool call beyond PreToolUse deny; `if:` filter fails open. [S4]
- **OpenCode**: Bun JS/TS plugins; `tool.execute.before` can **throw to cancel** a tool call; plugins can register entirely new tools; 25+ event types incl. `shell.env`, `session.compacted`. Strictly more powerful, but requires JS. [S14]
- **Copilot**: declarative JSON in `.github/hooks/`; three hook types (`command`, `http`, `prompt`); `preToolUse` can deny *and rewrite arguments*; cloud-agent hooks only run from the default branch and only `bash` entries execute in the sandbox. [S12]

### Subagents
- **Claude Code**: fresh context, no nesting, loaded at session start; built-in Explore/Plan skip CLAUDE.md + git status to stay cheap. [S3]
- **OpenCode**: primary vs subagent tiers, navigable child sessions, per-agent `ask/allow/deny` permission matrix (11 permission keys, bash wildcards) — the most granular privilege model of the three. [S16]
- **Copilot**: custom agents are closer to "modes" (prompt + tools + model, 30k-char cap); subagent-to-subagent invocation is off by default (`chat.subagents.allowInvocationsFromSubagents`). [S12]

### Always-on precedence
- **Copilot** layers personal > repository > organization instructions, plus AGENTS.md nearest-file-wins (which can unexpectedly shadow root instructions in monorepos). [S15]
- **OpenCode** merges seven config sources (remote → global → env → project → managed), first-match-wins per rule-file category, and reads `~/.claude/CLAUDE.md` only as a fallback. [S16]
- **Claude Code** loads the full ancestor hierarchy at launch and subdirectory CLAUDE.md lazily on file access. [S1]

## Migration Paths

| Signal that content has outgrown its type | Move | Why |
|---|---|---|
| Always-on file > ~200–300 lines, or lines fail the deletion test | CLAUDE.md → **path-scoped rules** (per-area standards) and **skills** (task workflows) | Adherence degrades with size; rules/skills defer the cost until relevant. [S5, S17] |
| A scoped rule has grown procedural (step-by-step how-to, >200 lines) | rule → **skill** with progressive disclosure; rule keeps a 1-line invariant + pointer | Rules are for standards-while-editing; multi-step procedures are skill-shaped, and skill level-3 files are free until read. [S2, S6] |
| A skill encodes something that must *never* be skipped | skill → **hook** for the enforcement core (skill keeps the "how/why" prose) | Skill activation is probabilistic (~50% baseline, 0% in subagents/headless); hooks are the only guarantee. [S17, S18, S19, S4] |
| A manual slash command gets used repeatedly in predictable contexts (heuristic: >3–4×/week) | command → auto-invocable **skill** with trigger-rich description | In Claude Code commands and skills are already unified; auto-invocation removes the "remember to type it" failure. [S2, S24] |
| Recurring research/exploration keeps flooding the main context | inline work → **subagent** (preload needed skills via `skills:` frontmatter or explicit prompt — auto-activation does not transfer) | Isolation: main context receives only the distilled summary. [S8, S9, S19] |
| MCP server used mostly for read-only lookups | MCP → **CLI calls + a thin skill** documenting usage | Measured 32x token difference (44,026 vs 1,365 tokens for an equivalent GitHub query); skills cost "a few dozen extra tokens" until loaded. [S22, S24] |
| Same conventions duplicated across Claude/OpenCode/Copilot configs | CLAUDE.md → **AGENTS.md as source of truth** + thin vendor wrapper or symlink | AGENTS.md and Agent Skills are the two multi-vendor standards; everything else is per-vendor. [S10, S15, S20] |
| Hook stderr/stdout growing into paragraphs of guidance | hook output → pointer to a rule/skill file | Hook output is capped (10k chars) and is the wrong place for prose; keep hooks as tripwires, not textbooks. [S4] |

**Default authoring sequence** (community consensus): start with a lean always-on file → extract recurring workflows into skills as patterns emerge → add hooks once a policy proves worth enforcing → reach for subagents only when isolation or parallelism is demonstrably needed. [S24]

## Canonical Further-Reading Links (annotated)

- **Anthropic — Effective context engineering for AI agents** — the theoretical foundation: finite "attention budget", context rot, just-in-time loading, subagents as context sandboxes. <https://www.anthropic.com/engineering/effective-context-engineering-for-ai-agents>
- **Anthropic — Equipping agents for the real world with Agent Skills** — the skills design rationale: 3-level progressive disclosure, skills as directories, bundled executable code. <https://www.anthropic.com/engineering/equipping-agents-for-the-real-world-with-agent-skills>
- **Claude Code docs: memory / skills / sub-agents / hooks** — the authoritative activation + token mechanics (load order, compaction budgets, exit-code protocol). <https://code.claude.com/docs/en/memory>, <https://code.claude.com/docs/en/skills>, <https://code.claude.com/docs/en/sub-agents>, <https://code.claude.com/docs/en/hooks>
- **agentskills.io specification** — the open standard: ~100-token metadata tier, <5k-token body, 1024-char description; adopter list (~35 products). <https://agentskills.io/specification>
- **OpenCode docs: rules / agents / skills / plugins** — the divergent model: always-on instruction globs, native `skill` tool, JS plugins that can cancel tool calls. <https://opencode.ai/docs/rules/>, <https://opencode.ai/docs/skills/>, <https://opencode.ai/docs/plugins/>
- **GitHub Copilot: custom-instructions support matrix + agent skills + hooks reference** — which mechanism works on which surface (the matrix is the part everyone gets wrong). <https://docs.github.com/en/copilot/reference/custom-instructions-support>, <https://docs.github.com/en/copilot/concepts/agents/about-agent-skills>, <https://docs.github.com/en/copilot/reference/hooks-reference>
- **Simon Willison — Claude Skills are awesome, maybe a bigger deal than MCP** (Oct 2025) — the token-economics argument that made skills the default knowledge vehicle. <https://simonwillison.net/2025/Oct/16/claude-skills/>
- **HumanLayer — Writing a good CLAUDE.md** (Nov 2025) — the ~150–200-instruction ceiling and pruning discipline. <https://humanlayer.dev/blog/writing-a-good-claude-md>
- **boucle.sh — What Claude Code hooks can and cannot enforce** (Apr 2026) — the definitive hooks-limitations taxonomy (pipe mode, tool circumvention). <https://blog.boucle.sh/posts/what-claude-code-hooks-can-and-cannot-enforce>
- **Jesse Vincent — Claude Code skills not triggering? It might not see them** (Dec 2025) — the silent description-budget truncation. <https://blog.fsck.com/2025/12/17/claude-code-skills-not-triggering-it-might-not-see-them/>
- **Gloaguen et al. (ETH Zurich) — Evaluating AGENTS.md: Are Repository-Level Context Files Helpful for Coding Agents?** (arXiv 2602.11988, Feb 2026) — empirical evidence that LLM-generated context files can be net-negative. <https://arxiv.org/abs/2602.11988>

## Durable Search Terms

For refreshing this research when it expires:

- `claude code memory CLAUDE.md rules paths frontmatter site:code.claude.com`
- `claude code skills progressive disclosure compaction token budget`
- `agent skills specification agentskills.io adopters`
- `claude code hooks exit code 2 PreToolUse fails open`
- `opencode rules instructions glob always loaded` / `opencode skill tool lazy`
- `opencode plugins tool.execute.before throw cancel`
- `copilot custom instructions support matrix applyTo instructions.md`
- `copilot agent skills .claude/skills discovery` / `copilot hooks reference preToolUse`
- `AGENTS.md standard adoption nearest file precedence`
- `claude code skill description activation rate trigger phrases`
- `skills not activating subagent headless claude -p`
- `CLAUDE.md too long instructions ignored deletion test`
- `MCP vs CLI token cost benchmark`
- `evaluating AGENTS.md context files coding agents arxiv`

## Sources (URL per claim)

Verification status: **[V]** = independently re-fetched and quote-checked in this research pass; **[P]** = verified with corrections (noted); **[D]** = fetched directly from live official docs by a research agent (single fetch).

| ID | Claim(s) backed | Source | Status |
|---|---|---|---|
| S1 | CLAUDE.md loaded every conversation as user message; ancestor files at launch, subdir lazily; `@imports` expand at launch; unscoped rules always-on; scoped rules fire on file read; auto-memory 200-line/25KB cap; HTML comments stripped; <200-line guidance | <https://code.claude.com/docs/en/memory> | [V] |
| S2 | Skill body loads on invocation only; persists for session; 1,536-char listing truncation; `disable-model-invocation` removes from context; compaction re-attach 5k/25k budgets; `context: fork`; commands merged into skills | <https://code.claude.com/docs/en/skills> | [D] |
| S3 | Subagents: fresh isolated context; no nesting; loaded at session start; `skills:` preloads full bodies; "use proactively" description phrasing; Explore/Plan skip CLAUDE.md | <https://code.claude.com/docs/en/sub-agents> | [D] |
| S4 | Hooks deterministic at lifecycle events; exit 2 blocks / exit 1 proceeds; 10k-char output cap; no controlling terminal; `if:` filter fails open — "use the permission system rather than a hook to enforce a hard allow or deny" | <https://code.claude.com/docs/en/hooks> | [D] |
| S5 | "CLAUDE.md is loaded every session… use skills instead"; deletion test; "hooks are deterministic… CLAUDE.md instructions are advisory"; "If your CLAUDE.md is too long, Claude ignores half of it"; subagents report back summaries | <https://code.claude.com/docs/en/best-practices> | [D] |
| S6 | Agent Skills spec: metadata ~100 tokens at startup; body <5,000 tokens / 500 lines recommended; description ≤1,024 chars encodes what+when; name/directory match rule | <https://agentskills.io/specification> | [V] |
| S7 | Skills = directories with 3-level progressive disclosure; bundled executable code never enters context; name+description are the routing mechanism | <https://www.anthropic.com/engineering/equipping-agents-for-the-real-world-with-agent-skills> | [D] |
| S8 | Attention budget; context rot ("ability to accurately recall information decreases"); minimal always-on set; just-in-time loading | <https://www.anthropic.com/engineering/effective-context-engineering-for-ai-agents> | [D] |
| S9 | Subagents "return only a condensed, distilled summary" — isolation as the canonical anti-bloat pattern | same as S8 | [D] |
| S10 | OpenCode discovers AGENTS.md + CLAUDE.md; precedence chain; `~/.claude/CLAUDE.md` as fallback; remote URL instructions | <https://opencode.ai/docs/rules/> | [D] |
| S11 | Copilot `.instructions.md`: `applyTo` glob + semantic activation; "applyTo pattern mismatches are the primary reason instructions fail to load"; order not guaranteed; surface gaps | <https://code.visualstudio.com/docs/agent-customization/custom-instructions>, <https://docs.github.com/en/copilot/reference/custom-instructions-support> | [D] |
| S12 | Copilot custom agents (.agent.md, 30k-char cap, subagent gating); hooks: command/http/prompt types, preToolUse deny/modify, default-branch requirement, policy→user→repo→plugin order | <https://docs.github.com/en/copilot/reference/custom-agents-configuration>, <https://docs.github.com/en/copilot/reference/hooks-reference> | [D] |
| S13 | Copilot Agent Skills GA (Dec 18, 2025); discovery incl. `.claude/skills/`; dual activation; 3-level loading | <https://github.blog/changelog/2025-12-18-github-copilot-now-supports-agent-skills/>, <https://docs.github.com/en/copilot/concepts/agents/about-agent-skills>, <https://code.visualstudio.com/docs/agent-customization/agent-skills> | [D] |
| S14 | OpenCode: subagent child sessions; per-agent ask/allow/deny permissions; JS/TS Bun plugins; `tool.execute.before` throw-to-cancel; plugins register tools; skills via native `skill` tool, 6 discovery paths | <https://opencode.ai/docs/agents/>, <https://opencode.ai/docs/plugins/>, <https://opencode.ai/docs/skills/> | [D] |
| S15 | copilot-instructions.md auto-added to chat requests (not inline suggestions); personal > repo > org precedence; AGENTS.md nearest-file-wins; CLAUDE.md/GEMINI.md recognized | <https://docs.github.com/copilot/customizing-copilot/adding-custom-instructions-for-github-copilot>, <https://docs.github.com/en/copilot/how-tos/configure-custom-instructions-in-your-ide/add-repository-instructions-in-your-ide> | [D] |
| S16 | OpenCode `instructions` array always-on ("All instruction files are combined with your AGENTS.md files"); seven-source config merge | <https://opencode.ai/docs/rules/>, <https://opencode.ai/docs/config/> | [D] |
| S17 | 73% of 214 audited community skills silently broken (Mar 2026); ~15,000-char combined description budget silently truncates (Claude Code 2.0.70, `SLASH_COMMAND_TOOL_CHAR_BUDGET` workaround) | <https://dev.to/thestack_ai/i-audited-214-claude-code-skills-73-were-silently-broken-2m9a>, <https://blog.fsck.com/2025/12/17/claude-code-skills-not-triggering-it-might-not-see-them/> | [V] |
| S18 | ~50% baseline auto-activation with weak descriptions; 20x improvement with explicit trigger conditions (650 trials, odds ratio 20.6); ~150–200-instruction consistency ceiling, <300-line guidance | <https://medium.com/@ivan.seleznov1/why-claude-code-skills-dont-activate-and-how-to-fix-it-86f679409af1>, <https://humanlayer.dev/blog/writing-a-good-claude-md> | [V] (HumanLayer token-count figure circulating in secondary posts is NOT in the source — line counts only) |
| S19 | Skills 0% auto-activation in Task-spawned subagents without explicit instruction (closed "not planned"); referenced context files dropped after compaction (issue is about a referenced `.claude/project-context.md`, not CLAUDE.md itself) | <https://github.com/anthropics/claude-code/issues/14016> [V], <https://github.com/anthropics/claude-code/issues/9796> | [P] |
| S20 | Agent Skills standard adopters (~35 incl. Copilot, OpenCode, Cursor, OpenAI Codex, Gemini CLI, Goose, JetBrains Junie); originated at Anthropic, released open | <https://agentskills.io> | [D] |
| S21 | Hooks skipped in pipe mode (`claude -p`); tool-blocking circumvented (`perl -e 'unlink'`); "tool-level enforcement is a game of whack-a-mole" | <https://blog.boucle.sh/posts/what-claude-code-hooks-can-and-cannot-enforce> (2026-04-01) | [V] |
| S22 | MCP vs CLI: 44,026 vs 1,365 tokens for equivalent GitHub query (~32x); parallel agents multiply quota linearly ("ten agents… ten times faster") | <https://onlycli.github.io/OnlyCLI/blog/mcp-token-cost-benchmark> [V], <https://www.cloudzero.com/blog/claude-code-agents> | [P] (CloudZero's $/day figures are unsourced — excluded) |
| S23 | LLM-generated context files: −3% task success vs none; human-written +4%; cost +20% | Gloaguen et al., ETH Zurich, arXiv 2602.11988 (Feb 2026), via <https://termdock.com/blog/skill-md-vs-claude-md-vs-agents-md> | [V] (paper existence + figures confirmed) |
| S24 | Skills cost "a few dozen extra tokens" until loaded; GitHub MCP "famously consumes tens of thousands of tokens"; >3–4×/week command → skill heuristic; authoring sequence consensus | <https://simonwillison.net/2025/Oct/16/claude-skills/> [V], <https://mindstudio.ai/blog/claude-code-skills-vs-slash-commands>, <https://dev.to/owen_fox/claude-code-hooks-subagents-and-skills-complete-guide> | [P] (Willison verified verbatim; the two heuristic posts single-fetch) |
