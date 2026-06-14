// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! `grim search` — query the registry catalog.
//!
//! Browses every configured registry through the shared
//! [`crate::catalog::load_catalog`] seam (the one `search` / `tui` / `mcp`
//! share): each registry's cached catalog is loaded or coordinately
//! refreshed, filtered with the [`SearchQuery`] matcher (whitespace-split
//! AND of terms over kind / repo / summary / description / keywords, plus
//! bare kind keywords — `skill`/`rule`/`bundle` and plurals — acting as kind
//! filters; an empty query lists everything), and badged against the scope's
//! lock + install-state. An explicit `--registry` collapses the browse set
//! to exactly that registry; otherwise the declared `[[registries]]` (or the
//! single default) are all browsed and flattened into one table.
//!
//! State is data: `search` always exits 0, even with no results. Offline
//! degrades — the catalog layer serves whatever is cached and never errors
//! on a network-absent run.

use clap::Args;

use crate::api::search_report::{SearchEntry, SearchReport};
use crate::catalog::{BadgeContext, SearchQuery};
use crate::cli::exit_code::ExitCode;
use crate::context::Context;
use crate::install::install_state::InstallState;
use crate::install::path_anchor::AnchorRoots;
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
/// transport/auth failure during an online rebuild. Offline never errors.
/// A registry always resolves (the built-in fallback is the last tier).
pub async fn run(ctx: &Context, args: &SearchArgs) -> anyhow::Result<(SearchReport, ExitCode)> {
    let access = super::access_seam(ctx)?;
    // Parse the raw query once for the truncation hint (the service applies
    // the same matcher per registry).
    let parsed = SearchQuery::parse(args.query.as_deref().unwrap_or(""));

    // Resolve the scope's registry set + the best-effort badge inputs once,
    // then browse every configured registry through the shared catalog
    // service (the single seam `search`/`tui`/`mcp` share). A registry given
    // via `--registry` collapses the set to exactly that registry.
    let (registries, lock, state, roots) = resolve_scope(ctx, args);
    let badges = BadgeContext {
        lock: lock.as_ref(),
        state: &state,
        roots: &roots,
    };
    let results = super::grim(
        crate::catalog::load_catalog(
            &ctx.paths(),
            &registries,
            args.query.as_deref().unwrap_or(""),
            &access,
            &badges,
            ctx.offline(),
            args.refresh,
        )
        .await,
    )?;

    // A non-empty query against a build that hit the repository cap may be
    // missing matches past the window. Surface it so a short or empty result
    // set is not read as exhaustive. (An empty query is an explicit browse
    // and the cap is the documented cut-line — no warning.)
    if results.any_truncated() && !parsed.is_empty() {
        tracing::warn!(
            "catalog listing capped at {} repositories; results may be incomplete — narrow the query or use a more specific term",
            crate::catalog::registry_catalog::MAX_CATALOG_REPOS
        );
    }

    // Flatten the registry groups into the flat search table (sorted by
    // `registry/repository`).
    let entries: Vec<SearchEntry> = results
        .into_flat_rows()
        .into_iter()
        .map(|r| SearchEntry {
            repo: r.repo(),
            kind: r.kind,
            summary: r.summary,
            description: r.description,
            repository: r.repository_url,
            latest_tag: r.latest_tag,
            version: r.version,
            status: r.badge,
        })
        .collect();

    Ok((SearchReport::new(entries), ExitCode::Success))
}

