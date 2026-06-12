# Grimoire Manual Test Rig

A hands-on harness for exercising `grim` against a real local OCI registry
with a committed sample catalog of skills, rules, and agents. This is **fully
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
| `catalog/agents/<name>.md` | Source-of-truth sample agents (committed) |
| `catalog/bundles/starter-pack.toml` | Bundle v1 member set (committed) |
| `catalog/bundles/starter-pack-v2.toml` | Bundle v2 member set — adds + removes members (committed) |
| `catalog/bundles/review-pack.toml` | Bundle sharing `code-reviewer` with starter-pack + an agent member (committed) |
| `project/grimoire.toml` | Ready-made consumer project (floating `:1` tags) |
| `scripts/env.sh` | `source` it to point `grim` at the rig |
| `scripts/bootstrap.sh` | Build `grim`, start registry, publish the version matrix |
| `scripts/release-update.sh` | Publish `code-reviewer` 1.3.0 (post-lock outdated / rolling-release demo) |
| `scripts/teardown.sh` | Wipe rig state (`--registry` also stops the registry) |
| `docker-compose.yml` | `registry:2` on `localhost:5050` |
| `.grim-home/` | Isolated `GRIM_HOME` (gitignored, ephemeral) |

## Quick start

```sh
test/manual/scripts/bootstrap.sh        # one command: build + registry + publish
source test/manual/scripts/env.sh       # point `grim` at the rig
```

Published catalog (a small **version matrix** — most artifacts ship one
1.0.0, a few carry extra versions for the upgrade / `↑ outdated` demos):

| Kind | Repo | Versions |
|------|------|----------|
| skill | `localhost:5050/grimoire/skills/hello-world` | 1.0.0 |
| skill | `localhost:5050/grimoire/skills/code-reviewer` | 1.0.0, 1.1.0, 1.2.0 (1.3.0 via `release-update.sh`) |
| skill | `localhost:5050/grimoire/skills/commit-helper` | 1.0.0, 2.0.0 |
| skill | `localhost:5050/grimoire/skills/architecture-guide` | 1.0.0 |
| rule | `localhost:5050/grimoire/rules/rust-style` | 1.0.0, 1.1.0 |
| rule | `localhost:5050/grimoire/rules/security-baseline` | 1.0.0 |
| rule | `localhost:5050/grimoire/rules/architecture-guide` | 1.0.0 |
| agent | `localhost:5050/grimoire/agents/reviewer` | 1.0.0, 1.1.0 |
| agent | `localhost:5050/grimoire/agents/release-bot` | 1.0.0 (vendor-override demo) |
| bundle | `localhost:5050/grimoire/bundles/starter-pack` | 1.0.0, 2.0.0 (v2 adds commit-helper, drops security-baseline) |
| bundle | `localhost:5050/grimoire/bundles/review-pack` | 1.0.0 (shares code-reviewer with starter-pack, adds the reviewer agent) |

Each full-semver release cascades the floating tags forward, e.g. `1.0.0`
also sets `1.0`, `1`, `latest`; publishing `code-reviewer` `1.2.0` then
moves `1.2`, `1`, `latest` onto it. Because of this, `bootstrap.sh`
publishes versions in **ascending** order per artifact, so the floating
`:1`/`:latest` the consumer project pins always land on the highest
version (code-reviewer `1.2.0`, commit-helper `2.0.0`, rust-style `1.1.0`).

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
`✘ integrity-missing` (magenta — recorded but files gone/edited away),
`· not-installed` (grey).

| Key | Action |
|-----|--------|
| `↑`/`↓` | move selection (scroll the detail pane while it is open) |
| `pgup`/`pgdn` | scroll the detail pane from any mode (no focus needed) |
| `space` | mark/unmark the selected row |
| `a` / `c` | mark all visible / clear marks |
| `i` / `u` / `d` | install / update / **uninstall** the marked set (or the selection if nothing marked) |
| `o` | open the selected entry's repository URL in the browser |
| `g` | toggle scope (project ⇄ global) — title shows the active scope |
| `/` | search; `enter` browse detail (`j`/`k` also scroll there); `r` refresh catalog; `q` quit |

Try: mark a couple with `space`, press `i` (batch install), watch the
state glyphs flip to green; `d` to batch-uninstall; `g` to see the same
catalog against the global scope's state. Tamper a file
(`echo x >> test/manual/project/.claude/skills/hello-world/SKILL.md`)
then refresh — it shows `✱ modified`; delete the dir and it shows
`✘ integrity-missing`.

