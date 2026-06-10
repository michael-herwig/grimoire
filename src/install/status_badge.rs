// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! The single source of truth for an artifact's install badge.
//!
//! `search` and `tui` both annotate a catalog repository with how it
//! relates to the current scope's lock + install-state. The derivation
//! logic is the same one `grim status` uses (`status.rs::derive_state`);
//! this helper factors the lock/install-state comparison so the badge is
//! computed once, not duplicated. The catalog is keyed by repository path
//! (no config binding name), so this matches a lock/install record by its
//! pinned repository rather than by the config key.

use crate::install::content_hash::content_hash;
use crate::install::install_state::InstallState;
use crate::lock::grimoire_lock::GrimoireLock;
use crate::lock::locked_artifact::LockedArtifact;

/// The install status of a catalog repository relative to the scope.
///
/// Closed internal enum (the binary is the only consumer) — matches stay
/// total, no `#[non_exhaustive]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusBadge {
    /// Declared, locked, recorded, and on-disk content matches.
    Installed,
    /// Not declared/locked/installed in this scope.
    NotInstalled,
    /// Locked + installed, but the lock pin advanced past the install
    /// record (a newer digest is locked than what is on disk).
    Outdated,
    /// Installed but the on-disk content drifted from the recorded hash.
    Modified,
}

impl std::fmt::Display for StatusBadge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Installed => "installed",
            Self::NotInstalled => "not-installed",
            Self::Outdated => "outdated",
            Self::Modified => "modified",
        })
    }
}

/// Derive the badge for the repository `registry/repository` from the
/// scope's lock and install-state.
///
/// Precedence mirrors `status.rs::derive_state`: no lock/install record ⇒
/// not-installed; a recorded output that drifted ⇒ modified; the locked
/// pin ahead of the recorded pin ⇒ outdated; otherwise installed.
pub fn derive_badge(
    registry: &str,
    repository: &str,
    lock: Option<&GrimoireLock>,
    state: &InstallState,
) -> StatusBadge {
    let Some(locked) = lock.and_then(|l| find_by_repo(l, registry, repository)) else {
        return StatusBadge::NotInstalled;
    };
    let Some(record) = state
        .iter_records()
        .find(|r| r.pinned.registry() == registry && r.pinned.repository() == repository)
    else {
        return StatusBadge::NotInstalled;
    };

    let outputs = record.client_outputs();
    if outputs.iter().any(|o| !o.target.exists()) {
        return StatusBadge::NotInstalled;
    }
    for out in &outputs {
        match content_hash(&out.target) {
            Ok(actual) if actual != out.content_hash => return StatusBadge::Modified,
            Ok(_) => {}
            Err(_) => return StatusBadge::NotInstalled,
        }
    }
    if record.pinned.eq_content(&locked.pinned) {
        StatusBadge::Installed
    } else {
        StatusBadge::Outdated
    }
}

/// Find the locked artifact whose pin is in `registry/repository`.
fn find_by_repo<'a>(lock: &'a GrimoireLock, registry: &str, repository: &str) -> Option<&'a LockedArtifact> {
    lock.skills
        .iter()
        .chain(lock.rules.iter())
        .find(|a| a.pinned.registry() == registry && a.pinned.repository() == repository)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::install::install_state::InstallRecord;
    use crate::lock::grimoire_lock::LockMetadata;
    use crate::lock::lock_version::LockVersion;
    use crate::oci::pinned_identifier::PinnedIdentifier;
    use crate::oci::{Algorithm, ArtifactKind, Digest, Identifier};

    fn pinned(repo: &str, byte: char) -> PinnedIdentifier {
        let id = Identifier::new_registry(repo, "localhost:5000")
            .clone_with_digest(Digest::Sha256(std::iter::repeat_n(byte, 64).collect()));
        PinnedIdentifier::try_from(id).unwrap()
    }

    fn lock_with(repo: &str, byte: char) -> GrimoireLock {
        GrimoireLock {
            metadata: LockMetadata {
                lock_version: LockVersion::V1,
                declaration_hash_version: 1,
                declaration_hash: format!("sha256:{}", "d".repeat(64)),
                generated_by: "grim test".to_string(),
                generated_at: "2026-01-01T00:00:00Z".to_string(),
            },
            skills: vec![LockedArtifact::direct(
                "x".to_string(),
                ArtifactKind::Skill,
                pinned(repo, byte),
            )],
            rules: vec![],
        }
    }

    fn state_with(repo: &str, byte: char, target: &std::path::Path) -> InstallState {
        let mut st = InstallState::empty(std::path::Path::new("/tmp/s.json"));
        st.record(InstallRecord {
            kind: ArtifactKind::Skill,
            name: "x".to_string(),
            pinned: pinned(repo, byte),
            content_hash: content_hash(target).unwrap(),
            target: target.to_path_buf(),
            clients: vec![],
        });
        st
    }

    #[test]
    fn not_installed_without_lock_or_record() {
        let st = InstallState::empty(std::path::Path::new("/tmp/s.json"));
        assert_eq!(
            derive_badge("localhost:5000", "acme/x", None, &st),
            StatusBadge::NotInstalled
        );
        let lk = lock_with("acme/x", 'a');
        assert_eq!(
            derive_badge("localhost:5000", "acme/x", Some(&lk), &st),
            StatusBadge::NotInstalled
        );
    }

    #[test]
    fn installed_outdated_modified_matrix() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("x.md");
        std::fs::write(&target, b"canonical\n").unwrap();
        let st = state_with("acme/x", 'a', &target);

        // Same pin, intact content ⇒ installed.
        assert_eq!(
            derive_badge("localhost:5000", "acme/x", Some(&lock_with("acme/x", 'a')), &st),
            StatusBadge::Installed
        );
        // Lock advanced to a different digest ⇒ outdated.
        assert_eq!(
            derive_badge("localhost:5000", "acme/x", Some(&lock_with("acme/x", 'b')), &st),
            StatusBadge::Outdated
        );
        // Tamper ⇒ modified.
        std::fs::write(&target, b"hand edited\n").unwrap();
        assert_eq!(
            derive_badge("localhost:5000", "acme/x", Some(&lock_with("acme/x", 'a')), &st),
            StatusBadge::Modified
        );
        let _ = Algorithm::Sha256;
    }

    #[test]
    fn display_is_lowercase_kebab() {
        assert_eq!(StatusBadge::Installed.to_string(), "installed");
        assert_eq!(StatusBadge::NotInstalled.to_string(), "not-installed");
        assert_eq!(StatusBadge::Outdated.to_string(), "outdated");
        assert_eq!(StatusBadge::Modified.to_string(), "modified");
    }
}
