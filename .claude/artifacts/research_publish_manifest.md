# Research: Batch Publish Manifest Prior Art

**Date:** 2026-06-12
**Researcher:** worker-researcher (persisted by orchestrator — worker had no Write tool)
**Consumed by:** `adr_grim_publish.md`, `plan_grim_publish.md`
**Scope:** Multi-package publish workflows, manifest file disambiguation patterns, idempotent re-publish semantics, OCI movable channel tags.

---

## Axis 1: Multi-Package Publish Workflows

**Cargo / cargo-release / release-plz (Rust)**

- `cargo publish --workspace` stable since Cargo 1.90 (Sept 2025). Publish scope = workspace membership (`[workspace.default-members]`). No external manifest. Dependency order: topological sort via a "registry overlay" (wraps upstream registry with a local overlay so crates resolve before their deps are uploaded). Parallel upload where dep-graph permits. No native skip-existing; `--dry-run` prevents upload. Retrying after partial failure re-publishes already-published crates and errors.
- **cargo-release**: layered config — workspace `Cargo.toml [workspace.metadata.release]` → per-crate `[package.metadata.release]` → `release.toml` at each level → CLI args. Key keys: `publish = false` (opt out), `dependent-version` (propagation policy), `shared-version` (version group lock). **Dry-run is the default; `--execute` opts in.**
- **release-plz**: `release-plz.toml` with `[workspace]` global defaults + `[[package]]` array-of-tables per crate (mandatory `name` key). Per-crate keys: `publish`, `semver_check`, `git_release_enable`, `version_group`. Workspace defaults cascade; package entries selectively override — clean two-level model.

**npm Changesets**

No single manifest. Version intent captured at contribution time in `.changeset/*.md` files (YAML front matter mapping package → bump level). `changeset version` aggregates into bumps + changelogs; `changeset publish` publishes everything whose version bumped. No dry-run on publish, no skip-existing. Dependency order implicit via npm workspace graph.

**Helm chart-releaser**

Charts in subdirectories; each has its own `Chart.yaml` `version:`. No aggregate manifest — directory scan = publish scope. `--skip-existing` / `CR_SKIP_EXISTING=true` is the **canonical CI pattern** — skip if release tag already exists, making pipelines fully re-runnable. Helm OCI (`helm push`) infers tag from `Chart.yaml`; no skip-existing built in.

**Melos (Dart/Flutter)**

Melos 7+: config under `melos:` key in root `pubspec.yaml`. Package list = workspace glob. `melos publish` defaults to dry-run; `--no-dry-run` required to actually publish. Effectively skip-existing — checks pub.dev version index and skips already-published versions.

---

## Axis 2: Manifest-Type Disambiguation

| Pattern | How it works | Examples | Best for |
|---|---|---|---|
| **Filename convention** | Each tool owns a canonical filename | `Cargo.toml`, `Chart.yaml`, `cr.yaml`, `publish.toml` | New standalone config file — cleanest, zero runtime cost |
| **Top-level marker key** | Required root key declares schema/version | `Chart.yaml apiVersion: v2`, `docker-compose.yml version:` | Shared/ambiguous filename; enables tool-version branching |
| **Namespaced sub-table** | Tool claims a sub-table in a shared file | `pyproject.toml [tool.ruff]`, `Cargo.toml [package.metadata.release]` | Multi-tool cohabitation in one file |
| **Explicit CLI flag** | `--config path` bypasses filename convention | `cargo --config`, `helm --values` | Maximum flexibility |

Key insight: dedicated `publish.toml` is clean and unambiguous. `[skills.foo]` keyed tables mirror release-plz's per-item keyed-table override model. Keyed tables beat `[[skills]]` arrays here because the package name is the natural key.

---

## Axis 3: Idempotent Re-Publish Semantics

