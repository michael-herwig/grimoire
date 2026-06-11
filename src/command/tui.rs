// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! `grim tui` — the interactive catalog browser entrypoint.
//!
//! This command diverges into a full-screen terminal session rather than
//! emitting a structured report, so (per subsystem-cli-api.md "Commands
//! That Exec a Child Process") it is exempt from the `Printable` /
//! `api/` path: any CLI-shaped message lives here. If stdout is not a TTY
//! it prints a clear message and exits 0 *without* attempting raw mode —
//! a non-interactive caller (pipe, CI, `</dev/null`) must never have its
//! terminal mangled.

use std::io::IsTerminal;

use clap::Args;

use crate::cli::exit_code::ExitCode;
use crate::config::scope::ConfigScope;
use crate::context::Context;
use crate::install::client_target::ClientTarget;
use crate::tui::app::{self, ScopeSwap, TuiContext};

use super::scope_resolution;

/// Human label for a scope (shown in the TUI title).
fn scope_label(scope: ConfigScope) -> &'static str {
    match scope {
        ConfigScope::Project => "project",
        ConfigScope::Global => "global",
    }
}

/// `grim tui` arguments.
#[derive(Debug, Args)]
pub struct TuiArgs {
    /// Registry to browse. Precedence (highest first): this flag (or the
    /// global `--registry`), then `GRIM_DEFAULT_REGISTRY`, then project config
    /// `default_registry`, then global config.
    #[arg(long)]
    pub registry: Option<String>,

    /// Force a catalog rebuild even if the cache is fresh (governs the
    /// initial load only; the interactive `r` key always forces a reload).
    #[arg(long)]
    pub refresh: bool,

    /// Browse against the global scope's lock/state instead of the
    /// discovered project.
    #[arg(long)]
    pub global: bool,

    /// Explicit project config path (for scope badge / install target).
    #[arg(long)]
    pub config: Option<std::path::PathBuf>,
}

/// Run `grim tui`.
///
/// # Errors
///
/// A missing registry (none resolvable) is a config error (78); a
/// terminal-setup failure propagates. A clean quit (or a non-TTY stdout)
/// exits 0.
pub async fn run(ctx: &Context, args: &TuiArgs) -> anyhow::Result<ExitCode> {
    if !std::io::stdout().is_terminal() {
        // Non-interactive: do not touch raw mode. Clear, zero-exit.
        println!("grim tui requires an interactive terminal (stdout is not a TTY)");
        return Ok(ExitCode::Success);
    }

    let registry = resolve_registry(ctx, args)?;

    let scope = scope_resolution::resolve(ctx, args.global, args.config.as_deref())
        .map_err(|e| anyhow::Error::from(crate::error::Error::from(e)))?;
    let access = super::access_seam(ctx)?;

    // Resolve the *other* scope too so the TUI can toggle Global ⇄
    // Project at runtime. It is best-effort: if the alternate scope
    // cannot be resolved (e.g. no project config discoverable), the
    // toggle is simply disabled rather than failing the whole TUI.
    let alt = scope_resolution::resolve(ctx, !args.global, args.config.as_deref())
        .ok()
        .filter(|other| other.scope != scope.scope)
        .map(|other| ScopeSwap {
            scope: other.scope,
            workspace: other.workspace.clone(),
            lock_path: other.lock_path.clone(),
            state_path: other.state_path.clone(),
            config_path: other.config_path.clone(),
            clients_default: other.options.clients.clone(),
            clients_selected: selected_clients(&other.workspace, other.scope, &other.options.clients),
            label: scope_label(other.scope).to_string(),
        });

    let tui_ctx = TuiContext {
        registry,
        catalog_path: ctx.paths().catalog_file(),
        access,
        offline: ctx.offline(),
        force_refresh: args.refresh,
        scope: scope.scope,
        workspace: scope.workspace.clone(),
        lock_path: scope.lock_path.clone(),
        state_path: scope.state_path.clone(),
        config_path: scope.config_path.clone(),
        clients_default: scope.options.clients.clone(),
        clients_selected: selected_clients(&scope.workspace, scope.scope, &scope.options.clients),
        scope_label: scope_label(scope.scope).to_string(),
        alt,
    };

    app::run(tui_ctx).await?;
    Ok(ExitCode::Success)
}

