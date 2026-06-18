// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! Map skill/rule frontmatter + metadata onto OCI manifest annotations.
//!
//! On publish the source-of-truth metadata in `SKILL.md` (and a rule's
//! body) is mirrored into the standard
//! `org.opencontainers.image.{title,description,version,licenses,source}`
//! keys plus the Grimoire-specific `com.grimoire.keywords` and an optional
//! `com.grimoire.summary` (a short, single-line blurb for catalog display,
//! distinct from the longer `description`). The artifact kind is carried on
//! the wire by the `com.grimoire.kind` annotation ([`KIND_ANNOTATION`]):
//! GitLab's media-type allowlist rejects both a custom config media type and
//! a custom OCI `artifactType`, so neither is written, and the config
//! descriptor is the OCI empty type (`adr_oci_empty_config_compat.md`). The
//! `artifactType` (see [`crate::oci::ArtifactKind::artifact_type`]) is still
//! resolved on the READ path as the first tier, to type artifacts published
//! before this change. The mapping is
//! **fully deterministic**: `org.opencontainers.image.created` is
//! intentionally omitted because a wall-clock timestamp would make a
//! re-release of identical content produce a different manifest digest,
//! breaking the idempotent-release contract (reproducible-build practice
//! drops volatile timestamps for the same reason). Rules have no
//! description frontmatter, so the title/description are derived from the
//! rule name and body with a sane default.
//!
//! `org.opencontainers.image.source` carries the authored `repository`
//! metadata value (an HTTPS URL to the artifact's source repository — the
//! OCI-spec meaning of the key) when present, falling back to the tagless
//! release reference (`registry/repository`) for continuity. Consumers
//! distinguish the two by the `https://` prefix.

use std::collections::BTreeMap;

use crate::config::project_config::BundleMetadata;
use crate::oci::ArtifactKind;
use crate::oci::artifact_kind::KIND_ANNOTATION;
use crate::oci::manifest::OciManifest;
use crate::skill::{AgentFrontmatter, RuleFrontmatter, SkillFrontmatter};

/// An authored `repository` metadata value that is not an HTTPS URL.
///
/// Raised at publish time (`grim build` / `grim release`) so a bad value
/// can never reach a registry; classified as DataError (65) through
/// [`crate::skill::SkillErrorKind::MetadataInvalid`].
#[derive(thiserror::Error, Debug)]
#[error("invalid value '{value}' for metadata key 'repository': expected an https:// URL")]
pub struct RepositoryUrlError {
    /// The rejected authored value.
    pub value: String,
}

/// Validate an authored repository URL (publish-time gate).
///
/// # Errors
///
/// [`RepositoryUrlError`] when `value` does not start with `https://`.
pub fn validate_repository_url(value: &str) -> Result<(), RepositoryUrlError> {
    if value.starts_with("https://") {
        Ok(())
    } else {
        Err(RepositoryUrlError {
            value: value.to_string(),
        })
    }
}

/// Infer the artifact kind from a pulled manifest, resolving three tiers in
/// order: (1) the OCI `artifactType` and (2) the config descriptor's media
/// type (`application/vnd.grimoire.<kind>.config.v1+json`) — both written by
/// grim *before* `adr_oci_empty_config_compat.md` and retained here so those
/// artifacts stay readable — then (3) the `com.grimoire.kind` annotation, the
/// discriminator grim writes today (the wire carries no `artifactType` because
/// GitLab rejects it). `None` when none names a known Grimoire kind (e.g. a
/// foreign image) — the single read path shared by `add` (kind inference) and
/// the catalog.
pub fn kind_from_manifest(manifest: &OciManifest) -> Option<ArtifactKind> {
    manifest
        .artifact_type
        .as_deref()
        .and_then(ArtifactKind::from_artifact_type)
        .or_else(|| {
            manifest
                .config_media_type
                .as_deref()
                .and_then(ArtifactKind::from_config_media_type)
        })
        .or_else(|| {
            manifest
                .annotations
                .get(KIND_ANNOTATION)
                .map(String::as_str)
                .and_then(ArtifactKind::from_kind_str)
        })
}

