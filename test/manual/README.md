# Grimoire Manual Test Rig

A hands-on harness for exercising `grim` against a real local OCI registry
with a committed sample catalog of skills and rules. This is **fully
separate** from the pytest acceptance suite (`test/tests/`): it runs its
own `registry:2` on **`localhost:5050`** (own container + volume), while
the suite uses `localhost:5000`. They are isolated on purpose — sharing
one registry let the suite's hundreds of throwaway `grim-test/<uuid>`
repos bleed into `grim search` / `grim tui` here as junk. It exists so
you can drive the tool by hand and see how it behaves.

Pattern mirrors the OCX manual rig: committed source-of-truth catalog,
idempotent `bootstrap.sh`, an isolated `GRIM_HOME`, a ready-made consumer
project, and a `teardown.sh`.

## Layout

| Path | Purpose |
|------|---------|
| `catalog/skills/<name>/SKILL.md` | Source-of-truth sample skills (committed) |
| `catalog/rules/<name>.md` | Source-of-truth sample rules (committed) |
| `project/grimoire.toml` | Ready-made consumer project (floating `:1` tags) |
| `scripts/env.sh` | `source` it to point `grim` at the rig |
| `scripts/bootstrap.sh` | Build `grim`, start registry, publish the catalog at 1.0.0 |
| `scripts/release-update.sh` | Publish `code-reviewer` 1.1.0 (rolling-release demo) |
| `scripts/teardown.sh` | Wipe rig state (`--registry` also stops the registry) |
| `docker-compose.yml` | `registry:2` on `localhost:5050` |
| `.grim-home/` | Isolated `GRIM_HOME` (gitignored, ephemeral) |

## Quick start

```sh
test/manual/scripts/bootstrap.sh        # one command: build + registry + publish
source test/manual/scripts/env.sh       # point `grim` at the rig
```

Published catalog:

| Kind | Repo | Versions |
|------|------|----------|
| skill | `localhost:5050/grimoire/skills/hello-world` | 1.0.0 |
| skill | `localhost:5050/grimoire/skills/code-reviewer` | 1.0.0 (1.1.0 via `release-update.sh`) |
| skill | `localhost:5050/grimoire/skills/commit-helper` | 1.0.0 |
| rule | `localhost:5050/grimoire/rules/rust-style` | 1.0.0 |
| rule | `localhost:5050/grimoire/rules/security-baseline` | 1.0.0 |

Each release cascades floating tags, e.g. `1.0.0` also sets `1.0`, `1`,
`latest`; `code-reviewer` `1.1.0` then moves `1.1`, `1`, `latest`.

## Scenarios

### 1. Browse the catalog

```sh
grim search                       # whole catalog
grim search review                # filter by keyword/description
grim search --format json
grim tui                          # interactive (requires a TTY)
```

### 1a. TUI: multi-select, batch, scope, delete

`grim tui` (needs a TTY). Each row shows a colored state glyph:
`✓ installed` (green), `↑ outdated` (yellow), `✱ modified` (red),
`⚠ integrity-missing` (magenta — recorded but files gone/edited away),
`· not-installed` (grey).

| Key | Action |
|-----|--------|
| `↑`/`↓` | move selection |
| `space` | mark/unmark the selected row |
| `a` / `c` | mark all visible / clear marks |
| `i` / `u` / `d` | install / update / **uninstall** the marked set (or the selection if nothing marked) |
| `g` | toggle scope (project ⇄ global) — title shows the active scope |
| `/` | search; `enter` browse detail; `r` refresh catalog; `q` quit |

Try: mark a couple with `space`, press `i` (batch install), watch the
state glyphs flip to green; `d` to batch-uninstall; `g` to see the same
catalog against the global scope's state. Tamper a file
(`echo x >> test/manual/project/.claude/skills/hello-world/SKILL.md`)
then refresh — it shows `✱ modified`; delete the dir and it shows
`⚠ integrity-missing`.

### 2. Lock & install into an editor

```sh
cd test/manual/project
grim lock                         # floating :1 -> pinned @sha256
cat grimoire.lock                 # byte-stable, digest-pinned
grim install                      # default editor: claude
ls -R .claude/skills .claude/rules
grim status                       # every artifact 'installed'
```

### 3. Multi-editor transform (Copilot rule transform)

```sh
grim install --target claude,copilot
cat .github/instructions/rust-style.instructions.md
# note: `paths:` frontmatter stripped + provenance header prepended
```

### 4. Integrity protection

```sh
echo "tampered" >> .claude/skills/hello-world/SKILL.md
grim status                       # hello-world -> 'modified'
grim install                      # refused (exit 65) — local edit protected
grim install --force              # overwrite the local edit
```

### 5. Rolling release / update

```sh
# in test/manual/project, with code-reviewer locked at 1.0.0 (via :1):
grep code-reviewer grimoire.lock          # 1.0.0 digest
../scripts/release-update.sh              # publishes code-reviewer 1.1.0
grim update                               # re-resolves :1 -> 1.1.0
grep code-reviewer grimoire.lock          # digest advanced
grim status
```

### 6. add / remove

```sh
grim add skill hello-world localhost:5050/grimoire/skills/hello-world:1
grim remove skill commit-helper
cat grimoire.toml grimoire.lock
```

### 7. Global scope

```sh
grim --global init
grim --global add rule security-baseline localhost:5050/grimoire/rules/security-baseline:1
grim --global install
```

### 8. Offline behavior

```sh
GRIM_OFFLINE=1 grim search        # serves cached catalog, exit 0
GRIM_OFFLINE=1 grim install       # warm blob cache succeeds; cold -> exit 81
```

## Teardown

```sh
test/manual/scripts/teardown.sh             # wipe rig state, keep registry
test/manual/scripts/teardown.sh --registry  # also stop + remove the registry
```

Re-run `scripts/bootstrap.sh` any time to recreate from the committed catalog.
