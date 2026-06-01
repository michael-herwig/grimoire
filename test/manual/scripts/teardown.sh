#!/usr/bin/env bash
# Tear down manual-rig state.
#
#   test/manual/scripts/teardown.sh              # wipe rig GRIM_HOME + materialized files
#   test/manual/scripts/teardown.sh --registry   # also stop the compose registry
#
# The committed catalog/ and project/grimoire.toml are never touched.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
MANUAL_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"

log() { printf '\033[1;34m==>\033[0m %s\n' "$*"; }

log "removing $MANUAL_DIR/.grim-home"
rm -rf "$MANUAL_DIR/.grim-home"

log "removing materialized editor output + lock from project/"
rm -rf \
    "$MANUAL_DIR/project/.claude" \
    "$MANUAL_DIR/project/.opencode" \
    "$MANUAL_DIR/project/.github" \
    "$MANUAL_DIR/project/grimoire.lock"

if [ "${1:-}" = "--registry" ]; then
    log "stopping compose registry"
    docker compose -f "$MANUAL_DIR/docker-compose.yml" down -v
fi

log "done. Re-run scripts/bootstrap.sh to recreate."
