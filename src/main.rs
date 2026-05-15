// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! `grim` — an OCI-backed package manager for AI skills and rules.
//!
//! This is the initial scaffold: a minimal clap CLI shell. Real commands
//! (add / install / publish / pull) are added by subsequent feature work.

use std::process::ExitCode;

use clap::error::ErrorKind;
use clap::{Parser, Subcommand};

/// `EX_USAGE` from sysexits.h — the conventional exit code for a command-line
/// usage error, so backend callers can distinguish "you invoked me wrong"
/// from a runtime failure.
const EX_USAGE: u8 = 64;

#[derive(Parser)]
#[command(
    name = "grim",
    version,
    about = "An OCI-backed package manager for AI skills and rules",
    long_about = None
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Print version information.
    Version,
}

fn main() -> ExitCode {
    init_tracing();

    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(err) => {
            // Help/version are a successful, intentional invocation; every
            // other parse failure is a usage error → EX_USAGE (64), not
            // clap's default 2.
            let _ = err.print();
            return match err.kind() {
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion => ExitCode::SUCCESS,
                _ => ExitCode::from(EX_USAGE),
            };
        }
    };

    match cli.command {
        Some(Command::Version) => {
            println!("grim {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        None => {
            // Bare `grim` prints help and exits successfully so backend
            // callers get a stable, zero-exit discovery path.
            use clap::CommandFactory;
            let _ = Cli::command().print_help();
            println!();
            ExitCode::SUCCESS
        }
    }
}

/// Initialize tracing from the `GRIM_LOG` env var (falls back to `warn`).
fn init_tracing() {
    use tracing_subscriber::EnvFilter;

    let filter = EnvFilter::try_from_env("GRIM_LOG").unwrap_or_else(|_| EnvFilter::new("warn"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
}
