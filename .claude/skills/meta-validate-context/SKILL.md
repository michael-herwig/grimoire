---
name: meta-validate-context
description: Use when auditing the freshness of `.claude/rules/subsystem-*.md` files against current codebase state, or when a subsystem has undergone significant change.
user-invocable: true
argument-hint: "all | subsystem-name"
triggers:
  - "validate context"
  - "audit subsystem rules"
  - "check subsystem rules"
  - "freshness of rules"
---

# Validate Context Rules

Check `.claude/rules/subsystem-*.md` files match current codebase.

## Workflow

For each `subsystem-*.md` rule file:

1. **Read the rule** — Extract type names, module paths, function names, error variants
2. **Grep the codebase** — Verify each reference exists
3. **Check for new additions** — Find new public types/modules not in rule
4. **Report** — List stale references + missing additions

## Subsystem Rules to Check

| Rule | Key References to Verify |
|------|------------------------|
| `subsystem-cli.md` | CLI context struct, command enum variants, output trait |
| `subsystem-cli-api.md` | Report data types, output trait conventions |
| `subsystem-cli-commands.md` | Documented command surface vs implemented commands |
| `subsystem-file-structure.md` | Storage layout, data-root constraints |
| `subsystem-tests.md` | Fixture names in conftest.py, test file names, runner methods, helper params |

Subsystem rules are provisional placeholders while the implementation is
scaffolded — verify against current `src/` as code lands.

## Verification Commands

```bash
# Check if a type still exists
grep -r "pub struct TypeName" src/
grep -r "pub enum TypeName" src/

# Check if a module still exists
ls src/module_name/

# Check for new public types not in the rule
grep -rn "^pub struct\|^pub enum\|^pub trait" src/ | grep -v test
```

## Output Format

```markdown
## Context Validation Report

### subsystem-cli.md
- OK: [type] still present
- STALE: [type] — renamed to [new_name] or removed
- MISSING: [new_type] — not documented in rule

### subsystem-file-structure.md
...
```

## When to Run

- After big refactors
- Before major feature branches
- Part of `/code-check` audits
- Monthly