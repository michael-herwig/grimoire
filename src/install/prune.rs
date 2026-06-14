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
use std::io;
use std::path::PathBuf;

use crate::install::install_state::InstallState;
use crate::install::path_anchor::{AnchorError, AnchorRoots};
use crate::install::uninstall::{UninstallError, uninstall};
use crate::lock::grimoire_lock::GrimoireLock;
use crate::oci::{ArtifactKind, Digest};

/// A failure while pruning.
///
/// The `Anchor` variant preserves the `AnchorError` identity so a
/// **security-class** anchor failure (a corrupt stored `relative` carrying
/// `../`, or a symlink that escapes its anchor root) propagates and maps to
/// `DataError(65)` via `classify_error`, rather than silently flattening into
/// an I/O error (`IoError(74)`) — the exit-code contract from ARCH-4/SC-03.
/// A resolution-absence failure (`AnchorRootAbsent`) is NOT surfaced here:
/// such a record is treated as an unresolvable orphan and reaped (see
/// [`is_modified`] and the [`uninstall`] interception in [`prune_orphans`]).
///
/// `thiserror`, lowercase no-period messages (`quality-rust-errors.md`),
/// `#[non_exhaustive]` (error-enum convention).
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PruneError {
    /// A security-class anchor failure (path traversal or symlink escape) in a
    /// stored anchored path. Carries the artifact path for error attribution.
    #[error("path traversal in stored install state at '{path}'")]
    Anchor {
        /// The artifact path context for reporting.
        path: PathBuf,
        /// The underlying anchor error.
        #[source]
        source: AnchorError,
    },
    /// An I/O failure while hashing or deleting, carrying the artifact path
    /// the failing operation acted on so the caller can attribute it precisely
    /// (a bare [`io::Error`] does not embed the path on stable Rust).
    #[error("I/O error at '{path}'")]
    Io {
        /// The artifact path the failing hash/delete acted on.
        path: PathBuf,
        /// The underlying I/O error.
        #[source]
        source: io::Error,
    },
}

