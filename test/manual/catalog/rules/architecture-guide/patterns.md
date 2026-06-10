# Worked Patterns

Support content referenced by the `architecture-guide` index rule. It lives
in the sibling `architecture-guide/` directory and is packed into the same
artifact, then installed beside the index as
`.claude/rules/architecture-guide/patterns.md`.

## Command pattern

```
args -> typed identifiers -> operation -> report data -> output
```

## Option-based lookups

"Not found" is `Option::None` at the lookup layer, not an error.

Edit this file after installing to see `grim status` report the rule as
`modified` (support-dir drift detection).
