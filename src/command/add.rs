// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! `grim add [--kind K] [--name N] <ref>` — declare a skill/rule/bundle and
//! lock it.
//!
//! The reference is the only required argument. A short reference is
//! expanded against the effective default registry — precedence
//! `--registry` flag > `GRIM_DEFAULT_REGISTRY` > project config
//! `[options].default_registry` > global config; the persisted config/lock
//! always carry the fully-qualified name. The artifact **kind** is inferred
//! from the pulled manifest's OCI `artifactType` when `--kind` is omitted,
//! and the binding **name** defaults to the reference's last path segment
//! when `--name` is omitted.
//!
//! Edits the discovered config's `[skills]`/`[rules]`/`[bundles]` table
//! (re-serializing the parsed config is acceptable — minimal formatting
//! churn for a provisional milestone), then re-resolves just that entry
//! under the config flock: a partial relock when a previous lock exists, a
//! full resolve otherwise. The new lock is saved with `generated_at`
//! preservation for the untouched entries.

use std::sync::Arc;

use clap::Args;

use crate::api::add_report::AddReport;
use crate::cli::exit_code::ExitCode;
use crate::command::command_error::CommandError;
use crate::context::Context;
use crate::lock::file_lock::ConfigFileLock;
use crate::lock::lock_io;
use crate::oci::access::{OciAccess, Operation};
use crate::oci::{ArtifactKind, Identifier, PinnedIdentifier};
use crate::resolve::resolve_options::ResolveOptions;
use crate::resolve::resolver::{resolve_lock, resolve_lock_partial};

use super::scope_resolution;

/// `grim add` arguments.
#[derive(Debug, Args)]
pub struct AddArgs {
    /// The artifact reference (`registry/repo:tag` or `@digest`). A short
    /// name is expanded against the effective default registry.
    pub reference: String,

    /// The artifact kind (`skill`, `rule`, or `bundle`). Inferred from the
    /// published manifest's OCI `artifactType` when omitted.
    #[arg(long, short = 'k', value_parser = ["skill", "rule", "bundle"])]
    pub kind: Option<String>,

    /// The config binding name. Defaults to the reference's last path
    /// segment (e.g. `ghcr.io/acme/code-review` ⇒ `code-review`).
    #[arg(long, short = 'n')]
    pub name: Option<String>,

    /// Operate on the global scope instead of the discovered project.
    #[arg(long)]
    pub global: bool,

    /// Explicit project config path.
    #[arg(long)]
    pub config: Option<std::path::PathBuf>,
}

