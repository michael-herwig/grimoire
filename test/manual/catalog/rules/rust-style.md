---
paths:
  - "**/*.rs"
  - "**/Cargo.toml"
summary: Idiomatic Rust style rules
keywords: rust,style,lints,quality
repository: https://github.com/grimoire-samples/rust-style
---

# Rust Style

A path-scoped rule. Use it to exercise grimoire's rule transform: under
Claude/OpenCode it materializes verbatim; under Copilot the `paths:`
frontmatter is stripped and a provenance header is prepended in
`.github/instructions/rust-style.instructions.md`.

- Prefer `?` over `unwrap()` outside tests.
- Domain types over `String` for identifiers.
- One concept per file; no `mod.rs`.
