# Bug Fix Plan: GitLab Container Registry compatibility (issue #11)

## Status

- **Plan:** bugfix_gitlab_registry_compat
- **Active phase:** 7 — Commit & Document (complete)
- **Step:** awaiting /finalize (review-fix loop applied all 5 clusters + both deferred items; uncommitted)
- **Last update:** 2026-06-19 (max-tier swarm-review + Codex gate, then review-fix: cleared all 5 clusters; then both deferred items — dry-run preview hint for verbatim-repository name-mismatch, and wired Identifier::parse to the shared OCI grammar (release/add/install now fail-fast, asymmetry closed). Gate green: 982 unit + 279 acceptance, clippy -D warnings)

---

## Overview

**Status:** In Progress
**GitHub Issue:** #11
**Severity:** High (publish/release unusable on a major SaaS registry)
**Workflow:** workflow-bugfix.md — Reproduce → RCA → Regression Test → Fix → Verify → Review → (user confirms) → close

Issue #11 (`bug`, reporter on grim 0.4.2, GitLab SaaS): `grim release` and
`grim publish` fail against GitLab Container Registry. Three distinct problems,
all confirmed by code investigation:

- **A — `400 MANIFEST_INVALID: unknown media type: application/vnd.grimoire.skill.v1`.**
  grim stamps the manifest's **config descriptor** `mediaType` to a custom
  per-kind type. GitLab validates every referenced media type against an
  allowlist and rejects off-list types.
- **B — `401` on `grim publish` (wrong repository path).** `publish` hardcodes the
  push target as `{registry}/{kind-plural}/{name}`, ignoring GitLab group/project
  nesting. `publish.toml` has no field to express a namespace.
- **C — `_catalog` discovery impractical on multi-user/SaaS registries.** `grim search`
  enumerates repos via the host-level OCI `_catalog` endpoint, which GitLab SaaS
  (and GHCR, Docker Hub) gate → browse returns empty.

## Root Cause Statement

> A: config descriptor `mediaType` set to a custom grimoire per-kind type that
> GitLab's referenced-media-type allowlist rejects. B: `plan_entries` hardcodes
> the repository as `{registry}/{kind.subdir()}/{name}`, with no manifest field
> for the group/project namespace. C: `_catalog` host enumeration is gated on
> SaaS registries by design; documented, not a code defect.

## Fix Approach (by axis)

- **Axis A** — config descriptor `mediaType` → `application/vnd.oci.empty.v1+json`
  (const `OCI_EMPTY_CONFIG_MEDIA_TYPE`); **drop the custom `artifactType`**
  (GitLab rejects it — confirmed against real SaaS), kind carried by the re-added
  `com.grimoire.kind` annotation. Read path is 3-tier (backward-compat):
  legacy `artifactType` → legacy `config.mediaType` → `com.grimoire.kind` annotation.
- **Axis B** — `repository_prefix` (manifest) + `repository` (per-entry) on
  `publish.toml`; `entry_repository` resolver: per-entry > prefix/{name} >
  kind-subdir/{name}. Charset/structural validation, exit 65.
- **Axis C** — docs (`docs/src/configuration.md` registry-compatibility section),
  `grim search` online-empty warning, TUI status line, grim-usage note.

## Commit structure

1. `fix(oci): use the OCI empty config media type so GitLab accepts manifests`
2. `docs(adr): supersede artifact-type ADR with empty-config compatibility`
3. `feat(publish): support repository_prefix / per-entry repository in publish.toml`
4. `docs: document registry compatibility for catalog discovery`

## Verification

1. `task verify` green.
2. New regression tests fail on HEAD, pass after fix.
3. **User confirmation required** (CI cannot prove — `registry:2` accepts everything):
   reporter runs against real GitLab SaaS. **Do not close #11** until confirmed.

## Notes

Full plan detail in the issue and PR description. Axis A's GitLab acceptance is the
only step CI cannot prove. Headline regression for B = `test_publish_nested_repository_prefix`.
