# ADR: Type Grimoire artifacts with the OCI empty config + a `com.grimoire.kind` annotation (drop the custom `artifactType` for GitLab)

## Metadata

**Status:** Accepted
**Date:** 2026-06-19
**Deciders:** Michael Herwig (maintainer)
**Tech Strategy Alignment:**
- [x] Decision follows Golden Path in `.claude/rules/product-tech-strategy.md`
      (OCI is the distribution substrate; this uses standard OCI 1.1 fields)
**Domain Tags:** integration, api
**Supersedes:** [adr_oci_artifact_type.md](./adr_oci_artifact_type.md)

## Context

`adr_oci_artifact_type.md` introduced a per-kind Grimoire config media type
(`application/vnd.grimoire.<kind>.config.v1+json`) as the config descriptor's
`mediaType`. This design works against `registry:2` (Docker Distribution) but
fails against GitLab Container Registry.

GitLab Container Registry validates **every media type referenced in a
manifest** — including the config descriptor `mediaType` and the top-level
`artifactType` — against a server-managed allowlist. Custom (non-OCI,
non-Docker) types are rejected. The initial fix attempt switched the config
descriptor to the OCI empty type (`application/vnd.oci.empty.v1+json`) while
keeping the custom `artifactType`. Real-GitLab testing
(`registry.gitlab.com/michael-herwig/grimoire-registry-test`, 2026-06-19)
revealed that GitLab rejects the custom `artifactType` too:

```
400 MANIFEST_INVALID: unknown media type: application/vnd.grimoire.skill.v1
```

Testing also confirmed that the custom **layer** media type
(`application/vnd.grimoire.artifact.layer.v1.tar`) is **accepted** by GitLab.

The `REGISTRY_FF_DYNAMIC_MEDIA_TYPES` server flag that would disable this
check is **not available on GitLab SaaS** (see [GitLab supported media
types][gitlab-media-types]).

The OCI image-spec "Guidance for an Empty Descriptor" (see [OCI manifest
spec][oci-manifest]) blesses `application/vnd.oci.empty.v1+json` for use as
the config descriptor when an artifact has no meaningful config payload. ORAS
follows the same convention (see [ORAS manifest-config docs][oras-manifest-config]).
This type is on GitLab's allowlist.

With both the custom config type and `artifactType` rejected, the only
registry-agnostic discriminator available is the `com.grimoire.kind` manifest
annotation. Dropping `artifactType` from the write path loses OCI-native type
discrimination and the Referrers API filtering path — a forward-feature cost
that grim does not yet use (no signatures, SBOMs, or attestations). A
per-registry strategy (emit `artifactType` only to registries known to accept
it) remains a safe non-breaking follow-up if that capability becomes necessary.

## Decision Drivers

- GitLab Container Registry is a target registry ("bring your own registry"
  principle, `product-context.md`). The previous design fails at push time on
  GitLab SaaS for both the config type and `artifactType`.
- The OCI empty config (`application/vnd.oci.empty.v1+json`, blob `{}`) is
  spec-blessed and on GitLab's allowlist.
- `com.grimoire.kind` is a manifest annotation: registry-agnostic, round-trips
  faithfully through any conformant registry, and is the only remaining
  Grimoire-specific field GitLab does not validate against an allowlist.
- No migration: the project is provisional and the existing `adr_oci_artifact_type.md`
  already accepted the consequence of digest changes on re-release.
- grim has no current use of `artifactType`-keyed Referrers API filtering,
  signatures, SBOMs, or attestations — the forward-feature cost of dropping it
  is deferred, not permanent.

## Considered Options

### Option 1 — OCI empty config + drop `artifactType` + `com.grimoire.kind` annotation — CHOSEN

Config descriptor `mediaType` = `application/vnd.oci.empty.v1+json` (blob
`{}`, sha256 `sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a`,
size 2). No `artifactType` field written. Kind carried by `com.grimoire.kind`
annotation. Confirmed against real GitLab SaaS on 2026-06-19.

| Pros | Cons |
|------|------|
| Passes GitLab's allowlist check for both config type and top-level type | Drops OCI-native `artifactType` discrimination — Referrers API filtering, signatures, SBOMs unavailable until a per-registry strategy is added |
| `com.grimoire.kind` is registry-agnostic and round-trips through any conformant registry | Manifest digest changes on re-release (same consequence accepted by the superseded ADR) |
| 3-tier read model keeps old artifacts (with `artifactType`) fully readable | Tag-tracking consumers see a re-release as an update |
| OCI empty config is spec-blessed (OCI image-spec + ORAS convention) | |

