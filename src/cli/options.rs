// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! Global CLI options, flattened into the top-level clap command.
//!
//! These flags are shared by every subcommand. Resolution-affecting flags
//! (`--offline`, `--config`, `--registry`) influence which artifacts are
//! looked up; presentation flags (`--format`, `--log-level`) only affect
//! rendering. By default Grimoire resolves floating tags fresh from the
//! registry (online); `--offline` restricts it to the cache.

use std::path::PathBuf;

use clap::{Args, ValueEnum};

/// Output rendering format for structured command results.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
#[clap(rename_all = "lowercase")]
pub enum OutputFormat {
    /// Human-readable aligned table.
    #[default]
    Plain,
    /// Machine-readable pretty JSON.
    Json,
}

impl std::fmt::Display for OutputFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Plain => "plain",
            Self::Json => "json",
        })
    }
}

/// Options available on every `grim` invocation.
///
/// Flattened into the top-level command via `#[command(flatten)]` so the
/// flags work positionally before or after a subcommand.
#[derive(Debug, Clone, Args)]
pub struct GlobalOptions {
    /// Output format for structured results.
    #[arg(long, value_enum, default_value_t = OutputFormat::Plain, global = true)]
    pub format: OutputFormat,

    /// Disable all network access; work from the cache only and fail
    /// rather than reach a registry.
    #[arg(long, global = true)]
    pub offline: bool,

    /// Override the tracing log level (e.g. `warn`, `info`, `debug`).
    #[arg(long, global = true)]
    pub log_level: Option<String>,

    /// Path to an explicit project config file.
    #[arg(long, global = true)]
    pub config: Option<PathBuf>,

    /// Operate on the global scope rather than the discovered project.
    #[arg(long, global = true)]
    pub global: bool,

    /// Default registry for short identifiers without an explicit registry.
    #[arg(long, global = true)]
    pub registry: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_format_default_is_plain() {
        assert_eq!(OutputFormat::default(), OutputFormat::Plain);
    }

    #[test]
    fn output_format_display_round_trips_value_enum() {
        for fmt in [OutputFormat::Plain, OutputFormat::Json] {
            let rendered = fmt.to_string();
            let parsed =
                OutputFormat::from_str(&rendered, true).unwrap_or_else(|_| panic!("'{rendered}' should parse back"));
            assert_eq!(parsed, fmt);
        }
    }
}
