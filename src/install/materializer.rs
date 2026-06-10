// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! The client-transform seam: turn an artifact blob into files on disk.
//!
//! [`ArtifactMaterializer`] is the trait Phase 5 extends with per-client
//! transforms (Copilot rule rewriting, etc.). This milestone ships only
//! [`DefaultMaterializer`]: the blob is an uncompressed tar of the
//! artifact tree (a skill is a directory tree rooted at `<name>/`; a rule
//! is a single `<name>.md` file), extracted safely into the destination.

use std::io::Read;
use std::path::{Component, Path, PathBuf};

use crate::oci::ArtifactKind;

use super::install_error::{InstallError, InstallErrorKind};

/// Turns an artifact blob into a set of files under a destination dir.
///
/// Implementations decide how the on-wire blob maps to on-disk files.
/// Phase 5 adds client-specific transforms behind this same trait.
pub trait ArtifactMaterializer {
    /// Materialize `blob` for the artifact `name` of `kind` into
    /// `dest_dir`, returning the written file paths relative to
    /// `dest_dir`, sorted.
    ///
    /// # Errors
    ///
    /// [`InstallErrorKind::MaterializeFailed`] for a corrupt or unsafe
    /// archive; [`InstallErrorKind::TargetIo`] for a filesystem failure.
    fn materialize(
        &self,
        kind: ArtifactKind,
        name: &str,
        blob: &[u8],
        dest_dir: &Path,
    ) -> Result<Vec<PathBuf>, InstallError>;
}

/// The Phase-4 materializer: the blob is an uncompressed tar of the
/// artifact tree. No client transform — the canonical bytes land verbatim.
#[derive(Debug, Default, Clone, Copy)]
pub struct DefaultMaterializer;

impl ArtifactMaterializer for DefaultMaterializer {
    fn materialize(
        &self,
        kind: ArtifactKind,
        name: &str,
        blob: &[u8],
        dest_dir: &Path,
    ) -> Result<Vec<PathBuf>, InstallError> {
        std::fs::create_dir_all(dest_dir).map_err(|e| target_io(dest_dir, e))?;

        let mut archive = tar::Archive::new(blob);
        let entries = archive
            .entries()
            .map_err(|e| materialize_failed(format!("cannot read tar entries: {e}")))?;

        let mut written: Vec<PathBuf> = Vec::new();
        for entry in entries {
            let mut entry = entry.map_err(|e| materialize_failed(format!("cannot read tar entry: {e}")))?;
            let entry_type = entry.header().entry_type();

            let raw_path = entry
                .path()
                .map_err(|e| materialize_failed(format!("tar entry has an invalid path: {e}")))?
                .into_owned();
            let safe_rel = safe_relative_path(&raw_path)?;

            // Directories are recreated implicitly when files land; skip
            // standalone directory entries (but still validate the path).
            if entry_type.is_dir() {
                let dir = dest_dir.join(&safe_rel);
                std::fs::create_dir_all(&dir).map_err(|e| target_io(&dir, e))?;
                continue;
            }
            if !entry_type.is_file() {
                return Err(materialize_failed(format!(
                    "tar entry '{}' is not a regular file or directory",
                    safe_rel.display()
                )));
            }

            let target = dest_dir.join(&safe_rel);
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent).map_err(|e| target_io(parent, e))?;
            }
            let mut bytes = Vec::with_capacity(entry.size() as usize);
            entry
                .read_to_end(&mut bytes)
                .map_err(|e| materialize_failed(format!("cannot read tar entry body: {e}")))?;
            std::fs::write(&target, &bytes).map_err(|e| target_io(&target, e))?;
            written.push(safe_rel);
        }

        // A rule is exactly one file (`<name>.md`); a skill is a tree. We
        // do not over-validate the tree shape here — the standard layer
        // (Phase 5) owns spec validation — but a totally empty archive is
        // a corrupt artifact regardless of kind.
        if written.is_empty() {
            return Err(materialize_failed(format!(
                "artifact '{name}' ({kind}) materialized no files"
            )));
        }

        written.sort();
        Ok(written)
    }
}

/// Reject absolute paths and any `..` / root component so a crafted tar
/// cannot escape `dest_dir`. Returns the cleaned relative path.
fn safe_relative_path(path: &Path) -> Result<PathBuf, InstallError> {
    let mut clean = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(seg) => clean.push(seg),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(materialize_failed(format!(
                    "tar entry '{}' escapes the destination directory",
                    path.display()
                )));
            }
        }
    }
    if clean.as_os_str().is_empty() {
        return Err(materialize_failed(format!(
            "tar entry '{}' has an empty path",
            path.display()
        )));
    }
    Ok(clean)
}

fn materialize_failed(msg: String) -> InstallError {
    InstallError::without_reference(InstallErrorKind::MaterializeFailed(msg))
}

