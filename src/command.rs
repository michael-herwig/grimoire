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
/// argument wins, else the context's `default_registry`. A miss is a
/// classifiable config error, not a panic.
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
