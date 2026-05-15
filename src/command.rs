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
pub fn access_seam(ctx: &crate::context::Context) -> anyhow::Result<std::sync::Arc<dyn crate::oci::access::OciAccess>> {
    map_access_io(ctx, ctx.access())
}

/// Like [`access_seam`] but with an explicit routing mode. `update` uses
/// this with [`crate::context::Context::update_access_mode`] so a rolling
/// release re-resolves the floating tag instead of serving the cached pin.
pub fn access_seam_with_mode(
    ctx: &crate::context::Context,
    mode: crate::oci::access::AccessMode,
) -> anyhow::Result<std::sync::Arc<dyn crate::oci::access::OciAccess>> {
    map_access_io(ctx, ctx.access_with_mode(mode))
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
