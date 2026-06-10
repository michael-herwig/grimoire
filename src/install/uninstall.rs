// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! The shared uninstall seam: the inverse of the installer's
//! materialize + record step.
//!
//! [`uninstall`] deletes every recorded client output for an artifact
//! from disk and drops its [`InstallState`] record. It is the single
//! source of truth for "remove an installed artifact's files", reused by
//! the `grim uninstall` command and the TUI delete action so neither
//! forks the logic. It deliberately does **not** touch the config
//! declaration or the lock — that is the caller's concern (a full
//! `uninstall` undeclares too; a TUI scope reset might not).
//!
//! Idempotent: a missing record, or already-absent target files, is not
//! an error — uninstall converges on "not installed" from any state.

use std::path::PathBuf;

use crate::install::install_state::InstallState;
use crate::oci::ArtifactKind;

/// What [`uninstall`] did.
///
/// Closed internal enum — matches stay total, no `#[non_exhaustive]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UninstallOutcome {
    /// A record existed; its outputs (if any were still present) were
    /// deleted and the record dropped.
    Removed,
    /// Nothing was recorded for this artifact — no-op.
    NotInstalled,
}

/// The outcome plus the paths actually deleted (for the report / status
/// line). Empty `removed` with [`UninstallOutcome::Removed`] means the
/// record existed but its files were already gone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UninstallResult {
    /// Whether a record was present and removed.
    pub outcome: UninstallOutcome,
    /// The on-disk targets actually deleted.
    pub removed: Vec<PathBuf>,
}

/// Remove every recorded client output for `(kind, name)` from disk and
/// drop its install-state record.
///
/// The caller still owns saving `state` and (for a full uninstall)
/// dropping the config/lock entry. A target that is a directory (a skill
/// tree) is removed recursively; a file (a rule) is unlinked. An absent
/// target is tolerated (idempotent).
///
/// # Errors
///
/// An [`std::io::Error`] from deleting a target that *is* present (other
/// than not-found). A symlinked target is unlinked, never followed.
pub fn uninstall(state: &mut InstallState, kind: ArtifactKind, name: &str) -> std::io::Result<UninstallResult> {
    let Some(record) = state.get(kind, name).cloned() else {
        return Ok(UninstallResult {
            outcome: UninstallOutcome::NotInstalled,
            removed: Vec::new(),
        });
    };

    let mut removed = Vec::new();
    for out in record.client_outputs() {
        match std::fs::symlink_metadata(&out.target) {
            Ok(meta) => {
                // `symlink_metadata` does not traverse links, so a
                // symlinked target is unlinked as a file, never followed
                // into an unrelated tree.
                if meta.is_dir() {
                    std::fs::remove_dir_all(&out.target)?;
                } else {
                    std::fs::remove_file(&out.target)?;
                }
                removed.push(out.target.clone());
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }

    state.remove(kind, name);
    Ok(UninstallResult {
        outcome: UninstallOutcome::Removed,
        removed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::install::content_hash::content_hash;
    use crate::install::install_state::{ClientRecord, InstallRecord};
    use crate::oci::pinned_identifier::PinnedIdentifier;
    use crate::oci::{Digest, Identifier};

    fn pinned(name: &str) -> PinnedIdentifier {
        let id = Identifier::new_registry(name, "localhost:5000").clone_with_digest(Digest::Sha256("a".repeat(64)));
        PinnedIdentifier::try_from(id).unwrap()
    }

    #[test]
    fn removes_skill_dir_and_rule_file_then_drops_records() {
        let dir = tempfile::tempdir().unwrap();
        let state_path = dir.path().join("state.json");

        // A skill materializes to a directory tree.
        let skill_dir = dir.path().join(".claude/skills/hello");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(skill_dir.join("SKILL.md"), b"hi\n").unwrap();
        // A rule materializes to a single file.
        let rule_file = dir.path().join(".claude/rules/style.md");
        std::fs::create_dir_all(rule_file.parent().unwrap()).unwrap();
        std::fs::write(&rule_file, b"rule\n").unwrap();

        let mut st = InstallState::empty(&state_path);
        st.record(InstallRecord {
            kind: ArtifactKind::Skill,
            name: "hello".to_string(),
            pinned: pinned("acme/hello"),
            content_hash: content_hash(&skill_dir).unwrap(),
            target: skill_dir.clone(),
            clients: vec![ClientRecord {
                client: "claude".to_string(),
                target: skill_dir.clone(),
                content_hash: content_hash(&skill_dir).unwrap(),
            }],
        });
        st.record(InstallRecord {
            kind: ArtifactKind::Rule,
            name: "style".to_string(),
            pinned: pinned("acme/style"),
            content_hash: content_hash(&rule_file).unwrap(),
            target: rule_file.clone(),
            clients: vec![],
        });

        let r = uninstall(&mut st, ArtifactKind::Skill, "hello").unwrap();
        assert_eq!(r.outcome, UninstallOutcome::Removed);
        assert_eq!(r.removed, vec![skill_dir.clone()]);
        assert!(!skill_dir.exists());
        assert!(st.get(ArtifactKind::Skill, "hello").is_none());

        let r = uninstall(&mut st, ArtifactKind::Rule, "style").unwrap();
        assert_eq!(r.outcome, UninstallOutcome::Removed);
        assert!(!rule_file.exists());
        assert!(st.get(ArtifactKind::Rule, "style").is_none());
    }

    #[test]
    fn absent_record_is_not_installed() {
        let dir = tempfile::tempdir().unwrap();
        let mut st = InstallState::empty(&dir.path().join("s.json"));
        let r = uninstall(&mut st, ArtifactKind::Skill, "nope").unwrap();
        assert_eq!(r.outcome, UninstallOutcome::NotInstalled);
        assert!(r.removed.is_empty());
    }

    #[test]
    fn already_gone_files_still_removed_record() {
        let dir = tempfile::tempdir().unwrap();
        let st_path = dir.path().join("s.json");
        let gone = dir.path().join(".claude/skills/ghost");
        let mut st = InstallState::empty(&st_path);
        st.record(InstallRecord {
            kind: ArtifactKind::Skill,
            name: "ghost".to_string(),
            pinned: pinned("acme/ghost"),
            content_hash: Digest::Sha256("b".repeat(64)),
            target: gone.clone(),
            clients: vec![],
        });
        // Files never existed on disk; record still drops cleanly.
        let r = uninstall(&mut st, ArtifactKind::Skill, "ghost").unwrap();
        assert_eq!(r.outcome, UninstallOutcome::Removed);
        assert!(r.removed.is_empty());
        assert!(st.get(ArtifactKind::Skill, "ghost").is_none());
    }
}
