---
name: commit-helper
description: Draft a Conventional Commit message from staged changes. Use when the user asks to commit, save progress, or write a commit message.
license: Apache-2.0
metadata:
  keywords: git commit conventional-commits workflow
  author: grimoire-manual-rig
---

# Commit Helper

A second single-file skill so the catalog has more than one entry to
browse, search, and select in the TUI.

## Procedure

1. Inspect the staged diff.
2. Pick the Conventional Commit type (`feat`, `fix`, `refactor`, `chore`, …).
3. Write a one-line subject plus a body explaining *why*.
