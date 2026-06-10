// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! `grim build` — validate + pack a local skill/rule, no push.
//!
//! Auto-detects the kind: a directory containing `SKILL.md` is a skill;
//! a single `.md` file is a rule (`--kind` overrides). The artifact is
//! validated against the Agent Skills standard, packed into the exact
//! uncompressed-tar layout the installer extracts, and the OCI
//! annotations are computed. Nothing is pushed — `build` is the local
//! pre-flight for `release`.

use std::path::Path;

use clap::Args;

use crate::api::build_report::BuildReport;
use crate::cli::exit_code::ExitCode;
use crate::context::Context;
use crate::oci::ArtifactKind;
use crate::oci::annotations::{annotations_for_rule, annotations_for_skill};
use crate::skill::rule_frontmatter::RuleFrontmatter;
use crate::skill::{pack_rule_file, pack_skill_dir, validate_rule_file, validate_skill_dir};

/// `grim build` arguments.
#[derive(Debug, Args)]
pub struct BuildArgs {
    /// Path to a skill directory or a rule `.md` file.
    pub path: std::path::PathBuf,

    /// Force the artifact kind instead of auto-detecting it.
    #[arg(long, value_parser = ["skill", "rule", "bundle"])]
    pub kind: Option<String>,
}

/// The validated + packed artifact, shared by `build` and `release`.
pub struct PackedArtifact {
    /// Skill or rule.
    pub kind: ArtifactKind,
    /// The artifact name (skill dir name / rule file stem).
    pub name: String,
    /// The uncompressed-tar layer bytes.
    pub tar: Vec<u8>,
    /// The OCI annotations for `version`.
    pub annotations: std::collections::BTreeMap<String, String>,
}

/// Detect the artifact kind from `path` and an optional `--kind`.
pub fn detect_kind(path: &Path, forced: Option<&str>) -> anyhow::Result<ArtifactKind> {
    if let Some(k) = forced {
        return Ok(match k {
            "skill" => ArtifactKind::Skill,
            "bundle" => ArtifactKind::Bundle,
            _ => ArtifactKind::Rule,
        });
    }
    if path.is_dir() && path.join("SKILL.md").is_file() {
        Ok(ArtifactKind::Skill)
    } else if path.is_file() && path.extension().is_some_and(|e| e == "toml") {
        // A `.toml` source file lists bundle members ([skills]/[rules]).
        Ok(ArtifactKind::Bundle)
    } else if path.is_file() && path.extension().is_some_and(|e| e == "md") {
        Ok(ArtifactKind::Rule)
    } else {
        Err(crate::error::Error::from(crate::skill::SkillError::new(
            path,
            crate::skill::SkillErrorKind::MissingSkillMd,
        ))
        .into())
    }
}

/// Validate, pack, and compute annotations for the artifact at `path`.
///
/// `version` is the release version used in the annotations (`build`
/// passes a placeholder; `release` passes the real version).
pub fn validate_and_pack(
    path: &Path,
    kind: ArtifactKind,
    version: &str,
    source: Option<&str>,
) -> anyhow::Result<PackedArtifact> {
    match kind {
        // Bundles are packed on a dedicated path (`pack_bundle`); the
        // skill/rule validator never receives one.
        ArtifactKind::Bundle => unreachable!("bundles are packed via the bundle path, not validate_and_pack"),
        ArtifactKind::Skill => {
            let fm = super::grim(validate_skill_dir(path))?;
            let tar = super::grim(pack_skill_dir(path))?;
            let annotations = annotations_for_skill(&fm, version, source);
            Ok(PackedArtifact {
                kind,
                name: fm.name.to_string(),
                tar,
                annotations,
            })
        }
        ArtifactKind::Rule => {
            let fm = super::grim(validate_rule_file(path))?;
            let doc = std::fs::read_to_string(path).map_err(|e| {
                crate::error::Error::from(crate::skill::SkillError::new(path, crate::skill::SkillErrorKind::Io(e)))
            })?;
            let parsed = super::grim(RuleFrontmatter::parse_doc(&doc, path))?;
            let name = path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "rule".to_string());
            let tar = super::grim(pack_rule_file(path))?;
            let annotations = annotations_for_rule(&name, &fm, &parsed.body, version, source);
            Ok(PackedArtifact {
                kind,
                name,
                tar,
                annotations,
            })
        }
    }
}

