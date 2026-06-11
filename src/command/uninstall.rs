// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! `grim uninstall <kind> <name>` — fully remove an installed skill/rule.
//!
//! Unlike `grim remove` (which only undeclares — config + lock — and
//! leaves materialized files on disk), `uninstall` is the *full* inverse
//! of `install`: it deletes the materialized client outputs and drops the
//! install-state record via the shared [`crate::install::uninstall`]
//! seam, **and** undeclares the entry from the config + lock so nothing
//! is left behind. The TUI delete action reuses the same seam.

use clap::Args;

use crate::api::uninstall_report::{UninstallReport, UninstallStatus};
use crate::cli::exit_code::ExitCode;
use crate::context::Context;
use crate::install::install_state::InstallState;
use crate::install::uninstall::{UninstallOutcome, uninstall};
use crate::lock::file_lock::ConfigFileLock;
use crate::lock::lock_io;
use crate::oci::ArtifactKind;

use super::add::write_config;
use super::scope_resolution;

/// `grim uninstall` arguments.
#[derive(Debug, Args)]
pub struct UninstallArgs {
    /// `skill` or `rule`.
    #[arg(value_parser = ["skill", "rule"])]
    pub kind: String,

    /// The config binding name to uninstall.
    pub name: String,

    /// Operate on the global scope instead of the discovered project.
    #[arg(long)]
    pub global: bool,

    /// Explicit project config path.
    #[arg(long)]
    pub config: Option<std::path::PathBuf>,
}

/// Run `grim uninstall`.
///
/// # Errors
///
/// Filesystem deletion (74), config (78/79/74), install-state save (74),
/// or lock save (74) failures propagate via the typed error chain. An
/// entry that is neither installed nor declared is reported, not an
/// error.
pub async fn run(ctx: &Context, args: &UninstallArgs) -> anyhow::Result<(UninstallReport, ExitCode)> {
    let kind = if args.kind == "skill" {
        ArtifactKind::Skill
    } else {
        ArtifactKind::Rule
    };

    let scope = super::grim(scope_resolution::resolve(ctx, args.global, args.config.as_deref()))?;

    let _guard = match scope_resolution::lockable_config_path(&scope) {
        Some(path) => Some(super::grim(ConfigFileLock::try_acquire(&path))?),
        None => None,
    };

    // 1. Delete materialized files + drop the install-state record.
    let mut state = super::grim(InstallState::load(&scope.state_path).map_err(|e| state_io(&scope.state_path, e)))?;
    let involved_clients: Vec<crate::install::client_target::ClientTarget> = state
        .get(kind, &args.name)
        .map(|r| {
            r.client_outputs()
                .iter()
                .filter_map(|c| c.client.parse().ok())
                .collect()
        })
        .unwrap_or_default();
    let result = super::grim(uninstall(&mut state, kind, &args.name).map_err(|e| state_io(&scope.workspace, e)))?;
    let file_removed = result.outcome == UninstallOutcome::Removed;
    if file_removed {
        super::grim(state.save().map_err(|e| state_io(&scope.state_path, e)))?;
    }

    // Converge vendor-owned config for every client the removed record
    // carried (e.g. drops OpenCode's managed `instructions` glob when the
    // last OpenCode rule is gone).
    for client in involved_clients {
        super::grim(
            client
                .vendor()
                .sync_config(&state, &scope.workspace, scope.scope)
                .map_err(|e| crate::install::install_error::InstallError::config_sync(client.to_string(), e)),
        )?;
    }

    // 2. Undeclare from the config + lock (the `remove` half), so a later
    //    `install` does not silently bring it back.
    let mut set = scope.set.clone();
    let declared = undeclare_and_unlock(
        &scope.config_path,
        &scope.lock_path,
        &scope.options,
        &mut set,
        kind,
        &args.name,
    )?;

    let status = if file_removed || declared {
        UninstallStatus::Uninstalled
    } else {
        UninstallStatus::NotInstalled
    };
    Ok((UninstallReport::new(kind, args.name.clone(), status), ExitCode::Success))
}

/// Undeclare `name` from the scope's config (when declared) and drop its
/// lock entry, restamping the lock's `declaration_hash` to the
/// post-removal declaration so the partial-relock staleness gate stays
/// consistent. The lock entry is dropped even when the config never
/// declared it (a legacy TUI install predating declared installs), so the
/// operation is the full inverse either way. Returns whether the config
/// had declared it. Shared by `grim uninstall` and the TUI delete action.
///
/// Bundles never reach this seam: `uninstall`'s `--kind` parser excludes
/// `"bundle"` and the TUI does not operate on bundles — fail loud rather
/// than silently skip if a caller ever passes one.
///
/// # Errors
///
/// A config write or lock save failure, wrapped in the top-level error
/// (via [`super::grim`]) so exit-code classification still works.
pub(crate) fn undeclare_and_unlock(
    config_path: &std::path::Path,
    lock_path: &std::path::Path,
    options: &crate::config::declaration::ConfigOptions,
    set: &mut crate::config::declaration::DesiredSet,
    kind: ArtifactKind,
    name: &str,
) -> anyhow::Result<bool> {
    let declared = match kind {
        ArtifactKind::Skill => set.skills.remove(name).is_some(),
        ArtifactKind::Rule => set.rules.remove(name).is_some(),
        ArtifactKind::Bundle => unreachable!("uninstall does not accept bundles; use `grim remove bundle <name>`"),
    };
    if declared {
        set.invalidate_declaration_hash_cache();
        super::grim(write_config(config_path, options, set))?;
    }
    if let Ok(previous) = lock_io::load(lock_path) {
        let mut new_lock = previous.clone();
        match kind {
            ArtifactKind::Skill => new_lock.skills.retain(|a| a.name != name),
            ArtifactKind::Rule => new_lock.rules.retain(|a| a.name != name),
            ArtifactKind::Bundle => unreachable!("uninstall does not accept bundles"),
        }
        new_lock.metadata.declaration_hash = set.declaration_hash_cached().to_string();
        super::grim(lock_io::save(lock_path, &new_lock, Some(&previous)))?;
    }
    Ok(declared)
}

