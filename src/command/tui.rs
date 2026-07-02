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
//!
//! When the requested scope has no `grimoire.toml` yet (project discovery
//! misses, or the global config file is absent), the command offers to
//! initialize one before the session starts via the popup-style
//! [`crate::tui::init_dialog`]: a confirm popup plus a registry input
//! pre-filled with the effective default registry (flag > env > config >
//! the built-in fallback), so plain Enter accepts — and persists as a
//! `[[registries]]` entry with `default = true`. Cancelling closes the
//! TUI cleanly with exit 0.

use std::io::IsTerminal;

use clap::Args;

use crate::cli::exit_code::ExitCode;
use crate::config::ResolvedRegistry;
use crate::config::scope::ConfigScope;
use crate::context::Context;
use crate::install::client_target::ClientTarget;
use crate::tui::app::{self, ScopeSwap, TuiContext};
use crate::tui::init_dialog::{InitDialog, InitDialogOutcome};

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
    /// Registries to browse; repeatable and comma-separated (`--registry a,b`
    /// or `--registry a --registry b`) to browse several at once. Precedence
    /// (highest first): this flag (or the global `--registry`), then
    /// `GRIM_DEFAULT_REGISTRY`, then project config `default_registry`, then
    /// global config.
    #[arg(long, value_delimiter = ',', action = clap::ArgAction::Append)]
    pub registry: Vec<String>,

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
/// A terminal-setup failure propagates. A clean quit (or a non-TTY
/// stdout) exits 0. A registry always resolves (the built-in fallback is
/// the last tier).
pub async fn run(ctx: &Context, args: &TuiArgs) -> anyhow::Result<ExitCode> {
    if !std::io::stdout().is_terminal() {
        // Non-interactive: do not touch raw mode. Clear, zero-exit.
        println!("grim tui requires an interactive terminal (stdout is not a TTY)");
        return Ok(ExitCode::Success);
    }

    // No config for the requested scope yet: offer to initialize one before
    // the session starts. The prompt runs before raw mode, so plain stdin
    // reads are safe; declining closes the TUI cleanly.
    if config_missing(ctx, args) && matches!(prompt_init(ctx, args).await?, InitPrompt::Cancelled) {
        return Ok(ExitCode::Success);
    }

    let scope = scope_resolution::resolve(ctx, args.global, args.config.as_deref())
        .map_err(|e| anyhow::Error::from(crate::error::Error::from(e)))?;
    let access = super::access_seam(ctx)?;

    // Resolve the full ordered registry set for the active scope via the shared
    // multi-registry seam (mirrors `grim search` / `grim mcp`).
    let registries = resolve_registries_for_tui(ctx, args, &scope);
    let primary_registry = crate::config::primary_registry(&registries).to_string();

    // Resolve the *other* scope too so the TUI can toggle Global ⇄
    // Project at runtime. It is best-effort: if the alternate scope
    // cannot be resolved (e.g. no project config discoverable), the
    // toggle is simply disabled rather than failing the whole TUI.
    let alt = scope_resolution::resolve(ctx, !args.global, args.config.as_deref())
        .ok()
        .filter(|other| other.scope != scope.scope)
        .map(|other| {
            let alt_registries = resolve_registries_for_tui(ctx, args, &other);
            let alt_primary = crate::config::primary_registry(&alt_registries).to_string();
            ScopeSwap {
                scope: other.scope,
                workspace: other.workspace.clone(),
                lock_path: other.lock_path.clone(),
                state_path: other.state_path.clone(),
                config_path: other.config_path.clone(),
                clients_default: other.options.clients.clone(),
                clients_selected: selected_clients(&other.workspace, other.scope, &other.options.clients),
                label: scope_label(other.scope).to_string(),
                roots: other.roots,
                tui_options: other.options.tui.clone(),
                registries: alt_registries,
                primary_registry: alt_primary,
            }
        });

    let tui_ctx = TuiContext {
        primary_registry,
        registries,
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
        roots: scope.roots,
        tui_options: scope.options.tui.clone(),
    };

    app::run(tui_ctx).await?;
    Ok(ExitCode::Success)
}

/// Outcome of the missing-config init prompt.
enum InitPrompt {
    /// A config exists now — continue into the TUI session.
    Ready,
    /// The user declined (or stdin cannot prompt) — close the TUI.
    Cancelled,
}

/// Whether the requested scope has no config yet. Global scope checks
/// `$GRIM_HOME/grimoire.toml` directly; project scope asks discovery. An
/// explicit `--config` path is never treated as missing here — `grim init`
/// writes only the canonical locations, so a bad explicit path keeps
/// surfacing as the usual hard error instead of initializing elsewhere.
fn config_missing(ctx: &Context, args: &TuiArgs) -> bool {
    if args.global {
        return !ctx.paths().global_config().exists();
    }
    if args.config.is_some() {
        return false;
    }
    match crate::config::project_config::ProjectConfig::discover(None) {
        Ok(_) => false,
        // Only a genuine "nothing found" offers init; a parse failure on an
        // existing file must surface through the normal resolve path.
        Err(e) => scope_resolution::config_not_found(&e),
    }
}

