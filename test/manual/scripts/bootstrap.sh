#!/usr/bin/env bash
# Bootstrap a local OCI registry with the manual-rig sample catalog.
#
#   test/manual/scripts/bootstrap.sh
#
# Re-runnable: re-publishes the *current* catalog content with `--force`, so
# an edited artifact moves its exact-version tag to the new digest (identical
# content resolves to the same digest, an effective no-op).
#
# Publishes a small VERSION MATRIX (see step 4) so upgrade / `↑ outdated`
# states are exercisable: most artifacts ship a single 1.0.0, but a few carry
# extra versions (code-reviewer 1.0.0/1.1.0/1.2.0, commit-helper 1.0.0/2.0.0,
# rust-style 1.0.0/1.1.0) and the `starter-pack` bundle ships 1.0.0 plus a
# 2.0.0 whose member set adds AND removes entries. Each full-semver release
# cascades the floating :MAJOR/:MINOR/:latest tags forward, so versions MUST
# be published in ASCENDING order per artifact — the floating :1 the consumer
# project pins then lands on the highest version. A post-lock bump above the
# matrix top (scripts/release-update.sh) produces a genuine `↑ outdated` lock.
set -euo pipefail
IFS=$'\n\t'

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
MANUAL_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
REPO_ROOT="$(cd "$MANUAL_DIR/../.." && pwd)"
CATALOG="$MANUAL_DIR/catalog"
# Own ports — deliberately NOT 5000 (the pytest acceptance registry). See
# docker-compose.yml: sharing one registry polluted `grim search` here
# with the suite's throwaway `grim-test/*` repos.
REGISTRY="localhost:5050"
NS="grimoire"
# Second registry for the multi-registry demo (`[[registries]]` aliases,
# browse-all-declared). Hosts a small `tools` subset; see step 6 and
# project-multi/grimoire.toml.
REGISTRY2="localhost:5051"
NS2="tools"

log() { printf '\033[1;34m==>\033[0m %s\n' "$*"; }

# 1. Build the binary the pytest harness path expects, if missing/stale.
if [ ! -x "$REPO_ROOT/test/bin/grim" ] ||
    [ "$REPO_ROOT/Cargo.toml" -nt "$REPO_ROOT/test/bin/grim" ]; then
    log "building release grim"
    (cd "$REPO_ROOT" && cargo build --release --locked)
    cp "$REPO_ROOT/target/release/grim" "$REPO_ROOT/test/bin/grim"
fi
GRIM="$REPO_ROOT/test/bin/grim"

# 2. Ensure both registries are reachable (reuse running ones, else compose).
#    A single `compose up -d` starts both services; bring it up if EITHER
#    registry is down, then wait for BOTH to answer /v2/.
if ! curl -fsS "http://$REGISTRY/v2/" >/dev/null 2>&1 ||
    ! curl -fsS "http://$REGISTRY2/v2/" >/dev/null 2>&1; then
    log "starting registries via docker compose"
    docker compose -f "$MANUAL_DIR/docker-compose.yml" up -d
fi
for reg in "$REGISTRY" "$REGISTRY2"; do
    for _ in $(seq 1 60); do
        curl -fsS "http://$reg/v2/" >/dev/null 2>&1 && break
        sleep 0.5
    done
    curl -fsS "http://$reg/v2/" >/dev/null 2>&1 ||
        {
            echo "registry not reachable at $reg" >&2
            exit 69
        }
done

# 3. Isolated GRIM_HOME for the rig.
export GRIM_HOME="$MANUAL_DIR/.grim-home"
export GRIM_DEFAULT_REGISTRY="$REGISTRY"
# GRIM_INSECURE_REGISTRIES is COMMA-separated (split on ','); :5050/:5051 are
# non-default loopback ports so they are NOT built-in HTTP and must opt in here.
export GRIM_INSECURE_REGISTRIES="$REGISTRY,$REGISTRY2"
mkdir -p "$GRIM_HOME"

