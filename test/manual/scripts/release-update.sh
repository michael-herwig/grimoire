#!/usr/bin/env bash
# Rolling-release demo step: publish a NEW version (1.1.0) of
# `code-reviewer` so a project that locked the floating `:1` tag at 1.0.0
# can roll forward with `grim update`.
#
# Run this AFTER `grim lock` in test/manual/project, then `grim update`.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
MANUAL_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
REPO_ROOT="$(cd "$MANUAL_DIR/../.." && pwd)"
REGISTRY="localhost:5050" # manual-rig registry (see docker-compose.yml)
GRIM="$REPO_ROOT/test/bin/grim"

export GRIM_HOME="$MANUAL_DIR/.grim-home"
export GRIM_DEFAULT_REGISTRY="$REGISTRY"
export GRIM_INSECURE_REGISTRIES="$REGISTRY"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
cp -r "$MANUAL_DIR/catalog/skills/code-reviewer" "$tmp/code-reviewer"
printf '\n## Changelog\n\n- 1.1.0: clarified the severity grouping.\n' \
    >>"$tmp/code-reviewer/SKILL.md"

printf '\033[1;34m==>\033[0m releasing code-reviewer:1.1.0 (moves :1, :latest)\n'
# --force so the demo is re-runnable after the catalog skill is edited (the
# rig's :5050 registry is throwaway — moving its tags is intended).
"$GRIM" release "$tmp/code-reviewer" "$REGISTRY/grimoire/skills/code-reviewer:1.1.0" --force

cat >&2 <<EOF

Now roll the project forward:
  cd test/manual/project
  grim status                 # code-reviewer still pinned at 1.0.0
  grim update                 # re-resolves :1 -> 1.1.0, re-materializes
  grep code-reviewer grimoire.lock
EOF