/// Run `grim add`.
///
/// # Errors
///
/// Config (78/79/74), invalid reference (65), or lock/resolve failures
/// propagate via the typed error chain.
pub async fn run(ctx: &Context, args: &AddArgs) -> anyhow::Result<(AddReport, ExitCode)> {
    let scope = super::grim(scope_resolution::resolve(ctx, args.global, args.config.as_deref()))?;

    // Hold the config flock for the read-modify-write + relock window.
    let _guard = match scope_resolution::lockable_config_path(&scope) {
        Some(path) => Some(super::grim(ConfigFileLock::try_acquire(&path))?),
        None => None,
    };

    // Expand the reference against the effective default registry —
    // precedence: --registry flag > GRIM_DEFAULT_REGISTRY > project config >
    // global config (the global config is consulted as the lowest-priority
    // fallback only when this is a project-scope run). The expanded
    // identifier is always fully-qualified, so the config and lock persist
    // the registry host explicitly — the default is a pure CLI-input
    // convenience.
    let global_default = super::global_config_default(ctx, scope.scope);
    let default_registry = super::resolve_default_registry(
        ctx,
        scope.options.default_registry.as_deref(),
        global_default.as_deref(),
    );
    let id = super::grim(parse_reference(&args.reference, default_registry.as_deref()))?;
    let id = if id.tag().is_none() && id.digest().is_none() {
        id.clone_with_tag("latest")
    } else {
        id
    };

    // The binding name defaults to the reference's last path segment.
    let name = args.name.clone().unwrap_or_else(|| id.name().to_string());

    let access: Arc<dyn OciAccess> = super::access_seam(ctx)?;

    // The kind: an explicit --kind wins; otherwise infer it from the
    // published manifest's OCI `artifactType` (the kind is persisted in the
    // OCI artifact type at release time).
    let kind = match args.kind.as_deref() {
        Some(k) => ArtifactKind::from_kind_str(k).unwrap_or(ArtifactKind::Rule),
        None => infer_kind(&access, &id).await?,
    };

    let mut set = scope.set.clone();
    match kind {
        ArtifactKind::Skill => {
            set.skills.insert(name.clone(), id.clone());
        }
        ArtifactKind::Rule => {
            set.rules.insert(name.clone(), id.clone());
        }
        ArtifactKind::Bundle => {
            set.bundles.insert(name.clone(), id.clone());
        }
    }
    set.invalidate_declaration_hash_cache();

    // Persist the edited config (re-serialize the parsed declaration).
    super::grim(write_config(&scope.config_path, &scope.options, &set))?;

    // Relock: a partial relock of just this entry when a previous lock
    // exists and is not stale; a full resolve otherwise (or when the
    // partial stale guard fires — caught and retried as a full resolve so
    // `add` always leaves a consistent lock).
    let previous = lock_io::load(&scope.lock_path).ok();
    // A bundle declaration expands into members whose names differ from the
    // bundle's binding name, so a partial relock keyed on the bundle name
    // cannot work — always do a full resolve for bundles.
    let new_lock = if kind == ArtifactKind::Bundle {
        super::grim(resolve_lock(&set, &access, scope.scope, &ResolveOptions::default()).await)?
    } else {
        match &previous {
            Some(prev) => {
                match resolve_lock_partial(
                    &set,
                    prev,
                    &access,
                    std::slice::from_ref(&name),
                    scope.scope,
                    &ResolveOptions::default(),
                )
                .await
                {
                    Ok(lock) => lock,
                    Err(e)
                        if matches!(
                            e.kind,
                            crate::resolve::resolve_error::ResolveErrorKind::StaleLock { .. }
                        ) =>
                    {
                        // The added entry made the predecessor stale; a full
                        // resolve is the correct recovery (every entry is
                        // declared, so this is consistent).
                        super::grim(resolve_lock(&set, &access, scope.scope, &ResolveOptions::default()).await)?
                    }
                    Err(e) => return Err(crate::error::Error::from(e).into()),
                }
            }
            None => super::grim(resolve_lock(&set, &access, scope.scope, &ResolveOptions::default()).await)?,
        }
    };
    super::grim(lock_io::save(&scope.lock_path, &new_lock, previous.as_ref()))?;

    // A bundle has no single pinned member to report; surface the bundle
    // reference itself. A skill/rule reports the digest it resolved to.
    let pinned = if kind == ArtifactKind::Bundle {
        id.to_string()
    } else {
        new_lock
            .skills
            .iter()
            .chain(new_lock.rules.iter())
            .find(|a| a.kind == kind && a.name == name)
            .map(|a| a.pinned.strip_advisory().to_string())
            .unwrap_or_else(|| id.to_string())
    };

    Ok((AddReport::new(kind, name, pinned), ExitCode::Success))
}

/// Parse `<ref>`, expanding a short identifier against `default_registry`
/// when one is configured.
pub(crate) fn parse_reference(
    reference: &str,
    default_registry: Option<&str>,
) -> Result<Identifier, crate::oci::identifier::error::IdentifierError> {
    match default_registry {
        Some(def) => Identifier::parse_with_default_registry(reference, def),
        None => Identifier::parse(reference),
    }
}

/// Infer the artifact kind from the published manifest's OCI `artifactType`
/// (falling back to the config descriptor's media type).
///
/// Resolves the reference to a digest (a pure `Query` — offline returns a
/// cache miss as `Ok(None)`), fetches the manifest, and reads the kind. A
/// reference that does not resolve, has no manifest, or carries no/unknown
/// kind annotation (a non-Grimoire image) yields
/// [`CommandError::KindInferenceFailed`] so the user can pass `--kind`.
///
/// # Errors
///
/// A registry/transport failure propagates with its own taxonomy;
/// inability to determine the kind is [`CommandError::KindInferenceFailed`].
async fn infer_kind(access: &Arc<dyn OciAccess>, id: &Identifier) -> anyhow::Result<ArtifactKind> {
    let not_inferable = || {
        crate::error::Error::from(CommandError::KindInferenceFailed {
            reference: id.to_string(),
        })
    };

    let digest = super::grim(access.resolve_digest(id, Operation::Query).await)?.ok_or_else(not_inferable)?;
    let pinned = PinnedIdentifier::try_from(id.clone_with_digest(digest)).map_err(|_| not_inferable())?;
    let manifest = super::grim(access.fetch_manifest(&pinned).await)?.ok_or_else(not_inferable)?;
    crate::oci::annotations::kind_from_manifest(&manifest).ok_or_else(|| not_inferable().into())
}

