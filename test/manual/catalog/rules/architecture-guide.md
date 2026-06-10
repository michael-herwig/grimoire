---
paths:
  - "src/**/*.rs"
summary: Architecture guide with examples
keywords: architecture,patterns,design,examples
---

# Architecture Guide

A **multi-file rule**: this index plus a sibling `architecture-guide/`
support directory. Use it to exercise grimoire's support-dir packing — the
index and the whole support tree must land together under each editor
target, and editing a support file must show up as `modified` in
`grim status`.

See the [worked patterns](./architecture-guide/patterns.md) for examples the
index references.

- Facade at the CLI boundary; business logic in plain modules.
- Strategy via traits for swappable implementations.
- Three-layer errors so batch operations diagnose per item.
