---
name: code-reviewer
description: Review a diff for SOLID/DRY violations, missing tests, and risky changes. Use when asked to review a pull request, audit a patch, or check code quality before merge.
license: Apache-2.0
metadata:
  summary: Multi-pass diff reviewer
  keywords: review,quality,solid,dry,audit
  author: grimoire-manual-rig
---

# Code Reviewer

A multi-file skill: this `SKILL.md` plus a `scripts/` helper. Use it to
exercise grimoire's directory-tree packing and the multi-editor
materialization (the whole tree must land intact under each editor target).

## Procedure

1. Read the diff.
2. Run `scripts/checklist.sh` for the review rubric.
3. Report findings grouped by severity (block / warn / suggest).
