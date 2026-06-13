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

use crate::install::install_state::InstallState;
use crate::install::path_anchor::AnchorRoots;
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
    roots: &AnchorRoots,
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

    // An unresolvable anchored target (corrupt `relative`, anchor root
    // absent on this machine) absorbs to NotInstalled — a read-only badge
    // never `?`-propagates an `AnchorError`.
    for out in &record.outputs {
        match out.resolved_target(roots) {
            Ok(resolved) if !resolved.exists() => return StatusBadge::NotInstalled,
            Ok(_) => {}
            Err(_) => return StatusBadge::NotInstalled,
        }
    }
    for out in &record.outputs {
        match out.current_hash(roots) {
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
///
/// Searches skills, rules, and agents so that installed agents are not
/// incorrectly reported as `NotInstalled` (SC-04).
fn find_by_repo<'a>(lock: &'a GrimoireLock, registry: &str, repository: &str) -> Option<&'a LockedArtifact> {
    lock.skills
        .iter()
        .chain(lock.rules.iter())
        .chain(lock.agents.iter())
        .find(|a| a.pinned.registry() == registry && a.pinned.repository() == repository)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::install::content_hash::content_hash;
    use crate::install::install_state::{ClientOutput, InstallRecord};
    use crate::install::path_anchor::{AnchorRoots, AnchoredPath, PathAnchor};
    use crate::lock::grimoire_lock::LockMetadata;
    use crate::lock::lock_version::LockVersion;
    use crate::oci::pinned_identifier::PinnedIdentifier;
    use crate::oci::{Algorithm, ArtifactKind, Digest, Identifier};
    use std::path::PathBuf;

    fn pinned(repo: &str, byte: char) -> PinnedIdentifier {
        let id = Identifier::new_registry(repo, "localhost:5000")
            .clone_with_digest(Digest::Sha256(std::iter::repeat_n(byte, 64).collect()));
        PinnedIdentifier::try_from(id).unwrap()
    }

    /// Build `AnchorRoots` with `workspace` set to `ws`, other roots absent.
    fn roots(ws: &std::path::Path) -> AnchorRoots {
        AnchorRoots {
            workspace: ws.to_path_buf(),
            grim_home: ws.to_path_buf(),
            claude_root: None,
            copilot_root: None,
            opencode_skills: None,
        }
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
            agents: vec![],
            bundles: vec![],
        }
    }

    /// Build an `InstallState` with one `Workspace`-anchored `ClientOutput`.
    /// `target_rel` is the relative path under `workspace`; `workspace` is
    /// the absolute root (needed so `content_hash` can read the actual file).
    fn state_with(repo: &str, byte: char, workspace: &std::path::Path, target_rel: &str) -> InstallState {
        let abs = workspace.join(target_rel);
        let mut st = InstallState::empty(std::path::Path::new("/tmp/s.json"));
        st.record(InstallRecord {
            kind: ArtifactKind::Skill,
            name: "x".to_string(),
            pinned: pinned(repo, byte),
            outputs: vec![ClientOutput {
                client: "claude".to_string(),
                target: AnchoredPath {
                    anchor: PathAnchor::Workspace,
                    relative: target_rel.to_string(),
                },
                content_hash: content_hash(&abs).unwrap(),
                support_dir: None,
            }],
        });
        st
    }

    #[test]
    fn not_installed_without_lock_or_record() {
        let dir = tempfile::tempdir().unwrap();
        let roots = roots(dir.path());
        let st = InstallState::empty(std::path::Path::new("/tmp/s.json"));
        assert_eq!(
            derive_badge("localhost:5000", "acme/x", None, &st, &roots),
            StatusBadge::NotInstalled
        );
        let lk = lock_with("acme/x", 'a');
        assert_eq!(
            derive_badge("localhost:5000", "acme/x", Some(&lk), &st, &roots),
            StatusBadge::NotInstalled
        );
    }

    #[test]
    fn installed_outdated_modified_matrix() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        let target_rel = "x.md";
        let target = ws.join(target_rel);
        std::fs::write(&target, b"canonical\n").unwrap();
        let st = state_with("acme/x", 'a', ws, target_rel);
        let roots = roots(ws);

        // Same pin, intact content ⇒ installed.
        assert_eq!(
            derive_badge("localhost:5000", "acme/x", Some(&lock_with("acme/x", 'a')), &st, &roots),
            StatusBadge::Installed
        );
        // Lock advanced to a different digest ⇒ outdated.
        assert_eq!(
            derive_badge("localhost:5000", "acme/x", Some(&lock_with("acme/x", 'b')), &st, &roots),
            StatusBadge::Outdated
        );
        // Tamper ⇒ modified.
        std::fs::write(&target, b"hand edited\n").unwrap();
        assert_eq!(
            derive_badge("localhost:5000", "acme/x", Some(&lock_with("acme/x", 'a')), &st, &roots),
            StatusBadge::Modified
        );
        let _ = Algorithm::Sha256;
        let _ = PathBuf::new();
    }

    #[test]
    fn display_is_lowercase_kebab() {
        assert_eq!(StatusBadge::Installed.to_string(), "installed");
        assert_eq!(StatusBadge::NotInstalled.to_string(), "not-installed");
        assert_eq!(StatusBadge::Outdated.to_string(), "outdated");
        assert_eq!(StatusBadge::Modified.to_string(), "modified");
    }

    // SC-04 regression: find_by_repo must search lock.agents so that an
    // installed agent is not incorrectly badged as NotInstalled.
    #[test]
    fn installed_agent_derives_installed_badge() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();

        // Write a fake agent file so resolved_target().exists() is true
        // and content_hash matches the recorded value.
        let target_rel = ".claude/agents/my-agent.md";
        let target_abs = ws.join(target_rel);
        std::fs::create_dir_all(target_abs.parent().unwrap()).unwrap();
        std::fs::write(&target_abs, b"# agent\n").unwrap();

        use crate::install::content_hash::content_hash;
        let hash = content_hash(&target_abs).unwrap();

        // Build state with Agent kind, Workspace anchor.
        let p = pinned("acme/my-agent", 'a');
        let mut st = InstallState::empty(std::path::Path::new("/tmp/s.json"));
        st.record(InstallRecord {
            kind: ArtifactKind::Agent,
            name: "my-agent".to_string(),
            pinned: p.clone(),
            outputs: vec![ClientOutput {
                client: "claude".to_string(),
                target: AnchoredPath {
                    anchor: PathAnchor::Workspace,
                    relative: target_rel.to_string(),
                },
                content_hash: hash,
                support_dir: None,
            }],
        });

        // Lock lists the agent — ONLY in the agents array.
        let lk = GrimoireLock {
            metadata: LockMetadata {
                lock_version: LockVersion::V1,
                declaration_hash_version: 1,
                declaration_hash: format!("sha256:{}", "d".repeat(64)),
                generated_by: "grim test".to_string(),
                generated_at: "2026-01-01T00:00:00Z".to_string(),
            },
            skills: vec![],
            rules: vec![],
            agents: vec![LockedArtifact::direct("my-agent".to_string(), ArtifactKind::Agent, p)],
            bundles: vec![],
        };

        let roots = roots(ws);
        // Before the SC-04 fix, find_by_repo only searched skills+rules and
        // returned NotInstalled. After the fix it must return Installed.
        assert_eq!(
            derive_badge("localhost:5000", "acme/my-agent", Some(&lk), &st, &roots),
            StatusBadge::Installed,
            "an installed agent must badge as Installed, not NotInstalled"
        );
    }

    // T10 spec: derive_badge with an unresolvable AnchoredPath (anchor root absent)
    // must return NotInstalled, never propagate AnchorError.
    #[test]
    fn unresolvable_anchor_root_returns_not_installed() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();

        // Build state with ClaudeRoot anchor but roots has claude_root = None.
        let mut st = InstallState::empty(std::path::Path::new("/tmp/s.json"));
        st.record(InstallRecord {
            kind: ArtifactKind::Skill,
            name: "x".to_string(),
            pinned: {
                let id = Identifier::new_registry("acme/x", "localhost:5000")
                    .clone_with_digest(Digest::Sha256("a".repeat(64)));
                PinnedIdentifier::try_from(id).unwrap()
            },
            outputs: vec![ClientOutput {
                client: "claude".to_string(),
                target: AnchoredPath {
                    anchor: PathAnchor::ClaudeRoot,
                    relative: "skills/x".to_string(),
                },
                content_hash: Digest::Sha256("a".repeat(64)),
                support_dir: None,
            }],
        });

        // Roots with no claude_root: resolved_target → AnchorRootAbsent.
        let no_claude_roots = AnchorRoots {
            workspace: ws.to_path_buf(),
            grim_home: ws.to_path_buf(),
            claude_root: None,
            copilot_root: None,
            opencode_skills: None,
        };

        let lk = lock_with("acme/x", 'a');
        // Contract: AnchorError must be absorbed, returning NotInstalled (never `?`-propagated).
        let badge = derive_badge("localhost:5000", "acme/x", Some(&lk), &st, &no_claude_roots);
        assert_eq!(
            badge,
            StatusBadge::NotInstalled,
            "unresolvable anchor root must degrade to NotInstalled, not error"
        );
    }
}
