// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! Validate a local skill directory / rule file against the Agent Skills
//! standard and pack it into the exact uncompressed-tar layout the
//! [`crate::install::materializer::DefaultMaterializer`] expects.
//!
//! The pack ↔ install round-trip is a hard contract: `pack_skill_dir`
//! emits entries rooted at `<name>/` and `pack_rule_file` emits a single
//! `<name>.md`, byte-for-byte what the materializer (and the acceptance
//! harness `make_artifact`) extracts. The tar entries are emitted in
//! sorted path order so the layer digest is deterministic.

use std::path::{Path, PathBuf};

use super::rule_frontmatter::{ParsedRule, RuleFrontmatter};
use super::skill_error::{SkillError, SkillErrorKind};
use super::skill_frontmatter::SkillFrontmatter;
use super::skill_name::SkillName;

/// Validate the skill directory at `dir`.
///
/// Checks: `SKILL.md` is present and readable; its frontmatter parses and
/// the required fields are well-formed; the frontmatter `name` equals the
/// directory name.
///
/// # Errors
///
/// [`SkillErrorKind::MissingSkillMd`], [`SkillErrorKind::FrontmatterParse`],
/// [`SkillErrorKind::NameMismatch`], [`SkillErrorKind::NameInvalid`], or
/// [`SkillErrorKind::Io`].
pub fn validate_skill_dir(dir: &Path) -> Result<SkillFrontmatter, SkillError> {
    let skill_md = dir.join("SKILL.md");
    if !skill_md.is_file() {
        return Err(SkillError::new(dir, SkillErrorKind::MissingSkillMd));
    }
    let doc = std::fs::read_to_string(&skill_md).map_err(|e| SkillError::new(&skill_md, SkillErrorKind::Io(e)))?;
    let fm = SkillFrontmatter::parse_doc(&doc, &skill_md)?;

    let dir_name = dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .ok_or_else(|| {
            SkillError::new(
                dir,
                SkillErrorKind::NameInvalid("skill path has no directory name".to_string()),
            )
        })?;

    // The dir name must itself be a valid skill name and equal the
    // frontmatter name (the Agent Skills standard's directory-equality
    // rule).
    SkillName::parse(&dir_name).map_err(|e| SkillError::new(dir, SkillErrorKind::NameInvalid(e)))?;
    if fm.name.as_str() != dir_name {
        return Err(SkillError::new(
            dir,
            SkillErrorKind::NameMismatch {
                frontmatter: fm.name.to_string(),
                dir: dir_name,
            },
        ));
    }
    Ok(fm)
}

/// Validate the rule file at `file`.
///
/// A rule is any `.md` file; its optional `---paths:---` frontmatter must
/// parse when present. The file name (sans `.md`) must be a valid skill
/// name (rules share the name charset).
///
/// # Errors
///
/// [`SkillErrorKind::Io`], [`SkillErrorKind::NameInvalid`], or
/// [`SkillErrorKind::FrontmatterParse`].
pub fn validate_rule_file(file: &Path) -> Result<RuleFrontmatter, SkillError> {
    let name = rule_name(file)?;
    SkillName::parse(&name).map_err(|e| SkillError::new(file, SkillErrorKind::NameInvalid(e)))?;
    let doc = std::fs::read_to_string(file).map_err(|e| SkillError::new(file, SkillErrorKind::Io(e)))?;
    let ParsedRule { frontmatter, .. } = RuleFrontmatter::parse_doc(&doc, file)?;
    Ok(frontmatter)
}

/// The rule's logical name: the file stem of a `.md` file.
fn rule_name(file: &Path) -> Result<String, SkillError> {
    let stem = file
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .ok_or_else(|| {
            SkillError::new(
                file,
                SkillErrorKind::NameInvalid("rule path has no file name".to_string()),
            )
        })?;
    Ok(stem)
}

/// Pack the skill directory at `dir` into an uncompressed tar whose
/// entries are rooted at `<name>/`, matching the materializer's expected
/// layout. The whole tree under `dir` is included; entries are emitted in
/// sorted path order for a deterministic digest.
///
/// # Errors
///
/// [`SkillErrorKind::Io`] for a walk/read failure.
pub fn pack_skill_dir(dir: &Path) -> Result<Vec<u8>, SkillError> {
    let name = dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .ok_or_else(|| {
            SkillError::new(
                dir,
                SkillErrorKind::NameInvalid("skill path has no directory name".to_string()),
            )
        })?;

    let mut files: Vec<(String, PathBuf)> = Vec::new();
    collect_files(dir, dir, &name, &mut files).map_err(|e| SkillError::new(dir, SkillErrorKind::Io(e)))?;
    files.sort_by(|a, b| a.0.cmp(&b.0));

    let mut builder = tar::Builder::new(Vec::new());
    for (entry_path, abs) in &files {
        let bytes = std::fs::read(abs).map_err(|e| SkillError::new(abs, SkillErrorKind::Io(e)))?;
        append_entry(&mut builder, entry_path, &bytes).map_err(|e| SkillError::new(abs, SkillErrorKind::Io(e)))?;
    }
    builder
        .into_inner()
        .map_err(|e| SkillError::new(dir, SkillErrorKind::Io(e)))
}