release() { # <path> <repo-subpath> <name> <version> [forced-kind]
    log "release $3:$4"
    # --force so re-seeding after editing the catalog moves the exact-version
    # tag to the new content. The rig owns this throwaway :5050 registry, so
    # overwriting an immutable version tag here is intended, not a footgun;
    # identical content still resolves to the same digest (an effective no-op).
    local args=("$1" "$REGISTRY/$NS/$2/$3:$4" --force)
    # A bare `.md` builds as a rule by shape; agents need the explicit kind.
    if [ -n "${5:-}" ]; then
        args+=(--kind "$5")
    fi
    "$GRIM" release "${args[@]}"
}

# Publish one artifact at each of its versions, in ASCENDING order so the
# floating :MAJOR/:MINOR/:latest tags end up on the highest version.
#   <path> <repo-subpath> <name> <space-separated versions, ascending> [forced-kind]
release_versions() {
    local path="$1" kind="$2" name="$3" versions_field="$4" forced_kind="${5:-}"
    local versions ver
    # Split the version field on whitespace regardless of the script's IFS.
    IFS=' ' read -r -a versions <<<"$versions_field"
    for ver in "${versions[@]}"; do
        release "$path" "$kind" "$name" "$ver" "$forced_kind"
    done
}

# 4. VERSION MATRIX. Keep it SMALL but covering: most artifacts ship one
#    1.0.0, a few carry extra versions for the upgrade / outdated demos.
#    Each record is `kind|name|path|space-separated versions (ascending)`.
#    A rule path is the index `<name>.md`; `grim release` packs the sibling
#    `<name>/` support dir automatically (the `rules/*.md` glob is
#    non-recursive, so a support file is never released as its own rule).
SKILL_MATRIX=(
    "skills|architecture-guide|$CATALOG/skills/architecture-guide|1.0.0"
    "skills|code-reviewer|$CATALOG/skills/code-reviewer|1.0.0 1.1.0 1.2.0"
    "skills|commit-helper|$CATALOG/skills/commit-helper|1.0.0 2.0.0"
    "skills|hello-world|$CATALOG/skills/hello-world|1.0.0"
    # old-reviewer carries metadata.deprecated -> com.grimoire.deprecated;
    # demos the deprecation surface (search marker, TUI ⚠, add warning).
    "skills|old-reviewer|$CATALOG/skills/old-reviewer|1.0.0"
)
RULE_MATRIX=(
    "rules|architecture-guide|$CATALOG/rules/architecture-guide.md|1.0.0"
    "rules|rust-style|$CATALOG/rules/rust-style.md|1.0.0 1.1.0"
    "rules|security-baseline|$CATALOG/rules/security-baseline.md|1.0.0"
)
# Agents are single `.md` files like rules, so `grim release` needs the
# explicit `--kind agent` (a bare `.md` builds as a rule by shape).
# reviewer carries two versions for the upgrade demos; release-bot demos
# vendor-namespaced metadata overrides (claude.model, opencode.temperature).
AGENT_MATRIX=(
    "agents|reviewer|$CATALOG/agents/reviewer.md|1.0.0 1.1.0"
    "agents|release-bot|$CATALOG/agents/release-bot.md|1.0.0"
)

# 4a. Publish skills.
for record in "${SKILL_MATRIX[@]}"; do
    IFS='|' read -r kind name path versions <<<"$record"
    release_versions "$path" "$kind" "$name" "$versions"
done

# 4b. Publish rules.
for record in "${RULE_MATRIX[@]}"; do
    IFS='|' read -r kind name path versions <<<"$record"
    release_versions "$path" "$kind" "$name" "$versions"
done

# 4c. Publish agents (forced kind, see AGENT_MATRIX comment).
for record in "${AGENT_MATRIX[@]}"; do
    IFS='|' read -r kind name path versions <<<"$record"
    release_versions "$path" "$kind" "$name" "$versions" agent
done

# 5. Publish bundles LAST — their members must already exist. The
#    `starter-pack` bundle ships two versions with differing member sets:
#      * 1.0.0 (starter-pack.toml):    code-reviewer + rust-style + security-baseline
#      * 2.0.0 (starter-pack-v2.toml): ADDS commit-helper, DROPS security-baseline
#    The published bundle name is the .toml file stem
#    (src/command/build.rs::read_bundle_members), so v2 is copied to a
#    mktemp `starter-pack.toml` first to publish under the SAME repo (else
#    :1 and :2 would be different repos and the upgrade demo would break).
bundle_tmp="$(mktemp -d)"
cleanup() { rm -rf "$bundle_tmp"; }
trap cleanup EXIT

