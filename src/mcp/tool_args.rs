// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! Input argument schemas for the `grim mcp` tools.
//!
//! Distinct from the clap `*Args` structs (which derive `clap::Args`): these
//! derive `serde::Deserialize` + `schemars::JsonSchema` so rmcp can publish a
//! JSON Schema and validate each `tools/call`. They carry only the
//! agent-supplied parameters — the install scope is fixed at server start
//! (see [`super::state::McpState`]), never per call.

use rmcp::schemars;
use serde::Deserialize;

/// Arguments for the `grim_search` tool.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SearchToolArgs {
    /// Search terms, whitespace-split and ANDed: each term substring-matches
    /// (case-insensitive) any of kind / repo / summary / description /
    /// keywords. A bare kind keyword (`skill`/`rule`/`bundle`/`agent`, singular
    /// or plural) filters by kind. Omit to list the whole catalog.
    #[serde(default)]
    pub query: Option<String>,

    /// Force a fresh catalog rebuild even if the cache is still warm.
    #[serde(default)]
    pub refresh: Option<bool>,
    // No `registry` override is exposed: the tool deliberately browses only the
    // registries the server's scope was configured with (`[[registries]]` +
    // fallback). Honoring an arbitrary agent-supplied registry would let a
    // prompt-injected agent point grim at an unconfigured host (SSRF, CWE-918);
    // the configured set is the security boundary (plan: "only configured
    // registries by default").
}