/// Pack the rule file at `file` into an uncompressed tar.
///
/// Emits the index `<name>.md` entry, plus — when a sibling support
/// directory `<parent>/<name>/` exists beside the index — every file under
/// it rooted at `<name>/<rel>`. Entries are emitted in sorted path order so
/// the layer digest is deterministic; a rule with no support directory
/// packs byte-identically to a single `<name>.md` entry.
///
/// # Errors
///
/// [`SkillErrorKind::Io`] for a read/walk failure.
pub fn pack_rule_file(file: &Path) -> Result<Vec<u8>, SkillError> {
    let name = rule_name(file)?;

    let mut files: Vec<(String, PathBuf)> = vec![(format!("{name}.md"), file.to_path_buf())];

    // The optional sibling support dir shares the index's stem: for
    // `rules/<name>.md` it is `rules/<name>/`. Include it only when it is a
    // real directory; any other sibling (or none) leaves the degenerate
    // single-file case untouched.
    let support = file.with_extension("");
    if support.is_dir() {
        collect_files(&support, &support, &name, &mut files)
            .map_err(|e| SkillError::new(&support, SkillErrorKind::Io(e)))?;
    }
    files.sort_by(|a, b| a.0.cmp(&b.0));

    let mut builder = tar::Builder::new(Vec::new());
    for (entry_path, abs) in &files {
        let bytes = std::fs::read(abs).map_err(|e| SkillError::new(abs, SkillErrorKind::Io(e)))?;
        append_entry(&mut builder, entry_path, &bytes).map_err(|e| SkillError::new(abs, SkillErrorKind::Io(e)))?;
    }
    builder
        .into_inner()
        .map_err(|e| SkillError::new(file, SkillErrorKind::Io(e)))
}

/// Append one regular-file entry with a stable header (mode 0o644, no
/// mtime/uid/gid noise) so the produced tar bytes are deterministic.
fn append_entry(builder: &mut tar::Builder<Vec<u8>>, path: &str, bytes: &[u8]) -> std::io::Result<()> {
    let mut header = tar::Header::new_gnu();
    header.set_size(bytes.len() as u64);
    header.set_mode(0o644);
    header.set_mtime(0);
    header.set_uid(0);
    header.set_gid(0);
    header.set_entry_type(tar::EntryType::Regular);
    header.set_cksum();
    builder.append_data(&mut header, path, bytes)
}

