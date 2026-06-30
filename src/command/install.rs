// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! `grim install` — materialize the locked artifacts into the client.
//!
//! Install does **not** resolve: it requires a lock that is present and
//! whose `declaration_hash` matches the live config (otherwise it tells
//! the user to run `grim lock`). It then fetches each pinned blob through
//! the cache, materializes it, and enforces the local-modification
//! integrity gate (refuse unless `--force`).

use clap::Args;

use std::io::IsTerminal;

use crate::api::artifact_status::InstallStatus;
use crate::api::install_report::{InstallEntry, InstallReport};
use crate::cli::exit_code::ExitCode;
use crate::cli::progress::StderrBar;
use crate::command::command_error::CommandError;
use crate::context::Context;
use crate::install::installer::{ArtifactInstall, InstallOutcome, install_all_with_progress};
use crate::install::materializer::DefaultMaterializer;
use crate::install::progress::{InstallProgress, SilentProgress};
use crate::install::target::InstallTarget;
use crate::lock::file_lock::ConfigFileLock;
use crate::lock::lock_io;

use super::scope_resolution::{self, ResolvedScope};

/// `grim install` arguments.
#[derive(Debug, Args)]
pub struct InstallArgs {
    /// Install the global scope instead of the discovered project.
    #[arg(long)]
    pub global: bool,

    /// Explicit project config path.
    #[arg(long)]
    pub config: Option<std::path::PathBuf>,

    /// Overwrite a locally modified artifact instead of refusing it.
    #[arg(long)]
    pub force: bool,

    /// AI client(s) to materialize into (comma-separated, repeatable;
    /// `claude`, `opencode`, `copilot`). Defaults to the config `clients`
    /// option, then all detected clients (vendor dir present), then
    /// `claude` when none are detected.
    #[arg(long = "client")]
    pub client: Vec<String>,
}

/// Run `grim install`.
///
/// # Errors
///
/// Lock missing / stale (79 / 65), integrity (65), offline (81),
/// registry (69), or I/O (74) failures propagate via the typed chain.
pub async fn run(ctx: &Context, args: &InstallArgs) -> anyhow::Result<(InstallReport, ExitCode)> {
    let scope = super::grim(scope_resolution::resolve(ctx, args.global, args.config.as_deref()))?;

    let _guard = match scope_resolution::lockable_config_path(&scope) {
        Some(path) => Some(super::grim(ConfigFileLock::try_acquire(&path))?),
        None => None,
    };

    let lock = require_fresh_lock(&scope)?;

    let target = super::grim(InstallTarget::parse(
        &scope.workspace,
        scope.scope,
        &args.client,
        &scope.options.clients,
    ))?;
    let access = super::access_seam(ctx)?;
    let mut state = super::grim(scope_resolution::load_state(&scope).map_err(|e| state_io(&scope.state_path, e)))?;
    let materializer = DefaultMaterializer;

    // Show a progress bar only on an interactive stderr; piped / redirected
    // runs (CI, `| jq`, tests) install silently so captured streams stay
    // free of control codes. The bar writes to stderr, never stdout, so the
    // structured report (and `--format json`) is untouched either way.
    let silent = SilentProgress;
    let bar = std::io::stderr().is_terminal().then(StderrBar::default);
    let progress: &dyn InstallProgress = match bar.as_ref() {
        Some(b) => b,
        None => &silent,
    };

    let outcomes = install_all_with_progress(
        &lock,
        &access,
        &materializer,
        &target,
        &mut state,
        &scope.roots,
        args.force,
        progress,
    )
    .await;

    // Persist whatever progress was made (some artifacts may have
    // installed before another failed) before surfacing the first error.
    // The single `persist` seam handles project-scope dir creation, the
    // atomic write, and the conditional legacy-file reap in one place.
    super::grim(
        state
            .persist(
                scope.scope,
                &scope.workspace,
                &scope.roots.grim_home,
                &scope.config_path,
            )
            .map_err(|e| match e {
                crate::install::install_state::PersistError::EnsureDir { path, source }
                | crate::install::install_state::PersistError::Save { path, source } => state_io(&path, source),
            }),
    )?;

    // Converge vendor-owned config on the new state (e.g. OpenCode's
    // managed `instructions` glob) for every involved client. The artifacts
    // and install state are already persisted, so a config-sync failure (an
    // unparseable / unreadable vendor config) is warn-only: the install
    // succeeds, registration is skipped, never a hard command failure.
    for client in target.clients() {
        if let Err(e) = client.vendor().sync_config(&state, &scope.workspace, scope.scope) {
            tracing::warn!(
                client = %client,
                error = %e,
                "vendor config sync failed; artifacts installed and state saved, registration skipped"
            );
        }
    }

    finish(outcomes)
}

/// Wrap an install-state I/O failure as the install-tier `TargetIo` error.
fn state_io(path: &std::path::Path, source: std::io::Error) -> crate::install::install_error::InstallError {
    crate::install::install_error::InstallError::without_reference(
        crate::install::install_error::InstallErrorKind::TargetIo {
            path: path.to_path_buf(),
            source,
        },
    )
}

