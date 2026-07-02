// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! The spine commands.
//!
//! Each command follows the pattern: parse → typed scope/refs →
//! operation → report built from operation results → render via
//! [`crate::cli::printer::Printable`]. `anyhow` is used here (the
//! application boundary); the lib subsystems stay on `thiserror`.

pub mod add;
pub mod build;
pub mod command_error;
pub mod config;
pub mod init;
pub mod install;
pub mod lock;
pub mod login;
pub mod logout;
pub mod mcp;
pub mod publish;
pub mod release;
pub mod remove;
pub mod schema;
pub mod scope_resolution;
pub mod search;
pub mod status;
pub mod tui;
pub mod uninstall;
pub mod update;

#[allow(unused_imports)]
pub use command_error::CommandError;

/// Resolve the registry for `login` / `logout`: an explicit (non-empty)
/// argument wins, else the `--registry` flag, else `$GRIM_DEFAULT_REGISTRY`
/// (the context's `default_registry`, which folds flag-then-env). A miss is
/// a classifiable config error, not a panic. The CLI argument stays first;
/// env beats config because config is not consulted on the login path.
///
/// # Errors
///
/// [`CommandError::NoLoginRegistry`] when neither an argument nor a
/// default registry is available.
pub fn resolve_login_registry(ctx: &crate::context::Context, explicit: Option<&str>) -> anyhow::Result<String> {
    if let Some(reg) = explicit.filter(|r| !r.is_empty()) {
        return Ok(reg.to_string());
    }
    ctx.default_registry()
        .map(str::to_string)
        .ok_or_else(|| anyhow::Error::from(crate::error::Error::from(command_error::CommandError::NoLoginRegistry)))
}

/// The built-in default registry for push-side and short-id expansion,
/// used only when no other tier configures one (no `--registry` flag, no
/// `$GRIM_DEFAULT_REGISTRY`, no config `default_registry`). First-party
/// packages live under the grimoire-rs org on GHCR.
pub const FALLBACK_REGISTRY: &str = "ghcr.io/grimoire-rs";

/// The built-in browse fallback: the public package index. Used as the
/// final tier of the browse-set resolution so an unconfigured `grim
/// search` / TUI / MCP lists the ecosystem through the index (GHCR gates
/// `_catalog`, so a bare registry fallback would browse empty).
pub const FALLBACK_INDEX: &str = "https://index.grimoire.rs";

/// The single registry-precedence helper: `--registry` flag, then
/// `$GRIM_DEFAULT_REGISTRY`, then the project config
/// `[options].default_registry`, then the global config
/// `[options].default_registry`, then the built-in
/// [`FALLBACK_REGISTRY`]. The first present value wins, so the fallback
/// applies only when nothing is configured anywhere.
///
/// The default registry is purely a CLI-input convenience — the expanded
/// [`crate::oci::Identifier`] is always fully-qualified, so the lock and
/// config persist the registry host explicitly regardless of which default
/// was applied. Every registry call site (`add` / `release` / `search` /
/// `tui`) routes through this so the precedence is single-sourced.
pub fn resolve_default_registry(
    ctx: &crate::context::Context,
    project_default: Option<&str>,
    global_default: Option<&str>,
) -> String {
    ctx.registry_flag()
        .or_else(|| ctx.registry_env())
        .or(project_default)
        .or(global_default)
        .unwrap_or(FALLBACK_REGISTRY)
        .to_string()
}

/// The global config's `[options].default_registry`, loaded best-effort as
/// the lowest-priority registry fallback. Returns `None` for a global-scope
/// run — the global config is already that run's active scope, so consulting
/// it here would double-count the same tier. Load failures degrade to `None`
/// (the global config is advisory at this tier, never fatal).
///
/// Single-sourced for every registry-resolving command (`add` / `search` /
/// `tui` / `release`) so the precedence chain stays identical across them.
pub fn global_config_default(
    ctx: &crate::context::Context,
    scope: crate::config::scope::ConfigScope,
) -> Option<String> {
    if scope == crate::config::scope::ConfigScope::Global {
        return None;
    }
    crate::config::global_config::GlobalConfig::load(&ctx.paths().global_config())
        .ok()
        .and_then(|cfg| cfg.options.default_registry)
}

