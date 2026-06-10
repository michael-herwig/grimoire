// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! `grim search` — query the registry catalog.
//!
//! Loads (or refreshes) the cached [`Catalog`] for the resolved registry,
//! filters entries whose repo / description / keywords contain the
//! case-insensitive query (an empty query lists everything), and annotates
//! each match with its install status derived from the discovered scope's
//! lock + install-state via the shared [`derive_badge`] helper (the same
//! derivation `grim status` uses — not duplicated here).
//!
//! State is data: `search` always exits 0, even with no results. Offline
//! degrades — the catalog layer serves whatever is cached and never errors
//! on a network-absent run.

use clap::Args;

use crate::api::search_report::{SearchEntry, SearchReport};
use crate::catalog::registry_catalog::Catalog;
use crate::cli::exit_code::ExitCode;
use crate::context::Context;
use crate::install::install_state::InstallState;
use crate::install::status_badge::derive_badge;
use crate::lock::grimoire_lock::GrimoireLock;
use crate::lock::lock_io;

use super::scope_resolution;

/// `grim search` arguments.
#[derive(Debug, Args)]
pub struct SearchArgs {
    /// Case-insensitive substring to match against repo / description /
    /// keywords. Empty ⇒ list the whole catalog.
    pub query: Option<String>,

    /// Force a catalog rebuild even if the cache is fresh.
    #[arg(long)]
    pub refresh: bool,

    /// Registry to search. Defaults to `--registry` / the config
    /// `default_registry` option / `GRIM_DEFAULT_REGISTRY`.
    #[arg(long)]
    pub registry: Option<String>,

    /// Search the global scope's lock/state for badges instead of the
    /// discovered project.
    #[arg(long)]
    pub global: bool,

    /// Explicit project config path (for scope badge derivation).
    #[arg(long)]
    pub config: Option<std::path::PathBuf>,
}

/// Run `grim search`.
///
/// # Errors
///
/// A catalog cache parse / version failure, or a genuine registry
/// transport/auth failure during an online rebuild. A missing registry
/// (none resolvable) is a config error (78). Offline never errors.
pub async fn run(ctx: &Context, args: &SearchArgs) -> anyhow::Result<(SearchReport, ExitCode)> {
    let registry = resolve_registry(ctx, args)?;

    let access = super::access_seam(ctx)?;
    let catalog_path = ctx.paths().catalog_file();
    let query = args.query.as_deref().unwrap_or("");
    // The query doubles as the catalog's repository-name prefilter so a
    // targeted search does not enumerate a (potentially huge) registry.
    let catalog = super::grim(
        Catalog::load_or_refresh(&catalog_path, &registry, query, &access, ctx.offline(), args.refresh).await,
    )?;

    // Scope badges are best-effort: `search` is not scope-bound, so a
    // missing project config just means "nothing installed" rather than a
    // hard failure.
    let (lock, state) = load_scope_best_effort(ctx, args);

    // The catalog was prefiltered by repo *name*; re-filter in memory so a
    // description/keyword-only match still surfaces among the built set.
    let mut entries: Vec<SearchEntry> = catalog
        .entries()
        .filter(|e| e.matches(query))
        .map(|e| SearchEntry {
            kind: e.kind.clone(),
            repo: e.repo(),
            summary: e.summary.clone(),
            description: e.description.clone(),
            latest_tag: e.latest_tag.clone(),
            version: e.version.clone(),
            status: derive_badge(&e.registry, &e.repository, lock.as_ref(), &state),
        })
        .collect();
    entries.sort_by(|a, b| a.repo.cmp(&b.repo));

    Ok((SearchReport::new(entries), ExitCode::Success))
}

/// Resolve the registry to search: `--registry` wins, then the config
/// `default_registry` option, then the context default
/// (`GRIM_DEFAULT_REGISTRY`).
fn resolve_registry(ctx: &Context, args: &SearchArgs) -> anyhow::Result<String> {
    if let Some(r) = &args.registry {
        return Ok(r.clone());
    }
    // The config `default_registry` option, when a scope resolves.
    if let Ok(scope) = scope_resolution::resolve(ctx, args.global, args.config.as_deref())
        && let Some(r) = scope.options.default_registry
    {
        return Ok(r);
    }
    if let Some(r) = ctx.default_registry() {
        return Ok(r.to_string());
    }
    Err(crate::error::Error::from(crate::command::command_error::CommandError::NoRegistry).into())
}

/// Load the scope's lock + install-state for badge derivation, degrading
/// to an empty state when no scope resolves or the files are
/// absent/corrupt (badges are advisory, never fail the search).
fn load_scope_best_effort(ctx: &Context, args: &SearchArgs) -> (Option<GrimoireLock>, InstallState) {
    let Ok(scope) = scope_resolution::resolve(ctx, args.global, args.config.as_deref()) else {
        return (None, InstallState::empty(std::path::Path::new("")));
    };
    let lock = lock_io::load(&scope.lock_path).ok();
    let state = InstallState::load(&scope.state_path).unwrap_or_else(|_| InstallState::empty(&scope.state_path));
    (lock, state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::options::{GlobalOptions, OutputFormat};

    fn opts() -> GlobalOptions {
        GlobalOptions {
            format: OutputFormat::Plain,
            offline: false,
            log_level: None,
            config: None,
            global: false,
            registry: None,
        }
    }

    fn args() -> SearchArgs {
        SearchArgs {
            query: None,
            refresh: false,
            registry: None,
            global: false,
            config: None,
        }
    }

    #[test]
    fn explicit_registry_flag_wins() {
        let ctx = Context::new(&opts());
        let mut a = args();
        a.registry = Some("ghcr.io".to_string());
        assert_eq!(resolve_registry(&ctx, &a).unwrap(), "ghcr.io");
    }

    #[test]
    fn no_registry_anywhere_is_config_error() {
        // No --registry, no GRIM_DEFAULT_REGISTRY in the test env, no
        // project config ⇒ NoRegistry, classified as a config error.
        let ctx = Context::new(&opts());
        let a = args();
        let err = resolve_registry(&ctx, &a).expect_err("no registry resolvable");
        assert_eq!(crate::error::classify_error(&err), ExitCode::ConfigError);
    }

    #[test]
    fn context_default_registry_used_as_fallback() {
        let mut o = opts();
        o.registry = Some("localhost:5000".to_string());
        let ctx = Context::new(&o);
        let a = args();
        assert_eq!(resolve_registry(&ctx, &a).unwrap(), "localhost:5000");
    }
}
