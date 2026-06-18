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
use crate::install::path_anchor::{AnchorError, AnchorRoots};
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

/// A failure during uninstall: either resolving an anchored target failed
/// (a corrupt/tampered `relative`) or deleting a present file did.
///
/// `thiserror`, `#[non_exhaustive]` (error-enum convention).
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum UninstallError {
    /// Resolving/validating an anchored target failed.
    #[error(transparent)]
    Anchor(#[from] AnchorError),
    /// Deleting a present target failed.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Remove every recorded client output for `(kind, name)` from disk and
/// drop its install-state record.
///
/// The caller still owns saving `state` and (for a full uninstall)
/// dropping the config/lock entry. A target that is a directory (a skill
/// tree) is removed recursively; a file (a rule) is unlinked. An absent
/// target is tolerated (idempotent).
///
/// A recorded output whose anchor root is absent on this machine (an
/// out-of-scope client — e.g. a global client whose vendor root is unset) is
/// tolerated and skipped: uninstall converges on "not installed" from any
/// state, dropping the record regardless.
///
/// # Errors
///
/// A [`UninstallError`] from a genuine containment failure resolving an
/// anchored target (a tampered `relative` — traversal / escaped anchor), or
/// from deleting a target that *is* present (other than not-found). A present
/// target is operated on through its resolved (canonicalized) path,
/// guaranteed contained within its anchor root; a missing target is operated
/// on via the raw anchor join. An anchor whose root is unresolvable is skipped
/// (see above), never an error.
pub fn uninstall(
    state: &mut InstallState,
    kind: ArtifactKind,
    name: &str,
    roots: &AnchorRoots,
) -> Result<UninstallResult, UninstallError> {
    let Some(record) = state.get(kind, name).cloned() else {
        return Ok(UninstallResult {
            outcome: UninstallOutcome::NotInstalled,
            removed: Vec::new(),
        });
    };

    let mut removed = Vec::new();
    for out in &record.outputs {
        // Tolerant resolve: a recorded output whose anchor root is absent on
        // this machine names a client out of scope here (e.g. a global client
        // whose vendor root is unset). Skip it — uninstall converges on "not
        // installed" from any state, and we can neither resolve nor delete
        // what we cannot anchor. A genuine containment failure (traversal /
        // escaped anchor) or an I/O error still surfaces.
        let target = match out.resolved_target(roots) {
            Ok(target) => target,
            Err(AnchorError::AnchorRootAbsent { .. }) => continue,
            Err(e) => return Err(e.into()),
        };
        // The index/target first, then a multi-file rule's sibling support
        // directory (`<parent>/<name>/`) so the whole footprint is reaped.
        remove_output(&target, &mut removed)?;
        match out.resolved_support_dir(roots) {
            Ok(Some(support_dir)) => remove_output(&support_dir, &mut removed)?,
            Ok(None) => {}
            Err(AnchorError::AnchorRootAbsent { .. }) => {}
            Err(e) => return Err(e.into()),
        }
    }

    state.remove(kind, name);
    Ok(UninstallResult {
        outcome: UninstallOutcome::Removed,
        removed,
    })
}

/// Remove one recorded output `path` (a file or directory), pushing it onto
/// `removed` when it was present. An absent path is tolerated (idempotent).
/// `symlink_metadata` does not traverse links, so a symlinked target is
/// unlinked as a file, never followed into an unrelated tree.
fn remove_output(path: &std::path::Path, removed: &mut Vec<PathBuf>) -> std::io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) => {
            if meta.is_dir() {
                std::fs::remove_dir_all(path)?;
            } else {
                std::fs::remove_file(path)?;
            }
            removed.push(path.to_path_buf());
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::install::content_hash::content_hash;
    use crate::install::install_state::{ClientOutput, InstallRecord};
    use crate::install::path_anchor::{AnchorRoots, AnchoredPath, PathAnchor};
    use crate::oci::pinned_identifier::PinnedIdentifier;
    use crate::oci::{Digest, Identifier};

    fn pinned(name: &str) -> PinnedIdentifier {
        let id = Identifier::new_registry(name, "localhost:5000").clone_with_digest(Digest::Sha256("a".repeat(64)));
        PinnedIdentifier::try_from(id).unwrap()
    }

    /// Build `AnchorRoots` rooted at `workspace` so `Workspace`-anchored paths
    /// resolve to absolute paths under `workspace`. Other anchors absent.
    fn roots(workspace: &std::path::Path) -> AnchorRoots {
        AnchorRoots {
            workspace: workspace.to_path_buf(),
            grim_home: workspace.to_path_buf(),
            claude_root: None,
            copilot_root: None,
            opencode_skills: None,
        }
    }

    /// Build a `ClientOutput` with a `Workspace`-anchored target at `relative`.
    fn client_output_at(relative: &str, content_hash: Digest) -> ClientOutput {
        ClientOutput {
            client: "claude".to_string(),
            target: AnchoredPath {
                anchor: PathAnchor::Workspace,
                relative: relative.to_string(),
            },
            content_hash,
            support_dir: None,
        }
    }

    /// Build a `ClientOutput` with a `Workspace`-anchored target + support dir.
    fn client_output_with_support(target_rel: &str, support_rel: &str, content_hash: Digest) -> ClientOutput {
        ClientOutput {
            client: "claude".to_string(),
            target: AnchoredPath {
                anchor: PathAnchor::Workspace,
                relative: target_rel.to_string(),
            },
            content_hash,
            support_dir: Some(AnchoredPath {
                anchor: PathAnchor::Workspace,
                relative: support_rel.to_string(),
            }),
        }
    }

    #[test]
    fn removes_skill_dir_and_rule_file_then_drops_records() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        let state_path = ws.join("state.json");

        // A skill materializes to a directory tree.
        let skill_dir = ws.join(".claude/skills/hello");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(skill_dir.join("SKILL.md"), b"hi\n").unwrap();
        // A rule materializes to a single file.
        let rule_file = ws.join(".claude/rules/style.md");
        std::fs::create_dir_all(rule_file.parent().unwrap()).unwrap();
        std::fs::write(&rule_file, b"rule\n").unwrap();

        let mut st = InstallState::empty(&state_path);
        st.record(InstallRecord {
            kind: ArtifactKind::Skill,
            name: "hello".to_string(),
            pinned: pinned("acme/hello"),
            outputs: vec![client_output_at(
                ".claude/skills/hello",
                content_hash(&skill_dir).unwrap(),
            )],
        });
        st.record(InstallRecord {
            kind: ArtifactKind::Rule,
            name: "style".to_string(),
            pinned: pinned("acme/style"),
            outputs: vec![client_output_at(
                ".claude/rules/style.md",
                content_hash(&rule_file).unwrap(),
            )],
        });

        let roots = roots(ws);
        let r = uninstall(&mut st, ArtifactKind::Skill, "hello", &roots).unwrap();
        assert_eq!(r.outcome, UninstallOutcome::Removed);
        assert_eq!(r.removed, vec![skill_dir.clone()]);
        assert!(!skill_dir.exists());
        assert!(st.get(ArtifactKind::Skill, "hello").is_none());

        let r = uninstall(&mut st, ArtifactKind::Rule, "style", &roots).unwrap();
        assert_eq!(r.outcome, UninstallOutcome::Removed);
        assert!(!rule_file.exists());
        assert!(st.get(ArtifactKind::Rule, "style").is_none());
    }

    #[test]
    fn removes_multi_file_rule_index_and_support_dir() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        let state_path = ws.join("state.json");

        // A multi-file rule: index file + sibling support directory.
        let index = ws.join(".claude/rules/my-rule.md");
        let support = ws.join(".claude/rules/my-rule");
        std::fs::create_dir_all(&support).unwrap();
        std::fs::write(&index, b"# index\n").unwrap();
        std::fs::write(support.join("examples.md"), b"# ex\n").unwrap();

        let mut st = InstallState::empty(&state_path);
        st.record(InstallRecord {
            kind: ArtifactKind::Rule,
            name: "my-rule".to_string(),
            pinned: pinned("acme/my-rule"),
            outputs: vec![client_output_with_support(
                ".claude/rules/my-rule.md",
                ".claude/rules/my-rule",
                content_hash(&index).unwrap(),
            )],
        });

        let roots = roots(ws);
        let r = uninstall(&mut st, ArtifactKind::Rule, "my-rule", &roots).unwrap();
        assert_eq!(r.outcome, UninstallOutcome::Removed);
        assert_eq!(r.removed, vec![index.clone(), support.clone()]);
        assert!(!index.exists(), "index file removed");
        assert!(!support.exists(), "support directory removed recursively");
        assert!(st.get(ArtifactKind::Rule, "my-rule").is_none());

        // Idempotent: a second uninstall reports nothing left to do.
        let again = uninstall(&mut st, ArtifactKind::Rule, "my-rule", &roots).unwrap();
        assert_eq!(again.outcome, UninstallOutcome::NotInstalled);
    }

    #[test]
    fn absent_record_is_not_installed() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        let mut st = InstallState::empty(&ws.join("s.json"));
        let roots = roots(ws);
        let r = uninstall(&mut st, ArtifactKind::Skill, "nope", &roots).unwrap();
        assert_eq!(r.outcome, UninstallOutcome::NotInstalled);
        assert!(r.removed.is_empty());
    }

    #[test]
    fn already_gone_files_still_removed_record() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        let st_path = ws.join("s.json");
        let mut st = InstallState::empty(&st_path);
        // Record with a path that never existed on disk.
        st.record(InstallRecord {
            kind: ArtifactKind::Skill,
            name: "ghost".to_string(),
            pinned: pinned("acme/ghost"),
            outputs: vec![client_output_at(".claude/skills/ghost", Digest::Sha256("b".repeat(64)))],
        });
        // Files never existed on disk; record still drops cleanly.
        let roots = roots(ws);
        let r = uninstall(&mut st, ArtifactKind::Skill, "ghost", &roots).unwrap();
        assert_eq!(r.outcome, UninstallOutcome::Removed);
        assert!(r.removed.is_empty());
        assert!(st.get(ArtifactKind::Skill, "ghost").is_none());
    }

    // C5: an unresolvable recorded client anchor (anchor root absent on this
    // machine — an out-of-scope client) is TOLERATED during uninstall: the
    // resolvable client's files are removed and the record is dropped, rather
    // than `?`-propagating an `AnchorError` and aborting the idempotent
    // uninstall. (Supersedes the prior intolerant contract.)
    #[test]
    fn uninstall_tolerates_unresolvable_client_anchor() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        let st_path = ws.join("s.json");

        // claude file present (workspace-anchored); copilot output anchored to
        // CopilotRoot, which is unresolvable here (copilot_root = None).
        let claude_file = ws.join(".claude/rules/orphan.md");
        std::fs::create_dir_all(claude_file.parent().unwrap()).unwrap();
        std::fs::write(&claude_file, b"# rule\n").unwrap();

        let mut st = InstallState::empty(&st_path);
        st.record(InstallRecord {
            kind: ArtifactKind::Rule,
            name: "orphan".to_string(),
            pinned: pinned("acme/orphan"),
            outputs: vec![
                ClientOutput {
                    client: "claude".to_string(),
                    target: AnchoredPath {
                        anchor: PathAnchor::Workspace,
                        relative: ".claude/rules/orphan.md".to_string(),
                    },
                    content_hash: Digest::Sha256("a".repeat(64)),
                    support_dir: None,
                },
                ClientOutput {
                    client: "copilot".to_string(),
                    target: AnchoredPath {
                        anchor: PathAnchor::CopilotRoot,
                        relative: "rules/orphan.md".to_string(),
                    },
                    content_hash: Digest::Sha256("c".repeat(64)),
                    support_dir: None,
                },
            ],
        });
        let roots = AnchorRoots {
            workspace: ws.to_path_buf(),
            grim_home: ws.to_path_buf(),
            claude_root: None,
            copilot_root: None,
            opencode_skills: None,
        };
        let result = uninstall(&mut st, ArtifactKind::Rule, "orphan", &roots)
            .expect("an unresolvable client anchor must be tolerated, not error");
        assert_eq!(result.outcome, UninstallOutcome::Removed);
        assert!(!claude_file.exists(), "the resolvable claude file is removed");
        assert!(
            st.get(ArtifactKind::Rule, "orphan").is_none(),
            "the record is dropped despite the unresolvable copilot output"
        );
    }
}
