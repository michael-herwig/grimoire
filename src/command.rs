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
pub mod init;
pub mod install;
pub mod lock;
pub mod login;
pub mod logout;
pub mod publish;
pub mod release;
pub mod remove;
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

/// The built-in default registry, used only when no other tier configures
/// one (no `--registry` flag, no `$GRIM_DEFAULT_REGISTRY`, no config
/// `default_registry`).
pub const FALLBACK_REGISTRY: &str = "grim.ocx.sh";

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

/// Build a classifiable usage error (exit 64) for a missing `login`
/// credential input, routed through the top-level error so
/// [`crate::error::classify_error`] sees it.
pub fn login_usage(message: &'static str) -> anyhow::Error {
    anyhow::Error::from(crate::error::Error::from(command_error::CommandError::LoginInput(
        message,
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
    use crate::context::Context;

    fn opts(registry: Option<&str>) -> GlobalOptions {
        GlobalOptions {
            format: OutputFormat::Plain,
            offline: false,
            log_level: None,
            config: None,
            global: false,
            registry: registry.map(str::to_string),
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
}
