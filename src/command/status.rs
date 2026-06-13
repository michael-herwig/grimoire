// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! `grim status` — read-only state report for every declared artifact.
//!
//! No network and no flock: state is data, not a failure, so `status`
//! exits 0 even when artifacts are missing or modified. The only failure
//! exits are a config (78/79) or lock (78) load error. Per declared
//! artifact the state is derived from: the live config vs. the lock's
//! declaration hash (`stale`), the lock pin vs. the install-state record
//! (`outdated`), the recorded pin missing (`missing`), and the on-disk
//! content hash vs. the recorded one (`modified`).

use clap::Args;

use crate::api::artifact_status::ArtifactStatus;
use crate::api::status_report::{StatusEntry, StatusReport};
use crate::cli::exit_code::ExitCode;
use crate::context::Context;
use crate::install::install_state::InstallState;
use crate::install::path_anchor::AnchorRoots;
use crate::lock::grimoire_lock::GrimoireLock;
use crate::lock::lock_io;
use crate::lock::locked_artifact::LockedArtifact;
use crate::oci::ArtifactKind;
use crate::oci::reference::ArtifactRef;

use super::scope_resolution;

/// `grim status` arguments.
#[derive(Debug, Args)]
pub struct StatusArgs {
    /// Report on the global scope instead of the discovered project.
    #[arg(long)]
    pub global: bool,

    /// Explicit project config path.
    #[arg(long)]
    pub config: Option<std::path::PathBuf>,
}

/// Run `grim status`.
///
/// # Errors
///
/// Only a config (78/79) or lock-parse (78) load failure; artifact state
/// is data and never fails the command.
pub async fn run(ctx: &Context, args: &StatusArgs) -> anyhow::Result<(StatusReport, ExitCode)> {
    let scope = super::grim(scope_resolution::resolve(ctx, args.global, args.config.as_deref()))?;

    // A missing lock is not a hard failure for `status` — it just means
    // every declared artifact is `missing`/`stale`. A *corrupt* lock is a
    // load failure (78) and propagates.
    let lock = match lock_io::load(&scope.lock_path) {
        Ok(l) => Some(l),
        Err(e) => {
            if let crate::lock::lock_error::LockErrorKind::Io(io) = &e.kind
                && io.kind() == std::io::ErrorKind::NotFound
            {
                None
            } else {
                return Err(crate::error::Error::from(e).into());
            }
        }
    };

    // A corrupt state file degrades to "nothing installed" for a
    // read-only report rather than failing the command (state is data).
    // Routes through the scope seam so a project legacy file (or a V1 global
    // file) migrates in memory; any load failure degrades to empty.
    let state = scope_resolution::load_state(&scope).unwrap_or_else(|_| InstallState::empty(&scope.state_path));

    let lock_matches_config =
        lock.as_ref().map(|l| l.metadata.declaration_hash.as_str()) == Some(scope.set.declaration_hash_cached());

    let mut entries = Vec::new();

    // Declared bundles: one row each so the user sees what they declared.
    // A bundle never installs itself — its state reflects whether it has
    // been expanded into a fresh lock.
    for name in scope.set.bundles.keys() {
        let state = if !lock_matches_config {
            ArtifactStatus::Stale
        } else if lock.is_none() {
            ArtifactStatus::Missing
        } else {
            ArtifactStatus::Installed
        };
        entries.push(StatusEntry {
            kind: ArtifactKind::Bundle,
            name: name.clone(),
            source: "direct".to_string(),
            pinned: None,
            state,
        });
    }

    // Directly-declared skills and rules.
    let declared: Vec<ArtifactRef> = collect_declared(&scope);
    for decl in declared {
        let locked = lock.as_ref().and_then(|l| find_locked(l, decl.kind, &decl.name));
        let state = derive_state(decl.kind, &decl.name, locked, &state, &scope.roots, lock_matches_config);
        entries.push(StatusEntry {
            kind: decl.kind,
            name: decl.name,
            source: "direct".to_string(),
            pinned: locked.map(|l| l.pinned.clone()),
            state,
        });
    }

    // Members contributed by bundles: read straight from the lock (they are
    // not in the declared skill/rule maps). A directly-declared name always
    // resolves to a `direct` lock entry, so these never duplicate the rows
    // above.
    if let Some(l) = lock.as_ref() {
        for member in l.iter_artifacts().filter(|a| a.is_from_bundle()) {
            let st = derive_state(
                member.kind,
                &member.name,
                Some(member),
                &state,
                &scope.roots,
                lock_matches_config,
            );
            // Every contributing bundle is listed (a shared member carries
            // multi-provenance), comma-joined in lock order.
            let repos: Vec<&str> = member.bundles.iter().map(|b| b.repo.as_str()).collect();
            entries.push(StatusEntry {
                kind: member.kind,
                name: member.name.clone(),
                source: format!("bundle: {}", repos.join(", ")),
                pinned: Some(member.pinned.clone()),
                state: st,
            });
        }
    }

    Ok((StatusReport::new(entries), ExitCode::Success))
}