/// Map a filesystem / install-state I/O failure to a classifiable
/// install-tier `TargetIo` error (exit 74), matching `command::install`.
fn state_io(path: &std::path::Path, source: std::io::Error) -> crate::install::install_error::InstallError {
    crate::install::install_error::InstallError::without_reference(
        crate::install::install_error::InstallErrorKind::TargetIo {
            path: path.to_path_buf(),
            source,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::declaration::{ConfigOptions, DesiredSet};
    use crate::lock::grimoire_lock::{GrimoireLock, LockMetadata};
    use crate::lock::lock_version::LockVersion;
    use crate::lock::locked_artifact::LockedArtifact;
    use crate::oci::{Digest, Identifier, PinnedIdentifier};
    use std::collections::BTreeMap;

    fn sha(byte: char) -> String {
        std::iter::repeat_n(byte, 64).collect()
    }

    fn pinned(repo: &str, byte: char) -> PinnedIdentifier {
        let id = Identifier::new_registry(repo, "localhost:5000").clone_with_digest(Digest::Sha256(sha(byte)));
        PinnedIdentifier::try_from(id).unwrap()
    }

    fn lock_with_skills(declaration_hash: &str, entries: &[(&str, char)]) -> GrimoireLock {
        GrimoireLock {
            metadata: LockMetadata {
                lock_version: LockVersion::V1,
                declaration_hash_version: 1,
                declaration_hash: declaration_hash.to_string(),
                generated_by: "grim test".to_string(),
                generated_at: "2026-06-11T00:00:00Z".to_string(),
            },
            skills: entries
                .iter()
                .map(|(name, byte)| {
                    LockedArtifact::direct(
                        name.to_string(),
                        ArtifactKind::Skill,
                        pinned(&format!("acme/{name}"), *byte),
                    )
                })
                .collect(),
            rules: Vec::new(),
        }
    }

    fn declared_set(names: &[&str]) -> DesiredSet {
        let skills: BTreeMap<String, Identifier> = names
            .iter()
            .map(|n| {
                (
                    n.to_string(),
                    Identifier::parse(&format!("localhost:5000/acme/{n}:latest")).unwrap(),
                )
            })
            .collect();
        DesiredSet::from_parts(skills, BTreeMap::new())
    }

    #[test]
    fn undeclare_drops_config_entry_lock_entry_and_restamps_hash() {
        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("grimoire.toml");
        let lock_path = tmp.path().join("grimoire.lock");

        let mut set = declared_set(&["alpha", "beta"]);
        write_config(&config_path, &ConfigOptions::default(), &set).unwrap();
        let lock = lock_with_skills(set.declaration_hash_cached(), &[("alpha", 'a'), ("beta", 'b')]);
        lock_io::save(&lock_path, &lock, None).unwrap();

        let declared = undeclare_and_unlock(
            &config_path,
            &lock_path,
            &ConfigOptions::default(),
            &mut set,
            ArtifactKind::Skill,
            "alpha",
        )
        .expect("undeclare succeeds");

        assert!(declared, "alpha was declared");
        let body = std::fs::read_to_string(&config_path).unwrap();
        assert!(!body.contains("alpha"), "config must drop the entry");
        assert!(body.contains("beta"), "other entries survive");
        let saved = lock_io::load(&lock_path).unwrap();
        assert!(
            saved.skills.iter().all(|a| a.name != "alpha"),
            "lock must drop the entry"
        );
        assert_eq!(saved.skills.len(), 1);
        assert_eq!(
            saved.metadata.declaration_hash,
            set.declaration_hash_cached(),
            "lock hash must match the post-removal declaration"
        );
    }

    #[test]
    fn undeclared_lock_entry_is_still_dropped_and_hash_reconciled() {
        // Regression: a lock entry the config never declared (a legacy TUI
        // install wrote lock-only) must still be dropped, and the lock's
        // declaration hash reconciled to the config, so a later partial
        // relock does not operate on a drifted lock.
        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("grimoire.toml");
        let lock_path = tmp.path().join("grimoire.lock");

        let mut set = declared_set(&[]);
        write_config(&config_path, &ConfigOptions::default(), &set).unwrap();
        // The drifted lock: an entry + a hash that matches nothing.
        let lock = lock_with_skills(&format!("sha256:{}", sha('f')), &[("ghost", 'c')]);
        lock_io::save(&lock_path, &lock, None).unwrap();

        let declared = undeclare_and_unlock(
            &config_path,
            &lock_path,
            &ConfigOptions::default(),
            &mut set,
            ArtifactKind::Skill,
            "ghost",
        )
        .expect("undeclare succeeds");

        assert!(!declared, "ghost was never declared");
        let saved = lock_io::load(&lock_path).unwrap();
        assert!(saved.skills.is_empty(), "the drifted lock entry must be dropped");
        assert_eq!(
            saved.metadata.declaration_hash,
            set.declaration_hash_cached(),
            "the lock hash must be reconciled to the declaration"
        );
    }
}
