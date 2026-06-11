// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! `grim remove <kind> <name>` — undeclare a skill/rule.
//!
//! Drops the entry from the discovered config's `[skills]`/`[rules]`
//! table and from the lock (re-saved). Materialized files are
//! **intentionally left on disk** this milestone — removing installed
//! client files is deferred (a future `grim clean`); `remove` only
//! affects the declaration and the lock so the change is reversible.

use clap::Args;

use crate::api::remove_report::{RemoveReport, RemoveStatus};
use crate::cli::exit_code::ExitCode;
use crate::context::Context;
use crate::lock::file_lock::ConfigFileLock;
use crate::lock::grimoire_lock::GrimoireLock;
use crate::lock::lock_io;
use crate::lock::locked_artifact::LockedArtifact;
use crate::oci::ArtifactKind;

use super::add::write_config;
use super::scope_resolution;

/// `grim remove` arguments.
#[derive(Debug, Args)]
pub struct RemoveArgs {
    /// `skill`, `rule`, `agent`, or `bundle`.
    #[arg(value_parser = ["skill", "rule", "agent", "bundle"])]
    pub kind: String,

    /// The config binding name to remove.
    pub name: String,

    /// Operate on the global scope instead of the discovered project.
    #[arg(long)]
    pub global: bool,

    /// Explicit project config path.
    #[arg(long)]
    pub config: Option<std::path::PathBuf>,
}

/// Run `grim remove`.
///
/// # Errors
///
/// Config (78/79/74) or lock save (74) failures propagate via the typed
/// error chain. An absent entry is reported, not an error.
pub async fn run(ctx: &Context, args: &RemoveArgs) -> anyhow::Result<(RemoveReport, ExitCode)> {
    // The value_parser above constrains the string to known kinds.
    let kind = match args.kind.as_str() {
        "skill" => ArtifactKind::Skill,
        "agent" => ArtifactKind::Agent,
        "bundle" => ArtifactKind::Bundle,
        _ => ArtifactKind::Rule,
    };

    let scope = super::grim(scope_resolution::resolve(ctx, args.global, args.config.as_deref()))?;

    let _guard = match scope_resolution::lockable_config_path(&scope) {
        Some(path) => Some(super::grim(ConfigFileLock::try_acquire(&path))?),
        None => None,
    };

    let mut set = scope.set.clone();
    // For a bundle, capture its (repo, tag) before removal so only the lock
    // members *this* bundle contributed are dropped — two bundles sharing a
    // repository at different tags must not evict each other's members.
    let removed_bundle = if kind == ArtifactKind::Bundle {
        set.bundles
            .get(&args.name)
            .map(|id| (id.registry_repository(), id.tag_or_latest().to_string()))
    } else {
        None
    };
    let removed = match kind {
        ArtifactKind::Skill => set.skills.remove(&args.name).is_some(),
        ArtifactKind::Rule => set.rules.remove(&args.name).is_some(),
        ArtifactKind::Agent => set.agents.remove(&args.name).is_some(),
        ArtifactKind::Bundle => set.bundles.remove(&args.name).is_some(),
    };

    if !removed {
        return Ok((
            RemoveReport::new(kind, args.name.clone(), RemoveStatus::Absent),
            ExitCode::Success,
        ));
    }
    set.invalidate_declaration_hash_cache();

    // Persist the edited config, then drop the entry from the lock and
    // re-stamp its declaration hash so a later `install` sees a fresh lock.
    super::grim(write_config(&scope.config_path, &scope.options, &set))?;

    if let Ok(previous) = lock_io::load(&scope.lock_path) {
        let new_lock = drop_from_lock(&previous, kind, &args.name, removed_bundle.as_ref(), &set);
        super::grim(lock_io::save(&scope.lock_path, &new_lock, Some(&previous)))?;
    }

    Ok((
        RemoveReport::new(kind, args.name.clone(), RemoveStatus::Removed),
        ExitCode::Success,
    ))
}

/// Return `previous` with `(kind, name)` dropped and the declaration hash
/// re-stamped from the edited set so the lock stays consistent. For a
/// bundle, `bundle` carries the declared `(registry/repo, tag)` so exactly
/// the members *this* bundle contributed are evicted. Shared with the
/// `uninstall` seam ([`super::uninstall::undeclare_and_unlock`]).
pub(crate) fn drop_from_lock(
    previous: &GrimoireLock,
    kind: ArtifactKind,
    name: &str,
    bundle: Option<&(String, String)>,
    set: &crate::config::declaration::DesiredSet,
) -> GrimoireLock {
    let mut lock = previous.clone();
    match kind {
        ArtifactKind::Skill => lock.skills.retain(|a| a.name != name),
        ArtifactKind::Rule => lock.rules.retain(|a| a.name != name),
        ArtifactKind::Agent => lock.agents.retain(|a| a.name != name),
        ArtifactKind::Bundle => {
            // Removing a bundle drops every locked member it contributed,
            // identified by BOTH the provenance repo and tag so a sibling
            // bundle at the same repository (different tag) is untouched.
            let keep = |a: &LockedArtifact| match bundle {
                Some((repo, tag)) => {
                    !(a.bundle.as_deref() == Some(repo.as_str()) && a.bundle_tag.as_deref() == Some(tag.as_str()))
                }
                None => true,
            };
            lock.skills.retain(&keep);
            lock.rules.retain(&keep);
            lock.agents.retain(&keep);
        }
    }
    lock.metadata.declaration_hash = set.declaration_hash_cached().to_string();
    lock
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::declaration::DesiredSet;
    use crate::lock::grimoire_lock::LockMetadata;
    use crate::lock::lock_version::LockVersion;
    use crate::lock::locked_artifact::LockedArtifact;
    use crate::oci::pinned_identifier::PinnedIdentifier;
    use crate::oci::{Digest, Identifier};
    use std::collections::BTreeMap;

    fn locked(name: &str) -> LockedArtifact {
        let id = Identifier::new_registry(name, "localhost:5000").clone_with_digest(Digest::Sha256("a".repeat(64)));
        LockedArtifact::direct(
            name.to_string(),
            ArtifactKind::Skill,
            PinnedIdentifier::try_from(id).unwrap(),
        )
    }

    #[test]
    fn drop_from_lock_removes_only_named_entry() {
        let prev = GrimoireLock {
            metadata: LockMetadata {
                lock_version: LockVersion::V1,
                declaration_hash_version: 1,
                declaration_hash: format!("sha256:{}", "d".repeat(64)),
                generated_by: "grim 0.1.0".to_string(),
                generated_at: "2026-01-01T00:00:00Z".to_string(),
            },
            skills: vec![locked("a"), locked("b")],
            rules: vec![],
            agents: vec![],
        };
        let set = DesiredSet::from_parts(BTreeMap::new(), BTreeMap::new());
        let after = drop_from_lock(&prev, ArtifactKind::Skill, "a", None, &set);
        assert_eq!(after.skills.len(), 1);
        assert_eq!(after.skills[0].name, "b");
        assert_eq!(after.metadata.declaration_hash, set.declaration_hash_cached());
    }
}