/// Recursively collect `(tar_entry_path, absolute_path)` for every regular
/// file under `dir`, rooting the entry path at `<root_name>/<rel>`.
fn collect_files(root: &Path, dir: &Path, root_name: &str, out: &mut Vec<(String, PathBuf)>) -> std::io::Result<()> {
    let mut children: Vec<PathBuf> = std::fs::read_dir(dir)?
        .map(|e| e.map(|e| e.path()))
        .collect::<std::io::Result<Vec<_>>>()?;
    children.sort();
    for path in children {
        let meta = std::fs::symlink_metadata(&path)?;
        if meta.is_dir() {
            collect_files(root, &path, root_name, out)?;
        } else if meta.is_file() {
            let rel = path.strip_prefix(root).unwrap_or(&path);
            let rel_str: Vec<String> = rel
                .components()
                .filter_map(|c| match c {
                    std::path::Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
                    _ => None,
                })
                .collect();
            let entry = format!("{root_name}/{}", rel_str.join("/"));
            out.push((entry, path));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::install::materializer::{ArtifactMaterializer, DefaultMaterializer};
    use crate::oci::ArtifactKind;

    fn write(p: &Path, body: &str) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }

    #[test]
    fn validate_skill_dir_ok() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("code-review");
        write(
            &dir.join("SKILL.md"),
            "---\nname: code-review\ndescription: Review code.\n---\n# Body\n",
        );
        let fm = validate_skill_dir(&dir).expect("valid skill");
        assert_eq!(fm.name.as_str(), "code-review");
    }

    #[test]
    fn validate_skill_dir_missing_skill_md() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("code-review");
        std::fs::create_dir_all(&dir).unwrap();
        let err = validate_skill_dir(&dir).expect_err("no SKILL.md");
        assert!(matches!(err.kind, SkillErrorKind::MissingSkillMd));
    }

    #[test]
    fn validate_skill_dir_name_mismatch() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("code-review");
        write(&dir.join("SKILL.md"), "---\nname: other-name\ndescription: d\n---\n");
        let err = validate_skill_dir(&dir).expect_err("name mismatch");
        assert!(matches!(err.kind, SkillErrorKind::NameMismatch { .. }));
    }

    #[test]
    fn validate_skill_dir_missing_frontmatter() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("s");
        write(&dir.join("SKILL.md"), "no frontmatter at all\n");
        let err = validate_skill_dir(&dir).expect_err("no frontmatter");
        assert!(matches!(err.kind, SkillErrorKind::MissingFrontmatter));
    }

    #[test]
    fn validate_rule_file_ok_and_bad() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("rust-style.md");
        write(&f, "---\npaths: [\"**/*.rs\"]\n---\n# Rust\n");
        let fm = validate_rule_file(&f).expect("valid rule");
        assert_eq!(fm.paths, vec!["**/*.rs"]);

        let bad = tmp.path().join("Bad_Name.md");
        write(&bad, "# x\n");
        assert!(matches!(
            validate_rule_file(&bad).expect_err("bad name").kind,
            SkillErrorKind::NameInvalid(_)
        ));
    }

    #[test]
    fn pack_skill_round_trips_through_materializer() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("code-review");
        write(
            &dir.join("SKILL.md"),
            "---\nname: code-review\ndescription: d\n---\n# Body\n",
        );
        write(&dir.join("scripts/run.sh"), "echo hi\n");

        let tar = pack_skill_dir(&dir).expect("pack");
        let dest = tmp.path().join("out");
        let written = DefaultMaterializer
            .materialize(ArtifactKind::Skill, "code-review", &tar, &dest)
            .expect("materialize");
        assert_eq!(
            written,
            vec![
                PathBuf::from("code-review/SKILL.md"),
                PathBuf::from("code-review/scripts/run.sh"),
            ]
        );
        assert_eq!(
            std::fs::read_to_string(dest.join("code-review/SKILL.md")).unwrap(),
            "---\nname: code-review\ndescription: d\n---\n# Body\n"
        );
        assert_eq!(
            std::fs::read_to_string(dest.join("code-review/scripts/run.sh")).unwrap(),
            "echo hi\n"
        );
    }

    #[test]
    fn pack_rule_round_trips_through_materializer() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("rust-style.md");
        write(&f, "---\npaths: [\"**/*.rs\"]\n---\n# Rust Style\n");
        let tar = pack_rule_file(&f).expect("pack");
        let dest = tmp.path().join("out");
        let written = DefaultMaterializer
            .materialize(ArtifactKind::Rule, "rust-style", &tar, &dest)
            .expect("materialize");
        assert_eq!(written, vec![PathBuf::from("rust-style.md")]);
        assert_eq!(
            std::fs::read_to_string(dest.join("rust-style.md")).unwrap(),
            "---\npaths: [\"**/*.rs\"]\n---\n# Rust Style\n"
        );
    }

    #[test]
    fn pack_rule_with_support_dir_round_trips_index_and_files() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("rules/my-rule.md");
        write(
            &f,
            "---\npaths: [\"**/*.rs\"]\n---\n# index\nsee ./my-rule/examples.md\n",
        );
        // Sibling support dir sharing the index stem.
        write(&tmp.path().join("rules/my-rule/examples.md"), "# examples\n");
        write(&tmp.path().join("rules/my-rule/schema.json"), "{}\n");

        let tar = pack_rule_file(&f).expect("pack");
        let dest = tmp.path().join("out");
        let written = DefaultMaterializer
            .materialize(ArtifactKind::Rule, "my-rule", &tar, &dest)
            .expect("materialize");
        // The materializer returns `written` sorted as `PathBuf`
        // (component-wise), so support files precede the index file.
        assert_eq!(
            written,
            vec![
                PathBuf::from("my-rule/examples.md"),
                PathBuf::from("my-rule/schema.json"),
                PathBuf::from("my-rule.md"),
            ]
        );
        assert_eq!(
            std::fs::read_to_string(dest.join("my-rule/examples.md")).unwrap(),
            "# examples\n"
        );
        assert_eq!(
            std::fs::read_to_string(dest.join("my-rule/schema.json")).unwrap(),
            "{}\n"
        );
    }

    #[test]
    fn pack_rule_without_support_dir_is_single_entry() {
        // The degenerate case must still pack to exactly one `<name>.md`
        // entry — no behavior change for plain single-file rules.
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("rust-style.md");
        write(&f, "---\npaths: [\"**/*.rs\"]\n---\n# Rust Style\n");
        let tar = pack_rule_file(&f).expect("pack");
        let dest = tmp.path().join("out");
        let written = DefaultMaterializer
            .materialize(ArtifactKind::Rule, "rust-style", &tar, &dest)
            .expect("materialize");
        assert_eq!(written, vec![PathBuf::from("rust-style.md")]);
    }

    #[test]
    fn pack_rule_with_support_dir_is_deterministic() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("my-rule.md");
        write(&f, "# index\n");
        write(&tmp.path().join("my-rule/a.md"), "a\n");
        write(&tmp.path().join("my-rule/nested/b.json"), "{}\n");
        let first = pack_rule_file(&f).unwrap();
        let second = pack_rule_file(&f).unwrap();
        assert_eq!(first, second, "multi-file rule pack must be byte-stable");
    }

    #[test]
    fn pack_is_deterministic() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("s");
        write(&dir.join("SKILL.md"), "---\nname: s\ndescription: d\n---\n");
        write(&dir.join("a/one.txt"), "1");
        write(&dir.join("b/two.txt"), "2");
        let first = pack_skill_dir(&dir).unwrap();
        let second = pack_skill_dir(&dir).unwrap();
        assert_eq!(first, second, "pack must be byte-stable");
    }
}