/// Interactive missing-config prompt, run as a popup-style modal TUI
/// session ([`crate::tui::init_dialog`]): confirm initialization, edit
/// the default browse source — pre-filled with the **effective** browse
/// primary (`--registry` flag > `[[registries]]` primary > legacy
/// `default_registry` chain > the built-in fallback **index**,
/// [`crate::command::FALLBACK_INDEX`]), so plain Enter persists a browse
/// source that actually lists packages — and create the scope's
/// `grimoire.toml` via `grim init` (which keys the entry `index` vs `oci`
/// by the locator's shape).
///
/// Accepting the pre-filled value deliberately snapshots it as a
/// `[[registries]]` entry with `default = true` in the new config: the
/// dialog's accepted value is an explicit user choice, unlike bare `grim init`
/// (which keeps the fallback floating — see `command/init.rs`).
///
/// # Errors
///
/// Propagates dialog I/O failures and any `grim init` error (e.g. a
/// config racing into existence maps to the usual exit-64 error).
async fn prompt_init(ctx: &Context, args: &TuiArgs) -> anyhow::Result<InitPrompt> {
    if !std::io::stdin().is_terminal() {
        eprintln!("no grimoire.toml found and stdin is not a terminal; run `grim init` first");
        return Ok(InitPrompt::Cancelled);
    }

    let label = if args.global {
        ctx.paths().global_config().display().to_string()
    } else {
        "./grimoire.toml".to_string()
    };
    let scope = if args.global {
        ConfigScope::Global
    } else {
        ConfigScope::Project
    };

    // The same precedence the session itself browses with — including the
    // built-in fallback — so the dialog's default and the browsed source
    // can never diverge.
    let default_registry = resolve_browse_default(ctx, args);
    let mut dialog = InitDialog::new(&label, scope_label(scope), default_registry);
    let registry = match crate::tui::init_dialog::run(&mut dialog)? {
        InitDialogOutcome::Cancelled => return Ok(InitPrompt::Cancelled),
        InitDialogOutcome::Confirmed { registry } => registry,
    };

    let init_args = crate::command::init::InitArgs {
        global: args.global,
        registry,
    };
    let (report, _) = crate::command::init::run(ctx, &init_args).await?;
    eprintln!("initialized {}", report.path.display());
    Ok(InitPrompt::Ready)
}

/// Resolve the init dialog's pre-fill: the primary **browse** source's
/// locator. An explicit `--registry` flag wins outright; otherwise the
/// scope's browse set resolves via the same seam the session browses with
/// ([`crate::command::registries_for_scope`]) and the primary entry's
/// locator is returned — which for an unconfigured user is the built-in
/// fallback **index** ([`crate::command::FALLBACK_INDEX`]), not the
/// push-side [`crate::command::FALLBACK_REGISTRY`] (a GHCR-style OCI entry
/// would browse empty because `_catalog` is gated).
///
/// On scope-resolution failure (no `grimoire.toml` discoverable — the
/// normal case for this dialog), the global-`[[registries]]`-aware
/// fallback set ([`crate::command::registries_global_fallback`]) is used so
/// a `[[registries]]`-only global config is still honored.
fn resolve_browse_default(ctx: &Context, args: &TuiArgs) -> String {
    if let Some(r) = args.registry.first() {
        return r.clone();
    }
    let set = match scope_resolution::resolve(ctx, args.global, args.config.as_deref()) {
        Ok(scope) => crate::command::registries_for_scope(ctx, &scope),
        Err(_) => crate::command::registries_global_fallback(ctx),
    };
    set.iter()
        .find(|r| r.is_default)
        .or_else(|| set.first())
        .map(|r| r.url.clone())
        // resolve_registries never returns an empty set; defensive only.
        .unwrap_or_else(|| crate::command::FALLBACK_INDEX.to_string())
}

/// Resolve the ordered registry set for a TUI session, mirroring the
/// `grim search` / `grim mcp` seam (`catalog_service::load_catalog`).
///
/// Behavior (D-RESOLVE):
/// - An explicit `--registry` flag (repeatable / comma-separated) collapses to
///   exactly those registries (in order, deduped, first is primary).
/// - Otherwise, `[[registries]]` is authoritative; the legacy scalar
///   `default_registry` and the global config tiers are folded in via
///   [`super::registries_for_scope`] — the same seam `grim search` uses.
/// - The built-in fallback (`FALLBACK_REGISTRY`) ensures a non-empty result.
fn resolve_registries_for_tui(
    ctx: &Context,
    args: &TuiArgs,
    scope: &scope_resolution::ResolvedScope,
) -> Vec<ResolvedRegistry> {
    if !args.registry.is_empty() {
        // Explicit --registry collapses to exactly those registries (in order,
        // deduped, first is primary).
        return crate::config::resolve_registries(
            &args.registry,
            &[],
            None,
            &[],
            None,
            crate::command::FALLBACK_REGISTRY,
            None,
        );
    }
    super::registries_for_scope(ctx, scope)
}

