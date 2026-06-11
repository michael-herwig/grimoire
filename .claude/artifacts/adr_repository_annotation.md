# ADR: Authored repository URL on the OCI source annotation

## Metadata

**Status:** Accepted
**Date:** 2026-06-11
**Deciders:** Michael Herwig (maintainer)
**Tech Strategy Alignment:**
- [x] Decision follows Golden Path in `.claude/rules/product-tech-strategy.md`
      (OCI distribution substrate unchanged; reuses the existing annotation
      mechanism — no new crate, no new infrastructure)
**Domain Tags:** api, integration, tui
**Supersedes:** N/A

## Context

Artifacts had no way to link back to the source repository they were
authored in. The catalog (TUI detail pane, `grim search`) could show the
summary, description, and keywords, but not "where does this skill live?"
— the single most useful provenance link for a consumer deciding whether
to trust or contribute to an artifact.

Meanwhile grim already emitted `org.opencontainers.image.source`, but
filled it with the **tagless release reference** (`registry/repository`).
Per the [OCI image-spec annotation registry], `source` is defined as the
"URL to get source code for building the image" — the release ref usage
was a spec misuse, and it blocked registry-side integrations (ghcr.io
links a package to its repository through exactly this key).

## Decision

1. **Authoring key `repository`** — an optional HTTPS URL to the source
   repository, authored per kind exactly where `summary`/`keywords` live:
   skill and agent `metadata` map, rule top-level frontmatter, bundle TOML
   top level (`BundleMetadata`). String-only, like all catalog metadata.
2. **Annotation = `org.opencontainers.image.source`** (spec-correct; no
   new `com.grimoire.*` key). An authored `repository` **wins**; when none
   is authored the previous behavior — the tagless release ref — is kept
   as the fallback for continuity (`fallback_source` parameter on the
   `annotations_for_*` builders).
3. **Publish-time hard gate** — a `repository` value not starting with
   `https://` fails `grim build` / `grim release` with DataError (65) via
   the existing `MetadataInvalid` plumbing, matching the vendor-metadata
   "bad literals hard-fail publish" precedent.
4. **Read-back guard** — the catalog keeps the source annotation only when
   it starts with `https://` (`CatalogEntry::repository_url`). Legacy
   artifacts carrying a release ref degrade to "no URL" instead of
   surfacing garbage. No catalog version bump: the field is optional with
   a serde default, and `catalog.json` is a disposable TTL cache.
5. **Surfacing** — the TUI detail pane shows `Repository:` in its metadata
   block and `o` opens the vetted URL via the platform opener (plain
   visible URL otherwise — no OSC 8, which ratatui's cell diffing breaks);
   `grim search --format json` exposes a `repository` field (`null` when
   absent). The plain search table is unchanged.

## Consequences

- ghcr.io and compatible registries now link grim packages back to their
  repositories automatically when authors set `repository`.
- The `https://` prefix is the discriminator between an authored URL and
  the legacy fallback — consumers must not assume the source annotation is
  always a URL on artifacts published before this change.
- Re-releases stay idempotent: the annotation map remains fully
  deterministic (no wall-clock keys).
- Dropping the fallback entirely (always-URL-or-absent) remains a possible
  follow-up; it was rejected for v1 to avoid churning every existing
  manifest digest on re-release.

## Alternatives Considered

- **`com.grimoire.repository`** — own namespace, no conflict, but loses
  the spec-correct meaning and the registry-side linking. Rejected.
- **Emit both keys** — redundant bytes on every manifest for no extra
  information; the prefix guard already disambiguates. Rejected.
- **`org.opencontainers.image.url`** — spec means "more information",
  not source code; weaker fit and no ghcr linking. Rejected.

<!-- external -->
[OCI image-spec annotation registry]: https://github.com/opencontainers/image-spec/blob/main/annotations.md
