# Research v2 — Project install-state storage (devcontainerId keying vs relocation)

> Round-2 research triggered by maintainer pushback during `/swarm-execute`:
> "could we key state by `${devcontainerId}` and keep it in `$GRIM_HOME`?" +
> "too many files in the workspace root; a `.grimoire/` dir feels odd too."
> Four parallel axes (devcontainer-id, layout-conventions, shared-home,
> codebase) + opus synthesis. Source workflow: `wf_52de9926-991`.

## TL;DR decision

**Confirm the existing ADR: relocate project state to `<workspace>/.grimoire/state.json`
(a tool-managed dot-directory) + anchor-relativize stored target paths.** Two
refinements added by this research:

1. **Bundle `GRIM_STATE_DIR`** (component-level override, mise/Poetry pattern) as
   an *optional* escape hatch — closes the only real weakness (read-only
   `/workspace` in CI) for ~1 env accessor + 1 branch. Replaces deferred Q4.
2. **Self-managed `.grimoire/.gitignore`** (containing `*`), written by grim on
   first project install — the user never edits their root `.gitignore`; the dir
   reads as "grim scratch space," not three loose root files. Neutralizes the
   clutter concern. Replaces Q1.

**devcontainerId keying (the maintainer's idea) is rejected for project scope** —
see below — but **reserved for the deferred `global.json` follow-up** where it is
genuinely the right tool.

## Why devcontainerId does NOT solve the collision

`${devcontainerId}` = `base32(SHA-256(JSON({devcontainer.local_folder,
devcontainer.config_file})))` — computed by devcontainer **tooling**, not Docker.

- It hashes the **host** workspace path + config path. Two containers that mount
  the *same host dir* at `/workspace` get the **same** id — identical collision to
  today's `sha256(canonical_config_path)`. It only disambiguates when the *host
  source dirs differ*.
- It is **not auto-injected** into the process environment — requires explicit
  `containerEnv: { "X": "${devcontainerId}" }` in `devcontainer.json`. Plain
  `docker run` never has it. ⇒ keying on it = **mandatory per-container config** for
  the common case.
- Zero-config fallbacks are disqualified: `/etc/machine-id` is shared from the
  image; `/proc/self/cgroup` is empty on cgroup v2 (Ubuntu 22.04+, WSL2);
  `$HOSTNAME` is rebuild-unstable and becomes `docker-desktop`/host-FQDN under
  `network_mode: host`.
- **Re-keying changes nothing about the stored absolute target paths.** Drift
  detection (`ClientRecord::current_hash`, install_state.rs:70) opens
  `/home/<user>/...` directly and breaks under a different `$HOME`. So Y needs
  **both** a 4-file keying change **and** the full anchoring rework — strictly more
  work than relocation, for worse ergonomics.

## Two tangled defects (score separately)

1. **Key collision** — *which* file (`<sha>.json`) the project lands in.
2. **Path portability** — *what's inside*: `ClientRecord.target`/`support_dir` are
   absolute `PathBuf`s, stale under a different `$HOME`.

Only **relocation onto the `/workspace` bind mount** fixes (1) *and* makes
project-scope targets uniform (`/workspace/.claude/...` everywhere), shrinking (2)
to "just the workspace anchor." Every other design still needs anchor+relative for
global / `$HOME`-rooted records. No project-scope design touches `global.json`
(multi-machine `--global` last-writer-wins remains the honest residual).

## Comparison matrix (opus synthesis)

`++` strong / `+` ok / `~` conditional / `−` weak / `−−` blocking

| Axis | X1 root file | **X2 `.grimoire/` dir** | Y env-keyed id | Z stateless | W `GRIM_STATE_DIR` |
|---|---|---|---|---|---|
| Collision fix | ++ | **++** | + (devcontainerId collides) | ++ | + (only if set) |
| Root clutter | − (3rd root file) | **+ (one dot-dir, self-ignore)** | ++ | ++ | ~ |
| Read-only `/workspace` | − (needs W) | **− (needs W)** | ++ | ++ | ++ (its job) |
| Global sharing (must keep) | ++ | **++** | ++ | ++ | ++ |
| Cross-container portability | + | **+** (project uniform; global needs anchor) | − (absolutes unchanged) | ~ | − |
| Migration / blast radius | + (2 sites) | **+ (2 sites)** | − (4 files) | −− (delete subsystem) | + (1 accessor) |
| Zero-config devcontainer? | ++ | **++** | −− (containerEnv) | ++ | − (per-container set) |
| Still needs anchoring? | partial | **partial** | yes (full) | sidesteps | yes |

X2 is the only design that is both **zero-config** and **root-clean**.

## Key precedents (Axis B / C)

- **Committed lock + gitignored sibling state at root is the dominant pattern**:
  Terraform (`.terraform.lock.hcl` committed + `.terraform/`/`terraform.tfstate`
  gitignored), mise (`.mise.toml` + `.mise.local.toml`), direnv, JetBrains, uv,
  pixi. ⇒ keep `grimoire.toml` + `grimoire.lock` at root (committed); only
  machine-local state goes in `.grimoire/`. **Do not move the committed files into
  the dot-dir.**
- **Dot-DIR for tool-managed local state is standard**: `.git/`, `.terraform/`,
  `.pixi/`, `.venv/`, `.idea/`. Not "odd."
- **Self-managed `.gitignore`** inside the dir: uv writes `.venv/.gitignore`, pixi
  writes `.pixi/.gitignore`. Adopt `.grimoire/.gitignore` = `*`.
- **XDG**: `XDG_STATE_HOME` is explicitly "not portable" — install-materialization
  facts are state, must not ride a shared volume. `$GRIM_HOME/blobs/` is the
  `/nix/store` analog (content-addressed, legitimately shareable).
- **Poetry & Bazel** hit the exact "same path, different machine" collision; both
  converged on workspace-local state or a flag/env escape hatch — **never**
  env-injected identity as the primary mechanism.
- **Named volume shadows a bind mount** at the same path → if a consumer mounts a
  named volume at `/workspace/.grimoire`, the bind-mounted `state.json` becomes
  invisible. Documentation-only risk; `.grimoire/` is not write-heavy so there's no
  performance reason to do so.

## Codebase grounding (Axis D)

- `$GRIM_HOME` resolves at `src/env.rs:26-34` (`$GRIM_HOME` > `$HOME/.grimoire` >
  `.grimoire`). No CLI/config layering. Captured once in `Context::new()`
  (`src/context.rs:83`).
- Project state path computed at **one chokepoint**:
  `src/command/scope_resolution.rs:80` →
  `InstallState::project_path(&paths.state_dir(), &canonical)` →
  `$GRIM_HOME/state/projects/<sha256(canonical)>.json`
  (`src/install/install_state.rs:142-147`).
- `state_path` flows opaquely through `ResolvedScope` to 8 consumers (install,
  update, uninstall, status, search, tui ×3). They never reconstruct the path.
- **Blast radius — X2 (relocate)**: 2 code sites (`install_state.rs:project_path`
  signature + `scope_resolution.rs:80`) + dir-create + self-managed gitignore.
  **No** `Context`/`env.rs`/`GrimPaths` change.
- **Blast radius — Y (env-key)**: 4 files (`env.rs`, `context.rs` +
  `Context::hermetic`, `install_state.rs`, `scope_resolution.rs`) + env-var docs.
- Env-override pattern (for `GRIM_STATE_DIR`): `const KEY` → typed
  `pub fn` calling `non_empty_var()` → field in `Context::new()` → accessor;
  mirror in `Context::hermetic()`.
- New-env wiring for `GRIM_STATE_DIR` is consulted at the `scope_resolution.rs:80`
  chokepoint only (read first; fall back to `<workspace>/.grimoire/`).

## Recommendation (verbatim, opus)

> Adopt **X2 + anchoring**, bundled with **W (`GRIM_STATE_DIR`)** as an optional
> override. Relocate project install-state to `<workspace>/.grimoire/state.json`
> (dot-directory, not a loose root file), write a self-managed `.grimoire/.gitignore`
> = `*`, and replace absolute `ClientRecord.target`/`support_dir` with an
> `(anchor, relative)` encoding resolved + containment-validated at read time. Add a
> `GRIM_STATE_DIR` override consulted first, falling back to `<workspace>/.grimoire/`.
> `$GRIM_HOME` (shared global state + content store) untouched — the must-stay-shared
> goal. Env-keyed identity (Y) is strictly dominated for project scope (still needs
> anchoring, still leaves absolutes broken, uniquely needs `containerEnv`); reserve
> Y for the deferred `global.json` follow-up. Stateless (Z) is the elegant end-state
> blocked today by the orphan-uninstall gap + a lock carrying no client-target
> mapping; relocation is a prerequisite step toward it, not a competitor. This
> confirms and sharpens `adr_install_state_portability.md` rather than overturning it.

## Net effect on the plan

- The accepted ADR Option 1 (relocate + anchor) **stands**.
- **Q1 (auto-gitignore)** → resolved: grim writes a self-managed `.grimoire/.gitignore`
  = `*`; never edits the root `.gitignore`.
- **Q4 (read-only workspace)** → resolved: **bundle `GRIM_STATE_DIR`** now (cheap,
  closes the CI gap) instead of deferring.
- **Q3 (global residual)** → keep `global.json` shared (matches the user's goal);
  document last-writer-wins; the devcontainerId/`GRIM_MACHINE_ID` keying idea is the
  right tool for *that* follow-up, not project scope.
- Dot-dir filename detail: state file `<workspace>/.grimoire/state.json` (keeps room
  for future machine-local siblings under the same namespace).