### Option 2 — OCI empty config + keep `artifactType` + `com.grimoire.kind` annotation

The initial fix attempt. Passed the config-type check but GitLab also rejects
the custom top-level `artifactType`. Ruled out by real-GitLab testing.

| Pros | Cons |
|------|------|
| Retains OCI-native `artifactType` discrimination | GitLab SaaS rejects the custom `artifactType` — confirmed 2026-06-19 |

### Option 3 — Keep per-kind config `mediaType`, request GitLab allowlist expansion

Ask GitLab to add `application/vnd.grimoire.*` types to their allowlist.

| Pros | Cons |
|------|------|
| No code change | Not actionable: allowlist is server-managed, unavailable on SaaS |
| | Blocks all GitLab SaaS tenants until/if accepted |
| | Per-OCI-spec guidance, custom configs should carry meaningful payload |

## Decision Outcome

**Chosen:** Option 1. No `artifactType` is written to the manifest. The
config descriptor `mediaType` is `application/vnd.oci.empty.v1+json` (blob
`{}`). The artifact kind is carried solely by the `com.grimoire.kind`
annotation on the write path. The 3-tier read resolver retains backward
compatibility with artifacts published under the previous ADR.

### Wire contract (per kind)

| Kind | `artifactType` | config `mediaType` | `com.grimoire.kind` annotation |
|------|----------------|--------------------|-------------------------------|
| skill | (none — not written) | `application/vnd.oci.empty.v1+json` | `skill` |
| rule | (none — not written) | `application/vnd.oci.empty.v1+json` | `rule` |
| agent | (none — not written) | `application/vnd.oci.empty.v1+json` | `agent` |
| bundle | (none — not written) | `application/vnd.oci.empty.v1+json` | `bundle` |

Layer media types unchanged: `application/vnd.grimoire.artifact.layer.v1.tar`
(skill/rule/agent payload), `application/vnd.grimoire.bundle.v1+json` (bundle
members layer) — GitLab confirmed these are accepted. Config blob:
deterministic `{}`.

### Read/write model

- **Write:** kind is known at the release site → write
  `config.mediaType = application/vnd.oci.empty.v1+json` + `com.grimoire.kind`
  annotation. No `artifactType` field. Metadata annotations
  (`org.opencontainers.image.*`, `com.grimoire.keywords`, `com.grimoire.summary`,
  `org.opencontainers.image.source`) are unchanged.
- **Read:** the single seam `kind_from_manifest` is a 3-tier resolver:
  1. `artifactType` — retained as a read-only backward-compat tier for
     artifacts published before this change. Not written by new grim.
  2. Legacy `config.mediaType` — retained for artifacts published under
     `adr_oci_artifact_type.md` (custom per-kind type strings). No
     strict-equality check forces a specific config type, so this tier never
     blocks reads of new artifacts.
  3. `com.grimoire.kind` annotation — the discriminator grim writes today;
     also covers the oldest pre-`artifactType` artifacts that carried only this
     annotation.
  A manifest that matches none of the three tiers → `None` → `grim add` errors
  `KindInferenceFailed` asking for `--kind` (unchanged UX).

### Backward and forward compatibility

| Scenario | Outcome |
|----------|---------|
| New grim reads a **legacy artifact** (custom `artifactType` + custom config type) | `artifactType` resolves at tier 1; tier 2 also resolves. No breakage. |
| Old grim 0.4.x reads a **new artifact** (empty config + annotation, no `artifactType`) | Resolves kind from `com.grimoire.kind` annotation at tier 3. No strict check on config type. No breakage. |
| Pre-`artifactType` grim reads a **new artifact** | `com.grimoire.kind` annotation resolves at tier 3. |
| Digest-pinned ref or existing lockfile | Resolves to the old immutable manifest (content-addressed). No breakage. |
| Tag-tracking consumer after a re-release | Sees an update (new manifest digest). Same consequence already accepted by the superseded ADR. |

### Consequences

**Positive:**
- `grim release`, `grim add`, and `grim publish --repository-prefix` succeed
  against GitLab Container Registry SaaS without any server-side configuration
  — confirmed against `registry.gitlab.com/michael-herwig/grimoire-registry-test`
  on 2026-06-19.