/// Parse a bundle source file (a `grimoire.toml`-shaped document whose
/// `[skills]`/`[rules]` tables are the members, with optional top-level
/// `summary`/`keywords`/`description`) into its name, member list, and
/// catalog metadata. The bundle name is the file stem.
///
/// # Errors
///
/// A config parse/validation failure (78/79/74) or an I/O error.
pub fn read_bundle_members(
    path: &Path,
) -> anyhow::Result<(
    String,
    Vec<crate::oci::bundle::BundleMember>,
    crate::config::project_config::BundleMetadata,
)> {
    use crate::oci::bundle::BundleMember;

    let content = std::fs::read_to_string(path).map_err(|e| {
        crate::error::Error::from(crate::skill::SkillError::new(path, crate::skill::SkillErrorKind::Io(e)))
    })?;
    let source = super::grim(crate::config::project_config::BundleSource::from_toml_str(&content))?;

    let mut members = Vec::new();
    for (name, id) in &source.skills {
        members.push(BundleMember {
            kind: ArtifactKind::Skill,
            name: name.clone(),
            id: id.to_string(),
        });
    }
    for (name, id) in &source.rules {
        members.push(BundleMember {
            kind: ArtifactKind::Rule,
            name: name.clone(),
            id: id.to_string(),
        });
    }

    let name = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "bundle".to_string());
    Ok((name, members, source.metadata))
}

/// Run `grim build`.
///
/// # Errors
///
/// A validation / packaging failure surfaces as a `SkillError`
/// (DataError 65) or an I/O error (74).
pub async fn run(_ctx: &Context, args: &BuildArgs) -> anyhow::Result<(BuildReport, ExitCode)> {
    let kind = detect_kind(&args.path, args.kind.as_deref())?;

    if kind == ArtifactKind::Bundle {
        let (name, members, _metadata) = read_bundle_members(&args.path)?;
        let manifest = crate::oci::bundle::BundleManifest::new(members);
        let layer = manifest
            .to_layer_bytes()
            .map_err(|e| anyhow::anyhow!("failed to serialize bundle layer: {e}"))?;
        let layer_digest = crate::oci::Algorithm::Sha256.hash(&layer).to_string();
        // Member count stands in for the annotation count in the report.
        let report = BuildReport::new(kind, name, args.path.clone(), layer_digest, manifest.members.len());
        return Ok((report, ExitCode::Success));
    }

    // `build` is a local pre-flight: the version is a placeholder, no
    // source — `release` recomputes annotations with the real version.
    let packed = validate_and_pack(&args.path, kind, "0.0.0-build", None)?;
    let layer_digest = crate::oci::Algorithm::Sha256.hash(&packed.tar).to_string();
    let report = BuildReport::new(
        packed.kind,
        packed.name,
        args.path.clone(),
        layer_digest,
        packed.annotations.len(),
    );
    Ok((report, ExitCode::Success))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(p: &Path, body: &str) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }

    #[test]
    fn detect_kind_skill_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("code-review");
        write(&dir.join("SKILL.md"), "---\nname: code-review\ndescription: d\n---\n");
        assert_eq!(detect_kind(&dir, None).unwrap(), ArtifactKind::Skill);
        assert_eq!(detect_kind(&dir, Some("rule")).unwrap(), ArtifactKind::Rule);
    }

    #[test]
    fn detect_kind_rule_file() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("rust-style.md");
        write(&f, "# rule\n");
        assert_eq!(detect_kind(&f, None).unwrap(), ArtifactKind::Rule);
    }

    #[test]
    fn detect_kind_rejects_unknown() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("notes.txt");
        write(&f, "x");
        assert!(detect_kind(&f, None).is_err());
    }

    #[test]
    fn validate_and_pack_skill_round_trips() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("code-review");
        write(
            &dir.join("SKILL.md"),
            "---\nname: code-review\ndescription: Review code.\nmetadata:\n  keywords: a,b\n---\n# Body\n",
        );
        let packed = validate_and_pack(&dir, ArtifactKind::Skill, "1.2.3", Some("src")).unwrap();
        assert_eq!(packed.name, "code-review");
        assert!(!packed.tar.is_empty());
        assert_eq!(packed.annotations["org.opencontainers.image.version"], "1.2.3");
        assert_eq!(packed.annotations["org.opencontainers.image.title"], "code-review");
        // The kind is carried by the OCI artifactType, not an annotation.
        assert!(!packed.annotations.contains_key("com.grimoire.kind"));
    }

    #[test]
    fn validate_and_pack_bad_skill_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("code-review");
        write(&dir.join("SKILL.md"), "---\nname: wrong-name\ndescription: d\n---\n");
        assert!(validate_and_pack(&dir, ArtifactKind::Skill, "1.0.0", None).is_err());
    }
}
