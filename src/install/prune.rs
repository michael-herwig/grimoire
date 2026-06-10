// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! Sweep materialized artifacts the current lock no longer declares.
//!
//! When a floating source rolls forward and drops an artifact — most
//! visibly a bundle that stops including a member — a fresh resolve omits
//! that artifact from the lock, but its already-materialized files and its
//! [`InstallState`] record linger on disk. [`prune_orphans`] reconciles the
//! materialized tree back to the lock: every recorded artifact whose
//! `(kind, name)` is absent from the lock is an orphan.
//!
//! Deleting an orphan is destructive, so it runs through the same integrity
//! gate as the installer: an orphan whose on-disk content has drifted from
//! the recorded hash (a local edit) is **preserved** unless `force`, and
//! reported as such, rather than silently discarding the user's work.
//!
//! File deletion + record drop reuse the shared [`uninstall`] seam so the
//! "remove an installed artifact" logic lives in exactly one place.

use std::collections::HashSet;
use std::path::PathBuf;

use crate::install::install_state::InstallState;
use crate::install::uninstall::uninstall;
use crate::lock::grimoire_lock::GrimoireLock;
use crate::oci::{ArtifactKind, Digest};

/// An I/O failure while pruning, carrying the artifact path the failing
/// operation acted on so the caller can attribute the error precisely
/// (a bare [`std::io::Error`] from hashing or deletion does not embed the
/// path on stable Rust).
#[derive(Debug)]
pub struct PruneError {
    /// The artifact path the failing hash/delete acted on.
    pub path: PathBuf,
    /// The underlying I/O error.
    pub source: std::io::Error,
}

/// What [`prune_orphans`] did to one orphaned artifact.
///
/// Closed internal enum — matches stay total, no `#[non_exhaustive]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PruneOutcome {
    /// Files deleted (if still present) and the install-state record dropped.
    Pruned,
    /// On-disk content drifted and `force` was not set — left untouched.
    KeptModified,
}

/// One orphan acted on during a prune pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrunedArtifact {
    /// Skill or rule.
    pub kind: ArtifactKind,
    /// The config binding name the record carried.
    pub name: String,
    /// The pin the orphan was last installed at (for the report's `old`).
    pub old: Digest,
    /// What happened to it.
    pub outcome: PruneOutcome,
    /// The on-disk targets actually deleted (empty for [`PruneOutcome::KeptModified`]
    /// or when the files were already gone).
    pub removed: Vec<std::path::PathBuf>,
}

/// Remove every materialized artifact absent from `lock`.
///
/// An orphan whose on-disk content matches its recorded hash (or whose
/// files are already gone) is pruned: its outputs are deleted and its
/// record dropped via [`uninstall`]. An orphan whose content has drifted is
/// preserved and reported as [`PruneOutcome::KeptModified`] unless `force`
/// is set, in which case it is pruned regardless.
///
/// Returns one entry per orphan acted on, in deterministic `(kind, name)`
/// order (the [`InstallState`] iteration order). The caller owns saving
/// `state`.
///
/// # Errors
///
/// A [`PruneError`] (carrying the failing artifact path) from hashing a
/// present output during the integrity check, or from deleting a present
/// target.
pub fn prune_orphans(
    state: &mut InstallState,
    lock: &GrimoireLock,
    force: bool,
) -> Result<Vec<PrunedArtifact>, PruneError> {
    // Keys the lock still declares; everything recorded but not here is an
    // orphan.
    let declared: HashSet<(ArtifactKind, String)> = lock
        .skills
        .iter()
        .chain(lock.rules.iter())
        .map(|a| (a.kind, a.name.clone()))
        .collect();

    // Snapshot the orphan keys (plus last-known digest and primary target)
    // before any mutation — `uninstall` borrows `state` mutably, so the
    // immutable iteration must finish first. `iter_records` is
    // `(kind, name)`-ordered, so the result is deterministic. The target is
    // carried so a deletion failure can be attributed to a real path.
    let orphans: Vec<(ArtifactKind, String, Digest, PathBuf)> = state
        .iter_records()
        .filter(|r| !declared.contains(&(r.kind, r.name.clone())))
        .map(|r| (r.kind, r.name.clone(), r.pinned.digest(), r.target.clone()))
        .collect();

    let mut acted = Vec::with_capacity(orphans.len());
    for (kind, name, old, target) in orphans {
        // Integrity gate: a locally modified orphan is preserved unless
        // forced. Deleting it would discard the user's edits.
        if !force && is_modified(state, kind, &name)? {
            acted.push(PrunedArtifact {
                kind,
                name,
                old,
                outcome: PruneOutcome::KeptModified,
                removed: Vec::new(),
            });
            continue;
        }

        let result = uninstall(state, kind, &name).map_err(|source| PruneError {
            path: target.clone(),
            source,
        })?;
        acted.push(PrunedArtifact {
            kind,
            name,
            old,
            outcome: PruneOutcome::Pruned,
            removed: result.removed,
        });
    }

    Ok(acted)
}

