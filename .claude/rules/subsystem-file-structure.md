---
paths:
  - src/**
---

# File Structure Subsystem

How Grimoire lays out its on-disk data: where downloaded artifacts, the
local index, and install links live under the data root.

> **Status: provisional.** The storage layer is not implemented yet (only
> `src/main.rs` exists). This file records intent and design constraints,
> not shipped behavior. Do not invent concrete storage internals — fill
> this in as the real design lands.

## Design Rationale (intended)

- **Single data root.** All Grimoire state lives under one directory
  (default `~/.grimoire`, overridable via `GRIM_HOME`). Keeping everything
  under one root makes atomic rename / hardlink operations possible because
  source and destination stay on one filesystem.
- **Content-addressed storage.** Downloaded artifacts are addressed by
  content hash so identical content is stored once and is immutable.
- **Mutable namespace on top.** Human-facing names (tags, "installed"
  links) are a thin mutable layer pointing at immutable content.
- **Cross-device safety.** Operations that rely on same-filesystem
  atomic rename must validate the data root sits on a single volume;
  cross-device hardlink/rename fails and must be handled explicitly.

## Install Layout (client targets)

### Skills

A **skill** materializes as a directory tree under the client's `skills/`
dir. Every file in the tree is copied verbatim, **except** `SKILL.md` when
it carries tool-namespaced metadata keys (e.g. `claude.user-invocable` in
the `metadata` map). In that case `SKILL.md` is **rendered per client**:
known `<client>.<field>` keys are lifted to native typed top-level
frontmatter, foreign-namespace keys are dropped, and the written file is
marked `generated: true`. A plain skill with no tool-namespaced keys takes
the fast path and installs byte-identical. See `arch-principles.md` ADR
index → `adr_tool_namespaced_metadata_rendering.md`.

### Rules

A **rule** materializes as the index `<name>.md` under `rules/`, and —
when the artifact carries an optional sibling support directory — that
directory installs **beside** the index as `rules/<name>/…` so the index's
relative links resolve. The two on-disk roots (index file + sibling dir)
are one footprint: the integrity hash folds both, and uninstall removes
both. See `arch-principles.md` ADR index → `adr_multifile_rules.md`.

**Codex declines rules entirely** (`Vendor::supports_kind(Rule) == false`):
it has no path-scoped instruction mechanism. The installer warns + skips,
writes no file, and records no output for Codex — see `arch-principles.md`
ADR index → `adr_codex_vendor.md`.

Per-client rule transforms:

- **Claude**: `paths:` is native Claude rule frontmatter. A plain rule
  carrying no tool-namespaced metadata keys installs verbatim, marked
  `generated: false` (fast path). A rule that carries any
  `<vendor>.<field>` entry inside its `metadata` map is re-rendered:
  own-namespace Claude keys lift per registry (empty today — unknown ones
  warn + drop), foreign vendor keys drop silently, plain keys survive.
  Written `generated: true`; if cleaned frontmatter is empty, the block
  is omitted entirely.
- **OpenCode**: frontmatter is stripped; the file written is a provenance
  comment followed by the rule body. Marked `generated: true`. Loading is
  wired through a managed glob entry in `opencode.json` (or `opencode.jsonc`
  when present). grim adds the entry when the first OpenCode rule installs
  and removes it when the last one uninstalls; the target file is
  `.opencode/rules/<name>.md`.
- **Copilot**: written to `.github/instructions/<name>.instructions.md`.
  Frontmatter maps `paths` → `applyTo` (comma-joined into a single string)
  and the optional `copilot.exclude-agent` key (authored in rule `metadata`)
  → `excludeAgent` (enum: `code-review` or `cloud-agent`). A rule with
  neither produces no frontmatter block at all. Marked `generated: true`.

Support directory files are copied verbatim for all three rule-supporting
clients (Claude, OpenCode, Copilot — Codex declines rules). Only the index
is ever transformed.

### Agents {#install-layout-agents}

An **agent** materializes as a single file in the client's agents
directory (no support directory). For Claude/OpenCode/Copilot it is a
Markdown file (Claude installs a plain agent verbatim; OpenCode and Copilot
project the frontmatter). For **Codex** it is a **TOML** file
(`<name>.toml`) — Codex is the only TOML-emitting vendor: the canonical
`name`/`description` plus the agent body (as `developer_instructions`) and
an optional `model` are serialized to TOML; the `tools` field has no Codex
equivalent and is dropped with a warning.

Per-client agent paths:

| Client | Global agent path |
|--------|-------------------|
| **Claude** | `<claude_root>/agents/<name>.md` |
| **Copilot** | `<copilot_root>/agents/<name>.md` |
| **OpenCode** | `<opencode_root>/agents/<name>.md` |
| **Codex** | `<codex_root>/agents/<name>.toml` |

`opencode_root` is the parent of the OpenCode skills directory (i.e. the
directory one level above the `skills/` subdir resolved from
`$OPENCODE_CONFIG_DIR` or the XDG default). `claude_root` and
`copilot_root` are the vendor roots described in the global-scope table
below. `codex_root` is `$CODEX_HOME` else `~/.codex`.

### Global-scope paths {#global-scope-paths}

For a **global-scope** install (`--global`), grim writes into each
client's **native** user-level discovery directory rather than under
`$GRIM_HOME`, so the files are found without extra configuration:

| Client | Skills root | Rules path | Agents path |
|--------|-------------|------------|-------------|
| **Claude** | `~/.claude/skills/<name>/` | `~/.claude/rules/<name>.md` | `~/.claude/agents/<name>.md` |
| **OpenCode** | `$XDG_CONFIG_HOME/opencode/skills/<name>/` | `$GRIM_HOME/.opencode/rules/<name>.md` (absolute glob registered in global `opencode.json`) | `$XDG_CONFIG_HOME/opencode/agents/<name>.md` |
| **Copilot** | `~/.copilot/skills/<name>/` | `$GRIM_HOME/.github/instructions/<name>.instructions.md` (inert — no documented user-level instructions path; grim warns) | `~/.copilot/agents/<name>.md` |
| **Codex** | `$HOME/.agents/skills/<name>/` (cross-vendor standard; independent of `$CODEX_HOME`) | **unsupported** — Codex has no path-scoped rule mechanism; grim warns + skips, writes no file | `$CODEX_HOME`\|`~/.codex/agents/<name>.toml` (TOML) |

`$XDG_CONFIG_HOME` falls back to `~/.config` when unset.

**Vendor env overrides** (each client's own variable; the four directory
variables are honored read-only, `OPENCODE_CONFIG` names a file grim reads
**and** rewrites; empty value = unset):

| Variable | Effect on global paths |
|----------|------------------------|
| `CLAUDE_CONFIG_DIR` | Replaces the entire `~/.claude` tree — Claude skills, rules, and agents root there |
| `COPILOT_HOME` | Replaces `~/.copilot` — Copilot skills and agents land under `$COPILOT_HOME/` |
| `OPENCODE_CONFIG_DIR` | OpenCode's additive scan dir — preferred over the XDG default for skills and agents when set |
| `OPENCODE_CONFIG` | Config **file** path only (global `opencode.json` edit target); no effect on skill/agent paths |
| `CODEX_HOME` | Replaces `~/.codex` — Codex **agents** root there. Does **not** relocate Codex skills (those follow the `$HOME/.agents/skills` cross-vendor standard) |

**Fallback**: env override → native default (`$HOME`-derived) → workspace
layout under `$GRIM_HOME` for the affected client.

## Install State {#install-state}

Grimoire records what it installed, where, and at what content hash. The
record location differs by scope.

### Project state {#install-state-project}

Project install state lives at `<workspace>/.grimoire/state.json` — inside
a `.grimoire/` directory co-located with `grimoire.toml`. The workspace
directory is the key; there is no content hash of the config path in the
filename. Each project has exactly one state file at this fixed location,
so two projects sharing a common ancestor (or a shared `GRIM_HOME` volume)
cannot collide.

**Self-managed `.gitignore`**: the first time grim creates the `.grimoire/`
directory it writes `.grimoire/.gitignore` with contents `*` (if absent —
never overwrites a user-edited one). The consumer's root `.gitignore` is
never touched. This mirrors the convention used by [uv] (`.venv/.gitignore`)
and [pixi] (`.pixi/.gitignore`): the tool owns its dot-dir and excludes
its own contents from version control.

**Devcontainer named-volume caution**: if a devcontainer mounts a named
Docker volume at `<workspace>/.grimoire`, that volume shadows the
bind-mounted workspace state. A `grim install` inside the container writes
to the named volume, which is invisible to the host and to other containers
that bind-mount the same workspace directory. Use a bind-mount (not a
named volume) at `<workspace>/.grimoire` if you need state to be shared.

**Non-UTF-8 path components**: any path component that is not valid UTF-8
is rejected at store time with `UnknownAnchor`. All anchor roots must be
representable as UTF-8.

**Reap window**: between the read-only `load()` in a first post-upgrade
`status` call and the mutating `save()` in the next mutating command, the
old legacy state file (`$GRIM_HOME/state/projects/<sha>.json`) still
exists. A concurrent observer looking at the legacy path during this window
may see the old file even though the in-memory view is already migrated.
This is transient; the next mutating command reaps the legacy file.

**Nesting constraint**: `GRIM_HOME` must not be nested inside a workspace
directory. If it is, `from_target` may match `GrimHome` as the anchor for
a path that should classify as `Workspace`, producing an incorrect record.

### Global state {#install-state-global}

Global install state lives at `$GRIM_HOME/state/global.json`. This
location is unchanged from previous versions.

**Residual risk under a shared GRIM_HOME**: when two machines or containers
share the same `GRIM_HOME` volume, both read and write the same
`global.json`. Anchoring makes the stored *paths* portable (each machine
resolves anchor roots from its own environment), but the *record set* in
the file is shared. Concurrent or serial `grim install --global` calls
from different machines are last-writer-wins on the record set — the same
class of collision that project state now avoids. **v1 stance: single
writer at a time.** Per-host segmentation (keyed by a machine identity
such as `devcontainerId`) is a tracked follow-up, not part of this change.
`atomic_write` prevents partial-file corruption; only record-set
last-writer-wins is a residual risk.

### PathAnchor set {#path-anchor-set}

Stored paths are anchor-relative rather than absolute, so a state file
written on one machine resolves correctly on another (portable `$HOME`,
devcontainer portability). Every stored path carries an `anchor` tag and a
`relative` string (forward-slash UTF-8, Normal components only — no `.`,
`..`, leading `/`, or drive prefix).

| Anchor | Resolved root |
|--------|---------------|
| `Workspace` | The workspace directory passed to the CLI |
| `ClaudeRoot` | `$CLAUDE_CONFIG_DIR` else `~/.claude` |
| `CopilotRoot` | `$COPILOT_HOME` else `~/.copilot` |
| `OpenCodeSkills` | `$OPENCODE_CONFIG_DIR/skills` else `$XDG_CONFIG_HOME/opencode/skills` |
| `OpenCodeRoot` | Parent of the `OpenCodeSkills` root (the directory one level above `skills/`) |
| `GrimHome` | `$GRIM_HOME` |
| `AgentsSkills` | `$HOME/.agents/skills` (Codex skills; cross-vendor standard, **not** under `$CODEX_HOME`) |
| `CodexRoot` | `$CODEX_HOME` else `~/.codex` (hosts Codex `agents/`) |

All roots are resolved once at scope-resolution time and passed as an
`AnchorRoots` struct so every downstream operation is a pure table-lookup
with no ambient environment access.

### Anchor root/remainder table {#anchor-remainder-table}

Authoritative mapping from `(scope, client, kind)` to `(anchor, stored relative)`:

| Scope · client · kind | Anchor | Stored `relative` |
|---|---|---|
| project · any · any | `Workspace` | `.claude/…`, `.opencode/…`, `.github/…`, `.agents/…`, `.codex/…` (full sub-path from workspace) |
| global · claude · skill | `ClaudeRoot` | `skills/<name>` |
| global · claude · rule | `ClaudeRoot` | `rules/<name>.md` |
| global · claude · agent | `ClaudeRoot` | `agents/<name>.md` |
| global · copilot · skill | `CopilotRoot` | `skills/<name>` |
| global · copilot · agent | `CopilotRoot` | `agents/<name>.md` |
| global · opencode · skill | `OpenCodeSkills` | `<name>` (root already ends `/skills`) |
| global · opencode · agent | `OpenCodeRoot` | `agents/<name>.md` |
| global · opencode · rule | `GrimHome` | `.opencode/rules/<name>.md` |
| global · copilot · rule | `GrimHome` | `.github/instructions/<name>…` (inert) |
| global · codex · skill | `AgentsSkills` | `<name>` (root already ends `/skills`) |
| global · codex · agent | `CodexRoot` | `agents/<name>.toml` |
| · codex · rule | — | **not classified** — declined at the `supports_kind` gate before anchoring; no output recorded |

### Path containment guard {#path-containment-guard}

`AnchoredPath::resolve` enforces containment through two layers before any
filesystem operation runs on the joined path:

**Layer 1 (always, works for absent paths)**: every component of `relative`
must be `Normal`. Any `ParentDir` (`..`), `CurDir` (`.`), `RootDir`, or
`Prefix` component causes an immediate `TraversalAttempt` error without
touching the filesystem.

**Layer 2 (only when the candidate path exists)**: `dunce::canonicalize`
resolves both the candidate path and the anchor root, then asserts
`candidate.starts_with(anchor_root)` at the component boundary. A symlink
inside the anchor pointing outside it yields `EscapedAnchor`.

No consumer joins anchor + relative manually. Every filesystem operation
(read, hash, delete) receives the result of `resolve()`, never the raw
`relative` string.

See `quality-security.md` for the path-traversal and symlink-escape guard
principles that this two-layer pattern implements.

## Client Detection (default install targets) {#client-detection}

When neither `--client` nor the config `[options].clients` selects a
client, `install` / `update` / TUI target **all detected clients**. A
client is detected when its vendor directory / config marker is present
for the active scope:

| Client | Project signal | Global signal |
|--------|----------------|---------------|
| **Claude** | `<workspace>/.claude` | native root (`$CLAUDE_CONFIG_DIR` or `~/.claude`) exists |
| **OpenCode** | `<workspace>/.opencode` | native skills root (`$OPENCODE_CONFIG_DIR` or `$XDG_CONFIG_HOME/opencode/skills`) exists **or** the resolved global `opencode.json` (`$OPENCODE_CONFIG` / XDG default) exists |
| **Copilot** | a Copilot-specific marker — **not** bare `.github` (nearly every repo carries it for CI): `<workspace>/.github/copilot-instructions.md` or `<workspace>/.github/instructions/` | native skills root (`$COPILOT_HOME/skills` or `~/.copilot/skills`) exists — the `skills/` subdir, not the bare `~/.copilot` parent |
| **Codex** | `<workspace>/.codex` — **not** the shared `.agents/skills` dir (a weak cross-vendor marker, like Copilot's bare `.github` caveat) | native config root (`$CODEX_HOME` or `~/.codex`) exists |

Detection lives on the [`Vendor`] trait (`Vendor::detect(workspace,
scope)`), driven by `install::target::detect_clients`, which iterates
`ClientTarget::ALL` so the set is deterministic. When **nothing** is
detected the set falls back to **all** clients so an install never
silently targets zero clients or prefers one. An explicit `[options].clients` and the `--client`
flag both override detection. The detected set is **not** persisted to
config — it is recomputed each run. Detection reuses the same vendor env
overrides documented in the table above.

## Constraints

- Never assume a path operation crosses filesystems silently — check first.
- Treat the content store as append-only / immutable; mutate only the
  name → content mapping.
- Concurrent processes must coordinate via advisory file locks for any
  read-modify-write on shared metadata.

## Cross-References

- `arch-principles.md` — overall architecture and utility discipline
- `quality-security.md` — path traversal / symlink-escape guards (two-layer containment pattern)

<!-- external -->
[uv]: https://docs.astral.sh/uv/
[pixi]: https://pixi.sh/
