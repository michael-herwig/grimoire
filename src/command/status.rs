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
use crate::install::content_hash::content_hash;
use crate::install::install_state::InstallState;
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
    let state = load_state_or_empty(&scope.state_path);

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
        let state = derive_state(decl.kind, &decl.name, locked, &state, lock_matches_config);
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
        for member in l.skills.iter().chain(l.rules.iter()).filter(|a| a.is_from_bundle()) {
            let st = derive_state(member.kind, &member.name, Some(member), &state, lock_matches_config);
            let repo = member.bundle.clone().unwrap_or_default();
            entries.push(StatusEntry {
                kind: member.kind,
                name: member.name.clone(),
                source: format!("bundle: {repo}"),
                pinned: Some(member.pinned.clone()),
                state: st,
            });
        }
    }

    Ok((StatusReport::new(entries), ExitCode::Success))
}

/// Load the install state, or an empty state if the file is absent or
/// corrupt (a read-only report must not fail on store damage).
fn load_state_or_empty(path: &std::path::Path) -> InstallState {
    InstallState::load(path).unwrap_or_else(|_| InstallState::empty(path))
}

/// Every declared artifact (skills then rules) as a reference.
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
    out
}

fn find_locked<'a>(lock: &'a GrimoireLock, kind: ArtifactKind, name: &str) -> Option<&'a LockedArtifact> {
    lock.skills
        .iter()
        .chain(lock.rules.iter())
        .find(|a| a.kind == kind && a.name == name)
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
    let outputs = record.client_outputs();
    // Any missing client output ⇒ the artifact is not fully installed.
    if outputs.iter().any(|o| !o.target.exists()) {
        return ArtifactStatus::Missing;
    }
    // Any drifted client output (canonical OR generated — the recorded
    // hash for a generated target is over its expected bytes) ⇒ modified.
    for out in &outputs {
        match content_hash(&out.target) {
            Ok(actual) if actual != out.content_hash => return ArtifactStatus::Modified,
            Ok(_) => {}
            // An unreadable target is effectively gone for reporting.
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
    use crate::install::install_state::InstallRecord;
    use crate::oci::pinned_identifier::PinnedIdentifier;
    use crate::oci::{Algorithm, Digest, Identifier};

    fn pinned(byte: char) -> PinnedIdentifier {
        let id = Identifier::new_registry("x", "localhost:5000")
            .clone_with_digest(Digest::Sha256(std::iter::repeat_n(byte, 64).collect()));
        PinnedIdentifier::try_from(id).unwrap()
    }

    fn locked(byte: char) -> LockedArtifact {
        LockedArtifact::direct("x".to_string(), ArtifactKind::Rule, pinned(byte))
    }

    #[test]
    fn stale_when_lock_does_not_match_config() {
        let dir = tempfile::tempdir().unwrap();
        let st = InstallState::load(&dir.path().join("s.json")).unwrap();
        let s = derive_state(ArtifactKind::Rule, "x", Some(&locked('a')), &st, false);
        assert_eq!(s, ArtifactStatus::Stale);
    }

    #[test]
    fn missing_when_not_locked_or_not_recorded() {
        let dir = tempfile::tempdir().unwrap();
        let st = InstallState::load(&dir.path().join("s.json")).unwrap();
        assert_eq!(
            derive_state(ArtifactKind::Rule, "x", None, &st, true),
            ArtifactStatus::Missing
        );
        assert_eq!(
            derive_state(ArtifactKind::Rule, "x", Some(&locked('a')), &st, true),
            ArtifactStatus::Missing
        );
    }

    #[test]
    fn installed_modified_outdated_transitions() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("x.md");
        std::fs::write(&target, b"canonical\n").unwrap();
        let hash = content_hash(&target).unwrap();

        let mut st = InstallState::load(&dir.path().join("s.json")).unwrap();
        st.record(InstallRecord {
            kind: ArtifactKind::Rule,
            name: "x".to_string(),
            pinned: pinned('a'),
            content_hash: hash.clone(),
            target: target.clone(),
            clients: vec![],
        });

        // Same pin, intact content ⇒ installed.
        assert_eq!(
            derive_state(ArtifactKind::Rule, "x", Some(&locked('a')), &st, true),
            ArtifactStatus::Installed
        );

        // Lock advanced to a different digest ⇒ outdated.
        assert_eq!(
            derive_state(ArtifactKind::Rule, "x", Some(&locked('b')), &st, true),
            ArtifactStatus::Outdated
        );

        // Tamper with the file ⇒ modified.
        std::fs::write(&target, b"hand edited\n").unwrap();
        assert_eq!(
            derive_state(ArtifactKind::Rule, "x", Some(&locked('a')), &st, true),
            ArtifactStatus::Modified
        );
        let _ = Algorithm::Sha256;
    }
}
