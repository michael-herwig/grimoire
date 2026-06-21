---
name: bugfix
description: Use when a bug is reported, something is broken, an error or crash needs fixing, a wrong output appears, or a regression shows up — guides Grimoire's Reproduce → Root-Cause → failing-test-FIRST → Fix → Verify discipline and will not let the fix begin until a failing regression test is recorded. Use also when the user says "bugfix", "/bugfix", "fix this bug", or "something is broken".
user-invocable: true
argument-hint: "[bug report / repro steps / issue number]"
triggers:
  - "fix a bug"
  - "bug fix"
  - "something is broken"
  - "regression test first"
  - "reproduce the bug"
---

# /bugfix — Guided Bug-Fix Workflow

Guard rail for the #1 bug-fix failure: **fixing before a failing test exists.**
This skill walks [`workflow-bugfix.md`](../../rules/workflow-bugfix.md) (the
single source of truth) and adds one hard, non-skippable gate — **Phase 3 must
produce a test that fails on the current code before any fix is written.**

> Why this exists: the rule alone was repeatedly skipped. This skill makes the
> order explicit and the gate blocking. Read `workflow-bugfix.md` for the full
> rationale; this file is the executable checklist.

## The non-negotiable sequence

```
Reproduce → Root-Cause Analysis → FAILING TEST → Fix → Verify → Review → Commit
```

Each phase finishes before the next starts. The **only** way past Phase 3 is a
test that you have **run** and **watched fail** for the right reason.

## Phase 1 — Reproduce (no guessing)

- State the exact wrong behavior: command, input, observed vs expected.
- Find the surface: which entry point(s)? CLI command, TUI path, library seam?
  A bug often lives on more than one surface (e.g. a CLI path AND its TUI twin);
  list every one that shares the broken code.
- Confirm it actually reproduces. If you can't reproduce it, keep digging —
  **no speculative fixes.**

Write a one-line repro: `"<cmd/inputs> → <observed>; expected <expected>"`.

## Phase 2 — Root-Cause Analysis (write it down)

- Trace the symptom to the line **and** the condition that made it fire.
- Output a root-cause statement in this exact shape — no "error on line N":

  > **X happens because Y, introduced by/located at Z.**

- Single bug or a pattern? Grep for the same defect elsewhere (sibling call
  sites, the other surface from Phase 1). If it's a pattern, the fix covers all
  instances.
- If the real cause needs an architectural change, stop and escalate to the
  feature workflow with a plan artifact — don't paper over it.

## Phase 3 — Failing test FIRST 🚧 GATE

**Do not edit any production code until this phase is done.**

1. Write the test that exercises the Phase-1 repro and targets the Phase-2 root
   cause (not just the symptom).
   - Acceptance-level → `test/tests/test_*.py` (real binary + registry).
   - Unit-level → inline `#[cfg(test)]` in the affected module.
2. **Run it. Paste the red output.** It must fail, and fail for the *bug's*
   reason (assertion on the wrong behavior) — not a compile error, typo, or
   missing fixture.
3. If an **existing** test encodes the buggy behavior (it asserts the wrong
   thing the user is now reporting), that test is part of the bug — note it; it
   will be corrected in the fix, with the reason recorded.

**Gate to pass Phase 3 — all three true:**
- [ ] A test exists that exercises the repro.
- [ ] You ran it and it **failed** (output shown).
- [ ] The failure is the bug, not test scaffolding.

If you cannot make a test fail, you have not reproduced or understood the bug —
return to Phase 1/2. Skipping this gate is the failure mode this skill prevents.

## Phase 4 — Fix (minimal, root-cause)

- Smallest change that addresses the Phase-2 cause. No drive-by refactor, no
  "while I'm here" cleanup (separate commit, separate type).
- Fix **every** instance the Phase-2 pattern search found.
- Correct any existing test that encoded the wrong behavior — state *why* in the
  diff/commit (user intent / corrected contract supersedes the old assumption).

## Phase 5 — Verify (evidence, not vibes)

- The Phase-3 test now **passes** (show it).
- Run the **subsystem** gate for the changed area (e.g. `task rust:verify`),
  then cache-bust + re-run unit tests if Rust (`touch` the edited files so a
  stale cache can't mask a failure).
- Manually confirm the Phase-1 repro no longer reproduces.
- Final gate before commit: `task --force verify` (fmt + clippy + build + unit +
  acceptance). Verify it yourself — do not trust "should pass".

## Phase 6 — Review-Fix Loop (+ optional Codex)

Apply the canonical Review-Fix Loop from `workflow-bugfix.md` (correctness vs
root cause, regression risk to other callers, minimality, test adequacy). For a
non-trivial fix, run one Codex adversarial pass over the diff as the cross-model
gate; fix real findings, re-verify, then converge.

## Phase 7 — Commit

- `fix:` conventional commit; body names the root cause and the regression test.
- Reference the issue (`fixes #N`) when one exists.
- Never push — the human decides (see [`workflow-git.md`](../../rules/workflow-git.md)).

## Anti-rationalizations (stop if you think these)

| Thought | Reality | Do this |
|---|---|---|
| "I know the cause, I'll just fix it." | No Phase-2 statement written. | Write the root-cause line first. |
| "The test is trivial, I'll add it after." | A test added after the fix never proved it caught the bug. | Write it first, watch it fail. |
| "Existing tests cover this area." | They may encode the *buggy* behavior. | Add a test for the *exact* broken path. |
| "Clippy/format nit nearby, I'll fix too." | Scope creep. | Separate `chore:`/`refactor:` commit. |

## References

- [`workflow-bugfix.md`](../../rules/workflow-bugfix.md) — full workflow + gates (source of truth)
- [`workflow-intent.md`](../../rules/workflow-intent.md) — work-type routing
- [`quality-core.md`](../../rules/quality-core.md) — verification honesty, review checklist
- [`workflow-git.md`](../../rules/workflow-git.md) — `fix:` commits, never push
