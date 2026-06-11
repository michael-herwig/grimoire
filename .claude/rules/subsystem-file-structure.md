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

Support directory files are copied verbatim for all three clients. Only
the index is ever transformed.

### Global-scope paths

For a **global-scope** install (`--global`), grim writes into each
client's **native** user-level discovery directory rather than under
`$GRIM_HOME`, so the files are found without extra configuration:

| Client | Skills root | Rules path |
|--------|-------------|------------|
| **Claude** | `~/.claude/skills/<name>/` | `~/.claude/rules/<name>.md` |
| **OpenCode** | `$XDG_CONFIG_HOME/opencode/skills/<name>/` | `$GRIM_HOME/.opencode/rules/<name>.md` (absolute glob registered in global `opencode.json`) |
| **Copilot** | `~/.copilot/skills/<name>/` | `$GRIM_HOME/.github/instructions/<name>.instructions.md` (inert — no documented user-level instructions path; grim warns) |

`$XDG_CONFIG_HOME` falls back to `~/.config` when unset.

**Vendor env overrides** (each client's own variable; the three directory
variables are honored read-only, `OPENCODE_CONFIG` names a file grim reads
**and** rewrites; empty value = unset):

| Variable | Effect on global paths |
|----------|------------------------|
| `CLAUDE_CONFIG_DIR` | Replaces the entire `~/.claude` tree — Claude skills **and** rules root there |
| `COPILOT_HOME` | Replaces `~/.copilot` — Copilot skills land in `$COPILOT_HOME/skills/` |
| `OPENCODE_CONFIG_DIR` | OpenCode's additive scan dir — preferred over the XDG default for skills when set |
| `OPENCODE_CONFIG` | Config **file** path only (global `opencode.json` edit target); no effect on skill paths |

**Fallback**: env override → native default (`$HOME`-derived) → workspace
layout under `$GRIM_HOME` for the affected client. The recorded install
path is always absolute, so uninstall and integrity checking are
unaffected regardless of which path was chosen at install time.

## Client Detection (default install targets)

When neither `--client` nor the config `[options].clients` selects a
client, `install` / `update` / TUI target **all detected clients**. A
client is detected when its vendor directory / config marker is present
for the active scope:

| Client | Project signal | Global signal |
|--------|----------------|---------------|
| **Claude** | `<workspace>/.claude` | native root (`$CLAUDE_CONFIG_DIR` or `~/.claude`) exists |
| **OpenCode** | `<workspace>/.opencode` | native skills root (`$OPENCODE_CONFIG_DIR` or `$XDG_CONFIG_HOME/opencode/skills`) exists **or** the resolved global `opencode.json` (`$OPENCODE_CONFIG` / XDG default) exists |
| **Copilot** | a Copilot-specific marker — **not** bare `.github` (nearly every repo carries it for CI): `<workspace>/.github/copilot-instructions.md` or `<workspace>/.github/instructions/` | native skills root (`$COPILOT_HOME` or `~/.copilot`) exists |

Detection lives on the [`Vendor`] trait (`Vendor::detect(workspace,
scope)`), driven by `install::target::detect_clients`, which iterates
`ClientTarget::ALL` so the set is deterministic. When **nothing** is
detected the set falls back to `[claude]` so an install never silently
targets zero clients. An explicit `[options].clients` and the `--client`
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
- `quality-security.md` — path traversal / symlink-escape guards
