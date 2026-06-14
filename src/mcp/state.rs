// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! Shared state for the `grim mcp` server.
//!
//! Built once at server start and shared (behind an `Arc`) across every
//! concurrent tool call. The install **scope** is fixed here at start
//! (`--global` / `--config`) rather than read per call, so an agent cannot
//! redirect a project-scoped session into global (`~/.claude`) writes — every
//! tool operates within the one scope the server was launched in.

use std::path::PathBuf;

use crate::context::Context;

/// Server-wide state shared by all tool handlers.
pub struct McpState {
    /// The per-invocation context (env-derived paths, registry flag/env,
    /// offline). Cheap to clone; the tools reuse it for every command.
    pub ctx: Context,
    /// Whether mutating tools are enabled. When `false` the write tools are
    /// neither advertised nor callable. No write tools are registered yet —
    /// this is the gate they will check once they land (Phase 2); until then
    /// the read-only surface is identical with or without `--allow-writes`.
    pub allow_writes: bool,
    /// The fixed scope: `--global` selects the global scope.
    pub global: bool,
    /// The fixed explicit project config path, if one was given.
    pub config: Option<PathBuf>,
}
