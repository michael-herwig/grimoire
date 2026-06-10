#!/usr/bin/env bash
# Bootstrap a local OCI registry with the manual-rig sample catalog.
#
#   test/manual/scripts/bootstrap.sh
#
# Re-runnable: re-publishes the *current* catalog content with `--force`, so
# an edited artifact moves its exact-version tag to the new digest (identical
# content resolves to the same digest, an effective no-op). Publishes every
# skill/rule/bundle at 1.0.0; the rolling-release / `grim update` flow gets a
# 1.1.0 of `code-reviewer` from scripts/release-update.sh.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
MANUAL_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
REPO_ROOT="$(cd "$MANUAL_DIR/../.." && pwd)"
CATALOG="$MANUAL_DIR/catalog"
# Own port — deliberately NOT 5000 (the pytest acceptance registry). See
# docker-compose.yml: sharing one registry polluted `grim search` here
# with the suite's throwaway `grim-test/*` repos.
REGISTRY="localhost:5050"
NS="grimoire"

log() { printf '\033[1;34m==>\033[0m %s\n' "$*"; }

# 1. Build the binary the pytest harness path expects, if missing/stale.
if [ ! -x "$REPO_ROOT/test/bin/grim" ] ||
    [ "$REPO_ROOT/Cargo.toml" -nt "$REPO_ROOT/test/bin/grim" ]; then
    log "building release grim"
    (cd "$REPO_ROOT" && cargo build --release --locked)
    cp "$REPO_ROOT/target/release/grim" "$REPO_ROOT/test/bin/grim"
fi
GRIM="$REPO_ROOT/test/bin/grim"

# 2. Ensure the registry is reachable (reuse a running one, else compose).
if ! curl -fsS "http://$REGISTRY/v2/" >/dev/null 2>&1; then
    log "starting registry via docker compose"
    docker compose -f "$MANUAL_DIR/docker-compose.yml" up -d
    for _ in $(seq 1 60); do
        curl -fsS "http://$REGISTRY/v2/" >/dev/null 2>&1 && break
        sleep 0.5
    done
fi
curl -fsS "http://$REGISTRY/v2/" >/dev/null 2>&1 ||
    {
        echo "registry not reachable at $REGISTRY" >&2
        exit 69
    }

# 3. Isolated GRIM_HOME for the rig.
export GRIM_HOME="$MANUAL_DIR/.grim-home"
export GRIM_DEFAULT_REGISTRY="$REGISTRY"
export GRIM_INSECURE_REGISTRIES="$REGISTRY"
mkdir -p "$GRIM_HOME"

release() { # <path> <repo-subpath> <name> <version>
    log "release $3:$4"
    # --force so re-seeding after editing the catalog moves the exact-version
    # tag to the new content. The rig owns this throwaway :5050 registry, so
    # overwriting an immutable version tag here is intended, not a footgun;
    # identical content still resolves to the same digest (an effective no-op).
    "$GRIM" release "$1" "$REGISTRY/$NS/$2/$3:$4" --force
}

# 4. Publish every skill at 1.0.0.
for dir in "$CATALOG"/skills/*/; do
    name="$(basename "$dir")"
    release "$dir" skills "$name" 1.0.0
done

# 5. Publish every rule at 1.0.0. An index `<name>.md` with a sibling
#    `<name>/` directory is a multi-file rule — `grim release` packs the
#    support dir automatically (the `rules/*.md` glob is non-recursive, so a
#    support file is never released as its own rule).
for file in "$CATALOG"/rules/*.md; do
    name="$(basename "$file" .md)"
    release "$file" rules "$name" 1.0.0
done

# 6. Publish every bundle at 1.0.0 (its members were published above).
for file in "$CATALOG"/bundles/*.toml; do
    [ -e "$file" ] || continue
    name="$(basename "$file" .toml)"
    release "$file" bundles "$name" 1.0.0
done

# Note: every skill/rule/bundle is published ONCE at 1.0.0. The rolling-release
# demo (publishing code-reviewer 1.1.0) is deliberately a separate step —
# run scripts/release-update.sh AFTER you have locked at 1.0.0 so `grim
# update` actually shows the pin rolling forward.

log "done. Catalog published to $REGISTRY/$NS/{skills,rules,bundles}/* at 1.0.0"
cat >&2 <<EOF

Next:
  source test/manual/scripts/env.sh
  grim search                       # browse the published catalog
  grim tui                          # interactive browser (needs a TTY)
  cd test/manual/project
  grim lock && grim install         # materialize into .claude/
  grim status                       # all 'installed'
  # then, for the rolling-release demo:
  test/manual/scripts/release-update.sh   # publishes code-reviewer 1.1.0
  grim update                       # rolls code-reviewer :1 -> 1.1.0
EOF