/// The effective selected clients for a scope's TUI display, derived from
/// the **same** resolution the install / update path uses:
/// [`InstallTarget::parse`] with no `--client` flag and the config
/// `[options].clients` as the default. That folds in detection (empty
/// config ⇒ detected clients for the scope, falling back to all clients),
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
            registry: Vec::new(),
        }
    }

    #[test]
    fn explicit_registry_wins() {
        let ctx = Context::new(&opts());
        let a = TuiArgs {
            registry: vec!["ghcr.io".to_string()],
            refresh: false,
            global: false,
            config: None,
        };
        assert_eq!(resolve_browse_default(&ctx, &a), "ghcr.io");
    }

    #[test]
    fn no_registry_anywhere_prefills_builtin_index() {
        // Hermetic: the developer's $GRIM_DEFAULT_REGISTRY / $GRIM_HOME /
        // a CWD-discovered project config must not leak in — pin all
        // three tiers explicitly. Nothing configured ⇒ the built-in
        // fallback INDEX (the public package index), never the push-side
        // OCI fallback — a GHCR-style entry would browse empty.
        let tmp = tempfile::tempdir().unwrap();
        let cfg = tmp.path().join("grimoire.toml");
        std::fs::write(&cfg, "[options]\n").unwrap();
        let ctx = Context::hermetic(tmp.path().to_path_buf());
        let a = TuiArgs {
            registry: Vec::new(),
            refresh: false,
            global: false,
            config: Some(cfg),
        };
        assert_eq!(resolve_browse_default(&ctx, &a), crate::command::FALLBACK_INDEX);
    }

    #[test]
    fn resolve_browse_default_honors_global_registries_array_when_no_project_config() {
        // Regression guard: a user with a [[registries]]-only global config
        // (no [options].default_registry) running `grim tui` from a directory
        // without a project grimoire.toml must get their declared registry —
        // not the built-in fallback. The Err branch previously bypassed
        // [[registries]] by calling only global_config_default +
        // resolve_default_registry.
        //
        // Point the hermetic ctx at a temp dir that has a global config with
        // [[registries]] but no project grimoire.toml. scope_resolution will
        // fail (no project config), triggering the Err branch.
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("grimoire.toml"),
            "[[registries]]\nurl = \"global-tui.example\"\ndefault = true\n",
        )
        .unwrap();
        let ctx = Context::hermetic(tmp.path().to_path_buf());
        // No --config, no --global: scope_resolution tries to discover a
        // project config in the CWD. In a hermetic test this may succeed or
        // fail depending on CWD; pass a missing explicit config path to force
        // scope resolution to error (no file at that path ⇒ Err branch).
        let missing_cfg = tmp.path().join("no-such/grimoire.toml");
        let a = TuiArgs {
            registry: Vec::new(),
            refresh: false,
            global: false,
            config: Some(missing_cfg),
        };
        assert_eq!(resolve_browse_default(&ctx, &a), "global-tui.example");
    }

    #[test]
    fn config_missing_global_checks_grim_home_file() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = Context::hermetic(tmp.path().to_path_buf());
        let a = TuiArgs {
            registry: Vec::new(),
            refresh: false,
            global: true,
            config: None,
        };
        assert!(config_missing(&ctx, &a), "absent global config offers init");

        std::fs::write(ctx.paths().global_config(), "[skills]\n\n[rules]\n").unwrap();
        assert!(!config_missing(&ctx, &a), "existing global config skips the prompt");
    }

    #[test]
    fn config_missing_never_fires_for_explicit_config_path() {
        // An explicit --config path (even a missing one) keeps the normal
        // hard-error path — init writes only canonical locations.
        let tmp = tempfile::tempdir().unwrap();
        let ctx = Context::hermetic(tmp.path().to_path_buf());
        let a = TuiArgs {
            registry: Vec::new(),
            refresh: false,
            global: false,
            config: Some(tmp.path().join("nope/grimoire.toml")),
        };
        assert!(!config_missing(&ctx, &a));
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
        // all-clients fallback when nothing is detected) — identical to the
        // install path's behavior for an unconfigured scope.
        let tmp = tempfile::tempdir().unwrap();
        let display = selected_clients(tmp.path(), ConfigScope::Project, &[]);
        let detected = crate::install::target::detect_clients(tmp.path(), ConfigScope::Project);
        assert_eq!(display, detected);
        // A bare workspace detects nothing ⇒ the shared all-clients fallback.
        assert_eq!(display, ClientTarget::ALL.to_vec());
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