/// Whether any recorded client output for `(kind, name)` that is still on
/// disk has drifted from its recorded content hash. An absent output is not
/// "modified" — it is simply gone, and safe to prune.
fn is_modified(state: &InstallState, kind: ArtifactKind, name: &str) -> Result<bool, PruneError> {
    let Some(record) = state.get(kind, name) else {
        return Ok(false);
    };
    for out in record.client_outputs() {
        if out.target.exists() {
            let actual = out.current_hash().map_err(|source| PruneError {
                path: out.target.clone(),
                source,
            })?;
            if actual != out.content_hash {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::install::content_hash::content_hash;
    use crate::install::install_state::{ClientRecord, InstallRecord};
    use crate::lock::grimoire_lock::LockMetadata;
    use crate::lock::lock_version::LockVersion;
    use crate::lock::locked_artifact::LockedArtifact;
    use crate::oci::Identifier;
    use crate::oci::pinned_identifier::PinnedIdentifier;

    fn pinned(name: &str) -> PinnedIdentifier {
        let id = Identifier::new_registry(name, "localhost:5000").clone_with_digest(Digest::Sha256("a".repeat(64)));
        PinnedIdentifier::try_from(id).unwrap()
    }

    /// Materialize a single-file rule on disk and record it in `state`.
    fn install_rule(state: &mut InstallState, root: &std::path::Path, name: &str) -> std::path::PathBuf {
        let file = root.join(format!(".claude/rules/{name}.md"));
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, format!("# {name}\n")).unwrap();
        let hash = content_hash(&file).unwrap();
        state.record(InstallRecord {
            kind: ArtifactKind::Rule,
            name: name.to_string(),
            pinned: pinned(name),
            content_hash: hash.clone(),
            target: file.clone(),
            clients: vec![ClientRecord {
                client: "claude".to_string(),
                target: file.clone(),
                content_hash: hash,
                support_dir: None,
            }],
        });
        file
    }

    /// Materialize a skill as a directory tree on disk and record it.
    fn install_skill(state: &mut InstallState, root: &std::path::Path, name: &str) -> std::path::PathBuf {
        let dir = root.join(format!(".claude/skills/{name}"));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("SKILL.md"), format!("---\nname: {name}\n---\n# {name}\n")).unwrap();
        let hash = content_hash(&dir).unwrap();
        state.record(InstallRecord {
            kind: ArtifactKind::Skill,
            name: name.to_string(),
            pinned: pinned(name),
            content_hash: hash.clone(),
            target: dir.clone(),
            clients: vec![ClientRecord {
                client: "claude".to_string(),
                target: dir.clone(),
                content_hash: hash,
                support_dir: None,
            }],
        });
        dir
    }

    fn locked_rule(name: &str) -> LockedArtifact {
        LockedArtifact::direct(name.to_string(), ArtifactKind::Rule, pinned(name))
    }

    fn lock_of(rules: Vec<LockedArtifact>) -> GrimoireLock {
        GrimoireLock {
            metadata: LockMetadata {
                lock_version: LockVersion::V1,
                declaration_hash_version: 1,
                declaration_hash: format!("sha256:{}", "d".repeat(64)),
                generated_by: "grim 0.1.0".to_string(),
                generated_at: "2026-01-01T00:00:00Z".to_string(),
            },
            skills: vec![],
            rules,
        }
    }

    #[test]
    fn prunes_clean_orphan_not_in_lock() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = InstallState::empty(&dir.path().join("state.json"));
        let keep = install_rule(&mut state, dir.path(), "keep");
        let drop = install_rule(&mut state, dir.path(), "drop");

        // Lock declares only "keep"; "drop" is an orphan.
        let lock = lock_of(vec![locked_rule("keep")]);
        let acted = prune_orphans(&mut state, &lock, false).unwrap();

        assert_eq!(acted.len(), 1);
        assert_eq!(acted[0].name, "drop");
        assert_eq!(acted[0].outcome, PruneOutcome::Pruned);
        assert!(!drop.exists(), "the orphan file is deleted");
        assert!(keep.exists(), "the still-declared file is untouched");
        assert!(state.get(ArtifactKind::Rule, "drop").is_none(), "record dropped");
        assert!(state.get(ArtifactKind::Rule, "keep").is_some(), "record kept");
    }

    #[test]
    fn keeps_modified_orphan_without_force() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = InstallState::empty(&dir.path().join("state.json"));
        let drop = install_rule(&mut state, dir.path(), "drop");
        // Hand-edit the orphan so its content drifts from the record.
        std::fs::write(&drop, b"locally edited\n").unwrap();

        let lock = lock_of(vec![]);
        let acted = prune_orphans(&mut state, &lock, false).unwrap();

        assert_eq!(acted.len(), 1);
        assert_eq!(acted[0].outcome, PruneOutcome::KeptModified);
        assert!(acted[0].removed.is_empty());
        assert!(drop.exists(), "a modified orphan is preserved without --force");
        assert!(
            state.get(ArtifactKind::Rule, "drop").is_some(),
            "its record is preserved too"
        );
    }

    #[test]
    fn force_prunes_modified_orphan() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = InstallState::empty(&dir.path().join("state.json"));
        let drop = install_rule(&mut state, dir.path(), "drop");
        std::fs::write(&drop, b"locally edited\n").unwrap();

        let lock = lock_of(vec![]);
        let acted = prune_orphans(&mut state, &lock, true).unwrap();

        assert_eq!(acted.len(), 1);
        assert_eq!(acted[0].outcome, PruneOutcome::Pruned);
        assert!(!drop.exists(), "--force removes a modified orphan");
        assert!(state.get(ArtifactKind::Rule, "drop").is_none());
    }

    #[test]
    fn no_orphans_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = InstallState::empty(&dir.path().join("state.json"));
        install_rule(&mut state, dir.path(), "keep");
        let lock = lock_of(vec![locked_rule("keep")]);
        let acted = prune_orphans(&mut state, &lock, false).unwrap();
        assert!(acted.is_empty());
    }

    #[test]
    fn prunes_clean_skill_directory_orphan() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = InstallState::empty(&dir.path().join("state.json"));
        let skill = install_skill(&mut state, dir.path(), "code-review");

        // Lock declares nothing; the skill directory is an orphan.
        let lock = lock_of(vec![]);
        let acted = prune_orphans(&mut state, &lock, false).unwrap();

        assert_eq!(acted.len(), 1);
        assert_eq!(acted[0].outcome, PruneOutcome::Pruned);
        assert!(!skill.exists(), "the orphan skill directory is removed recursively");
        assert!(state.get(ArtifactKind::Skill, "code-review").is_none());
    }

    #[test]
    fn keeps_modified_skill_directory_orphan_without_force() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = InstallState::empty(&dir.path().join("state.json"));
        let skill = install_skill(&mut state, dir.path(), "code-review");
        // Edit a file inside the skill tree so the directory hash drifts.
        std::fs::write(skill.join("SKILL.md"), b"hand edited\n").unwrap();

        let lock = lock_of(vec![]);
        let acted = prune_orphans(&mut state, &lock, false).unwrap();

        assert_eq!(acted.len(), 1);
        assert_eq!(acted[0].outcome, PruneOutcome::KeptModified);
        assert!(skill.exists(), "a modified skill tree is preserved without --force");
        assert!(state.get(ArtifactKind::Skill, "code-review").is_some());

        // --force prunes the modified skill tree.
        let acted = prune_orphans(&mut state, &lock, true).unwrap();
        assert_eq!(acted[0].outcome, PruneOutcome::Pruned);
        assert!(!skill.exists());
    }

    #[test]
    fn already_gone_orphan_files_still_drop_record() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = InstallState::empty(&dir.path().join("state.json"));
        let drop = install_rule(&mut state, dir.path(), "drop");
        // Files vanished out from under us; the record must still be reaped.
        std::fs::remove_file(&drop).unwrap();

        let lock = lock_of(vec![]);
        let acted = prune_orphans(&mut state, &lock, false).unwrap();
        assert_eq!(acted.len(), 1);
        assert_eq!(acted[0].outcome, PruneOutcome::Pruned);
        assert!(state.get(ArtifactKind::Rule, "drop").is_none());
    }
}