release "$CATALOG/bundles/starter-pack.toml" bundles starter-pack 1.0.0

cp "$CATALOG/bundles/starter-pack-v2.toml" "$bundle_tmp/starter-pack.toml"
release "$bundle_tmp/starter-pack.toml" bundles starter-pack 2.0.0

# `review-pack` shares its code-reviewer member with starter-pack (same
# identifier) and adds the reviewer agent — the shared-member demo bundle.
release "$CATALOG/bundles/review-pack.toml" bundles review-pack 1.0.0

# 6. SECOND REGISTRY (localhost:5051, namespace `tools`) — the multi-registry
#    demo. Publishes a SMALL distinct subset (one skill, one rule) from the
#    same committed catalog so:
#      * `grim search` from a project declaring both registries browses BOTH
#        (browse-all-declared), and
#      * a `[[registries]]` alias `tools` resolves `tools/skills/commit-helper`
#        to `localhost:5051/tools/skills/commit-helper` (see project-multi/).
#    Same `--force` re-seed semantics as the primary registry.
release2() { # <path> <repo-subpath> <name> <version> [forced-kind]
    log "release2 $3:$4 -> $REGISTRY2/$NS2"
    local args=("$1" "$REGISTRY2/$NS2/$2/$3:$4" --force)
    if [ -n "${5:-}" ]; then
        args+=(--kind "$5")
    fi
    "$GRIM" release "${args[@]}"
}
release2 "$CATALOG/skills/commit-helper" skills commit-helper 1.0.0
release2 "$CATALOG/rules/security-baseline.md" rules security-baseline 1.0.0

log "done. Primary catalog at $REGISTRY/$NS/{skills,rules,bundles}/*; multi-registry subset at $REGISTRY2/$NS2/{skills,rules}/*"
cat >&2 <<EOF

Next:
  source test/manual/scripts/env.sh
  grim search                       # browse the catalog (Version column = highest semver)
  grim tui                          # interactive browser (needs a TTY)
  cd test/manual/project
  grim lock && grim install         # materialize into .claude/
  grim status                       # all 'installed'

Multi-registry demo (browse-all-declared across two registries):
  cd test/manual/project-multi
  grim search                       # browses BOTH 5050/grimoire and 5051/tools
  grim lock                         # pins each FQ ref to its own registry
  grim install && grim status       # all 'installed' from across both registries
  # alias form is a 'grim add' convenience:
  grim add tools/skills/commit-helper:1   # 'tools' -> localhost:5051/tools/...

Outdated / update demo (lock at an OLD pin, then roll forward):
  # the project pins code-reviewer at the floating :1, so 'grim lock' here
  # records the newest published version (1.2.0). To force a real '↑ outdated'
  # lock, publish a version ABOVE the matrix top AFTER locking:
  test/manual/scripts/release-update.sh   # publishes code-reviewer 1.3.0
  grim status                             # code-reviewer -> 'outdated'
  grim update                             # rolls :1 -> 1.3.0; back to 'installed'

Bundle add/remove-on-upgrade demo:
  grim add bundle starter-pack localhost:5050/grimoire/bundles/starter-pack:1
  # resolves code-reviewer + rust-style + security-baseline
  grim add bundle starter-pack localhost:5050/grimoire/bundles/starter-pack:2
  # :2 ADDS commit-helper and DROPS security-baseline

Agent demo (per-client rendering + vendor overrides):
  grim install --client claude,opencode,copilot
  cat .claude/agents/release-bot.md      # claude.model override -> model: opus
  cat .opencode/agents/release-bot.md    # common model: sonnet + temperature
  cat .github/agents/release-bot.md      # tools as a YAML list, no model

Shared bundle members demo (removing one bundle spares shared members):
  grim add localhost:5050/grimoire/bundles/starter-pack:1
  grim add localhost:5050/grimoire/bundles/review-pack:1
  grim status                            # code-reviewer: bundle provenance x2
  grim remove bundle review-pack
  grim status                            # code-reviewer survives via starter-pack
EOF