/// The global config's `[[registries]]`, loaded best-effort as a
/// lower-priority tier than the project `[[registries]]`. Returns an empty
/// vec for a global-scope run (the global config is already that run's
/// active scope, so it must not be folded in twice) and on any load failure
/// (the registries are advisory at this tier, never fatal).
///
/// Single-sourced alongside [`global_config_default`] so every
/// registry-resolving command assembles the same browse set.
pub fn global_config_registries(
    ctx: &crate::context::Context,
    scope: crate::config::scope::ConfigScope,
) -> Vec<crate::config::declaration::RegistryConfig> {
    if scope == crate::config::scope::ConfigScope::Global {
        return Vec::new();
    }
    crate::config::global_config::GlobalConfig::load(&ctx.paths().global_config())
        .map(|cfg| cfg.registries)
        .unwrap_or_default()
}

/// Assemble the ordered registry browse set for a resolved scope.
///
/// The single seam `search` / `tui` / `mcp` call to get the multi-registry
/// set: the `--registry` flag(s) (`ctx.registry_flags`) collapse to exactly
/// those registries; otherwise the scope's `[[registries]]` are authoritative; when
/// no `[[registries]]` exist the legacy single-default chain
/// (`$GRIM_DEFAULT_REGISTRY` > project `[options].default_registry` > global >
/// fallback) applies — all via [`crate::config::resolve_registries`] so the
/// precedence is single-sourced.
pub fn registries_for_scope(
    ctx: &crate::context::Context,
    scope: &scope_resolution::ResolvedScope,
) -> Vec<crate::config::ResolvedRegistry> {
    let global_registries = global_config_registries(ctx, scope.scope);
    let global_default = global_config_default(ctx, scope.scope);
    crate::config::resolve_registries(
        ctx.registry_flags(),
        &scope.registries,
        scope.options.default_registry.as_deref(),
        &global_registries,
        global_default.as_deref(),
        FALLBACK_INDEX,
        ctx.registry_env(),
    )
}

/// The primary registry for a resolved scope via the same seam
/// `add` / `search` / `mcp` use: `primary_registry(&registries_for_scope(…))`.
///
/// This is the unified consumer seam — `release` and `tui` route through it
/// so that a `[[registries]]`-only config (no `[options].default_registry`)
/// is honored by all commands, removing the inconsistency where PATH-A
/// commands (`release_default_registry`, `resolve_registry`) previously
/// resolved only through the legacy `default_registry` chain.
///
/// On scope-resolution failure the scope is absent; call
/// [`primary_registry_global_fallback`] instead, which folds the global
/// `[[registries]]` tier so a `[[registries]]`-only global config is still
/// honored.
pub fn primary_registry_for_scope(ctx: &crate::context::Context, scope: &scope_resolution::ResolvedScope) -> String {
    or_fallback_registry(crate::config::registry_resolve::primary_registry(
        &registries_for_scope(ctx, scope),
    ))
}

/// Index sources never expand short ids, so a browse set holding only
/// index sources (notably the built-in [`FALLBACK_INDEX`] tier) yields an
/// empty primary — substitute the push-side [`FALLBACK_REGISTRY`] so
/// `add`/`release` short ids keep a concrete registry host.
fn or_fallback_registry(primary: &str) -> String {
    if primary.is_empty() {
        FALLBACK_REGISTRY.to_string()
    } else {
        primary.to_string()
    }
}

/// The primary registry when scope resolution fails (e.g. `release` or `tui`
/// run outside any project): folds the global `[[registries]]` and the legacy
/// `[options].default_registry` tiers so a `[[registries]]`-only global config
/// is honored — the same contract as [`registries_for_scope`]'s global-tier
/// folding, but without a resolved project scope.
///
/// Precedence (mirrors [`crate::config::resolve_registries`] with empty project
/// tiers):
/// 1. `--registry` flag(s) (`ctx.registry_flags`): collapse to exactly those
///    registries. Only the flag collapses; `$GRIM_DEFAULT_REGISTRY` is a tier-3 default.
/// 2. Global `[[registries]]` (first `default = true`, else first entry)
/// 3. `$GRIM_DEFAULT_REGISTRY` (`ctx.registry_env`) → global
///    `[options].default_registry` → built-in [`FALLBACK_REGISTRY`]
///    (legacy single-default chain, only when no `[[registries]]` present)
pub fn primary_registry_global_fallback(ctx: &crate::context::Context) -> String {
    or_fallback_registry(crate::config::registry_resolve::primary_registry(
        &registries_global_fallback(ctx),
    ))
}

