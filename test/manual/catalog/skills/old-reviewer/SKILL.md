---
name: old-reviewer
description: A retired diff reviewer kept only to exercise grim's deprecation surface end to end. Use code-reviewer instead.
license: Apache-2.0
metadata:
  summary: Retired diff reviewer (deprecated)
  keywords: review,deprecated,legacy,demo
  author: grimoire-manual-rig
  deprecated: superseded by code-reviewer — migrate before the next release
---

# Old Reviewer (deprecated)

This skill exists only to drive grimoire's deprecation surface. Its
`metadata.deprecated` notice is emitted as the `com.grimoire.deprecated`
manifest annotation, which then powers:

- a comma-suffixed `deprecated` in the `grim search` Status cell (and a
  `deprecated` field in `grim search --format json`),
- a yellow `⚠ deprecated` after the status label in the TUI's Status column
  plus the `Deprecated:` detail-pane entry in `grim tui`,
- the stderr warning printed by `grim add` when you acquire this reference.

It carries no real behavior — use `code-reviewer` instead.
