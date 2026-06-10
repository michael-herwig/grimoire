// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! Map skill/rule frontmatter + metadata onto OCI manifest annotations.
//!
//! On publish the source-of-truth metadata in `SKILL.md` (and a rule's
//! body) is mirrored into the standard
//! `org.opencontainers.image.{title,description,version,licenses,source}`
//! keys plus the Grimoire-specific `com.grimoire.kind` /
//! `com.grimoire.keywords`. The mapping is **fully deterministic**:
//! `org.opencontainers.image.created` is intentionally omitted because a
//! wall-clock timestamp would make a re-release of identical content
//! produce a different manifest digest, breaking the idempotent-release
//! contract (reproducible-build practice drops volatile timestamps for
//! the same reason). Rules have no description frontmatter, so the
//! title/description are derived from the rule name and body with a sane
//! default.

use std::collections::BTreeMap;

use crate::oci::ArtifactKind;
use crate::oci::artifact_kind::KIND_ANNOTATION;
use crate::oci::manifest::OciManifest;
use crate::skill::{RuleFrontmatter, SkillFrontmatter};

/// Read the artifact kind from a pulled manifest's `com.grimoire.kind`
/// annotation. `None` when the annotation is absent or not a known kind —
/// the single read path shared by `add` (kind inference) and the catalog.
pub fn kind_from_manifest(manifest: &OciManifest) -> Option<ArtifactKind> {
    manifest
        .annotations
        .get(KIND_ANNOTATION)
        .and_then(|s| ArtifactKind::from_annotation(s))
}

/// Build the manifest annotation map for a skill.
///
/// `source` is the optional canonical source reference (e.g. the release
/// ref). `com.grimoire.keywords` is emitted only when the frontmatter
/// `metadata.keywords` key is present. The map is fully deterministic
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
    a.insert(KIND_ANNOTATION.to_string(), "skill".to_string());
    if let Some(kw) = fm.metadata.get("keywords") {
        a.insert("com.grimoire.keywords".to_string(), kw.clone());
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
    a.insert(KIND_ANNOTATION.to_string(), "rule".to_string());
    if let Some(kw) = keywords_from_extra(fm) {
        a.insert("com.grimoire.keywords".to_string(), kw);
    }
    a
}

/// Build the manifest annotation map for a bundle.
///
/// A bundle has no frontmatter: the title is its `name`, the description is
/// a deterministic summary of the member count. Deterministic (no
/// wall-clock `created`) so re-release is idempotent — see
/// [`annotations_for_skill`].
pub fn annotations_for_bundle(
    name: &str,
    version: &str,
    member_count: usize,
    source: Option<&str>,
) -> BTreeMap<String, String> {
    let mut a = BTreeMap::new();
    a.insert("org.opencontainers.image.title".to_string(), name.to_string());
    a.insert(
        "org.opencontainers.image.description".to_string(),
        format!("grimoire bundle of {member_count} members"),
    );
    a.insert("org.opencontainers.image.version".to_string(), version.to_string());
    if let Some(src) = source {
        a.insert("org.opencontainers.image.source".to_string(), src.to_string());
    }
    a.insert(KIND_ANNOTATION.to_string(), "bundle".to_string());
    a
}

/// Extract a `keywords` value from a rule's forward-compat `extra` map,
/// accepting either a scalar string or a YAML sequence of strings.
fn keywords_from_extra(fm: &RuleFrontmatter) -> Option<String> {
    match fm.extra.get("keywords")? {
        serde_yaml::Value::String(s) => Some(s.clone()),
        serde_yaml::Value::Sequence(seq) => {
            let parts: Vec<String> = seq.iter().filter_map(|v| v.as_str().map(str::to_string)).collect();
            if parts.is_empty() { None } else { Some(parts.join(",")) }
        }
        _ => None,
    }
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
        assert_eq!(a["com.grimoire.kind"], "skill");
        assert_eq!(a["com.grimoire.keywords"], "review,quality");
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
    }

    #[test]
    fn rule_annotations_derive_title_and_description() {
        let rf = RuleFrontmatter::default();
        let a = annotations_for_rule("rust-style", &rf, "# Rust Style\nbody\n", "3.0.0", None);
        assert_eq!(a["org.opencontainers.image.title"], "rust-style");
        assert_eq!(a["org.opencontainers.image.description"], "Rust Style");
        assert_eq!(a["com.grimoire.kind"], "rule");
        assert_eq!(a["org.opencontainers.image.version"], "3.0.0");
    }

    #[test]
    fn rule_without_body_uses_default_description() {
        let rf = RuleFrontmatter::default();
        let a = annotations_for_rule("rust-style", &rf, "\n\n", "1.0.0", None);
        assert_eq!(a["org.opencontainers.image.description"], "grimoire rule rust-style");
    }

    #[test]
    fn kind_from_manifest_reads_annotation() {
        use crate::oci::manifest::OciManifest;
        let mut annotations = BTreeMap::new();
        annotations.insert(KIND_ANNOTATION.to_string(), "rule".to_string());
        let manifest = OciManifest {
            media_type: None,
            layers: vec![],
            annotations,
        };
        assert_eq!(kind_from_manifest(&manifest), Some(crate::oci::ArtifactKind::Rule));

        // Absent / unknown annotation ⇒ None (caller must ask for --kind).
        let bare = OciManifest {
            media_type: None,
            layers: vec![],
            annotations: BTreeMap::new(),
        };
        assert_eq!(kind_from_manifest(&bare), None);
    }

    #[test]
    fn rule_keywords_from_extra_sequence() {
        let doc = "---\npaths: [\"a\"]\nkeywords:\n  - rust\n  - style\n---\nbody\n";
        let parsed = RuleFrontmatter::parse_doc(doc, Path::new("r.md")).unwrap();
        let a = annotations_for_rule("r", &parsed.frontmatter, &parsed.body, "1.0.0", None);
        assert_eq!(a["com.grimoire.keywords"], "rust,style");
    }
}