- Wire format uses the spec-blessed OCI empty config descriptor (OCI
  image-spec "Guidance for an Empty Descriptor"). Precision: the empty config
  *blob* is spec-blessed, but OCI image-spec v1.1.0 says `artifactType` MUST be
  set when the config media type is the empty value, and the ORAS convention
  pairs the empty config *with* an `artifactType`. Emitting the empty config
  with NO `artifactType` (forced by GitLab's allowlist) is therefore a
  deliberate, documented spec deviation — see Negative/Risks. The per-registry
  follow-up that re-adds `artifactType` where accepted restores conformance.
- Full backward compatibility on both read directions (see table above).

**Negative / Risks:**
- Manifest shape change → existing published artifacts get new digests on
  re-release. Acceptable: provisional project, no install base to migrate.
- Dropping `artifactType` loses OCI-native type discrimination: Referrers API
  *type filtering* keys on `artifactType`. With it absent, a referrers response
  descriptor falls back to the config media type (`application/vnd.oci.empty.v1+json`)
  — non-discriminating — so filtering grim artifacts by kind via
  `?artifactType=…` will not work. This affects not only signatures/SBOMs/
  attestations but also any future `grim list --remote` that would enumerate
  grim artifacts by kind. grim uses none of these today. Note (precision):
  cosign can still sign a grim artifact — `cosign sign <digest>` works by
  subject digest and requires no `artifactType` on the subject; what is lost is
  referrers-API *discovery* of those signatures by type. A per-registry strategy
  (emit `artifactType` only to registries known to accept it) is a safe,
  non-breaking follow-up if needed.
- Kind discrimination now rests entirely on a Grimoire-private manifest
  annotation (`com.grimoire.kind`), which any publisher can set — it is
  trivially forgeable, with no registry-allowlist semantics behind it. The read
  path is strict (an unknown/mismatched annotation yields `None` → `grim add`
  asks for `--kind`), so this is not a correctness defect, but it does narrow
  the trust model versus an OCI-native discriminator. This relocates a
  pre-existing property (the old `artifactType` was equally publisher-set), not
  a new weakness; payload-shape cross-validation remains a possible follow-up.

## Validation

- Rust unit tests: manifest round-trip confirms `config.mediaType` is
  `application/vnd.oci.empty.v1+json` and no `artifactType` is emitted;
  `kind_from_manifest` resolves from `artifactType` (tier 1), from legacy config
  type (tier 2), and from annotation (tier 3); annotation builders emit
  `com.grimoire.kind`.
- Acceptance: release → `add`/install → catalog against live `registry:2`
  (proves kind inference works end-to-end and idempotent re-releases produce
  stable digests).
- GitLab compatibility: confirmed against
  `registry.gitlab.com/michael-herwig/grimoire-registry-test` on 2026-06-19.
  `grim release` (with cascade tags), `grim add` (kind inferred from
  `com.grimoire.kind`), and `grim publish --repository-prefix` all succeed.
  Both the custom config media type and the custom `artifactType` are rejected
  by GitLab; the custom layer media type
  (`application/vnd.grimoire.artifact.layer.v1.tar`) is accepted.

## Links

- Supersedes: [`adr_oci_artifact_type.md`](./adr_oci_artifact_type.md)
- [OCI image-spec — "Guidance for an Empty Descriptor"][oci-manifest]
- [ORAS manifest-config concepts][oras-manifest-config]
- [GitLab Container Registry supported media types][gitlab-media-types]

[oci-manifest]: https://github.com/opencontainers/image-spec/blob/main/manifest.md
[oras-manifest-config]: https://oras.land/docs/concepts/manifest/
[gitlab-media-types]: https://docs.gitlab.com/ee/user/packages/container_registry/

---

## Changelog

| Date | Author | Change |
|------|--------|--------|
| 2026-06-19 | Michael Herwig | Initial draft, accepted; supersedes adr_oci_artifact_type.md |
| 2026-06-19 | Michael Herwig | Revised after real-GitLab testing: drop the custom artifactType too (GitLab rejects it); kind rides on the com.grimoire.kind annotation |
| 2026-06-19 | Michael Herwig | Precision amendments (max-tier review): note the empty-config-without-artifactType spec deviation (OCI 1.1 MUST); expand the Referrers/cosign forward-cost (sign-by-digest still works; type-filtering + future `grim list --remote` lost); record that kind now rests on a forgeable annotation (pre-existing trust model, relocated) |
