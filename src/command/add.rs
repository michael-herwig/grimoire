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

    /// The artifact kind (`skill`, `rule`, `agent`, or `bundle`). Inferred from
    /// the published manifest's OCI `artifactType` when omitted.
    #[arg(long, short = 'k', value_parser = ["skill", "rule", "agent", "bundle"])]
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

    // Resolve the reference against the scope's registry set: a qualified
    // `alias/repo` substitutes that alias's url, an explicit registry parses
    // as-is, and a bare short id expands against the primary registry
    // (precedence: --registry flag > GRIM_DEFAULT_REGISTRY > the declared
    // `[[registries]]` / `[options].default_registry` > global config > the
    // built-in fallback). The expanded identifier is always fully-qualified,
    // so the config and lock persist the registry host explicitly.
    let registries = super::registries_for_scope(ctx, &scope);
    let id = super::grim(crate::config::resolve_reference(&args.reference, &registries))?;
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
    // OCI artifact type at release time). The value_parser above constrains
    // the string to a known kind, so from_kind_str never returns None here.
    let kind = match args.kind.as_deref() {
        Some(k) => ArtifactKind::from_kind_str(k).unwrap_or(ArtifactKind::Rule),
        None => infer_kind(&access, &id).await?,
    };

    let mut set = scope.set.clone();
    declare(&mut set, kind, name.clone(), id.clone());

    // Persist the edited config (re-serialize the parsed declaration).
    super::grim(write_config(
        &scope.config_path,
        &scope.options,
        &scope.registries,
        &set,
    ))?;

    // Relock: a partial relock of just this entry when a previous lock
    // exists and is not stale; a full resolve otherwise (or when the
    // partial stale guard fires — caught and retried as a full resolve so
    // `add` always leaves a consistent lock).
    let previous = lock_io::load(&scope.lock_path).ok();
    let new_lock = super::grim(relock_declared(&set, previous.as_ref(), kind, &name, &access, scope.scope).await)?;
    super::grim(lock_io::save(&scope.lock_path, &new_lock, previous.as_ref()))?;

    // A bundle has no single pinned member to report; surface the bundle
    // reference itself. A skill/rule/agent reports the digest it resolved to.
    let pinned = if kind == ArtifactKind::Bundle {
        id.to_string()
    } else {
        new_lock
            .iter_artifacts()
            .find(|a| a.kind == kind && a.name == name)
            .map(|a| a.pinned.strip_advisory().to_string())
            .unwrap_or_else(|| id.to_string())
    };

    Ok((AddReport::new(kind, name, pinned), ExitCode::Success))
}

/// Declare `name = id` in the kind's config table
/// (`[skills]`/`[rules]`/`[agents]`/`[bundles]`) and invalidate the
/// declaration-hash cache. The kind-dispatch seam shared by `grim add`
/// and the TUI install action so a bundle can never be coerced into a
/// skill/rule/agent table.
pub(crate) fn declare(
    set: &mut crate::config::declaration::DesiredSet,
    kind: ArtifactKind,
    name: String,
    id: Identifier,
) {
    match kind {
        ArtifactKind::Skill => {
            set.skills.insert(name, id);
        }
        ArtifactKind::Rule => {
            set.rules.insert(name, id);
        }
        ArtifactKind::Agent => {
            set.agents.insert(name, id);
        }
        ArtifactKind::Bundle => {
            set.bundles.insert(name, id);
        }
    }
    set.invalidate_declaration_hash_cache();
}

/// Re-lock after declaring `(kind, name)`: a bundle always full-resolves
/// (its members' names differ from the bundle's binding name, so a partial
/// relock keyed on the bundle name cannot work); a skill/rule goes through
/// [`relock_entry`]. Shared by `grim add` and the TUI install/update
/// action so both declare-and-lock through one seam.
///
/// # Errors
///
/// Any [`ResolveError`](crate::resolve::resolve_error::ResolveError) from
/// the underlying resolve (tag-not-found, auth, registry-unreachable,
/// timeout, bundle expansion failures).
pub(crate) async fn relock_declared(
    set: &crate::config::declaration::DesiredSet,
    previous: Option<&crate::lock::grimoire_lock::GrimoireLock>,
    kind: ArtifactKind,
    name: &str,
    access: &Arc<dyn OciAccess>,
    scope: crate::config::scope::ConfigScope,
) -> Result<crate::lock::grimoire_lock::GrimoireLock, crate::resolve::resolve_error::ResolveError> {
    if kind == ArtifactKind::Bundle {
        resolve_lock(set, access, scope, &ResolveOptions::default()).await
    } else {
        relock_entry(set, previous, name, access, scope).await
    }
}