/// Build the manifest annotation map for a skill.
///
/// `fallback_source` is the release reference used for
/// `org.opencontainers.image.source` only when the frontmatter has no
/// authored `metadata.repository` URL. `com.grimoire.keywords` and
/// `com.grimoire.summary` are emitted only when the frontmatter
/// `metadata.keywords` / `metadata.summary` keys are present. The map is
/// fully deterministic (no wall-clock `created`) so re-release is
/// idempotent.
pub fn annotations_for_skill(
    fm: &SkillFrontmatter,
    version: &str,
    fallback_source: Option<&str>,
) -> BTreeMap<String, String> {
    let mut a = BTreeMap::new();
    a.insert("org.opencontainers.image.title".to_string(), fm.name.to_string());
    a.insert(
        "org.opencontainers.image.description".to_string(),
        fm.description.to_string(),
    );
    a.insert("org.opencontainers.image.version".to_string(), version.to_string());
    // Registry-agnostic kind fallback (`adr_oci_empty_config_compat.md`): the
    // config descriptor no longer carries the kind, so mirror it here for any
    // reader that cannot rely on the OCI `artifactType`.
    a.insert(KIND_ANNOTATION.to_string(), ArtifactKind::Skill.to_string());
    if let Some(license) = &fm.license {
        a.insert("org.opencontainers.image.licenses".to_string(), license.clone());
    }
    if let Some(src) = fm.metadata.get("repository").map(String::as_str).or(fallback_source) {
        a.insert("org.opencontainers.image.source".to_string(), src.to_string());
    }
    // `org.opencontainers.image.created` is intentionally OMITTED: a
    // wall-clock timestamp in the manifest would make a re-release of
    // identical content produce a different manifest digest, breaking the
    // idempotent-re-release contract (a hard requirement for a package
    // manager). A deterministic content digest is the stronger guarantee;
    // reproducible-build practice drops volatile timestamps for the same
    // reason.
    if let Some(kw) = fm.metadata.get("keywords") {
        a.insert("com.grimoire.keywords".to_string(), kw.clone());
    }
    if let Some(summary) = fm.metadata.get("summary") {
        a.insert("com.grimoire.summary".to_string(), summary.clone());
    }
    a
}

/// Build the manifest annotation map for a rule.
///
/// A rule has no description frontmatter: the title is the rule `name`,
/// the description is the first heading/paragraph of `body` or a
/// deterministic default. Keywords come from the rule frontmatter's
/// `extra` map (`keywords` key, comma-joined if a sequence). An authored
/// `repository` URL (also from `extra`) wins over `fallback_source` for
/// `org.opencontainers.image.source`.
pub fn annotations_for_rule(
    name: &str,
    fm: &RuleFrontmatter,
    body: &str,
    version: &str,
    fallback_source: Option<&str>,
) -> BTreeMap<String, String> {
    let mut a = BTreeMap::new();
    a.insert("org.opencontainers.image.title".to_string(), name.to_string());
    let description = RuleFrontmatter::derive_description(body).unwrap_or_else(|| format!("grimoire rule {name}"));
    a.insert("org.opencontainers.image.description".to_string(), description);
    a.insert("org.opencontainers.image.version".to_string(), version.to_string());
    // Registry-agnostic kind fallback — see `annotations_for_skill`.
    a.insert(KIND_ANNOTATION.to_string(), ArtifactKind::Rule.to_string());
    if let Some(src) = string_from_extra(fm, "repository").or_else(|| fallback_source.map(str::to_string)) {
        a.insert("org.opencontainers.image.source".to_string(), src);
    }
    // Omitted for idempotent re-release — see `annotations_for_skill`.
    if let Some(kw) = string_from_extra(fm, "keywords") {
        a.insert("com.grimoire.keywords".to_string(), kw);
    }
    if let Some(summary) = string_from_extra(fm, "summary") {
        a.insert("com.grimoire.summary".to_string(), summary);
    }
    a
}

