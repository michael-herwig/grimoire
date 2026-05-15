// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! Post-parse application entry point.
//!
//! `main.rs` keeps clap parsing and the `EX_USAGE` (64) mapping; once a
//! [`Cli`] is parsed, all real work happens here. Phase 1 only handles the
//! existing `version` / bare-invocation behaviour, returning a typed
//! [`ExitCode`].

use std::io::Write;

use crate::cli::exit_code::ExitCode;
use crate::context::Context;
use crate::{Cli, Command};

/// Runs the parsed CLI and returns the exit code to surface.
///
/// # Errors
///
/// Returns any error a command produces; `main.rs` logs it with `{err:#}`
/// and classifies it into an exit code.
pub async fn run(cli: Cli) -> anyhow::Result<ExitCode> {
    // Built once per invocation. Unused in Phase 1's two paths, but the
    // construction itself is the seam later commands hang off of.
    let _ctx = Context::new(&cli.global);

    match cli.command {
        Some(Command::Version) => {
            let mut out = std::io::stdout().lock();
            writeln!(out, "grim {}", env!("CARGO_PKG_VERSION"))?;
            Ok(ExitCode::Success)
        }
        None => {
            // Bare `grim` prints help and exits successfully so backend
            // callers get a stable, zero-exit discovery path.
            use clap::CommandFactory;
            let mut cmd = Cli::command();
            cmd.print_help()?;
            println!();
            Ok(ExitCode::Success)
        }
    }
}