/// Re-serialize the declaration to `path` as the shared
/// `[options]`/`[bundles]`/`[skills]`/`[rules]` schema. Atomic via the
/// store primitive so a crash never truncates the config. The `[bundles]`
/// table is emitted only when at least one bundle is declared, so a
/// bundle-free config is byte-identical to one written before bundles
/// existed.
pub(crate) fn write_config(
    path: &std::path::Path,
    options: &crate::config::declaration::ConfigOptions,
    set: &crate::config::declaration::DesiredSet,
) -> Result<(), crate::config::config_error::ConfigError> {
    use std::fmt::Write as _;

    let mut out = String::new();
    if options.default_registry.is_some() || !options.clients.is_empty() {
        out.push_str("[options]\n");
        if let Some(r) = &options.default_registry {
            let _ = writeln!(out, "default_registry = \"{r}\"");
        }
        if !options.clients.is_empty() {
            let list = options
                .clients
                .iter()
                .map(|c| format!("\"{c}\""))
                .collect::<Vec<_>>()
                .join(", ");
            let _ = writeln!(out, "clients = [{list}]");
        }
        out.push('\n');
    }
    if !set.bundles.is_empty() {
        out.push_str("[bundles]\n");
        for (name, id) in &set.bundles {
            let _ = writeln!(out, "{name} = \"{id}\"");
        }
        out.push('\n');
    }
    out.push_str("[skills]\n");
    for (name, id) in &set.skills {
        let _ = writeln!(out, "{name} = \"{id}\"");
    }
    out.push_str("\n[rules]\n");
    for (name, id) in &set.rules {
        let _ = writeln!(out, "{name} = \"{id}\"");
    }

    crate::store::atomic_write::atomic_write(path, out.as_bytes()).map_err(|e| {
        crate::config::config_error::ConfigError::new(path, crate::config::config_error::ConfigErrorKind::Io(e))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::declaration::{ConfigOptions, DesiredSet};
    use crate::config::project_config::ProjectConfig;
    use std::collections::BTreeMap;

    #[test]
    fn write_config_round_trips_through_parser() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("grimoire.toml");
        let mut skills = BTreeMap::new();
        skills.insert(
            "code-review".to_string(),
            Identifier::parse("ghcr.io/acme/code-review:stable").unwrap(),
        );
        let mut rules = BTreeMap::new();
        rules.insert(
            "rust-style".to_string(),
            Identifier::parse("ghcr.io/acme/rust-style:v3").unwrap(),
        );
        let set = DesiredSet::from_parts(skills, rules);
        let opts = ConfigOptions {
            default_registry: Some("ghcr.io/acme".to_string()),
            clients: vec!["claude".to_string(), "opencode".to_string()],
        };
        write_config(&path, &opts, &set).unwrap();

        let body = std::fs::read_to_string(&path).unwrap();
        let cfg = ProjectConfig::from_toml_str(&body).expect("re-serialized config must parse");
        // The clients list round-trips as a TOML array.
        assert_eq!(cfg.options.clients, vec!["claude".to_string(), "opencode".to_string()]);
        assert_eq!(cfg.set.skills.len(), 1);
        assert_eq!(cfg.set.rules.len(), 1);
        assert_eq!(cfg.options.default_registry.as_deref(), Some("ghcr.io/acme"));
    }

    #[test]
    fn write_config_omits_options_when_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("grimoire.toml");
        let set = DesiredSet::from_parts(BTreeMap::new(), BTreeMap::new());
        write_config(&path, &ConfigOptions::default(), &set).unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(!body.contains("[options]"));
        assert!(ProjectConfig::from_toml_str(&body).is_ok());
    }
}
