// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! The `grim mcp` STDIO server: an rmcp [`ServerHandler`] exposing
//! Grimoire's catalog and install state as MCP tools.
//!
//! Read tools (`grim_search`, `grim_status`) are always available; they wrap
//! the existing `command::*::run` seams and serialize the same report the CLI
//! emits under `--format json`, so the MCP payload and the CLI JSON are one
//! source of truth. Mutating tools are gated behind `--allow-writes` and land
//! in a later change.
//!
//! The server runs over stdio: stdout is the JSON-RPC channel, so the handlers
//! never print to it — all diagnostics go through `tracing` (stderr). The
//! service shuts down cleanly when the client closes stdin (EOF).

use std::sync::Arc;

use rmcp::handler::server::wrapper::Parameters;
use rmcp::{ErrorData, ServerHandler, ServiceExt, tool, tool_handler, tool_router};

use crate::cli::exit_code::ExitCode;
use crate::command::mcp::McpArgs;
use crate::context::Context;
use crate::mcp::state::McpState;
use crate::mcp::tool_args::SearchToolArgs;

/// The MCP server handler. Cloned per request by rmcp (a cheap `Arc` bump).
#[derive(Clone)]
pub struct GrimMcpServer {
    inner: Arc<McpState>,
}

#[tool_router]
impl GrimMcpServer {
    /// Browse the configured registries' catalog, filtered by an optional
    /// query and annotated with each repository's install status. Returns the
    /// same JSON payload as `grim search --format json`.
    #[tool(
        description = "Search the configured Grimoire registries for installable skills, rules, agents, and bundles. Returns a JSON array of matches with kind, repo, summary, version, and install status."
    )]
    async fn grim_search(&self, Parameters(args): Parameters<SearchToolArgs>) -> Result<String, ErrorData> {
        let search_args = crate::command::search::SearchArgs {
            query: args.query,
            refresh: args.refresh.unwrap_or(false),
            // Locked to the server's configured registry set — the tool exposes
            // no registry override (SSRF / CWE-918; see `SearchToolArgs`).
            registry: None,
            global: self.inner.global,
            config: self.inner.config.clone(),
        };
        match crate::command::search::run(&self.inner.ctx, &search_args).await {
            Ok((report, _)) => to_json(&report),
            Err(e) => Err(tool_error("search", &e)),
        }
    }

    /// Report the install status of every declared artifact in the fixed
    /// scope. Returns the same JSON payload as `grim status --format json`.
    #[tool(
        description = "Show the install status of every artifact declared in the active Grimoire scope (installed / outdated / modified / not-installed). Returns a JSON array."
    )]
    async fn grim_status(&self) -> Result<String, ErrorData> {
        let status_args = crate::command::status::StatusArgs {
            global: self.inner.global,
            config: self.inner.config.clone(),
        };
        match crate::command::status::run(&self.inner.ctx, &status_args).await {
            Ok((report, _)) => to_json(&report),
            Err(e) => Err(tool_error("status", &e)),
        }
    }
}

#[tool_handler]
impl ServerHandler for GrimMcpServer {
    fn get_info(&self) -> rmcp::model::ServerInfo {
        // `ServerInfo` / `Implementation` are `#[non_exhaustive]` (cannot be
        // struct-literal'd from outside rmcp); start from the default and set
        // the fields we own.
        let mut info = rmcp::model::ServerInfo::default();
        info.server_info.name = "grim".to_string();
        info.server_info.version = env!("CARGO_PKG_VERSION").to_string();
        info.instructions = Some(
            "Grimoire MCP server: browse and inspect OCI-distributed AI-agent configuration \
             (skills, rules, agents, bundles). Read-only unless started with --allow-writes."
                .to_string(),
        );
        info
    }
}

/// Serialize a report to a JSON string, mapping a serialization failure to an
/// MCP error rather than panicking (no `.unwrap()` on the protocol path).
fn to_json<T: serde::Serialize>(report: &T) -> Result<String, ErrorData> {
    serde_json::to_string(report).map_err(|e| ErrorData::internal_error(format!("serialize: {e}"), None))
}

/// Map a command error chain to an MCP tool error, preserving the full
/// `{:#}` chain in the message (stderr-style, lowercase library wording).
fn tool_error(op: &str, err: &anyhow::Error) -> ErrorData {
    ErrorData::internal_error(format!("{op} failed: {err:#}"), None)
}

/// Run the MCP server over stdio until the client disconnects (stdin EOF).
///
/// # Errors
///
/// A transport setup failure. A clean client disconnect returns
/// `Ok(ExitCode::Success)`.
pub async fn serve(ctx: &Context, args: &McpArgs) -> anyhow::Result<ExitCode> {
    let state = McpState {
        ctx: ctx.clone(),
        allow_writes: args.allow_writes,
        global: args.global,
        config: args.config.clone(),
    };
    let server = GrimMcpServer { inner: Arc::new(state) };
    tracing::info!(allow_writes = args.allow_writes, "grim mcp server starting on stdio");
    let service = server.serve(rmcp::transport::stdio()).await?;
    service.waiting().await?;
    tracing::info!("grim mcp server stopped (client disconnected)");
    Ok(ExitCode::Success)
}
