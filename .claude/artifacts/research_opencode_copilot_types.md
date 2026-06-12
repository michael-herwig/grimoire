# Research: agentskills.io Spec + OpenCode + GitHub Copilot — Skills, Rules/Instructions, Agents

- **Date**: 2026-06-12
- **Method**: deep-research harness — 5 parallel search agents (spec, OpenCode skills/rules/config, OpenCode+Copilot agents, Copilot instructions/skills, cross-client portability), followed by a direct verification pass against primary sources (agentskills.io/specification, opencode.ai/docs/skills + /agents, code.claude.com/docs/en/skills, docs.github.com about-agent-skills).
- **Confidence convention**: claims cite a URL. Claims sourced only from community mirrors (deepwiki, gists) or open GitHub issues are marked **[community]** or **[issue]** — treat as medium confidence.

---

## Executive Summary

1. **The agentskills.io spec is tiny and stable**: two required frontmatter fields (`name` ≤64 chars lowercase-hyphen, `description` 1–1024 chars), four optional (`license`, `compatibility` ≤500 chars, `metadata` string map, `allowed-tools` — experimental). Three optional directories with normative semantics: `scripts/` (executable), `references/` (read on demand), `assets/` (static resources). Guidance: SKILL.md < 500 lines / < 5,000 tokens body; metadata tier ~100 tokens; file references one level deep. No spec version number, no releases; governed by the `agentskills/agentskills` GitHub org (Apache-2.0 / CC-BY-4.0), originated at Anthropic Oct 2025, opened Dec 2025. (https://agentskills.io/specification)
2. **Skills are the portable artifact.** Project-level `.claude/skills/<name>/SKILL.md` is scanned by all three clients (Claude Code natively; OpenCode and Copilot as documented compat paths). The portable frontmatter core is `name` + `description` only; every other field is client-specific or experimental. All three use the same three-tier progressive-disclosure activation.
3. **Rules are NOT portable via a single mechanism.** OpenCode and Copilot read `AGENTS.md`; Claude Code still does not (open feature requests #6235 / #34235 as of June 2026). Conversely all three read `CLAUDE.md` in some mode (Claude native; OpenCode fallback when no AGENTS.md; Copilot CLI/VS Code/coding agent as an "agent-specific" instruction file). Glob-scoped rules are three incompatible systems (Claude `paths:`, Copilot `applyTo:` in `.instructions.md`, OpenCode: none).
4. **Agents are the least portable type.** Three different formats/paths: `.claude/agents/*.md` (Claude; also read by VS Code Copilot), `.github/agents/*.agent.md` (Copilot), `.opencode/agents/*.md` + `opencode.json` `agent` key (OpenCode). OpenCode does **not** read `.claude/agents/`.
5. **Hard limits worth designing around**: Copilot code review reads only the **first 4,000 characters** of `copilot-instructions.md`; Copilot custom-agent body max 30,000 chars; Claude truncates `description` + `when_to_use` at 1,536 chars in the skill listing; OpenCode has **no size guard** on always-on rules (large AGENTS.md can exhaust context).

---

## Detailed Findings

### 1. agentskills.io specification

#### 1.1 Frontmatter (verbatim-verified against https://agentskills.io/specification)

| Field | Required | Constraints |
|---|---|---|
| `name` | Yes | 1–64 chars; lowercase alphanumeric (`a-z`, `0-9`) + hyphens only; no leading/trailing hyphen; no consecutive hyphens (`--`); **must match parent directory name** |
| `description` | Yes | 1–1024 chars, non-empty; should state both *what* the skill does and *when* to use it, with activation keywords |
| `license` | No | License name or reference to a bundled license file; keep short |
| `compatibility` | No | 1–500 chars; environment requirements (intended product, system packages, network). Spec note: "Most skills do not need the `compatibility` field." |
| `metadata` | No | Map of string keys → string values; the spec's blessed extension point ("clients can use this to store additional properties not defined by the Agent Skills spec"); recommend unique key names |
| `allowed-tools` | No | Space-separated string of pre-approved tools, e.g. `Bash(git:*) Bash(jq:*) Read`. **"Experimental. Support for this field may vary between agent implementations."** |

No `version`, `author`, or `tags` top-level fields exist — they go in `metadata`. (https://agentskills.io/specification)

#### 1.2 Directory layout and reserved-directory semantics

A skill is one directory containing, at minimum, `SKILL.md` (exact case). Optional directories carry normative semantics (https://agentskills.io/specification):

- `scripts/` — executable code agents run via their shell tool when SKILL.md instructs; must be self-contained or document dependencies, emit helpful errors, support non-interactive execution. Languages depend on implementation (Python, Bash, JavaScript common).
- `references/` — documentation read on demand (`REFERENCE.md`, `FORMS.md`, domain files). "Keep individual reference files focused … smaller files mean less use of context."
- `assets/` — static resources: templates, images, data files (lookup tables, schemas).
- Any additional files/directories are allowed. File references use relative paths from the skill root and should stay **one level deep** from SKILL.md.

They are "optional directories" rather than hard-reserved names, but compliant clients are expected to understand their semantics (read vs execute vs template-input). (https://agentskills.io/specification; https://agentskills.io/skill-creation/using-scripts.md)

#### 1.3 Progressive disclosure & size guidance (https://agentskills.io/specification)

| Tier | Loads | When | Budget |
|---|---|---|---|
| 1 Metadata | `name` + `description` | startup, all skills | ~100 tokens/skill |
| 2 Instructions | full SKILL.md body | on activation | < 5,000 tokens recommended |
| 3 Resources | scripts/references/assets | when referenced | as needed |

"Keep your main `SKILL.md` under 500 lines. Move detailed reference material to separate files."

#### 1.4 Portability & compliance

From the client-implementation guide (https://agentskills.io/client-implementation/adding-skills-support.md):
- Compliant clients must: discover directories containing `SKILL.md`; implement three-tier progressive disclosure; parse YAML frontmatter; handle name collisions.
- **Lenient validation** is the recommended posture: warn-and-load for most violations (name/dir mismatch, name > 64 chars); **skip only** when `description` is missing/empty or YAML is unparseable. Unknown frontmatter fields are ignored.
- The spec does not mandate scan paths. `.agents/skills/` is described as an emerged cross-client convention; most implementations also scan `.claude/skills/` for pragmatic compatibility.
- Collision-handling caveat: the implementation guide describes project-over-user precedence as the "universal convention," but Claude Code's own docs state the opposite order (enterprise > personal > project) — see §4.1. Treat collision precedence as **client-specific**, not spec-guaranteed.
- Trust model (recommended, not required): gate project-skill loading on workspace trust. Clients should exempt skill content from context compaction.
- Validation tooling: `skills-ref validate ./my-skill` (https://github.com/agentskills/agentskills/tree/main/skills-ref).

#### 1.5 Recent changes & governance

- Oct 16, 2025: Agent Skills launched by Anthropic; Dec 18–19, 2025: republished as open standard at agentskills.io under the `agentskills` GitHub org; Apache-2.0 code / CC-BY-4.0 docs. (https://www.anthropic.com/engineering/equipping-agents-for-the-real-world-with-agent-skills; https://simonwillison.net/2025/Dec/19/agent-skills/; https://github.com/agentskills/agentskills)
- No tagged releases, no frontmatter `version` field, no formal spec version numbering as of June 2026; `allowed-tools` remains the only experimental area. Possible future AAIF governance is speculation (Simon Willison), not confirmed.
- Adopters (homepage carousel, June 2026): Claude/Claude Code, GitHub Copilot/VS Code, Gemini CLI, OpenAI Codex, Cursor, JetBrains Junie, OpenHands, Roo Code, Amp, Goose, Letta, plus ~25 more. (https://agentskills.io)

---

### 2. OpenCode (opencode.ai)

#### 2.1 Skills

**Discovery — six paths, verbatim-verified** (https://opencode.ai/docs/skills/):

| Scope | Path |
|---|---|
| Project, native | `.opencode/skills/<name>/SKILL.md` |
| Global, native | `~/.config/opencode/skills/<name>/SKILL.md` |
| Project, Claude-compat | `.claude/skills/<name>/SKILL.md` |
| Global, Claude-compat | `~/.claude/skills/<name>/SKILL.md` |
| Project, agent-compat | `.agents/skills/<name>/SKILL.md` |
| Global, agent-compat | `~/.agents/skills/<name>/SKILL.md` |

- Frontmatter documented: required `name`, `description`; optional `license`, `compatibility`, `metadata` — i.e., the agentskills.io schema. Docs state explicitly: **"Unknown frontmatter fields are ignored."** (https://opencode.ai/docs/skills/)
- Activation: two-phase — name+description injected into the system prompt at startup; full body loaded on demand via a native `skill` tool call. **[community]** (https://deepwiki.com/sst/opencode/5.7-skills-system)
- Scripts: no built-in runner; the model runs `scripts/` content through the normal `bash` tool, subject to OpenCode permission gating. (https://opencode.ai/docs/skills/, https://agentskills.io/specification)
- Permission gating per skill name pattern via `opencode.json` → `"permission": { "skill": { "*": "allow", "internal-*": "ask" } }`. (https://opencode.ai/docs/permissions/)
- Collision: first-found wins, warning logged. **[community]** (https://deepwiki.com/sst/opencode/5.7-skills-system)
- Opt-out env vars: `OPENCODE_DISABLE_CLAUDE_CODE`, `OPENCODE_DISABLE_CLAUDE_CODE_PROMPT`, `OPENCODE_DISABLE_CLAUDE_CODE_SKILLS`, `OPENCODE_DISABLE_EXTERNAL_SKILLS`, `OPENCODE_PURE`. **[medium confidence — reported from docs/rules + community; not present on the skills page at verification time]** (https://opencode.ai/docs/rules/; https://github.com/anomalyco/opencode/issues/12432 **[issue]**)

#### 2.2 Rules / instructions

- `AGENTS.md` is the native rules file: discovered by walking up from cwd to the git worktree root; global file at `~/.config/opencode/AGENTS.md`; `/init` generates it. (https://opencode.ai/docs/rules/)
- `CLAUDE.md` legacy fallback: project `CLAUDE.md` used when no `AGENTS.md` at that level; global `~/.claude/CLAUDE.md` when no global AGENTS.md. Filename precedence at each level: `AGENTS.md` > `CLAUDE.md`. (https://opencode.ai/docs/rules/)
- `opencode.json` `instructions` array: local paths, **glob patterns** (e.g. `packages/*/AGENTS.md`, `.cursor/rules/*.md`), and **remote URLs** (5 s fetch timeout); concatenated + deduplicated across config layers, additive to AGENTS.md. (https://opencode.ai/docs/config/; https://deepwiki.com/sst/opencode/3.1-configuration-structure **[community]**)
- Activation model: **always-on**. All rules content goes into the system prompt every loop; there is no glob-scoped or on-demand rules tier, and **no size guard** — an oversized AGENTS.md can trigger context compaction. **[issue]** (https://github.com/anomalyco/opencode/issues/18037)
- Known sharp edges **[issues — verify against current release]**: global AGENTS.md silently ignored when project AGENTS.md exists (https://github.com/anomalyco/opencode/issues/22020); `instructions` array in `.jsonc` not loaded (https://github.com/anomalyco/opencode/issues/4758); subagents inherit CLAUDE.md/AGENTS.md (https://github.com/sst/opencode/issues/4483).

#### 2.3 Agents

- Markdown definitions: project `.opencode/agents/<name>.md`, global `~/.config/opencode/agents/<name>.md` (filename = agent id); or inline in `opencode.json` under `"agent"`. Docs use singular/plural dir spelling inconsistently in places; plural is current. (https://opencode.ai/docs/agents/)
- Fields: `description` (required), `mode` (`primary` | `subagent` | `all`, default `all`), `model`, `temperature`, `top_p`, `prompt` (supports `{file:./path}`), `steps`, `permission` (per-tool allow/ask/deny; `tools` map is deprecated), `disable`, `hidden`, `color`. (https://opencode.ai/docs/agents/)
- Built-ins: primary `build` (default) and `plan` (edit/bash restricted); subagents `general`, `explore` (read-only code), `scout` (read-only external docs); hidden system agents `compaction`, `title`, `summary`. Subagent invocation: automatic via `task` tool or manual `@name` mention; gateable via `permission.task`. (https://opencode.ai/docs/agents/)
- **OpenCode does not read `.claude/agents/*.md`** — Claude compat covers skills and CLAUDE.md only. (https://opencode.ai/docs/agents/; https://github.com/sst/opencode/issues/6266 **[issue]**)

#### 2.4 Config

- Load order (low→high): remote `.well-known/opencode` → global `~/.config/opencode/opencode.json(c)` → `OPENCODE_CONFIG` file → project `opencode.json(c)` → `.opencode/` dir → `OPENCODE_CONFIG_CONTENT` → managed/MDM. `$schema`: `https://opencode.ai/config.json`. Variable substitution: `{env:VAR}`, `{file:path}`. (https://opencode.ai/docs/config/)
- `.opencode/` subdirs: `agents/`, `skills/`, `commands/`, `plugins/`, `tools/`, `themes/`. (https://opencode.ai/docs/config/)

---

### 3. GitHub Copilot

#### 3.1 Repository-wide instructions — `.github/copilot-instructions.md`

- Always-on for Copilot Chat (VS Code, JetBrains, Visual Studio, github.com), code review, coding agent, and CLI. (https://docs.github.com/en/copilot/customizing-copilot/adding-custom-instructions-for-github-copilot)
- **Code review hard limit: first 4,000 characters only**; content beyond is silently dropped. Soft guidance ≈1,000 lines max before instruction-following degrades. (https://github.blog/ai-and-ml/github-copilot/unlocking-the-full-power-of-copilot-code-review-master-your-instructions-files/)
- VS Code master toggle: `github.copilot.chat.codeGeneration.useInstructionFiles` (default true). (https://code.visualstudio.com/docs/agent-customization/custom-instructions)

#### 3.2 Path-specific instructions — `.github/instructions/**/*.instructions.md`

- YAML frontmatter: `applyTo` (required glob, comma-separated multi-patterns e.g. `"**/*.ts,**/*.tsx"`), optional `excludeAgent` (`"code-review"` | `"cloud-agent"`), `name`, `description`. (https://code.visualstudio.com/docs/agent-customization/custom-instructions)
- Honored by Chat (auto-injected when `applyTo` matches context), code review (since 2025-11-12), coding agent (since 2025-07-23), CLI. (https://github.blog/changelog/2025-11-12-copilot-code-review-and-coding-agent-now-support-agent-specific-instructions/; https://github.blog/changelog/2025-07-23-github-copilot-coding-agent-now-supports-instructions-md-custom-instructions/)
- VS Code settings: `chat.instructionsFilesLocations` (default `.github/instructions` on, `~/.claude/rules` off), `chat.includeApplyingInstructions` (default true). (https://code.visualstudio.com/docs/agents/reference/copilot-settings)

#### 3.3 AGENTS.md in Copilot

- GA. Coding agent support since 2025-08-28: root `AGENTS.md` = primary instructions, nested = additional. VS Code: `chat.useAgentsMdFile` default true; nested files behind `chat.useNestedAgentsMdFiles` (default false). CLI reads `AGENTS.md` from repo root, cwd, and `COPILOT_CUSTOM_INSTRUCTIONS_DIRS`. Also reads `CLAUDE.md` and `GEMINI.md` as equivalents (VS Code: `chat.useClaudeMdFile` default true). (https://github.blog/changelog/2025-08-28-copilot-coding-agent-now-supports-agents-md-custom-instructions/; https://code.visualstudio.com/docs/agents/reference/copilot-settings; https://docs.github.com/en/copilot/how-tos/copilot-cli/customize-copilot/add-custom-instructions)
- Documented precedence: personal > path-specific `.instructions.md` > `.github/copilot-instructions.md` > agent-specific (`AGENTS.md`/`CLAUDE.md`/`GEMINI.md`) > organization (org instructions GA 2026-04-02). All applicable levels are combined. (https://docs.github.com/copilot/concepts/about-customizing-github-copilot-chat-responses; https://github.blog/changelog/2026-04-02-copilot-organization-custom-instructions-are-generally-available/)

#### 3.4 Skills in Copilot

- Discovery (verbatim-verified): project `.github/skills`, `.claude/skills`, `.agents/skills`; personal `~/.copilot/skills`, `~/.agents/skills`. Org/enterprise scopes "coming soon." (https://docs.github.com/en/copilot/concepts/agents/about-agent-skills)
- VS Code additionally defaults `chat.agentSkillsLocations` to include `~/.claude/skills` (and `.claude/skills`); master switch `chat.useAgentSkills` (default true). **[VS Code docs; the github.com personal-dirs list does not include `~/.claude/skills`]** (https://code.visualstudio.com/docs/agent-customization/agent-skills)
- Surfaces: Copilot cloud agent, code review, Copilot CLI, GitHub Copilot app, VS Code agent mode. (https://docs.github.com/en/copilot/concepts/agents/about-agent-skills)
- Frontmatter honored (VS Code docs): `name`, `description` (the only discovery-time signal), `license`, `allowed-tools`, `argument-hint`, `user-invocable`, `disable-model-invocation`, `context: "fork"` (experimental, behind `github.copilot.chat.skillTool.enabled`). (https://code.visualstudio.com/docs/agent-customization/agent-skills)
- Distribution: `gh skill` (search/preview/install/update/publish) — public preview, GitHub CLI ≥ 2.90.0. (https://github.com/github/awesome-copilot/blob/main/docs/README.skills.md)

#### 3.5 Custom agents in Copilot

- Format: `.agent.md` files. Repo: `.github/agents/<name>.agent.md`; org/enterprise: `agents/` in `.github-private`; user: `~/.copilot/agents/`. (https://docs.github.com/en/copilot/reference/custom-agents-configuration)
- Frontmatter: `name` (optional), `description` (required), `tools` (list or `["*"]`; aliases `execute`, `read`, `edit`, `search`, `agent`, `web`, `todo`), `model`, `target` (`vscode` | `github-copilot`), `user-invocable`, `disable-model-invocation`, `mcp-servers` (GitHub.com only), `metadata`; deprecated `infer`. VS Code adds `argument-hint`, `agents`, `handoffs`, `hooks` (preview). **Body max 30,000 characters.** (https://docs.github.com/en/copilot/reference/custom-agents-configuration; https://code.visualstudio.com/docs/agent-customization/custom-agents)
- Chat modes are dead: "Custom agents were previously known as custom chat modes" — rename `.chatmode.md` → `.agent.md`. (https://code.visualstudio.com/docs/agent-customization/custom-agents)
- **VS Code also detects `.md` files in `.claude/agents/`, "following the Claude sub-agents format"** — the one cross-client agent surface. (https://code.visualstudio.com/docs/agent-customization/custom-agents)
- AGENTS.md ≠ `.agent.md`: the former is injected context; the latter is a named, selectable specialist profile. (https://docs.github.com/en/copilot/how-tos/copilot-cli/customize-copilot/add-custom-instructions)

---

### 4. Claude Code reference points (needed for the cross-client picture)

#### 4.1 Skills (verbatim-verified, https://code.claude.com/docs/en/skills)

- Locations: enterprise (managed settings), personal `~/.claude/skills/`, project `.claude/skills/`, plugin `<plugin>/skills/`. **Claude Code does not scan `.agents/skills/` or `.github/skills/`.** Same-name precedence: **enterprise > personal > project** (note: opposite of the agentskills.io guide's "project over user" convention).
- Monorepo discovery: `.claude/skills/` in every parent dir up to repo root, plus nested `.claude/skills/` discovered on demand; `--add-dir` directories load skills too.
- Custom commands merged into skills (`.claude/commands/deploy.md` ≡ `.claude/skills/deploy/SKILL.md`).
- Frontmatter: **all fields optional** (name defaults to directory name; description falls back to first body paragraph). Fields: `name`, `description`, `when_to_use`, `argument-hint`, `arguments`, `disable-model-invocation`, `user-invocable`, `allowed-tools`, `disallowed-tools`, `model`, `effort`, `context: fork`, `agent`, `hooks`, `paths` (glob-gated activation), `shell`. String substitutions: `$ARGUMENTS`, `$ARGUMENTS[N]`, `$N`, `$name`, plus `` !`command` `` dynamic injection. `description` + `when_to_use` truncated at **1,536 chars** in the skill listing.
- Claude Code states it follows the agentskills.io standard and extends it.

#### 4.2 Rules

- Reads `CLAUDE.md` (+ `@import` syntax, path-specific rules in `.claude/rules/` with `paths:` globs — https://code.claude.com/docs/en/memory). **No native AGENTS.md support as of June 2026**; feature requests open since Aug 2025 (https://github.com/anthropics/claude-code/issues/6235, 5,200+ reactions) and Mar 2026 (https://github.com/anthropics/claude-code/issues/34235). Verified workarounds: `@AGENTS.md` import inside CLAUDE.md, or symlink. The circulating "reads AGENTS.md as fallback" claim is **debunked**. (https://gist.github.com/yurukusa/d36197848911f025add142abefcde685)

---

## Cross-Client Constraints (authoring rules for a portable artifact)

### Skills (best portability)

1. **Place project skills in `.claude/skills/<name>/SKILL.md`** — the only project directory all three clients scan (Claude native; OpenCode compat path; Copilot documented path). `.agents/skills/` covers OpenCode + Copilot but **not** Claude Code. For personal scope there is no single guaranteed directory: `~/.claude/skills/` covers Claude + OpenCode + VS Code (default setting) but not Copilot cloud agent/CLI (`~/.copilot/skills`, `~/.agents/skills`).
2. **Portable frontmatter core = `name` + `description`.** Conform `name` to the strictest rule set (≤64, lowercase/digits/hyphens, no edge/double hyphens, == directory name); pack activation keywords into `description` (≤1024 chars; Claude truncates listing text at 1,536 incl. `when_to_use`). Write the description in third person, stating what + when. (https://agentskills.io/specification; https://platform.claude.com/docs/en/agents-and-tools/agent-skills/best-practices)
3. **Vendor fields degrade silently** (all three ignore unknown keys): Claude-only `when_to_use`, `paths`, `disallowed-tools`, `model`, `effort`, `agent`, `hooks`, `shell`, `arguments`, `$ARGUMENTS` substitution, `` !`cmd` `` injection — none are processed elsewhere ($-placeholders/backtick commands appear as **literal text** in other clients; never make correctness depend on them). `disable-model-invocation` / `user-invocable` / `context: fork` exist in Claude and VS Code Copilot but not OpenCode. `allowed-tools` is experimental everywhere — treat as a hint, not a guarantee; a skill must remain safe if every tool call prompts for permission.
4. **Behavior must live in the markdown body + spec directories**: instructions in prose, executables in `scripts/` (self-contained, non-interactive, documented deps), docs in `references/`, templates in `assets/`, references one level deep, body < 500 lines / < 5,000 tokens. No client auto-runs scripts; all route them through their shell tool + permission system.
5. Use `metadata:` for grim/vendor bookkeeping (e.g. `metadata: {version: "1.0"}`) and `compatibility:` only when the skill genuinely needs a specific environment.
6. **Do not rely on cross-level collision precedence** — Claude resolves personal-over-project; the spec guide describes project-over-user as convention; OpenCode keeps first-found. Unique names are the only portable strategy.

### Rules / instructions (no single portable file)

7. There is **no one file all three load natively in the same role**. Closest approximations: (a) maintain `AGENTS.md` (OpenCode native + Copilot all surfaces) and import it from `CLAUDE.md` via `@AGENTS.md` for Claude; or (b) ship `CLAUDE.md` (Claude native; OpenCode fallback **only if no AGENTS.md exists**; Copilot reads it as agent-specific instructions). A package manager targeting all three (Grimoire's case) should write per-client native files rather than betting on either.
8. **Glob-scoped activation does not port.** Claude `paths:` (rules/skills) ↔ Copilot `applyTo:` in `.github/instructions/*.instructions.md` ↔ OpenCode (nothing; everything is always-on). A scoped rule must be re-materialized per client; for OpenCode either inline it (always-on cost) or convert it to a skill (on-demand, but activation becomes model-discretionary).
9. **Size budgets**: write rules assuming the worst consumer — Copilot code review reads only the first 4,000 chars; long files (>~1,000 lines) degrade instruction-following on all Copilot surfaces; OpenCode pays the full token cost every loop with no guard. Short imperative bullets, most-critical-first, no external URLs as normative content (Copilot does not fetch them).

### Agents (assume non-portable)

10. Three incompatible formats: Claude `.claude/agents/*.md`, Copilot `.github/agents/*.agent.md` (30k-char body cap, `tools` as a list with Copilot aliases), OpenCode `.opencode/agents/*.md` (`mode`, `permission` map). The only sharing: **VS Code Copilot also reads `.claude/agents/*.md`** (Claude sub-agent format). OpenCode reads neither foreign path. Portable strategy: keep the agent's prompt body vendor-neutral and generate the three frontmatter envelopes per client; only `description` is conceptually common (and it is the required field in all three).

### Activation-model map

| Model | Claude Code | OpenCode | Copilot |
|---|---|---|---|
| Always-on | CLAUDE.md (+imports) | AGENTS.md / CLAUDE.md / `instructions[]` | copilot-instructions.md, AGENTS.md/CLAUDE.md/GEMINI.md |
| Glob-scoped | `.claude/rules` + skill `paths:` | — (none) | `.instructions.md` `applyTo:` |
| On-demand (progressive) | skills | skills (`skill` tool) | skills |
| Manual-only | `/skill` + `disable-model-invocation` | `@agent` mention | `/skill`, `@agent`, `disable-model-invocation` |

---

## Canonical Further-Reading Links (annotated)

| Link | Why it matters |
|---|---|
| https://agentskills.io/specification | The normative SKILL.md spec — field table, directory semantics, 500-line guidance. Verified verbatim 2026-06-12. |
| https://agentskills.io/client-implementation/adding-skills-support.md | Compliance requirements, lenient-validation rules, trust model — what a *client* (like grim's install targets) must do. |
| https://github.com/agentskills/agentskills | Spec governance, `skills-ref` validator — the tool to validate grim-published skills. |
| https://code.claude.com/docs/en/skills | Claude Code's full 16-field frontmatter extension set + discovery/precedence — the superset to degrade from. |
| https://platform.claude.com/docs/en/agents-and-tools/agent-skills/best-practices | Anthropic authoring guidance: third-person descriptions, progressive disclosure, one-hop references. |
| https://opencode.ai/docs/skills/ | OpenCode's six skill scan paths incl. `.claude/skills/` compat; "unknown fields are ignored." |
| https://opencode.ai/docs/rules/ + https://opencode.ai/docs/config/ + https://opencode.ai/docs/agents/ | AGENTS.md/CLAUDE.md fallback chain, `instructions[]` globs/URLs, agent markdown format + permission model. |
| https://docs.github.com/en/copilot/concepts/agents/about-agent-skills | Copilot's official skill scan dirs and supported surfaces. |
| https://code.visualstudio.com/docs/agent-customization/agent-skills | Richest Copilot skill frontmatter doc (incl. `context: fork`, settings). |
| https://docs.github.com/en/copilot/reference/custom-agents-configuration | `.agent.md` schema, tool aliases, 30k-char cap. |
| https://code.visualstudio.com/docs/agent-customization/custom-agents | chatmode→agent migration; `.claude/agents/` cross-read. |
| https://docs.github.com/en/copilot/customizing-copilot/adding-custom-instructions-for-github-copilot | Instruction-type precedence and coexistence rules. |
| https://github.blog/ai-and-ml/github-copilot/unlocking-the-full-power-of-copilot-code-review-master-your-instructions-files/ | The 4,000-char code-review limit + documented instruction failure modes. |
| https://agents.md | AGENTS.md standard site (Linux Foundation/AAIF orbit, 60k+ projects, adopter list). |
| https://gist.github.com/yurukusa/d36197848911f025add142abefcde685 | Claude×AGENTS.md interop ground truth; debunks the fallback myth. |

## Durable Search Terms

- `agentskills.io specification SKILL.md frontmatter` / `agentskills spec allowed-tools experimental`
- `skills-ref validate agentskills`
- `opencode docs skills ".claude/skills" compatibility` / `opencode AGENTS.md rules instructions array` / `opencode agents mode subagent permission`
- `OPENCODE_DISABLE_CLAUDE_CODE skills`
- `copilot "about agent skills" site:docs.github.com` / `chat.agentSkillsLocations` / `gh skill install preview`
- `".instructions.md" applyTo excludeAgent copilot` / `copilot-instructions.md 4000 characters code review`
- `".agent.md" custom agents configuration site:docs.github.com` / `chatmode.md renamed custom agents vscode`
- `copilot coding agent AGENTS.md changelog` / `organization custom instructions GA copilot`
- `claude code skills frontmatter "disable-model-invocation" "context: fork" paths`
- `claude code AGENTS.md issue 6235 34235`

## Sources

Primary (verified by direct fetch 2026-06-12): agentskills.io/specification; opencode.ai/docs/skills/; opencode.ai/docs/agents/; code.claude.com/docs/en/skills; docs.github.com/en/copilot/concepts/agents/about-agent-skills.

Secondary (vendor docs/blogs via search agents): agentskills.io/client-implementation/adding-skills-support.md; agentskills.io/skill-creation/{best-practices,using-scripts}.md; github.com/agentskills/agentskills; anthropic.com/engineering/equipping-agents-for-the-real-world-with-agent-skills; platform.claude.com/docs/en/agents-and-tools/agent-skills/best-practices; opencode.ai/docs/{rules,config,permissions}/; docs.github.com/en/copilot/{customizing-copilot/adding-custom-instructions-for-github-copilot, reference/custom-agents-configuration, concepts/agents/cloud-agent/about-custom-agents, how-tos/copilot-cli/customize-copilot/*, how-tos/copilot-on-github/customize-copilot/customize-cloud-agent/add-skills, tutorials/use-custom-instructions}; code.visualstudio.com/docs/{agent-customization/{custom-agents,custom-instructions,agent-skills}, agents/reference/copilot-settings}; github.blog changelogs 2025-07-23, 2025-08-28, 2025-11-12, 2026-04-02; github.blog code-review instructions post; agents.md; simonwillison.net/2025/Dec/19/agent-skills/.

Tertiary / medium-confidence (community or open issues, flagged inline): deepwiki.com/sst/opencode/{5.7-skills-system,3.1-configuration-structure}; gist.github.com/rmk40/cde7a98c1c90614a27478216cc01551f; gist.github.com/yurukusa/d36197848911f025add142abefcde685; opencode issues #4483, #4758, #6266, #9282, #11534, #12432, #18037, #22020; anthropics/claude-code issues #6235, #34235; github.com/github/awesome-copilot.
