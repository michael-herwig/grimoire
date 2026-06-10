// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! Map skill/rule frontmatter + metadata onto OCI manifest annotations.
//!
//! On publish the source-of-truth metadata in `SKILL.md` (and a rule's
//! body) is mirrored into the standard
//! `org.opencontainers.image.{title,description,version,licenses,source}`
//! keys plus the Grimoire-specific `com.grimoire.keywords` and an optional
//! `com.grimoire.summary` (a short, single-line blurb for catalog display,
//! distinct from the longer `description`). The artifact
//! kind is NOT an annotation — it is carried by the OCI `artifactType`
//! (see [`crate::oci::ArtifactKind::artifact_type`]). The mapping is
//! **fully deterministic**: `org.opencontainers.image.created` is
//! intentionally omitted because a wall-clock timestamp would make a
//! re-release of identical content produce a different manifest digest,
//! breaking the idempotent-release contract (reproducible-build practice
//! drops volatile timestamps for the same reason). Rules have no
//! description frontmatter, so the title/description are derived from the
//! rule name and body with a sane default.

use std::collections::BTreeMap;

use crate::oci::ArtifactKind;
use crate::oci::manifest::OciManifest;
use crate::skill::{RuleFrontmatter, SkillFrontmatter};

/// Infer the artifact kind from a pulled manifest's OCI type: the
/// `artifactType` first, then the config descriptor's media type as a
/// fallback. `None` when neither names a known Grimoire kind (e.g. a foreign
/// image) — the single read path shared by `add` (kind inference) and the
/// catalog.
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
}

/// Build the manifest annotation map for a skill.
///
/// `source` is the optional canonical source reference (e.g. the release
/// ref). `com.grimoire.keywords` and `com.grimoire.summary` are emitted
/// only when the frontmatter `metadata.keywords` / `metadata.summary`
/// keys are present. The map is fully deterministic
/// (no wall-clock `created`) so re-release is idempotent.
pub fn annotations_for_skill(fm: &SkillFrontmatter, version: &str, source: Option<&str>) -> BTreeMap<String, String> {
    let mut a = BTreeMap::new();
    a.insert("org.opencontainers.image.title".to_string(), fm.name.to_string());
    a.insert(
        "org.opencontainers.image.description".to_string(),
        fm.description.to_string(),
    );
    a.insert("org.opencontainers.image.version".to_string(), version.to_string());
    if let Some(license) = &fm.license {
        a.insert("org.opencontainers.image.licenses".to_string(), license.clone());
    }
    if let Some(src) = source {
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
/// `extra` map (`keywords` key, comma-joined if a sequence).
pub fn annotations_for_rule(
    name: &str,
    fm: &RuleFrontmatter,
    body: &str,
    version: &str,
    source: Option<&str>,
) -> BTreeMap<String, String> {
    let mut a = BTreeMap::new();
    a.insert("org.opencontainers.image.title".to_string(), name.to_string());
    let description = RuleFrontmatter::derive_description(body).unwrap_or_else(|| format!("grimoire rule {name}"));
    a.insert("org.opencontainers.image.description".to_string(), description);
    a.insert("org.opencontainers.image.version".to_string(), version.to_string());
    if let Some(src) = source {
        a.insert("org.opencontainers.image.source".to_string(), src.to_string());
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

/// Build the manifest annotation map for a bundle.
///
/// The title is the bundle `name`. Catalog metadata is authored at the top
/// of the bundle source file: `summary` / `keywords` emit the Grimoire
/// annotations (only when present), and `description` overrides the
/// otherwise-deterministic `grimoire bundle of N members` default. The map
/// stays deterministic (no wall-clock `created`) so re-release is
/// idempotent — see [`annotations_for_skill`].
pub fn annotations_for_bundle(
    name: &str,
    version: &str,
    member_count: usize,
    source: Option<&str>,
    summary: Option<&str>,
    keywords: Option<&str>,
    description: Option<&str>,
) -> BTreeMap<String, String> {
    let mut a = BTreeMap::new();
    a.insert("org.opencontainers.image.title".to_string(), name.to_string());
    let description = description.map_or_else(|| format!("grimoire bundle of {member_count} members"), str::to_string);
    a.insert("org.opencontainers.image.description".to_string(), description);
    a.insert("org.opencontainers.image.version".to_string(), version.to_string());
    if let Some(src) = source {
        a.insert("org.opencontainers.image.source".to_string(), src.to_string());
    }
    if let Some(summary) = summary {
        a.insert("com.grimoire.summary".to_string(), summary.to_string());
    }
    if let Some(keywords) = keywords {
        a.insert("com.grimoire.keywords".to_string(), keywords.to_string());
    }
    a
}

/// Extract a scalar string `key` from a rule's forward-compat `extra` map.
/// Catalog metadata (`keywords`, `summary`) is authored as a plain string —
/// keywords are comma-separated, matching the on-the-wire annotation and the
/// skill `metadata` map. Non-string values are ignored.
fn string_from_extra(fm: &RuleFrontmatter, key: &str) -> Option<String> {
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
        // The kind is NOT an annotation — it rides on the OCI artifactType.
        assert!(!a.contains_key("com.grimoire.kind"));
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
        assert!(!a.contains_key("com.grimoire.kind"));
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
    fn bundle_metadata_emits_annotations_and_overrides_description() {
        let a = annotations_for_bundle(
            "python-stack",
            "1.0.0",
            3,
            None,
            Some("Python dev stack"),
            Some("python,lint"),
            Some("Skills and rules for Python work"),
        );
        assert_eq!(a["org.opencontainers.image.title"], "python-stack");
        assert_eq!(
            a["org.opencontainers.image.description"],
            "Skills and rules for Python work"
        );
        assert_eq!(a["com.grimoire.summary"], "Python dev stack");
        assert_eq!(a["com.grimoire.keywords"], "python,lint");
    }

    #[test]
    fn bundle_without_metadata_uses_default_description() {
        let a = annotations_for_bundle("python-stack", "1.0.0", 2, None, None, None, None);
        assert_eq!(
            a["org.opencontainers.image.description"],
            "grimoire bundle of 2 members"
        );
        assert!(!a.contains_key("com.grimoire.summary"));
        assert!(!a.contains_key("com.grimoire.keywords"));
    }
}
