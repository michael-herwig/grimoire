// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! The `grim mcp` local STDIO Model Context Protocol server.
//!
//! Exposes Grimoire's catalog and install state to an AI agent as MCP tools,
//! built on the official `rmcp` SDK. Read tools wrap the existing
//! `command::*::run` seams (so the MCP payload equals the CLI `--format json`
//! output); mutating tools are gated behind `--allow-writes`. The install
//! scope is fixed at server start.

pub mod server;
pub mod state;
pub mod tool_args;