/// Build the manifest annotation map for an agent.
///
/// Agents carry a real `name`/`description` in their required frontmatter
/// (like skills); catalog metadata (`keywords`, `summary`, `repository`)
/// comes from the `metadata` map. The map stays deterministic (no
/// wall-clock `created`) so re-release is idempotent — see
/// [`annotations_for_skill`].
pub fn annotations_for_agent(
    fm: &AgentFrontmatter,
    version: &str,
    fallback_source: Option<&str>,
) -> BTreeMap<String, String> {
    let mut a = BTreeMap::new();
    a.insert("org.opencontainers.image.title".to_string(), fm.name.to_string());
    a.insert(
        "org.opencontainers.image.description".to_string(),
        fm.description.to_string(),
    );
    a.insert("org.opencontainers.image.version".to_string(), version.to_string());
    // Registry-agnostic kind fallback — see `annotations_for_skill`.
    a.insert(KIND_ANNOTATION.to_string(), ArtifactKind::Agent.to_string());
    if let Some(src) = fm.metadata.get("repository").map(String::as_str).or(fallback_source) {
        a.insert("org.opencontainers.image.source".to_string(), src.to_string());
    }
    // Omitted `created` for idempotent re-release — see `annotations_for_skill`.
    if let Some(kw) = fm.metadata.get("keywords") {
        a.insert("com.grimoire.keywords".to_string(), kw.clone());
    }
    if let Some(summary) = fm.metadata.get("summary") {
        a.insert("com.grimoire.summary".to_string(), summary.clone());
    }
    a
}

/// Build the manifest annotation map for a bundle.
///
/// The title is the bundle `name`. Catalog metadata is authored at the top
/// of the bundle source file: `summary` / `keywords` emit the Grimoire
/// annotations (only when present), `description` overrides the
/// otherwise-deterministic `grimoire bundle of N members` default, and an
/// authored `repository` URL wins over `fallback_source` for
/// `org.opencontainers.image.source`. The map stays deterministic (no
/// wall-clock `created`) so re-release is idempotent — see
/// [`annotations_for_skill`].
pub fn annotations_for_bundle(
    name: &str,
    version: &str,
    member_count: usize,
    fallback_source: Option<&str>,
    metadata: &BundleMetadata,
) -> BTreeMap<String, String> {
    let mut a = BTreeMap::new();
    a.insert("org.opencontainers.image.title".to_string(), name.to_string());
    let description = metadata
        .description
        .clone()
        .unwrap_or_else(|| format!("grimoire bundle of {member_count} members"));
    a.insert("org.opencontainers.image.description".to_string(), description);
    a.insert("org.opencontainers.image.version".to_string(), version.to_string());
    // Registry-agnostic kind fallback — see `annotations_for_skill`.
    a.insert(KIND_ANNOTATION.to_string(), ArtifactKind::Bundle.to_string());
    if let Some(src) = metadata.repository.as_deref().or(fallback_source) {
        a.insert("org.opencontainers.image.source".to_string(), src.to_string());
    }
    if let Some(summary) = &metadata.summary {
        a.insert("com.grimoire.summary".to_string(), summary.clone());
    }
    if let Some(keywords) = &metadata.keywords {
        a.insert("com.grimoire.keywords".to_string(), keywords.clone());
    }
    a
}

