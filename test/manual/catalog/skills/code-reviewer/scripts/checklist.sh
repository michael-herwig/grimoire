#!/usr/bin/env bash
# A stand-in helper bundled inside the skill tree. Its only purpose in the
# manual rig is to prove that supporting files survive the pack → push →
# pull → materialize round-trip byte-for-byte.
set -euo pipefail

cat <<'RUBRIC'
Review rubric:
  [ ] Single responsibility per unit
  [ ] No duplicated logic across 2+ callers
  [ ] External input validated at boundaries
  [ ] Tests cover the changed behavior
  [ ] No drive-by changes mixed into the diff
RUBRIC
