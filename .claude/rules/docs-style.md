---
paths:
  - docs/**
---

# Documentation Instructions

Conventions for writing documentation pages. Apply to new doc pages under
`docs/`.

> **Status: provisional.** Grimoire has no docs site yet — docs are plain
> Markdown. These are general writing conventions, not tool-specific.

---

## Narrative Structure

Every `##` section opens with two-three short paragraphs that build in
sequence:

1. **The idea** — what concept does this section cover? One-sentence frame.
2. **The problem** — why does it matter? Show concrete real-world pain
   (not hypothetical). Use examples from familiar tools the reader knows.
3. **The solution** — how does Grimoire address it? Short, direct.

Then subsections for depth, comparisons, and design decisions.

No sales pitch or marketing opener. Let examples make the case.

---

## Paragraph Style

- **Short paragraphs.** One idea per paragraph, especially section intros.
- **No stop-and-go.** Each paragraph leads to the next; transition between
  concepts.
- **Tables and code blocks follow prose** — set context first.
- **No command dumps** without explaining what they represent.

---

## Headers

- Short headers — they appear in the TOC and should read as compelling
  chapter titles.
- Bad: `### Automatic Detection of Behavior`. Good: `### Auto-Detection`.
- Use `{#custom-anchor}` on section headings; nest as
  `{#parent-subsection}`.

---

## Real-World Examples and External Links

**Always search the internet** before writing comparisons or analogies. Do
not describe other tools from memory — fetch real docs and examples.

- Use concrete command sequences, real filenames, real repo links — not
  abstract descriptions.
- **Every external tool mentioned must hyperlink** — every occurrence, not
  just the first.

---

## Analogies and Cross-References

When introducing a design concept, compare it to something the reader
knows. Keep analogies in a dedicated callout/aside, not inline, so the
main prose stays clean.

---

## Precision and Nuance

Be exactly correct. State the precise behavior, not a convenient
simplification. Where a behavior is a convention rather than enforced, say
so explicitly.

---

## Internal Links

- **Every reference to another part of the system must hyperlink.**
- Link only to sections that exist and have real content — check the
  anchor target before linking.
- Use consistent, predictable anchor IDs.

---

## Link Syntax

Use reference-style links — **never inline `[text](url)` in the body**:

```markdown
See the [OCI distribution spec][oci-dist] for details.

[oci-dist]: https://github.com/opencontainers/distribution-spec
```

Collect all link definitions at the **bottom of the file**, grouped with
comments (`<!-- external -->`, `<!-- internal -->`, etc.).

---

## Before Writing

1. Read the source code to understand actual behavior — do not document
   from memory.
2. Search the internet for real examples from other ecosystems.
3. Identify the problem the feature solves before writing the solution.
4. Verify internal links point to sections that exist and have content.
5. Check every external tool or project mentioned has a hyperlink.
