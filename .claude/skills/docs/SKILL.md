---
name: docs
description: Use when authoring or editing Grimoire documentation — user guide, reference pages, doc pages under `docs/`, or doc narrative structure.
---

# Grimoire Documentation

Role: write user-facing docs for Grimoire (Markdown under `docs/`).

## Workflow

1. **Read source code** — no memory docs
2. **Read product context** — `.claude/rules/product-context.md` before user-facing writing
3. **Search real-world examples** from other ecosystems before comparisons
4. **Identify problem** feature solves before solution
5. **Draft narratively** — idea → problem → solution → depth
6. **Verify internal links** point to existing sections with content

## Narrative Standards

- **Idea → problem → solution, then depth** — never start "Grimoire is a..."
- **No marketing tone** — examples make case
- **Reference-style links only** — never inline `[text](url)`; definitions at file bottom
- **Every external tool hyperlinked** — every occurrence, not first
- **Analogies in `:::info` callout boxes**, not inline
- **Custom anchors on every heading** — `{#parent-subsection}`

## Relevant Rules (load explicitly for planning)

- `.claude/rules/docs-style.md` — narrative + linking + anchor conventions
- `.claude/rules/product-context.md` — positioning and product identity

## Tool Preferences

- **WebFetch / WebSearch** — real-world examples from other tools before comparisons

## Constraints

- NEVER inline links — reference-style only
- ALWAYS read source before documenting; never from memory

## Handoff

- To Architect — docs revealing design ambiguity
- To Builder — code changes uncovered while writing docs

$ARGUMENTS