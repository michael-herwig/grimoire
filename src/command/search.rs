// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! `grim search` — query the registry catalog.
//!
//! Loads (or refreshes) the cached [`Catalog`] for the resolved registry,
//! filters entries with the shared [`SearchQuery`] matcher (whitespace-split
//! AND of terms over kind / repo / summary / description / keywords, plus
//! bare kind keywords — `skill`/`rule`/`bundle` and plurals — acting as kind
//! filters; an empty query lists everything), and annotates each match with
//! its install status derived from the discovered scope's lock + install-state
//! via the shared [`derive_badge`] helper (the same derivation `grim status`
//! uses — not duplicated here).
//!
//! State is data: `search` always exits 0, even with no results. Offline
//! degrades — the catalog layer serves whatever is cached and never errors
//! on a network-absent run.

use clap::Args;

use crate::api::search_report::{SearchEntry, SearchReport};
use crate::catalog::SearchQuery;
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
    /// Search terms, whitespace-split and ANDed: each term substring-matches
    /// (case-insensitive) any of kind / repo / summary / description /
    /// keywords. A bare kind keyword (`skill`/`rule`/`bundle`, singular or
    /// plural) filters by kind instead of matching as text. Empty ⇒ list the
    /// whole catalog.
    pub query: Option<String>,

    /// Force a catalog rebuild even if the cache is fresh.
    #[arg(long)]
    pub refresh: bool,

    /// Registry to search. Precedence (highest first): this flag (or the
    /// global `--registry`), then `GRIM_DEFAULT_REGISTRY`, then project config
    /// `default_registry`, then global config.
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
    // Parse the raw query once: the in-memory matcher reuses it per row.
    let parsed = SearchQuery::parse(args.query.as_deref().unwrap_or(""));
    // The build's repository-name prefilter is derived from the parsed query,
    // not the raw string: a single text term scopes the build cheaply; a
    // multi-term or kind-keyword query yields an empty prefilter (no single
    // substring can AND across terms) and builds the capped browse window,
    // then narrows in memory below.
    let catalog = super::grim(
        Catalog::load_or_refresh(
            &catalog_path,
            &registry,
            parsed.prefilter_term(),
            &access,
            ctx.offline(),
            args.refresh,
        )
        .await,
    )?;

    // Scope badges are best-effort: `search` is not scope-bound, so a
    // missing project config just means "nothing installed" rather than a
    // hard failure.
    let (lock, state) = load_scope_best_effort(ctx, args);

    // The catalog was prefiltered by repo *name*; re-filter in memory so a
    // summary/description/keyword-only match (and multi-term AND / kind
    // filters) still narrows the built set correctly.
    let mut entries: Vec<SearchEntry> = catalog
        .entries()
        .filter(|e| e.matches(&parsed))
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

/// Resolve the registry to search via the centralized precedence:
/// `--registry` flag > `GRIM_DEFAULT_REGISTRY` > the resolved scope's
/// project config `[options].default_registry` > the global config
/// (consulted as the lowest-priority fallback only for a non-global run).
/// A miss is a classifiable config error.
fn resolve_registry(ctx: &Context, args: &SearchArgs) -> anyhow::Result<String> {
    // `--registry` on the command is the top precedence; fold it into the
    // helper as the flag-equivalent so the single helper owns the order.
    if let Some(r) = &args.registry {
        return Ok(r.clone());
    }
    let project_default = scope_resolution::resolve(ctx, args.global, args.config.as_deref())
        .ok()
        .and_then(|scope| scope.options.default_registry);
    let global_default = global_config_default(ctx, args.global);
    super::resolve_default_registry(ctx, project_default.as_deref(), global_default.as_deref())
        .ok_or_else(|| crate::error::Error::from(crate::command::command_error::CommandError::NoRegistry).into())
}

/// The global config's `[options].default_registry`, loaded best-effort as
/// the lowest-priority registry fallback for a non-global run (a global run
/// already resolved the global config as its active scope). Load failures
/// degrade to `None`.
fn global_config_default(ctx: &Context, global: bool) -> Option<String> {
    if global {
        return None;
    }
    crate::config::global_config::GlobalConfig::load(&ctx.paths().global_config())
        .ok()
        .and_then(|cfg| cfg.options.default_registry)
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

    #[test]
    fn flag_beats_project_config_default_registry() {
        // A project config declaring `default_registry` is the lowest CLI
        // tier; the `--registry` flag (here folded through the context, the
        // same precedence the `--registry` arg uses) must win over it. This
        // pins the reordered chain: flag/env > project config > global config.
        let tmp = tempfile::tempdir().unwrap();
        let cfg = tmp.path().join("grimoire.toml");
        std::fs::write(&cfg, "[options]\ndefault_registry = \"config.example\"\n").unwrap();

        let mut o = opts();
        o.registry = Some("flag.example".to_string());
        let ctx = Context::new(&o);
        let mut a = args();
        a.config = Some(cfg);
        assert_eq!(
            resolve_registry(&ctx, &a).unwrap(),
            "flag.example",
            "the registry flag/env tier wins over the project config default"
        );
    }
}