/// Whether an [`AnchorError`] is **security-class** — a deliberate traversal
/// or symlink escape that must be FATAL even on the prune path. These
/// propagate (→ `DataError(65)`) and are NEVER reaped.
///
/// `AnchorRootAbsent` (the anchor root is unresolvable on this machine) and
/// plain I/O are *resolution-absence*, not tampering: such a record is an
/// unresolvable orphan, safe to reap. `AnchorError` is `#[non_exhaustive]`;
/// an unknown future variant is treated as non-fatal/absorb (matching the
/// read-only leniency), so only the two named security variants are fatal.
fn is_security_class(err: &AnchorError) -> bool {
    matches!(
        err,
        AnchorError::TraversalAttempt { .. } | AnchorError::EscapedAnchor { .. }
    )
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
    /// The client names the removed record carried. A prune can orphan an
    /// artifact installed for clients *outside* the current run's
    /// `--client` selection — the caller must run the vendor config sync
    /// for these too, or a managed config entry (e.g. OpenCode's
    /// `instructions` glob) goes stale.
    pub clients: Vec<String>,
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
    roots: &AnchorRoots,
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

    // Snapshot the orphan keys (plus last-known digest, primary target, and
    // recorded clients) before any mutation — `uninstall` borrows `state`
    // mutably, so the immutable iteration must finish first. `iter_records`
    // is `(kind, name)`-ordered, so the result is deterministic. The target
    // is carried so a deletion failure can be attributed to a real path;
    // the clients are carried because the record is gone after `uninstall`
    // and the caller needs them for the post-prune vendor config sync.
    struct Orphan {
        kind: ArtifactKind,
        name: String,
        old: Digest,
        target: PathBuf,
        clients: Vec<String>,
    }
    let orphans: Vec<Orphan> = state
        .iter_records()
        .filter(|r| !declared.contains(&(r.kind, r.name.clone())))
        .map(|r| Orphan {
            kind: r.kind,
            name: r.name.clone(),
            old: r.pinned.digest(),
            // Best-effort path for error attribution: the first output's
            // resolved target, falling back to the workspace root when the
            // record is unresolvable.
            target: r
                .outputs
                .first()
                .and_then(|o| o.resolved_target(roots).ok())
                .unwrap_or_else(|| roots.workspace.clone()),
            clients: r.outputs.iter().map(|c| c.client.clone()).collect(),
        })
        .collect();

    let mut acted = Vec::with_capacity(orphans.len());
    for Orphan {
        kind,
        name,
        old,
        target,
        clients,
    } in orphans
    {
        // Integrity gate: a locally modified orphan is preserved unless
        // forced. Deleting it would discard the user's edits.
        if !force && is_modified(state, kind, &name, roots)? {
            acted.push(PrunedArtifact {
                kind,
                name,
                old,
                outcome: PruneOutcome::KeptModified,
                removed: Vec::new(),
                clients,
            });
            continue;
        }

        // A resolution-absence AnchorError from uninstall (e.g. the anchor
        // root is unresolvable on this machine) means we cannot resolve the
        // target to delete it, but the record itself is still
        // garbage-collectable: warn + drop the record, treating the artifact
        // as absent/orphaned (consistent with the is_modified contract and the
        // status Missing semantic — §6/T10, ARCH-4/SC-03).
        // A SECURITY-CLASS AnchorError (tampered `../` relative or symlink
        // escape) is FATAL — it propagates as PruneError::Anchor (→
        // DataError(65)) and is NEVER reaped. A genuine I/O error
        // (PruneError::Io) still propagates too.
        let (outcome, removed) = match uninstall(state, kind, &name, roots) {
            Ok(result) => (PruneOutcome::Pruned, result.removed),
            Err(UninstallError::Anchor(anchor_err)) if is_security_class(&anchor_err) => {
                return Err(prune_error(target.clone(), UninstallError::Anchor(anchor_err)));
            }
            Err(UninstallError::Anchor(anchor_err)) => {
                tracing::warn!(
                    "unresolvable anchor for orphan '{name}' during prune; dropping record without file delete: {anchor_err}"
                );
                state.remove(kind, &name);
                (PruneOutcome::Pruned, Vec::new())
            }
            Err(other) => return Err(prune_error(target.clone(), other)),
        };
        acted.push(PrunedArtifact {
            kind,
            name,
            old,
            outcome,
            removed,
            clients,
        });
    }

    Ok(acted)
}

