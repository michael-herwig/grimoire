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
    let result = super::grim(uninstall(&mut state, kind, &args.name).map_err(|e| state_io(&scope.workspace, e)))?;
    let file_removed = result.outcome == UninstallOutcome::Removed;
    if file_removed {
        super::grim(state.save().map_err(|e| state_io(&scope.state_path, e)))?;
    }

    // 2. Undeclare from the config + lock (the `remove` half), so a later
    //    `install` does not silently bring it back.
    let mut set = scope.set.clone();
    let declared = match kind {
        ArtifactKind::Skill => set.skills.remove(&args.name).is_some(),
        ArtifactKind::Rule => set.rules.remove(&args.name).is_some(),
        // Bundles do not materialize, so there is nothing to uninstall; the
        // `--kind` parser excludes "bundle", so this is unreachable. Fail
        // loud rather than silently skip if the parser ever gains it.
        ArtifactKind::Bundle => unreachable!("uninstall does not accept bundles; use `grim remove bundle <name>`"),
    };
    if declared {
        set.invalidate_declaration_hash_cache();
        super::grim(write_config(&scope.config_path, &scope.options, &set))?;
        if let Ok(previous) = lock_io::load(&scope.lock_path) {
            let mut new_lock = previous.clone();
            match kind {
                ArtifactKind::Skill => new_lock.skills.retain(|a| a.name != args.name),
                ArtifactKind::Rule => new_lock.rules.retain(|a| a.name != args.name),
                // Unreachable: the `--kind` parser excludes "bundle".
                ArtifactKind::Bundle => unreachable!("uninstall does not accept bundles"),
            }
            new_lock.metadata.declaration_hash = set.declaration_hash_cached().to_string();
            super::grim(lock_io::save(&scope.lock_path, &new_lock, Some(&previous)))?;
        }
    }

    let status = if file_removed || declared {
        UninstallStatus::Uninstalled
    } else {
        UninstallStatus::NotInstalled
    };
    Ok((UninstallReport::new(kind, args.name.clone(), status), ExitCode::Success))
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