/// Every declared artifact (skills, then rules, then agents) as a reference.
fn collect_declared(scope: &scope_resolution::ResolvedScope) -> Vec<ArtifactRef> {
    let mut out = Vec::new();
    for (name, id) in &scope.set.skills {
        out.push(ArtifactRef {
            kind: ArtifactKind::Skill,
            name: name.clone(),
            id: id.clone(),
        });
    }
    for (name, id) in &scope.set.rules {
        out.push(ArtifactRef {
            kind: ArtifactKind::Rule,
            name: name.clone(),
            id: id.clone(),
        });
    }
    for (name, id) in &scope.set.agents {
        out.push(ArtifactRef {
            kind: ArtifactKind::Agent,
            name: name.clone(),
            id: id.clone(),
        });
    }
    out
}

fn find_locked<'a>(lock: &'a GrimoireLock, kind: ArtifactKind, name: &str) -> Option<&'a LockedArtifact> {
    lock.iter_artifacts().find(|a| a.kind == kind && a.name == name)
}

/// Derive the reported state for one declared artifact.
///
/// Precedence: a declaration-hash mismatch makes everything `stale`
/// (the lock no longer reflects the config). Otherwise, no lock entry or
/// no install record ⇒ `missing`; recorded but content drifted ⇒
/// `modified`; installed digest != lock digest ⇒ `outdated`; else
/// `installed`.
fn derive_state(
    kind: ArtifactKind,
    name: &str,
    locked: Option<&LockedArtifact>,
    state: &InstallState,
    roots: &AnchorRoots,
    lock_matches_config: bool,
) -> ArtifactStatus {
    if !lock_matches_config {
        return ArtifactStatus::Stale;
    }
    let Some(locked) = locked else {
        return ArtifactStatus::Missing;
    };
    let Some(record) = state.get(kind, name) else {
        return ArtifactStatus::Missing;
    };
    // An unresolvable anchored target (corrupt/tampered `relative`, or an
    // anchor root absent on this machine) is degraded to `Missing` for a
    // read-only report — never `?`-propagated (state is data; status exits 0).
    for out in &record.outputs {
        match out.resolved_target(roots) {
            Ok(resolved) if !resolved.exists() => return ArtifactStatus::Missing,
            Ok(_) => {}
            Err(_) => return ArtifactStatus::Missing,
        }
    }
    // Any drifted client output (canonical OR generated — the recorded
    // hash for a generated target is over its expected bytes) ⇒ modified.
    for out in &record.outputs {
        match out.current_hash(roots) {
            Ok(actual) if actual != out.content_hash => return ArtifactStatus::Modified,
            Ok(_) => {}
            // An unreadable / unresolvable target is effectively gone.
            Err(_) => return ArtifactStatus::Missing,
        }
    }
    if record.pinned.eq_content(&locked.pinned) {
        ArtifactStatus::Installed
    } else {
        ArtifactStatus::Outdated
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::install::content_hash::content_hash;
    use crate::install::install_state::{ClientOutput, InstallRecord};
    use crate::install::path_anchor::{AnchorRoots, AnchoredPath, PathAnchor};
    use crate::oci::pinned_identifier::PinnedIdentifier;
    use crate::oci::{Algorithm, Digest, Identifier};
    use std::path::PathBuf;

    fn pinned(byte: char) -> PinnedIdentifier {
        let id = Identifier::new_registry("x", "localhost:5000")
            .clone_with_digest(Digest::Sha256(std::iter::repeat_n(byte, 64).collect()));
        PinnedIdentifier::try_from(id).unwrap()
    }

    fn locked(byte: char) -> LockedArtifact {
        LockedArtifact::direct("x".to_string(), ArtifactKind::Rule, pinned(byte))
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

    #[test]
    fn stale_when_lock_does_not_match_config() {
        let dir = tempfile::tempdir().unwrap();
        let roots = roots(dir.path());
        let st = InstallState::load(&dir.path().join("s.json")).unwrap();
        let s = derive_state(ArtifactKind::Rule, "x", Some(&locked('a')), &st, &roots, false);
        assert_eq!(s, ArtifactStatus::Stale);
    }

    #[test]
    fn missing_when_not_locked_or_not_recorded() {
        let dir = tempfile::tempdir().unwrap();
        let roots = roots(dir.path());
        let st = InstallState::load(&dir.path().join("s.json")).unwrap();
        assert_eq!(
            derive_state(ArtifactKind::Rule, "x", None, &st, &roots, true),
            ArtifactStatus::Missing
        );
        assert_eq!(
            derive_state(ArtifactKind::Rule, "x", Some(&locked('a')), &st, &roots, true),
            ArtifactStatus::Missing
        );
    }

    #[test]
    fn installed_modified_outdated_transitions() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        let target = ws.join("x.md");
        std::fs::write(&target, b"canonical\n").unwrap();
        let hash = content_hash(&target).unwrap();
        let roots = roots(ws);

        let mut st = InstallState::load(&ws.join("s.json")).unwrap();
        st.record(InstallRecord {
            kind: ArtifactKind::Rule,
            name: "x".to_string(),
            pinned: pinned('a'),
            outputs: vec![ClientOutput {
                client: "claude".to_string(),
                target: AnchoredPath {
                    anchor: PathAnchor::Workspace,
                    relative: "x.md".to_string(),
                },
                content_hash: hash.clone(),
                support_dir: None,
            }],
        });

        // Same pin, intact content ⇒ installed.
        assert_eq!(
            derive_state(ArtifactKind::Rule, "x", Some(&locked('a')), &st, &roots, true),
            ArtifactStatus::Installed
        );

        // Lock advanced to a different digest ⇒ outdated.
        assert_eq!(
            derive_state(ArtifactKind::Rule, "x", Some(&locked('b')), &st, &roots, true),
            ArtifactStatus::Outdated
        );

        // Tamper with the file ⇒ modified.
        std::fs::write(&target, b"hand edited\n").unwrap();
        assert_eq!(
            derive_state(ArtifactKind::Rule, "x", Some(&locked('a')), &st, &roots, true),
            ArtifactStatus::Modified
        );
        let _ = Algorithm::Sha256;
        let _ = PathBuf::new();
    }

    // T10 spec: derive_state with an unresolvable AnchoredPath must degrade to
    // Missing via match — never propagate AnchorError as a command failure.
    #[test]
    fn unresolvable_anchored_path_degrades_to_missing_not_error() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        let roots = roots(ws);

        let mut st = InstallState::load(&ws.join("s.json")).unwrap();
        // Record a rule with ClaudeRoot anchor but roots.claude_root = None.
        st.record(InstallRecord {
            kind: ArtifactKind::Rule,
            name: "x".to_string(),
            pinned: pinned('a'),
            outputs: vec![ClientOutput {
                client: "claude".to_string(),
                target: AnchoredPath {
                    anchor: PathAnchor::ClaudeRoot,
                    relative: "rules/x.md".to_string(),
                },
                content_hash: Digest::Sha256("a".repeat(64)),
                support_dir: None,
            }],
        });

        // Roots with claude_root = None → resolved_target returns AnchorRootAbsent.
        // Contract: must return Missing via match, NOT propagate the error.
        // Until T8 this panics with unimplemented!; after T8 it must return Missing.
        let state = derive_state(ArtifactKind::Rule, "x", Some(&locked('a')), &st, &roots, true);
        assert_eq!(
            state,
            ArtifactStatus::Missing,
            "unresolvable AnchoredPath must degrade to Missing, not error"
        );
    }
}