/// Whether any recorded client output for `(kind, name)` that is still on
/// disk has drifted from its recorded content hash. An absent output is not
/// "modified" — it is simply gone, and safe to prune.
///
/// An output unresolvable for a *resolution-absence* reason (the anchor root
/// is absent on this machine, or plain I/O) is treated as **absent/orphaned**
/// (safe to reap) with a `tracing::warn!`, consistent with status `Missing`
/// — never silently retained. A **security-class** `AnchorError` (a tampered
/// `../` relative or a symlink that escapes its anchor root) is FATAL: it
/// propagates as [`PruneError::Anchor`] (→ `DataError(65)`) and is never
/// reaped — ARCH-4/SC-03.
///
/// # Errors
///
/// [`PruneError::Anchor`] when resolving/hashing an output hits a
/// security-class anchor failure.
fn is_modified(state: &InstallState, kind: ArtifactKind, name: &str, roots: &AnchorRoots) -> Result<bool, PruneError> {
    let Some(record) = state.get(kind, name) else {
        return Ok(false);
    };
    for out in &record.outputs {
        let resolved = match out.resolved_target(roots) {
            Ok(resolved) => resolved,
            Err(e) if is_security_class(&e) => {
                return Err(PruneError::Anchor {
                    path: roots.workspace.clone(),
                    source: e,
                });
            }
            Err(e) => {
                tracing::warn!("treating unresolvable orphan '{name}' as reapable: {e}");
                return Ok(false);
            }
        };
        if resolved.exists() {
            let actual = match out.current_hash(roots) {
                Ok(actual) => actual,
                Err(e) if is_security_class(&e) => {
                    return Err(PruneError::Anchor {
                        path: resolved,
                        source: e,
                    });
                }
                Err(e) => {
                    tracing::warn!("treating unresolvable orphan '{name}' as reapable: {e}");
                    return Ok(false);
                }
            };
            if actual != out.content_hash {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

/// Map a [`UninstallError`] to a [`PruneError`] attributed to `path`.
///
/// A security-class [`AnchorError`] is preserved as [`PruneError::Anchor`]
/// (not flattened to I/O) so `classify_error` maps a path-traversal to
/// `DataError(65)` rather than `IoError(74)` — ARCH-4/SC-03. A plain I/O
/// failure maps to [`PruneError::Io`].
fn prune_error(path: PathBuf, source: UninstallError) -> PruneError {
    match source {
        UninstallError::Anchor(e) => PruneError::Anchor { path, source: e },
        UninstallError::Io(io) => PruneError::Io { path, source: io },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::install::content_hash::content_hash;
    use crate::install::install_state::{ClientOutput, InstallRecord};
    use crate::install::path_anchor::{AnchorRoots, AnchoredPath, PathAnchor};
    use crate::lock::grimoire_lock::LockMetadata;
    use crate::lock::lock_version::LockVersion;
    use crate::lock::locked_artifact::LockedArtifact;
    use crate::oci::Identifier;
    use crate::oci::pinned_identifier::PinnedIdentifier;

    fn pinned(name: &str) -> PinnedIdentifier {
        let id = Identifier::new_registry(name, "localhost:5000").clone_with_digest(Digest::Sha256("a".repeat(64)));
        PinnedIdentifier::try_from(id).unwrap()
    }

    /// Build `AnchorRoots` rooted at `workspace`.
    fn roots(workspace: &std::path::Path) -> AnchorRoots {
        AnchorRoots {
            workspace: workspace.to_path_buf(),
            grim_home: workspace.to_path_buf(),
            claude_root: None,
            copilot_root: None,
            opencode_skills: None,
            agents_skills: None,
            codex_root: None,
        }
    }

    /// Materialize a single-file rule on disk and record it in `state` using
    /// `Workspace`-anchored `ClientOutput`.
    fn install_rule(state: &mut InstallState, root: &std::path::Path, name: &str) -> std::path::PathBuf {
        let file = root.join(format!(".claude/rules/{name}.md"));
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, format!("# {name}\n")).unwrap();
        let hash = content_hash(&file).unwrap();
        state.record(InstallRecord {
            kind: ArtifactKind::Rule,
            name: name.to_string(),
            pinned: pinned(name),
            outputs: vec![ClientOutput {
                client: "claude".to_string(),
                target: AnchoredPath {
                    anchor: PathAnchor::Workspace,
                    relative: format!(".claude/rules/{name}.md"),
                },
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
            outputs: vec![ClientOutput {
                client: "claude".to_string(),
                target: AnchoredPath {
                    anchor: PathAnchor::Workspace,
                    relative: format!(".claude/skills/{name}"),
                },
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
            agents: vec![],
            bundles: vec![],
        }
    }

    #[test]
    fn prunes_clean_orphan_not_in_lock() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        let mut state = InstallState::empty(&ws.join("state.json"));
        let keep = install_rule(&mut state, ws, "keep");
        let drop = install_rule(&mut state, ws, "drop");

        // Lock declares only "keep"; "drop" is an orphan.
        let lock = lock_of(vec![locked_rule("keep")]);
        let roots = roots(ws);
        let acted = prune_orphans(&mut state, &lock, &roots, false).unwrap();

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
        let ws = dir.path();
        let mut state = InstallState::empty(&ws.join("state.json"));
        let drop = install_rule(&mut state, ws, "drop");
        // Hand-edit the orphan so its content drifts from the record.
        std::fs::write(&drop, b"locally edited\n").unwrap();

        let lock = lock_of(vec![]);
        let roots = roots(ws);
        let acted = prune_orphans(&mut state, &lock, &roots, false).unwrap();

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
        let ws = dir.path();
        let mut state = InstallState::empty(&ws.join("state.json"));
        let drop = install_rule(&mut state, ws, "drop");
        std::fs::write(&drop, b"locally edited\n").unwrap();

        let lock = lock_of(vec![]);
        let roots = roots(ws);
        let acted = prune_orphans(&mut state, &lock, &roots, true).unwrap();

        assert_eq!(acted.len(), 1);
        assert_eq!(acted[0].outcome, PruneOutcome::Pruned);
        assert!(!drop.exists(), "--force removes a modified orphan");
        assert!(state.get(ArtifactKind::Rule, "drop").is_none());
    }

    #[test]
    fn no_orphans_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        let mut state = InstallState::empty(&ws.join("state.json"));
        install_rule(&mut state, ws, "keep");
        let lock = lock_of(vec![locked_rule("keep")]);
        let roots = roots(ws);
        let acted = prune_orphans(&mut state, &lock, &roots, false).unwrap();
        assert!(acted.is_empty());
    }

    #[test]
    fn prunes_clean_skill_directory_orphan() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        let mut state = InstallState::empty(&ws.join("state.json"));
        let skill = install_skill(&mut state, ws, "code-review");

        // Lock declares nothing; the skill directory is an orphan.
        let lock = lock_of(vec![]);
        let roots = roots(ws);
        let acted = prune_orphans(&mut state, &lock, &roots, false).unwrap();

        assert_eq!(acted.len(), 1);
        assert_eq!(acted[0].outcome, PruneOutcome::Pruned);
        assert!(!skill.exists(), "the orphan skill directory is removed recursively");
        assert!(state.get(ArtifactKind::Skill, "code-review").is_none());
    }

    #[test]
    fn keeps_modified_skill_directory_orphan_without_force() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        let mut state = InstallState::empty(&ws.join("state.json"));
        let skill = install_skill(&mut state, ws, "code-review");
        // Edit a file inside the skill tree so the directory hash drifts.
        std::fs::write(skill.join("SKILL.md"), b"hand edited\n").unwrap();

        let lock = lock_of(vec![]);
        let roots = roots(ws);
        let acted = prune_orphans(&mut state, &lock, &roots, false).unwrap();

        assert_eq!(acted.len(), 1);
        assert_eq!(acted[0].outcome, PruneOutcome::KeptModified);
        assert!(skill.exists(), "a modified skill tree is preserved without --force");
        assert!(state.get(ArtifactKind::Skill, "code-review").is_some());

        // --force prunes the modified skill tree.
        let acted = prune_orphans(&mut state, &lock, &roots, true).unwrap();
        assert_eq!(acted[0].outcome, PruneOutcome::Pruned);
        assert!(!skill.exists());
    }

    #[test]
    fn already_gone_orphan_files_still_drop_record() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        let mut state = InstallState::empty(&ws.join("state.json"));
        let drop = install_rule(&mut state, ws, "drop");
        // Files vanished out from under us; the record must still be reaped.
        std::fs::remove_file(&drop).unwrap();

        let lock = lock_of(vec![]);
        let roots = roots(ws);
        let acted = prune_orphans(&mut state, &lock, &roots, false).unwrap();
        assert_eq!(acted.len(), 1);
        assert_eq!(acted[0].outcome, PruneOutcome::Pruned);
        assert!(state.get(ArtifactKind::Rule, "drop").is_none());
    }

    // §6/T10: a record that is unresolvable for a RESOLUTION-ABSENCE reason
    // (the anchor root is unresolvable on this machine — AnchorRootAbsent) is
    // treated as absent/orphaned (reapable) with a tracing::warn, never
    // silently retained. The test verifies the contract: the orphan is dropped
    // and prune_orphans returns Ok (the Err branch is falsifiable). A
    // security-class AnchorError is covered separately below.
    #[test]
    fn unresolvable_record_treated_as_orphan_and_reaped() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        let mut state = InstallState::empty(&ws.join("state.json"));

        // Build a record anchored at ClaudeRoot, which resolves to None in the
        // test `roots` (claude_root: None) → resolve() yields AnchorRootAbsent,
        // a resolution-absence (NOT security-class) failure.
        state.record(InstallRecord {
            kind: ArtifactKind::Rule,
            name: "absent-root".to_string(),
            pinned: {
                let id = Identifier::new_registry("absent-root", "localhost:5000")
                    .clone_with_digest(Digest::Sha256("a".repeat(64)));
                PinnedIdentifier::try_from(id).unwrap()
            },
            outputs: vec![ClientOutput {
                client: "claude".to_string(),
                target: AnchoredPath {
                    anchor: PathAnchor::ClaudeRoot,
                    relative: "rules/absent-root.md".to_string(),
                },
                content_hash: Digest::Sha256("d".repeat(64)),
                support_dir: None,
            }],
        });

        let lock = lock_of(vec![]); // not in lock → orphan
        let roots = roots(ws); // claude_root is None
        // is_modified() + the uninstall interception absorb AnchorRootAbsent →
        // treat as absent → Pruned. prune_orphans MUST return Ok (the absence
        // case is reaped, never an Err — so the Err branch here is falsifiable).
        let acted = prune_orphans(&mut state, &lock, &roots, false)
            .expect("AnchorRootAbsent must be absorbed → reaped; prune_orphans must return Ok");
        // T10 contract: an absence-unresolvable record is reaped, never retained.
        assert_eq!(acted.len(), 1, "unresolvable orphan must be reaped");
        assert_eq!(acted[0].outcome, PruneOutcome::Pruned);
        assert!(state.get(ArtifactKind::Rule, "absent-root").is_none());
    }

    // ARCH-4/SC-03 regression: a SECURITY-CLASS AnchorError (a tampered `../`
    // relative → TraversalAttempt at resolve) is FATAL even on the prune path.
    // It must NOT be reaped — prune_orphans must return Err(PruneError::Anchor),
    // and applying the exact update.rs mapping closure must classify to
    // DataError(65), not IoError(74). This drives the real flow (no bypass): a
    // recorded orphan with a `../escape` relative goes through prune_orphans.
    // The test fails if the security-class distinction is reverted (the error
    // would be absorbed → Ok → the assertion on Err would fail).
    #[test]
    fn security_class_traversal_propagates_from_prune_to_data_error() {
        use crate::cli::exit_code::ExitCode;
        use crate::error::{Error, classify_error};

        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        let mut state = InstallState::empty(&ws.join("state.json"));

        // Record an orphan whose stored target.relative is a traversal attempt.
        // Layer 1 of resolve() rejects it with TraversalAttempt (security-class)
        // WITHOUT touching the filesystem.
        state.record(InstallRecord {
            kind: ArtifactKind::Rule,
            name: "evil".to_string(),
            pinned: {
                let id = Identifier::new_registry("evil", "localhost:5000")
                    .clone_with_digest(Digest::Sha256("a".repeat(64)));
                PinnedIdentifier::try_from(id).unwrap()
            },
            outputs: vec![ClientOutput {
                client: "claude".to_string(),
                target: AnchoredPath {
                    anchor: PathAnchor::Workspace,
                    relative: "../escape/target.md".to_string(),
                },
                content_hash: Digest::Sha256("d".repeat(64)),
                support_dir: None,
            }],
        });

        let lock = lock_of(vec![]); // not in lock → orphan
        let roots = roots(ws);

        // Drive the real flow: a security-class traversal must PROPAGATE, never
        // be reaped.
        let err = prune_orphans(&mut state, &lock, &roots, false)
            .expect_err("a security-class TraversalAttempt must propagate, not be reaped");
        assert!(
            matches!(err, PruneError::Anchor { .. }),
            "expected PruneError::Anchor, got {err:?}"
        );
        // The record must NOT have been dropped — a fatal error never reaps.
        assert!(
            state.get(ArtifactKind::Rule, "evil").is_some(),
            "a security-class error must not reap the record"
        );

        // Apply the exact update.rs mapping closure and assert the exit code.
        let top_err: anyhow::Error = match err {
            PruneError::Anchor { source, .. } => Error::Anchor(source).into(),
            PruneError::Io { path, source } => {
                Error::from(crate::install::install_error::InstallError::without_reference(
                    crate::install::install_error::InstallErrorKind::TargetIo { path, source },
                ))
                .into()
            }
        };
        assert_eq!(
            classify_error(&top_err),
            ExitCode::DataError,
            "a path-traversal through the prune path must classify as DataError(65), not IoError(74)"
        );
    }
}
