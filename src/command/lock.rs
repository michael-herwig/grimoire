// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! `grim lock` — resolve every declared floating tag to a pinned digest
//! and write `grimoire.lock`.
//!
//! Acquires the advisory flock on the config file, resolves the whole
//! declared set, loads any previous lock for `generated_at` preservation,
//! writes atomically, and releases. Each report row's action is `locked`
//! when the pin is new or changed and `unchanged` when the previous lock
//! already carried the same content.

use std::sync::Arc;

use clap::Args;

use crate::api::artifact_status::LockAction;
use crate::api::lock_report::{LockEntry, LockReport};
use crate::cli::exit_code::ExitCode;
use crate::context::Context;
use crate::lock::file_lock::ConfigFileLock;
use crate::lock::grimoire_lock::GrimoireLock;
use crate::lock::lock_io;
use crate::lock::locked_artifact::LockedArtifact;
use crate::oci::access::OciAccess;
use crate::resolve::resolve_options::ResolveOptions;
use crate::resolve::resolver::resolve_lock;

use super::scope_resolution::{self, ResolvedScope};

/// `grim lock` arguments.
#[derive(Debug, Args)]
pub struct LockArgs {
    /// Lock the global scope instead of the discovered project.
    #[arg(long)]
    pub global: bool,

    /// Explicit project config path.
    #[arg(long)]
    pub config: Option<std::path::PathBuf>,
}

/// Run `grim lock`.
///
/// # Errors
///
/// Config (78), tag-not-found (79), auth (80), registry-unreachable (69),
/// or flock-contended (75) failures propagate via the typed error chain.
pub async fn run(ctx: &Context, args: &LockArgs) -> anyhow::Result<(LockReport, ExitCode)> {
    let scope = super::grim(scope_resolution::resolve(ctx, args.global, args.config.as_deref()))?;

    // Hold the advisory flock for the resolve+write window. A global
    // config that does not exist yet has no file to lock — the atomic
    // lock-file write is still safe on its own.
    let _guard = match scope_resolution::lockable_config_path(&scope) {
        Some(path) => Some(super::grim(ConfigFileLock::try_acquire(&path))?),
        None => None,
    };

    let access: Arc<dyn OciAccess> = super::access_seam(ctx)?;
    let previous = lock_io::load(&scope.lock_path).ok();

    let lock = super::grim(resolve_lock(&scope.set, &access, scope.scope, &ResolveOptions::default()).await)?;
    super::grim(lock_io::save(&scope.lock_path, &lock, previous.as_ref()))?;

    let report = build_report(&lock, previous.as_ref(), &scope);
    Ok((report, ExitCode::Success))
}

/// Build the per-artifact report: `unchanged` when the previous lock
/// already pinned the same content, `locked` otherwise.
fn build_report(lock: &GrimoireLock, previous: Option<&GrimoireLock>, _scope: &ResolvedScope) -> LockReport {
    let entries = lock
        .iter_artifacts()
        .map(|a| {
            let action = if previous_has_same(previous, a) {
                LockAction::Unchanged
            } else {
                LockAction::Locked
            };
            LockEntry {
                kind: a.kind,
                name: a.name.clone(),
                pinned: a.pinned.clone(),
                action,
            }
        })
        .collect();
    LockReport::new(entries)
}

fn previous_has_same(previous: Option<&GrimoireLock>, artifact: &LockedArtifact) -> bool {
    let Some(prev) = previous else { return false };
    prev.iter_artifacts()
        .any(|p| p.kind == artifact.kind && p.name == artifact.name && p.pinned.eq_content(&artifact.pinned))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lock::grimoire_lock::LockMetadata;
    use crate::lock::lock_version::LockVersion;
    use crate::oci::pinned_identifier::PinnedIdentifier;
    use crate::oci::{Algorithm, ArtifactKind, Digest, Identifier};

    fn locked(name: &str, kind: ArtifactKind, byte: char) -> LockedArtifact {
        let id = Identifier::new_registry(name, "localhost:5000")
            .clone_with_digest(Digest::Sha256(std::iter::repeat_n(byte, 64).collect()));
        LockedArtifact::direct(name.to_string(), kind, PinnedIdentifier::try_from(id).unwrap())
    }

    fn lock_of(skills: Vec<LockedArtifact>) -> GrimoireLock {
        GrimoireLock {
            metadata: LockMetadata {
                lock_version: LockVersion::V1,
                declaration_hash_version: 1,
                declaration_hash: format!("sha256:{}", "d".repeat(64)),
                generated_by: "grim 0.1.0".to_string(),
                generated_at: "2026-01-01T00:00:00Z".to_string(),
            },
            skills,
            rules: vec![],
            agents: vec![],
        }
    }

    #[test]
    fn action_is_locked_without_previous() {
        let lock = lock_of(vec![locked("a", ArtifactKind::Skill, 'a')]);
        let r = build_report(
            &lock,
            None,
            &ResolvedScope {
                scope: crate::config::scope::ConfigScope::Project,
                set: Default::default(),
                options: Default::default(),
                config_path: Default::default(),
                lock_path: Default::default(),
                state_path: Default::default(),
                workspace: Default::default(),
            },
        );
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v[0]["action"], "locked");
        let _ = Algorithm::Sha256;
    }

    #[test]
    fn action_is_unchanged_when_previous_pins_same_content() {
        let lock = lock_of(vec![locked("a", ArtifactKind::Skill, 'a')]);
        let prev = lock_of(vec![locked("a", ArtifactKind::Skill, 'a')]);
        let scope = ResolvedScope {
            scope: crate::config::scope::ConfigScope::Project,
            set: Default::default(),
            options: Default::default(),
            config_path: Default::default(),
            lock_path: Default::default(),
            state_path: Default::default(),
            workspace: Default::default(),
        };
        let r = build_report(&lock, Some(&prev), &scope);
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v[0]["action"], "unchanged");
    }
}