/// Require a present lock whose declaration hash matches the live config.
///
/// Lock missing ⇒ NotFound (79); declaration drift ⇒ DataError (65). Both
/// messages tell the user to run `grim lock`.
pub(crate) fn require_fresh_lock(scope: &ResolvedScope) -> anyhow::Result<crate::lock::grimoire_lock::GrimoireLock> {
    let lock = lock_io::load(&scope.lock_path).map_err(|e| {
        // A missing lock surfaces as the lock-tier Io(NotFound); re-key it
        // as the command-tier `LockMissing` so it classifies as NotFound.
        if let crate::lock::lock_error::LockErrorKind::Io(io) = &e.kind
            && io.kind() == std::io::ErrorKind::NotFound
        {
            return anyhow::Error::from(crate::error::Error::from(CommandError::LockMissing {
                path: scope.lock_path.clone(),
            }));
        }
        anyhow::Error::from(crate::error::Error::from(e))
    })?;

    let current = scope.set.declaration_hash_cached();
    if lock.metadata.declaration_hash != current {
        return Err(crate::error::Error::from(CommandError::LockStale {
            locked: lock.metadata.declaration_hash.clone(),
            current: current.to_string(),
        })
        .into());
    }
    Ok(lock)
}

/// Turn per-artifact outcomes into the report + the worst exit code. A
/// refusal or a hard error makes the run fail; a clean install/no-op is
/// success.
fn finish(outcomes: Vec<ArtifactInstall>) -> anyhow::Result<(InstallReport, ExitCode)> {
    let mut entries = Vec::with_capacity(outcomes.len());
    let mut first_error: Option<crate::error::Error> = None;

    for ArtifactInstall {
        reference,
        target,
        result,
    } in outcomes
    {
        let status = match result {
            Ok(InstallOutcome::Installed) => InstallStatus::Installed,
            Ok(InstallOutcome::Updated) => InstallStatus::Updated,
            Ok(InstallOutcome::AlreadyInstalled) => InstallStatus::Unchanged,
            Ok(InstallOutcome::Skipped(_)) => InstallStatus::Skipped,
            Ok(InstallOutcome::Refused { recorded, actual }) => {
                if first_error.is_none() {
                    let r = reference.clone();
                    first_error = Some(crate::error::Error::from(
                        crate::install::install_error::InstallError::with_reference(
                            r,
                            crate::install::install_error::InstallErrorKind::IntegrityMismatch { recorded, actual },
                        ),
                    ));
                }
                InstallStatus::Refused
            }
            Err(e) => {
                if first_error.is_none() {
                    first_error = Some(e);
                }
                InstallStatus::Skipped
            }
        };
        entries.push(InstallEntry {
            kind: reference.kind,
            name: reference.name,
            target,
            status,
        });
    }

    let report = InstallReport::new(entries);
    if let Some(err) = first_error {
        return Err(err.into());
    }
    Ok((report, ExitCode::Success))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::install::install_error::{InstallError, InstallErrorKind};
    use crate::oci::reference::ArtifactRef;
    use crate::oci::{ArtifactKind, Identifier};

    fn aref(name: &str) -> ArtifactRef {
        ArtifactRef {
            kind: ArtifactKind::Rule,
            name: name.to_string(),
            id: Identifier::parse("localhost:5000/x:latest").unwrap(),
        }
    }

    #[test]
    fn finish_maps_outcomes_to_statuses() {
        let outcomes = vec![
            ArtifactInstall {
                reference: aref("a"),
                target: "/t/a".into(),
                result: Ok(InstallOutcome::Installed),
            },
            ArtifactInstall {
                reference: aref("b"),
                target: "/t/b".into(),
                result: Ok(InstallOutcome::AlreadyInstalled),
            },
        ];
        let (report, code) = finish(outcomes).expect("clean run is success");
        assert_eq!(code, ExitCode::Success);
        let v = serde_json::to_value(&report).unwrap();
        assert_eq!(v[0]["status"], "installed");
        assert_eq!(v[1]["status"], "unchanged");
    }

    #[test]
    fn finish_errors_on_refusal_as_data_error() {
        let outcomes = vec![ArtifactInstall {
            reference: aref("a"),
            target: "/t/a".into(),
            result: Ok(InstallOutcome::Refused {
                recorded: crate::oci::Digest::Sha256("a".repeat(64)),
                actual: crate::oci::Digest::Sha256("b".repeat(64)),
            }),
        }];
        let err = finish(outcomes).expect_err("refusal must fail the run");
        assert_eq!(crate::error::classify_error(&err), ExitCode::DataError);
    }

    #[test]
    fn finish_propagates_first_error() {
        let outcomes = vec![ArtifactInstall {
            reference: aref("a"),
            target: "/t/a".into(),
            result: Err(crate::error::Error::from(InstallError::without_reference(
                InstallErrorKind::BlobMissing,
            ))),
        }];
        let err = finish(outcomes).expect_err("hard error must propagate");
        assert_eq!(crate::error::classify_error(&err), ExitCode::NotFound);
    }
}