/// Re-lock a single declared skill/rule entry: a partial relock of just
/// `name` when a previous lock exists, a full resolve otherwise — or when
/// the partial stale guard fires, in which case the full resolve is the
/// correct recovery (every entry is declared, so the result is
/// consistent). Bundle declarations go through [`relock_declared`].
///
/// # Errors
///
/// Any [`ResolveError`] other than the recovered stale-lock guard
/// (tag-not-found, auth, registry-unreachable, timeout).
pub(crate) async fn relock_entry(
    set: &crate::config::declaration::DesiredSet,
    previous: Option<&crate::lock::grimoire_lock::GrimoireLock>,
    name: &str,
    access: &Arc<dyn OciAccess>,
    scope: crate::config::scope::ConfigScope,
) -> Result<crate::lock::grimoire_lock::GrimoireLock, crate::resolve::resolve_error::ResolveError> {
    let names = [name.to_string()];
    match previous {
        Some(prev) => {
            match resolve_lock_partial(set, prev, access, &names, scope, &ResolveOptions::default()).await {
                Ok(lock) => Ok(lock),
                Err(e)
                    if matches!(
                        e.kind,
                        crate::resolve::resolve_error::ResolveErrorKind::StaleLock { .. }
                    ) =>
                {
                    // The edited entry made the predecessor stale; resolve
                    // everything fresh.
                    resolve_lock(set, access, scope, &ResolveOptions::default()).await
                }
                Err(e) => Err(e),
            }
        }
        None => resolve_lock(set, access, scope, &ResolveOptions::default()).await,
    }
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
    registries: &[crate::config::declaration::RegistryConfig],
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
    // Preserve declared `[[registries]]` verbatim — re-serializing the
    // declaration must never silently drop a user's registry array.
    for rc in registries {
        out.push_str("[[registries]]\n");
        if let Some(alias) = &rc.alias {
            let _ = writeln!(out, "alias = \"{alias}\"");
        }
        let _ = writeln!(out, "url = \"{}\"", rc.url);
        if rc.default {
            let _ = writeln!(out, "default = true");
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
    if !set.agents.is_empty() {
        out.push_str("\n[agents]\n");
        for (name, id) in &set.agents {
            let _ = writeln!(out, "{name} = \"{id}\"");
        }
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
        write_config(&path, &opts, &[], &set).unwrap();

        let body = std::fs::read_to_string(&path).unwrap();
        let cfg = ProjectConfig::from_toml_str(&body).expect("re-serialized config must parse");
        // The clients list round-trips as a TOML array.
        assert_eq!(cfg.options.clients, vec!["claude".to_string(), "opencode".to_string()]);
        assert_eq!(cfg.set.skills.len(), 1);
        assert_eq!(cfg.set.rules.len(), 1);
        assert_eq!(cfg.options.default_registry.as_deref(), Some("ghcr.io/acme"));
    }

    #[test]
    fn write_config_preserves_registries_array() {
        // Regression guard: re-serializing a declaration must never drop a
        // user's `[[registries]]` table (an `add`/`remove`/TUI edit would
        // otherwise silently erase multi-registry config).
        use crate::config::declaration::RegistryConfig;
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("grimoire.toml");
        let set = DesiredSet::from_parts(BTreeMap::new(), BTreeMap::new());
        let registries = vec![
            RegistryConfig {
                alias: Some("acme".to_string()),
                url: "ghcr.io/acme".to_string(),
                default: true,
            },
            RegistryConfig {
                alias: None,
                url: "registry.corp/team".to_string(),
                default: false,
            },
        ];
        write_config(&path, &ConfigOptions::default(), &registries, &set).unwrap();

        let body = std::fs::read_to_string(&path).unwrap();
        let cfg = ProjectConfig::from_toml_str(&body).expect("re-serialized config must parse");
        assert_eq!(cfg.registries, registries, "registries must round-trip verbatim");
    }

    #[test]
    fn write_config_omits_options_when_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("grimoire.toml");
        let set = DesiredSet::from_parts(BTreeMap::new(), BTreeMap::new());
        write_config(&path, &ConfigOptions::default(), &[], &set).unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(!body.contains("[options]"));
        assert!(ProjectConfig::from_toml_str(&body).is_ok());
    }
}