/// Resolve the registry browse set and best-effort badge inputs for the
/// search. The registry set spans every configured `[[registries]]` (or the
/// single default), so `grim search` browses all of them at once; an
/// explicit `--registry` collapses the set to exactly that registry. Badge
/// derivation is best-effort — a missing project config just means "nothing
/// installed" rather than a hard failure.
fn resolve_scope(
    ctx: &Context,
    args: &SearchArgs,
) -> (
    Vec<crate::config::ResolvedRegistry>,
    Option<GrimoireLock>,
    InstallState,
    AnchorRoots,
) {
    // An explicit `--registry` on the command collapses the browse set to
    // exactly that registry (historical single-registry `--registry`
    // behavior), independent of any `[[registries]]` declared in config.
    if let Some(r) = &args.registry {
        let registries = vec![crate::config::ResolvedRegistry {
            url: r.clone(),
            alias: None,
            is_default: true,
        }];
        let (lock, state, roots) = load_badges_best_effort(ctx, args);
        return (registries, lock, state, roots);
    }

    let Ok(scope) = scope_resolution::resolve(ctx, args.global, args.config.as_deref()) else {
        // No scope resolves: browse the env/flag/fallback registry (no
        // config tiers) with empty badge inputs.
        let registries =
            crate::config::resolve_registries(ctx.default_registry(), &[], None, &[], None, super::FALLBACK_REGISTRY);
        let roots = AnchorRoots::resolve(std::path::PathBuf::new(), ctx);
        return (registries, None, InstallState::empty(std::path::Path::new("")), roots);
    };
    let registries = super::registries_for_scope(ctx, &scope);
    let lock = lock_io::load(&scope.lock_path).ok();
    let state = scope_resolution::load_state(&scope).unwrap_or_else(|_| InstallState::empty(&scope.state_path));
    (registries, lock, state, scope.roots)
}

/// Load the scope's lock + install-state + anchor roots for badge
/// derivation, degrading to an empty state when no scope resolves or the
/// files are absent/corrupt (badges are advisory, never fail the search).
fn load_badges_best_effort(ctx: &Context, args: &SearchArgs) -> (Option<GrimoireLock>, InstallState, AnchorRoots) {
    let Ok(scope) = scope_resolution::resolve(ctx, args.global, args.config.as_deref()) else {
        let roots = AnchorRoots::resolve(std::path::PathBuf::new(), ctx);
        return (None, InstallState::empty(std::path::Path::new("")), roots);
    };
    let lock = lock_io::load(&scope.lock_path).ok();
    let state = scope_resolution::load_state(&scope).unwrap_or_else(|_| InstallState::empty(&scope.state_path));
    (lock, state, scope.roots)
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
    fn explicit_registry_collapses_browse_set() {
        // `--registry` on the command collapses the browse set to exactly
        // that registry (historical single-registry behavior), regardless of
        // any configured `[[registries]]`. Hermetic so the developer's
        // environment cannot interpose.
        let tmp = tempfile::tempdir().unwrap();
        let ctx = Context::hermetic(tmp.path().to_path_buf());
        let mut a = args();
        a.registry = Some("ghcr.io".to_string());
        let (registries, ..) = resolve_scope(&ctx, &a);
        assert_eq!(registries.len(), 1);
        assert_eq!(registries[0].url, "ghcr.io");
    }

    #[test]
    fn no_registry_anywhere_browses_builtin_fallback() {
        // No --registry, no env, no config default anywhere ⇒ the built-in
        // fallback registry is the sole browse target (never an error).
        let tmp = tempfile::tempdir().unwrap();
        let cfg = tmp.path().join("grimoire.toml");
        std::fs::write(&cfg, "[options]\n").unwrap();
        let ctx = Context::hermetic(tmp.path().to_path_buf());
        let mut a = args();
        a.config = Some(cfg);
        let (registries, ..) = resolve_scope(&ctx, &a);
        assert_eq!(registries.len(), 1);
        assert_eq!(registries[0].url, crate::command::FALLBACK_REGISTRY);
    }

    #[test]
    fn declared_registries_become_the_browse_set() {
        // A project config declaring `[[registries]]` browses all of them.
        let tmp = tempfile::tempdir().unwrap();
        let cfg = tmp.path().join("grimoire.toml");
        std::fs::write(
            &cfg,
            "[[registries]]\nalias = \"acme\"\nurl = \"ghcr.io/acme\"\n\n[[registries]]\nurl = \"registry.corp/team\"\n",
        )
        .unwrap();
        let ctx = Context::hermetic(tmp.path().to_path_buf());
        let mut a = args();
        a.config = Some(cfg);
        let (registries, ..) = resolve_scope(&ctx, &a);
        let urls: Vec<&str> = registries.iter().map(|r| r.url.as_str()).collect();
        assert_eq!(urls, vec!["ghcr.io/acme", "registry.corp/team"]);
    }
}