| Tool / registry | Default on re-publish | Skip flag | CI norm |
|---|---|---|---|
| crates.io | Hard error | None native; `--idempotent` open request (rust-lang/cargo#13397) | `cargo-publish-all` implements skip via version-existence check |
| npm | Hard error (403) | None | Version bump required |
| PyPI / twine | Hard error (400/409) | `--skip-existing` (fragile message parsing; broken for non-PyPI indexes in twine 6.2.0) | Check version before upload |
| Helm chart-releaser | **Skip** | `--skip-existing` (well-supported) | The expected pattern; re-runnable pipelines |
| OCI registries | **Overwrite** (tags mutable) | Registry-level immutability is opt-in | Semver tags immutable by convention only |
| ORAS / Helm OCI push | Overwrite | None built in | Use registry immutability settings if required |

Industry trend: tooling-layer defaults converge on **skip-existing** for CI workflows (Helm, Melos, cargo-publish-all). Hard error is the registry guarantee, not the recommended tooling default.

---

## Axis 4: Movable Tags / Channel Tags in OCI

- Digests always immutable; tags mutable by default on all major registries (GHCR, ECR, Docker Hub); immutable-tag policies opt-in.
- Semver tags treated as immutable by social convention. Never overwrite a semver tag with different content.
- Channel tag conventions: `latest` (most recent stable), `stable` (blessed pointer, Flux pattern), `canary` / `edge` (pre-release), `vMAJOR` / `vMAJOR.MINOR` (rolling pointers).
- ORAS pattern: push semver tag, then `oras tag registry/image:v1.2.3 latest` to move the channel pointer. Helm OCI has no built-in "also tag latest" — a known gap; grim's cascade tags already cover the rolling-major case natively.

---

## Key Takeaways for `grim publish`

1. `publish.toml` filename convention is correct — dedicated file = unambiguous; `[skills.foo]` keyed tables idiomatic (release-plz model).
2. Two-level config (top-level defaults + per-entry overrides) is the established pattern (cargo-release, release-plz).
3. Dry-run-as-default exists in the wild (cargo-release, melos) — a deliberate alternative to skip-existing default.
4. Helm chart-releaser's `--skip-existing` model is the closest analogue for OCI-backed idempotent CI publishing.
5. Channel tags should be first-class, not post-publish scripting (grim cascade tags + `--tag` cover this).
6. Dependency ordering only needed for intra-manifest deps (bundles after members) — fixed kind ordering suffices; topological sort is overkill at this scale.
7. Partial-failure recovery: skip-existing default makes re-run-from-top sufficient; `--only` covers surgical recovery.

---

## Sources

- https://doc.rust-lang.org/cargo/commands/cargo-publish.html — `--workspace`, `--dry-run`
- https://doc.rust-lang.org/cargo/reference/workspaces.html — `[workspace.default-members]`
- https://www.tweag.io/blog/2025-07-10-cargo-package-workspace/ — registry overlay, topo sort, Cargo 1.90
- https://github.com/crate-ci/cargo-release/blob/master/docs/reference.md — layered config, dry-run default
- https://release-plz.dev/docs/config — `[[package]]` + `[workspace]` override model
- https://github.com/changesets/changesets/blob/main/packages/cli/README.md — changeset files, version+publish phases
- https://github.com/helm/chart-releaser/pull/111 — `--skip-existing` rationale
- https://helm.sh/docs/topics/registries/ — Helm OCI semantics
- https://github.com/rust-lang/cargo/issues/13397 — open `--idempotent` request
- https://github.com/pypa/twine/issues/332 — twine skip-existing fragility
- https://packaging.python.org/en/latest/specifications/pyproject-toml/ — `[tool.*]` namespacing
- https://oras.land/docs/commands/oras_tag/ — movable tags
- https://fluxcd.io/flux/cheatsheets/oci-artifacts/ — channel tag patterns
- https://www.docker.com/blog/docker-best-practices-using-tags-and-labels-to-manage-docker-image-sprawl/ — movable vs immutable tags
- https://github.com/invertase/melos — dry-run-by-default publish