/// The ordered browse set when scope resolution fails (no project config):
/// the `--registry` flag(s), else the global `[[registries]]`, else the
/// legacy single-default chain ending in the built-in [`FALLBACK_INDEX`].
/// The set-building seam behind [`primary_registry_global_fallback`],
/// exposed so browse-side consumers (the TUI init dialog pre-fill) can read
/// the primary *browse* locator — which may be an index source that
/// [`primary_registry_global_fallback`] deliberately substitutes away for
/// push-side use.
pub fn registries_global_fallback(ctx: &crate::context::Context) -> Vec<crate::config::ResolvedRegistry> {
    let global_regs = global_config_registries(ctx, crate::config::scope::ConfigScope::Project);
    let global_default = global_config_default(ctx, crate::config::scope::ConfigScope::Project);
    crate::config::resolve_registries(
        ctx.registry_flags(),
        &[],
        None,
        &global_regs,
        global_default.as_deref(),
        FALLBACK_INDEX,
        ctx.registry_env(),
    )
}

/// Build a classifiable usage error (exit 64) for a missing `login`
/// credential input, routed through the top-level error so
/// [`crate::error::classify_error`] sees it.
pub fn login_usage(message: &'static str) -> anyhow::Error {
    anyhow::Error::from(crate::error::Error::from(command_error::CommandError::LoginInput(
        message,
    )))
}

/// Build a classifiable usage error (exit 64) for `grim config`: unknown
/// key, duplicate alias, or other contract violation.
pub fn config_usage(msg: impl Into<String>) -> anyhow::Error {
    anyhow::Error::from(crate::error::Error::from(command_error::CommandError::ConfigUsage(
        msg.into(),
    )))
}

/// Build a classifiable data error (exit 65) for `grim config set`: a
/// syntactically valid but semantically rejected value.
pub fn config_value(msg: impl Into<String>) -> anyhow::Error {
    anyhow::Error::from(crate::error::Error::from(command_error::CommandError::ConfigValue(
        msg.into(),
    )))
}

/// Map a subsystem `Result` into an `anyhow::Result` whose error is wrapped
/// in the top-level [`crate::error::Error`].
///
/// The bare `?` operator converts a subsystem error straight into
/// `anyhow::Error` via the blanket `From` impl, which bypasses
/// [`crate::error::classify_error`] (it only downcasts the top
/// [`crate::error::Error`]). Routing through this helper keeps every
/// command's exit-code mapping correct.
pub fn grim<T, E>(result: Result<T, E>) -> anyhow::Result<T>
where
    crate::error::Error: From<E>,
{
    result.map_err(|e| anyhow::Error::from(crate::error::Error::from(e)))
}

/// Build the OCI-access seam from the context, mapping a `$GRIM_HOME`
/// layout I/O failure to a classifiable install-tier `TargetIo` error
/// (exit 74) rather than the generic fall-through.
///
/// The seam is always-fresh online unless the invocation is offline, so a
/// rolling release re-resolves the floating tag instead of serving a
/// cached pin — no separate "remote" routing mode is needed.
pub fn access_seam(ctx: &crate::context::Context) -> anyhow::Result<std::sync::Arc<dyn crate::oci::access::OciAccess>> {
    map_access_io(ctx, ctx.access())
}

