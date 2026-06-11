---
name: architecture-guide
description: Sketch a module layout and pick design patterns for a new feature. Use when asked to plan an architecture, choose between approaches, or document a design decision before implementation.
license: Apache-2.0
metadata:
  summary: Design-pattern walkthrough skill
  keywords: architecture,patterns,design,planning
  author: grimoire-manual-rig
---

# Architecture Guide

A single-file skill that shares its name with the `architecture-guide`
**rule**. Use it to exercise the catalog's same-name-across-kinds case — a
skill and a rule resolve to different repos
(`skills/architecture-guide` vs `rules/architecture-guide`) and must stay
disambiguated in `grim search`, the TUI, and `grim status`.

## Procedure

1. Map the feature onto modules; keep business logic out of the CLI facade.
2. Pick patterns deliberately — strategy traits for swappable behaviour,
   three-layer errors so batch operations diagnose per item.
3. Record the decision and its trade-offs before writing code.