/// Resolve the registry to browse via the centralized precedence (mirrors
/// `grim search`): `--registry` flag > `GRIM_DEFAULT_REGISTRY` > project
/// config `default_registry` > global config (the global config is the
/// lowest-priority fallback only for a non-global run).
fn resolve_registry(ctx: &Context, args: &TuiArgs) -> anyhow::Result<String> {
    if let Some(r) = &args.registry {
        return Ok(r.clone());
    }
    let project_default = scope_resolution::resolve(ctx, args.global, args.config.as_deref())
        .ok()
        .and_then(|scope| scope.options.default_registry);
    // `--global` maps to the global scope; the centralized helper gates the
    // global-config fallback on it (same mapping `scope_resolution` applies).
    let scope = if args.global {
        ConfigScope::Global
    } else {
        ConfigScope::Project
    };
    let global_default = crate::command::global_config_default(ctx, scope);
    crate::command::resolve_default_registry(ctx, project_default.as_deref(), global_default.as_deref())
        .ok_or_else(|| crate::error::Error::from(crate::command::command_error::CommandError::NoRegistry).into())
}

/// The effective selected clients for a scope's TUI display, derived from
/// the **same** resolution the install / update path uses:
/// [`InstallTarget::parse`] with no `--client` flag and the config
/// `[options].clients` as the default. That folds in detection (empty
/// config ⇒ detected clients for the scope, falling back to `[claude]`),
/// so the status line never shows a target set that diverges from what an
/// install would actually write to.
///
/// The display is best-effort: an unparseable config `clients` entry makes
/// `parse` error (the install path surfaces that hard error to the user),
/// so here it degrades to the detected set rather than failing the TUI.
fn selected_clients(workspace: &std::path::Path, scope: ConfigScope, config_clients: &[String]) -> Vec<ClientTarget> {
    match crate::install::target::InstallTarget::parse(workspace, scope, &[], config_clients) {
        Ok(target) => target.clients().to_vec(),
        Err(_) => crate::install::target::detect_clients(workspace, scope),
    }
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

    #[test]
    fn explicit_registry_wins() {
        let ctx = Context::new(&opts());
        let a = TuiArgs {
            registry: Some("ghcr.io".to_string()),
            refresh: false,
            global: false,
            config: None,
        };
        assert_eq!(resolve_registry(&ctx, &a).unwrap(), "ghcr.io");
    }

    #[test]
    fn no_registry_is_config_error() {
        let ctx = Context::new(&opts());
        let a = TuiArgs {
            registry: None,
            refresh: false,
            global: false,
            config: None,
        };
        let err = resolve_registry(&ctx, &a).expect_err("no registry");
        assert_eq!(crate::error::classify_error(&err), ExitCode::ConfigError);
    }

    #[test]
    fn selected_clients_matches_install_target_resolution() {
        // The TUI display must derive from the same resolution the install
        // path uses: an explicit config `clients` list resolves to exactly
        // what `InstallTarget::parse` would target (parse + dedup + order),
        // not a separately re-parsed list.
        let tmp = tempfile::tempdir().unwrap();
        let cfg = ["copilot,claude".to_string()];
        let display = selected_clients(tmp.path(), ConfigScope::Project, &cfg);
        let installed = crate::install::target::InstallTarget::parse(tmp.path(), ConfigScope::Project, &[], &cfg)
            .unwrap()
            .clients()
            .to_vec();
        assert_eq!(display, installed);
        assert_eq!(display, vec![ClientTarget::Copilot, ClientTarget::Claude]);
    }

    #[test]
    fn selected_clients_empty_config_uses_detection() {
        // An empty config `clients` list folds into detection (and the
        // `[claude]` fallback when nothing is detected) — identical to the
        // install path's behavior for an unconfigured scope.
        let tmp = tempfile::tempdir().unwrap();
        let display = selected_clients(tmp.path(), ConfigScope::Project, &[]);
        let detected = crate::install::target::detect_clients(tmp.path(), ConfigScope::Project);
        assert_eq!(display, detected);
        // A bare workspace detects nothing ⇒ the shared `[claude]` fallback.
        assert_eq!(display, vec![ClientTarget::Claude]);
    }

    #[test]
    fn selected_clients_unknown_name_degrades_to_detection() {
        // An unparseable config entry makes `InstallTarget::parse` error (the
        // install path surfaces that to the user); the display must degrade
        // to the detected set rather than panicking or silently dropping.
        let tmp = tempfile::tempdir().unwrap();
        let cfg = ["vscode".to_string()];
        let display = selected_clients(tmp.path(), ConfigScope::Project, &cfg);
        assert_eq!(
            display,
            crate::install::target::detect_clients(tmp.path(), ConfigScope::Project)
        );
    }
}