The detail pane (`enter`) shows the centered identifier, a `Summary:` /
`Description:` section, and a `Metadata:` block (version + status stay on
the catalog row). Most rig artifacts carry an authored `repository` URL
(`https://github.com/grimoire-samples/…`, emitted as the
`org.opencontainers.image.source` annotation) — `o` opens it.
`hello-world` and `security-baseline` intentionally carry none, so they
demo the `Repository: -` fallback and the "no repository URL for this
entry" status line. The `architecture-guide` **skill** ships a
deliberately long description so its pane overflows a small terminal —
open it and scroll (`↑`/`↓` or `j`/`k`), or page it from the list with
`pgup`/`pgdn` without opening it at all.

### 2. Lock & install into a client

```sh
cd test/manual/project
grim lock                         # floating :1 -> pinned @sha256
cat grimoire.lock                 # byte-stable, digest-pinned
grim install                      # targets the detected clients (all when none detected)
ls -R .claude/skills .claude/rules .claude/agents
grim status                       # every artifact 'installed'
```

### 3. Multi-client transform (Copilot rule transform)

```sh
grim install --client claude,copilot
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

### 5. Rolling release / outdated / update

`bootstrap.sh` publishes `code-reviewer` ascending to 1.2.0, so locking the
floating `:1` records 1.2.0 (state `installed`, NOT `outdated`). To produce
a genuine `↑ outdated` lock, publish a version ABOVE the matrix top AFTER
locking — that is exactly what `release-update.sh` does (1.3.0):

```sh
# in test/manual/project, after `grim lock` (code-reviewer pinned at 1.2.0):
grep code-reviewer grimoire.lock          # 1.2.0 digest
../scripts/release-update.sh              # publishes code-reviewer 1.3.0, moves :1
grim status                               # code-reviewer -> 'outdated'
grim update                               # re-resolves :1 -> 1.3.0
grep code-reviewer grimoire.lock          # digest advanced
grim status                               # back to 'installed'
```

### 5a. Bundle add/remove on upgrade

The `starter-pack` bundle ships two versions with different member sets, so
upgrading `:1 -> :2` adds AND removes members:

```sh
# v1: code-reviewer + rust-style + security-baseline
# `add` infers kind=bundle from the published manifest's artifactType
grim add localhost:5050/grimoire/bundles/starter-pack:1
cat grimoire.toml grimoire.lock           # inspect the resolved members

# v2 ADDS commit-helper, DROPS security-baseline
grim add localhost:5050/grimoire/bundles/starter-pack:2
grim update                               # commit-helper added, security-baseline pruned
cat grimoire.toml grimoire.lock
```

### 5b. Shared bundle members

`starter-pack` and `review-pack` both declare `code-reviewer` at the same
identifier, so declaring both coalesces it to ONE lock entry that records
BOTH bundles as provenance. Removing one bundle strips only that bundle's
provenance entry — the member survives until the last holder goes.

Run this in a **scratch project**: the rig's ready-made project declares
every bundle member directly, and a direct declaration always wins over
bundle provenance (you would see `direct`, not `bundle: …`).

```sh
mkdir -p /tmp/grim-shared-demo && cd /tmp/grim-shared-demo
grim init
grim add localhost:5050/grimoire/bundles/starter-pack:1
grim add localhost:5050/grimoire/bundles/review-pack:1
grim status                       # code-reviewer source: "bundle: ...starter-pack, ...review-pack"
grep -B3 -A3 'skill.bundles' grimoire.lock    # multi-provenance [[skill.bundles]] rows

grim remove bundle review-pack
grim status                       # code-reviewer still locked (held by starter-pack)
grim remove bundle starter-pack
grim status                       # now gone — the last holder was removed
```

The same holds in the TUI: deleting one of the two bundle rows keeps the
shared member's files on disk; only members the deleted bundle exclusively
owns are uninstalled.

### 5c. Agents (per-client rendering + vendor overrides)

The project declares the `reviewer` agent; `release-bot` carries
vendor-namespaced metadata (`claude.model: opus`,
`claude.permission-mode: plan`, `opencode.temperature: "0.2"`) that
overrides or extends the projected common fields per client:

```sh
cd test/manual/project
grim add localhost:5050/grimoire/agents/release-bot:1
grim install --client claude,opencode,copilot

cat .claude/agents/release-bot.md     # claude.model override: model: opus (+ permissionMode)
cat .opencode/agents/release-bot.md   # common model: sonnet kept; temperature lifted; no name:
cat .github/agents/release-bot.md     # tools: as a YAML list; no model
cat .claude/agents/reviewer.md        # common fields only -> installed verbatim
```

### 6. add / remove

```sh
grim add localhost:5050/grimoire/skills/hello-world:1
grim remove skill commit-helper
cat grimoire.toml grimoire.lock
```

### 7. Global scope

```sh
grim --global init
grim --global add localhost:5050/grimoire/rules/security-baseline:1
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