fn target_io(path: &Path, source: std::io::Error) -> InstallError {
    InstallError::without_reference(InstallErrorKind::TargetIo {
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build an uncompressed tar from `(path, bytes)` pairs.
    fn tar_of(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        for (path, bytes) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(bytes.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder.append_data(&mut header, path, *bytes).unwrap();
        }
        builder.into_inner().unwrap()
    }

    #[test]
    fn rule_single_file_materializes() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("dest");
        let blob = tar_of(&[("rust-style.md", b"# Rust Style\n")]);
        let m = DefaultMaterializer;
        let written = m
            .materialize(ArtifactKind::Rule, "rust-style", &blob, &dest)
            .expect("materialize");
        assert_eq!(written, vec![PathBuf::from("rust-style.md")]);
        assert_eq!(std::fs::read(dest.join("rust-style.md")).unwrap(), b"# Rust Style\n");
    }

    #[test]
    fn multi_file_rule_materializes_index_and_support() {
        // A rule tar carrying the index plus a sibling support tree
        // extracts both, sorted, exactly like any multi-entry archive.
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("dest");
        let blob = tar_of(&[
            ("my-rule.md", b"# index\n"),
            ("my-rule/examples.md", b"# ex\n"),
            ("my-rule/schema.json", b"{}\n"),
        ]);
        let m = DefaultMaterializer;
        let written = m
            .materialize(ArtifactKind::Rule, "my-rule", &blob, &dest)
            .expect("materialize");
        // `written` is sorted as `PathBuf` (component-wise), so the support
        // files (two components) sort before the one-component index file.
        assert_eq!(
            written,
            vec![
                PathBuf::from("my-rule/examples.md"),
                PathBuf::from("my-rule/schema.json"),
                PathBuf::from("my-rule.md"),
            ]
        );
        assert!(dest.join("my-rule.md").is_file());
        assert!(dest.join("my-rule/examples.md").is_file());
        assert!(dest.join("my-rule/schema.json").is_file());
    }

    #[test]
    fn skill_tree_materializes_sorted() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("dest");
        let blob = tar_of(&[
            ("code-review/SKILL.md", b"---\nname: code-review\n---\n"),
            ("code-review/scripts/run.sh", b"echo hi\n"),
        ]);
        let m = DefaultMaterializer;
        let written = m
            .materialize(ArtifactKind::Skill, "code-review", &blob, &dest)
            .expect("materialize");
        assert_eq!(
            written,
            vec![
                PathBuf::from("code-review/SKILL.md"),
                PathBuf::from("code-review/scripts/run.sh"),
            ]
        );
        assert!(dest.join("code-review/SKILL.md").is_file());
        assert!(dest.join("code-review/scripts/run.sh").is_file());
    }

    /// Build a single-entry tar whose header `name` field is written
    /// verbatim, bypassing `tar::Builder`'s own relative-path validation.
    /// This is the only way to forge the malicious archive a hostile
    /// registry could serve; the materializer must reject it.
    fn tar_with_raw_name(name: &str, body: &[u8]) -> Vec<u8> {
        let mut header = tar::Header::new_gnu();
        header.set_size(body.len() as u64);
        header.set_mode(0o644);
        header.set_entry_type(tar::EntryType::Regular);
        // Write the name bytes directly into the legacy 100-byte field.
        let name_field = &mut header.as_old_mut().name;
        let bytes = name.as_bytes();
        name_field[..bytes.len()].copy_from_slice(bytes);
        header.set_cksum();

        let mut out = Vec::new();
        out.extend_from_slice(header.as_bytes());
        out.extend_from_slice(body);
        // Pad the body to a 512-byte block, then two zero blocks (EOF).
        let pad = (512 - body.len() % 512) % 512;
        out.extend(std::iter::repeat_n(0u8, pad));
        out.extend(std::iter::repeat_n(0u8, 1024));
        out
    }

    #[test]
    fn rejects_parent_traversal() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("dest");
        let blob = tar_with_raw_name("../escape.md", b"evil\n");
        let m = DefaultMaterializer;
        let err = m
            .materialize(ArtifactKind::Rule, "x", &blob, &dest)
            .expect_err("traversal must reject");
        assert!(matches!(err.kind, InstallErrorKind::MaterializeFailed(_)));
        assert!(!dir.path().join("escape.md").exists());
    }

    #[test]
    fn rejects_absolute_path() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("dest");
        let blob = tar_with_raw_name("/etc/passwd", b"evil\n");
        let m = DefaultMaterializer;
        let err = m
            .materialize(ArtifactKind::Rule, "x", &blob, &dest)
            .expect_err("absolute path must reject");
        assert!(matches!(err.kind, InstallErrorKind::MaterializeFailed(_)));
    }

    #[test]
    fn empty_archive_is_corrupt() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("dest");
        let blob = tar_of(&[]);
        let m = DefaultMaterializer;
        let err = m
            .materialize(ArtifactKind::Rule, "x", &blob, &dest)
            .expect_err("empty archive must reject");
        assert!(matches!(err.kind, InstallErrorKind::MaterializeFailed(_)));
    }
}