fn map_access_io(
    ctx: &crate::context::Context,
    result: std::io::Result<std::sync::Arc<dyn crate::oci::access::OciAccess>>,
) -> anyhow::Result<std::sync::Arc<dyn crate::oci::access::OciAccess>> {
    result.map_err(|e| {
        anyhow::Error::from(crate::error::Error::from(
            crate::install::install_error::InstallError::without_reference(
                crate::install::install_error::InstallErrorKind::TargetIo {
                    path: ctx.paths().root().to_path_buf(),
                    source: e,
                },
            ),
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::options::{GlobalOptions, OutputFormat};
    use crate::config::declaration::RegistryConfig;
    use crate::context::Context;

    fn opts(registry: Option<&str>) -> GlobalOptions {
        GlobalOptions {
            format: OutputFormat::Plain,
            offline: false,
            log_level: None,
            config: None,
            global: false,
            registry: registry.into_iter().map(str::to_string).collect(),
        }
    }

    #[test]
    fn precedence_flag_beats_config() {
        // The `--registry` flag wins over every config default. (The env is
        // not set in the test environment; the flag is the highest tier.)
        let ctx = Context::new(&opts(Some("flag.example")));
        assert_eq!(
            resolve_default_registry(&ctx, Some("proj.example"), Some("glob.example")),
            "flag.example"
        );
    }

    #[test]
    fn precedence_project_config_beats_global_config() {
        // No flag, no env ⇒ project config wins over the global fallback.
        // Hermetic: a developer's $GRIM_DEFAULT_REGISTRY must not interpose.
        let tmp = tempfile::tempdir().unwrap();
        let ctx = Context::hermetic(tmp.path().to_path_buf());
        assert_eq!(
            resolve_default_registry(&ctx, Some("proj.example"), Some("glob.example")),
            "proj.example"
        );
    }

    #[test]
    fn precedence_global_config_beats_builtin_fallback() {
        // Hermetic: a developer's $GRIM_DEFAULT_REGISTRY must not interpose.
        let tmp = tempfile::tempdir().unwrap();
        let ctx = Context::hermetic(tmp.path().to_path_buf());
        assert_eq!(
            resolve_default_registry(&ctx, None, Some("glob.example")),
            "glob.example"
        );
    }

    #[test]
    fn no_registry_anywhere_falls_back_to_builtin() {
        // Hermetic: a developer's $GRIM_DEFAULT_REGISTRY must not leak in.
        // Nothing configured anywhere ⇒ the built-in default applies.
        let tmp = tempfile::tempdir().unwrap();
        let ctx = Context::hermetic(tmp.path().to_path_buf());
        assert_eq!(resolve_default_registry(&ctx, None, None), FALLBACK_REGISTRY);
    }

    #[test]
    fn global_config_default_is_none_for_global_scope() {
        // A global-scope run already has the global config as its active
        // scope; the helper must not re-consult it as a fallback tier.
        let ctx = Context::new(&opts(None));
        assert_eq!(
            global_config_default(&ctx, crate::config::scope::ConfigScope::Global),
            None
        );
    }

    // ── Contract (e)/(f) — primary_registry_for_scope regression guard ────

    /// Build a minimal `ResolvedScope` from an in-memory registries slice so
    /// tests can exercise `primary_registry_for_scope` without writing disk files.
    fn make_scope(tmp: &tempfile::TempDir, registries: Vec<RegistryConfig>) -> scope_resolution::ResolvedScope {
        use crate::config::declaration::DesiredSet;
        use crate::install::install_state::InstallState;
        use crate::install::path_anchor::AnchorRoots;
        scope_resolution::ResolvedScope {
            scope: crate::config::scope::ConfigScope::Project,
            set: DesiredSet::default(),
            options: crate::config::declaration::ConfigOptions::default(),
            registries,
            config_path: tmp.path().join("grimoire.toml"),
            lock_path: tmp.path().join("grimoire.lock"),
            state_path: InstallState::project_state_path(tmp.path()),
            workspace: tmp.path().to_path_buf(),
            roots: AnchorRoots {
                workspace: tmp.path().to_path_buf(),
                grim_home: tmp.path().to_path_buf(),
                claude_root: None,
                copilot_root: None,
                opencode_skills: None,
            },
        }
    }

    #[test]
    fn primary_registry_for_scope_returns_registries_primary() {
        // Contract (e): primary_registry_for_scope returns the [[registries]]
        // primary when present — NOT the fallback, NOT default_registry.
        let tmp = tempfile::tempdir().unwrap();
        let ctx = Context::hermetic(tmp.path().to_path_buf());
        let regs = vec![RegistryConfig {
            alias: None,
            oci: Some("array.example".to_string()),
            index: None,
            default: true,
        }];
        let scope = make_scope(&tmp, regs);
        assert_eq!(primary_registry_for_scope(&ctx, &scope), "array.example");
    }

    #[test]
    fn primary_registry_for_scope_falls_back_when_no_registries() {
        // Contract (e) boundary: no [[registries]] → legacy chain → fallback.
        let tmp = tempfile::tempdir().unwrap();
        let ctx = Context::hermetic(tmp.path().to_path_buf());
        let scope = make_scope(&tmp, vec![]);
        // No registries, no default_registry in options, hermetic ctx (no env) →
        // must fall back to FALLBACK_REGISTRY.
        assert_eq!(primary_registry_for_scope(&ctx, &scope), FALLBACK_REGISTRY);
    }

    #[test]
    fn global_config_default_degrades_to_none_when_absent() {
        // No global config on disk under the hermetic $GRIM_HOME ⇒ the
        // best-effort load degrades to `None` rather than failing. (An
        // empty tempdir pins "absent" — the developer's real global
        // config must not leak in.)
        let tmp = tempfile::tempdir().unwrap();
        let ctx = Context::hermetic(tmp.path().to_path_buf());
        assert_eq!(
            global_config_default(&ctx, crate::config::scope::ConfigScope::Project),
            None
        );
    }

    // ── Regression guard: primary_registry_global_fallback ──────────────────
    //
    // These tests lock the contract of the shared Err-branch helper used by
    // `release_default_registry` and `resolve_registry` when scope resolution
    // fails (no project `grimoire.toml`). Before the fix both branches called
    // `global_config_default` + `resolve_default_registry`, which ignored the
    // global `[[registries]]` tier — a user with a `[[registries]]`-only
    // global config always got the built-in fallback instead of their registry.

    #[test]
    fn global_fallback_honors_registries_array_in_global_config() {
        // Regression: [[registries]]-only global config (no [options].default_registry)
        // must resolve to the declared registry, not the built-in fallback.
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("grimoire.toml"),
            "[[registries]]\nurl = \"global.example\"\ndefault = true\n",
        )
        .unwrap();
        let ctx = Context::hermetic(tmp.path().to_path_buf());
        assert_eq!(primary_registry_global_fallback(&ctx), "global.example");
    }

    #[test]
    fn global_fallback_honors_legacy_default_registry_in_global_config() {
        // A global config with only [options].default_registry (no [[registries]])
        // must still return that value.
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("grimoire.toml"),
            "[options]\ndefault_registry = \"legacy.example\"\n",
        )
        .unwrap();
        let ctx = Context::hermetic(tmp.path().to_path_buf());
        assert_eq!(primary_registry_global_fallback(&ctx), "legacy.example");
    }

    #[test]
    fn global_fallback_uses_builtin_when_no_global_config() {
        // No global config on disk ⇒ built-in fallback.
        let tmp = tempfile::tempdir().unwrap();
        let ctx = Context::hermetic(tmp.path().to_path_buf());
        assert_eq!(primary_registry_global_fallback(&ctx), FALLBACK_REGISTRY);
    }

    #[test]
    fn global_fallback_flag_registry_overrides_global_config() {
        // Only the --registry flag collapses the browse set — it must win even
        // when a [[registries]] entry is declared in the global config. Note:
        // $GRIM_DEFAULT_REGISTRY is NOT a collapse trigger; it only heads the
        // tier-3 single-default chain when no [[registries]] are declared.
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("grimoire.toml"),
            "[[registries]]\nurl = \"global.example\"\ndefault = true\n",
        )
        .unwrap();
        // Inject the flag tier via opts (the flag is in ctx directly via
        // `registry_flag`; no hermetic override needed for this tier).
        let ctx = Context::new(&opts(Some("flag.example")));
        assert_eq!(primary_registry_global_fallback(&ctx), "flag.example");
    }
}