/// Extract a scalar string `key` from a rule's forward-compat `extra` map.
/// Catalog metadata (`keywords`, `summary`, `repository`) is authored as a
/// plain string — keywords are comma-separated, matching the on-the-wire
/// annotation and the skill `metadata` map. Non-string values are ignored.
pub fn string_from_extra(fm: &RuleFrontmatter, key: &str) -> Option<String> {
    fm.extra.get(key)?.as_str().map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn skill_fm() -> SkillFrontmatter {
        let doc = "---\nname: code-review\ndescription: Review code.\nlicense: Apache-2.0\nmetadata:\n  keywords: review,quality\n---\n";
        SkillFrontmatter::parse_doc(doc, Path::new("SKILL.md")).unwrap()
    }

    #[test]
    fn skill_annotations_are_fully_deterministic() {
        let fm = skill_fm();
        let a = annotations_for_skill(&fm, "1.2.3", Some("ghcr.io/acme/code-review:1.2.3"));
        assert_eq!(a["org.opencontainers.image.title"], "code-review");
        assert_eq!(a["org.opencontainers.image.description"], "Review code.");
        assert_eq!(a["org.opencontainers.image.version"], "1.2.3");
        assert_eq!(a["org.opencontainers.image.licenses"], "Apache-2.0");
        assert_eq!(a["org.opencontainers.image.source"], "ghcr.io/acme/code-review:1.2.3");
        assert_eq!(a["com.grimoire.keywords"], "review,quality");
        // The kind rides on the OCI artifactType AND is mirrored into the
        // `com.grimoire.kind` annotation as a registry-agnostic fallback
        // (`adr_oci_empty_config_compat.md`).
        assert_eq!(a["com.grimoire.kind"], "skill");
        // `created` is intentionally absent so re-release is idempotent.
        assert!(!a.contains_key("org.opencontainers.image.created"));

        // Identical inputs ⇒ byte-identical annotations (idempotency).
        let b = annotations_for_skill(&fm, "1.2.3", Some("ghcr.io/acme/code-review:1.2.3"));
        assert_eq!(a, b);
    }

    #[test]
    fn skill_without_license_or_keywords_omits_them() {
        let fm = SkillFrontmatter::parse_doc("---\nname: s\ndescription: d\n---\n", Path::new("SKILL.md")).unwrap();
        let a = annotations_for_skill(&fm, "0.1.0", None);
        assert!(!a.contains_key("org.opencontainers.image.licenses"));
        assert!(!a.contains_key("org.opencontainers.image.source"));
        assert!(!a.contains_key("com.grimoire.keywords"));
        assert!(!a.contains_key("com.grimoire.summary"));
    }

    #[test]
    fn skill_summary_from_metadata() {
        let doc = "---\nname: s\ndescription: A long description that explains the skill in detail.\nmetadata:\n  summary: short blurb\n---\n";
        let fm = SkillFrontmatter::parse_doc(doc, Path::new("SKILL.md")).unwrap();
        let a = annotations_for_skill(&fm, "0.1.0", None);
        assert_eq!(a["com.grimoire.summary"], "short blurb");
        // The long description is still emitted verbatim and untouched.
        assert_eq!(
            a["org.opencontainers.image.description"],
            "A long description that explains the skill in detail."
        );
    }

    #[test]
    fn rule_summary_from_extra() {
        let doc = "---\npaths: [\"a\"]\nsummary: terse rule blurb\n---\n# Rule\nbody\n";
        let parsed = RuleFrontmatter::parse_doc(doc, Path::new("r.md")).unwrap();
        let a = annotations_for_rule("r", &parsed.frontmatter, &parsed.body, "1.0.0", None);
        assert_eq!(a["com.grimoire.summary"], "terse rule blurb");
    }

    #[test]
    fn rule_without_summary_omits_it() {
        let rf = RuleFrontmatter::default();
        let a = annotations_for_rule("r", &rf, "# Rule\nbody\n", "1.0.0", None);
        assert!(!a.contains_key("com.grimoire.summary"));
    }

    #[test]
    fn rule_annotations_derive_title_and_description() {
        let rf = RuleFrontmatter::default();
        let a = annotations_for_rule("rust-style", &rf, "# Rust Style\nbody\n", "3.0.0", None);
        assert_eq!(a["org.opencontainers.image.title"], "rust-style");
        assert_eq!(a["org.opencontainers.image.description"], "Rust Style");
        assert_eq!(a["org.opencontainers.image.version"], "3.0.0");
        assert_eq!(a["com.grimoire.kind"], "rule");
    }

    #[test]
    fn rule_without_body_uses_default_description() {
        let rf = RuleFrontmatter::default();
        let a = annotations_for_rule("rust-style", &rf, "\n\n", "1.0.0", None);
        assert_eq!(a["org.opencontainers.image.description"], "grimoire rule rust-style");
    }

    #[test]
    fn kind_from_manifest_prefers_artifact_type() {
        use crate::oci::manifest::OciManifest;
        // artifactType is authoritative even if the config media type is generic.
        let manifest = OciManifest {
            media_type: None,
            artifact_type: Some("application/vnd.grimoire.rule.v1".to_string()),
            config_media_type: Some("application/vnd.oci.image.config.v1+json".to_string()),
            layers: vec![],
            annotations: BTreeMap::new(),
        };
        assert_eq!(kind_from_manifest(&manifest), Some(crate::oci::ArtifactKind::Rule));
    }

    #[test]
    fn kind_from_manifest_falls_back_to_config_media_type() {
        use crate::oci::manifest::OciManifest;
        // No artifactType (e.g. a registry that dropped it) ⇒ use config media type.
        let manifest = OciManifest {
            media_type: None,
            artifact_type: None,
            config_media_type: Some("application/vnd.grimoire.bundle.config.v1+json".to_string()),
            layers: vec![],
            annotations: BTreeMap::new(),
        };
        assert_eq!(kind_from_manifest(&manifest), Some(crate::oci::ArtifactKind::Bundle));
    }

    #[test]
    fn kind_from_manifest_none_for_foreign_image() {
        use crate::oci::manifest::OciManifest;
        // Generic image config, no artifactType ⇒ None (caller must ask for --kind).
        let bare = OciManifest {
            media_type: None,
            artifact_type: None,
            config_media_type: Some("application/vnd.oci.image.config.v1+json".to_string()),
            layers: vec![],
            annotations: BTreeMap::new(),
        };
        assert_eq!(kind_from_manifest(&bare), None);
    }

    #[test]
    fn kind_from_manifest_artifact_type_tier_wins_when_present() {
        use crate::oci::manifest::OciManifest;
        // A legacy / non-GitLab artifact that still carries `artifactType`
        // alongside the OCI empty config and the `com.grimoire.kind` annotation:
        // tier 1 (`artifactType`) wins, and the empty config must NOT be
        // mistaken for a kind. (grim's own output today carries no
        // `artifactType` — that path is the tier-3 test below.)
        let mut annotations = BTreeMap::new();
        annotations.insert("com.grimoire.kind".to_string(), "skill".to_string());
        let manifest = OciManifest {
            media_type: None,
            artifact_type: Some("application/vnd.grimoire.skill.v1".to_string()),
            config_media_type: Some("application/vnd.oci.empty.v1+json".to_string()),
            layers: vec![],
            annotations,
        };
        assert_eq!(kind_from_manifest(&manifest), Some(crate::oci::ArtifactKind::Skill));
    }

    #[test]
    fn kind_from_manifest_falls_back_to_annotation_tier3() {
        use crate::oci::manifest::OciManifest;
        // No `artifactType` and the OCI empty config (or any non-grimoire
        // config) — the kind resolves from the `com.grimoire.kind` annotation
        // alone. This is the GitLab contingency path (registry drops/rejects
        // `artifactType`) and the pre-`artifactType` grim read path.
        let mut annotations = BTreeMap::new();
        annotations.insert("com.grimoire.kind".to_string(), "agent".to_string());
        let manifest = OciManifest {
            media_type: None,
            artifact_type: None,
            config_media_type: Some("application/vnd.oci.empty.v1+json".to_string()),
            layers: vec![],
            annotations,
        };
        assert_eq!(kind_from_manifest(&manifest), Some(crate::oci::ArtifactKind::Agent));
    }

    #[test]
    fn kind_from_manifest_legacy_config_only_still_resolves() {
        use crate::oci::manifest::OciManifest;
        // A legacy artifact (custom config type, no `artifactType`, no
        // annotation) still types via tier 2 — old published artifacts stay
        // readable.
        let manifest = OciManifest {
            media_type: None,
            artifact_type: None,
            config_media_type: Some("application/vnd.grimoire.rule.config.v1+json".to_string()),
            layers: vec![],
            annotations: BTreeMap::new(),
        };
        assert_eq!(kind_from_manifest(&manifest), Some(crate::oci::ArtifactKind::Rule));
    }

    #[test]
    fn kind_from_manifest_unknown_annotation_value_is_none() {
        use crate::oci::manifest::OciManifest;
        // A `com.grimoire.kind` annotation that is not a known kind must not
        // resolve — the read path stays strict (caller falls back to --kind).
        let mut annotations = BTreeMap::new();
        annotations.insert("com.grimoire.kind".to_string(), "widget".to_string());
        let manifest = OciManifest {
            media_type: None,
            artifact_type: None,
            config_media_type: Some("application/vnd.oci.empty.v1+json".to_string()),
            layers: vec![],
            annotations,
        };
        assert_eq!(kind_from_manifest(&manifest), None);
    }

    #[test]
    fn rule_keywords_from_extra_string() {
        let doc = "---\npaths: [\"a\"]\nkeywords: rust,style\n---\nbody\n";
        let parsed = RuleFrontmatter::parse_doc(doc, Path::new("r.md")).unwrap();
        let a = annotations_for_rule("r", &parsed.frontmatter, &parsed.body, "1.0.0", None);
        assert_eq!(a["com.grimoire.keywords"], "rust,style");
    }

    #[test]
    fn rule_keywords_sequence_is_ignored_string_only() {
        // Keywords are string-only in every authoring format; a YAML list is
        // not accepted (it is silently ignored, not joined).
        let doc = "---\npaths: [\"a\"]\nkeywords:\n  - rust\n  - style\n---\nbody\n";
        let parsed = RuleFrontmatter::parse_doc(doc, Path::new("r.md")).unwrap();
        let a = annotations_for_rule("r", &parsed.frontmatter, &parsed.body, "1.0.0", None);
        assert!(!a.contains_key("com.grimoire.keywords"));
    }

    #[test]
    fn agent_annotations_are_deterministic_and_complete() {
        let doc = "---\nname: code-reviewer\ndescription: Reviews diffs.\nmodel: sonnet\nmetadata:\n  keywords: review,agent\n  summary: terse blurb\n---\nbody\n";
        let parsed = AgentFrontmatter::parse_doc(doc, Path::new("code-reviewer.md")).unwrap();
        let a = annotations_for_agent(&parsed.frontmatter, "1.0.0", Some("ghcr.io/acme/code-reviewer:1.0.0"));
        assert_eq!(a["org.opencontainers.image.title"], "code-reviewer");
        assert_eq!(a["org.opencontainers.image.description"], "Reviews diffs.");
        assert_eq!(a["org.opencontainers.image.version"], "1.0.0");
        assert_eq!(a["org.opencontainers.image.source"], "ghcr.io/acme/code-reviewer:1.0.0");
        assert_eq!(a["com.grimoire.keywords"], "review,agent");
        assert_eq!(a["com.grimoire.summary"], "terse blurb");
        assert_eq!(a["com.grimoire.kind"], "agent");
        assert!(!a.contains_key("org.opencontainers.image.created"));
        let b = annotations_for_agent(&parsed.frontmatter, "1.0.0", Some("ghcr.io/acme/code-reviewer:1.0.0"));
        assert_eq!(a, b);
    }

    #[test]
    fn agent_without_catalog_metadata_omits_optional_keys() {
        let doc = "---\nname: a\ndescription: d\n---\nbody\n";
        let parsed = AgentFrontmatter::parse_doc(doc, Path::new("a.md")).unwrap();
        let a = annotations_for_agent(&parsed.frontmatter, "0.1.0", None);
        assert!(!a.contains_key("org.opencontainers.image.source"));
        assert!(!a.contains_key("com.grimoire.keywords"));
        assert!(!a.contains_key("com.grimoire.summary"));
    }

    #[test]
    fn bundle_metadata_emits_annotations_and_overrides_description() {
        let metadata = BundleMetadata {
            summary: Some("Python dev stack".to_string()),
            keywords: Some("python,lint".to_string()),
            description: Some("Skills and rules for Python work".to_string()),
            repository: None,
        };
        let a = annotations_for_bundle("python-stack", "1.0.0", 3, None, &metadata);
        assert_eq!(a["org.opencontainers.image.title"], "python-stack");
        assert_eq!(
            a["org.opencontainers.image.description"],
            "Skills and rules for Python work"
        );
        assert_eq!(a["com.grimoire.summary"], "Python dev stack");
        assert_eq!(a["com.grimoire.keywords"], "python,lint");
        assert_eq!(a["com.grimoire.kind"], "bundle");
    }

    #[test]
    fn bundle_without_metadata_uses_default_description() {
        let a = annotations_for_bundle("python-stack", "1.0.0", 2, None, &BundleMetadata::default());
        assert_eq!(
            a["org.opencontainers.image.description"],
            "grimoire bundle of 2 members"
        );
        assert!(!a.contains_key("com.grimoire.summary"));
        assert!(!a.contains_key("com.grimoire.keywords"));
    }

    #[test]
    fn validate_repository_url_requires_https() {
        assert!(validate_repository_url("https://github.com/acme/x").is_ok());
        for bad in [
            "http://github.com/acme/x",
            "git@github.com:acme/x.git",
            "ssh://git@x",
            "",
        ] {
            let err = validate_repository_url(bad).unwrap_err();
            assert!(err.to_string().contains("expected an https:// URL"), "{bad}: {err}");
        }
    }

    #[test]
    fn skill_repository_wins_over_fallback_source() {
        let doc = "---\nname: s\ndescription: d\nmetadata:\n  repository: https://github.com/acme/s\n---\n";
        let fm = SkillFrontmatter::parse_doc(doc, Path::new("SKILL.md")).unwrap();
        let a = annotations_for_skill(&fm, "1.0.0", Some("ghcr.io/acme/s"));
        assert_eq!(a["org.opencontainers.image.source"], "https://github.com/acme/s");
        // Without an authored repository the fallback ref is kept (continuity).
        let plain = SkillFrontmatter::parse_doc("---\nname: s\ndescription: d\n---\n", Path::new("SKILL.md")).unwrap();
        let b = annotations_for_skill(&plain, "1.0.0", Some("ghcr.io/acme/s"));
        assert_eq!(b["org.opencontainers.image.source"], "ghcr.io/acme/s");
    }

    #[test]
    fn rule_repository_wins_over_fallback_source() {
        let doc = "---\npaths: [\"a\"]\nrepository: https://gitlab.com/acme/r\n---\nbody\n";
        let parsed = RuleFrontmatter::parse_doc(doc, Path::new("r.md")).unwrap();
        let a = annotations_for_rule("r", &parsed.frontmatter, &parsed.body, "1.0.0", Some("ghcr.io/acme/r"));
        assert_eq!(a["org.opencontainers.image.source"], "https://gitlab.com/acme/r");
        // Non-string `repository` (string-only convention) is ignored ⇒ fallback.
        let seq = "---\npaths: [\"a\"]\nrepository:\n  - https://gitlab.com/acme/r\n---\nbody\n";
        let parsed = RuleFrontmatter::parse_doc(seq, Path::new("r.md")).unwrap();
        let b = annotations_for_rule("r", &parsed.frontmatter, &parsed.body, "1.0.0", Some("ghcr.io/acme/r"));
        assert_eq!(b["org.opencontainers.image.source"], "ghcr.io/acme/r");
    }

    #[test]
    fn agent_repository_wins_over_fallback_source() {
        let doc = "---\nname: a\ndescription: d\nmetadata:\n  repository: https://github.com/acme/a\n---\nbody\n";
        let parsed = AgentFrontmatter::parse_doc(doc, Path::new("a.md")).unwrap();
        let a = annotations_for_agent(&parsed.frontmatter, "1.0.0", Some("ghcr.io/acme/a"));
        assert_eq!(a["org.opencontainers.image.source"], "https://github.com/acme/a");
    }

    #[test]
    fn bundle_repository_wins_over_fallback_source() {
        let metadata = BundleMetadata {
            repository: Some("https://github.com/acme/stack".to_string()),
            ..BundleMetadata::default()
        };
        let a = annotations_for_bundle("stack", "1.0.0", 2, Some("ghcr.io/acme/stack"), &metadata);
        assert_eq!(a["org.opencontainers.image.source"], "https://github.com/acme/stack");
        let b = annotations_for_bundle(
            "stack",
            "1.0.0",
            2,
            Some("ghcr.io/acme/stack"),
            &BundleMetadata::default(),
        );
        assert_eq!(b["org.opencontainers.image.source"], "ghcr.io/acme/stack");
    }
}
