// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! `grim mcp` — run a local STDIO Model Context Protocol server.
//!
//! Diverges into a long-running server loop rather than emitting a structured
//! report, so (per `subsystem-cli-api.md` "Commands That Exec a Child
//! Process") it is exempt from the `Printable` / `api/` path and returns an
//! [`ExitCode`] directly — the same exemption `tui` and `schema` use. The
//! server exposes Grimoire's catalog/status as MCP tools; mutating tools are
//! gated behind `--allow-writes` (read-only by default).

use clap::Args;

use crate::cli::exit_code::ExitCode;
use crate::context::Context;

/// `grim mcp` arguments.
#[derive(Debug, Args)]
pub struct McpArgs {
    /// Enable mutating tools (add / install / update / uninstall). Off by
    /// default: the server is read-only unless this is set.
    #[arg(long)]
    pub allow_writes: bool,

    /// Operate on the global scope instead of the discovered project. The
    /// scope is fixed for the server's lifetime — tools cannot redirect it.
    #[arg(long)]
    pub global: bool,

    /// Explicit project config path (scope resolution for status/write tools).
    #[arg(long)]
    pub config: Option<std::path::PathBuf>,
}

/// Run `grim mcp`. Returns when the client closes stdin (EOF).
///
/// # Errors
///
/// A transport setup failure, or an error building the server. A clean client
/// disconnect exits `Success`.
pub async fn run(ctx: &Context, args: &McpArgs) -> anyhow::Result<ExitCode> {
    crate::mcp::server::serve(ctx, args).await
}
