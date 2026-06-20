// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! `grim` — an OCI-backed package manager for AI skills and rules.
//!
//! `main` owns clap parsing and the usage-error mapping; everything after
//! a successful parse is delegated to [`app::run`].

// Phase 1 lands the domain core, error taxonomy, exit codes, and output
// layer ahead of the commands that consume them (Phases 2–6). These APIs
// are exercised by their own unit tests but have no production call site
// yet; the allow is removed as later phases wire them in.
#![allow(dead_code)]
// `unwrap_used`/`expect_used` are library-style discipline for production
// code; tests are explicitly permitted to unwrap (quality-rust.md). The
// restriction lints do not auto-skip the test target under
// `--all-targets`, so scope the allowance to `cfg(test)` here.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

mod api;
mod app;
mod auth;
mod catalog;
mod cli;
mod command;
mod config;
mod context;
mod env;
mod error;
mod install;
mod lock;
mod log_switch;
mod mcp;
mod oci;
mod resolve;
mod skill;
mod store;
mod tui;

use clap::error::ErrorKind;
use clap::{Parser, Subcommand};

use crate::cli::exit_code::ExitCode;
use crate::cli::options::GlobalOptions;
use crate::command::add::AddArgs;
use crate::command::build::BuildArgs;
use crate::command::init::InitArgs;
use crate::command::install::InstallArgs;
use crate::command::lock::LockArgs;
use crate::command::login::LoginArgs;
use crate::command::logout::LogoutArgs;
use crate::command::mcp::McpArgs;
use crate::command::publish::PublishArgs;
use crate::command::release::ReleaseArgs;
use crate::command::remove::RemoveArgs;
use crate::command::schema::SchemaArgs;
use crate::command::search::SearchArgs;
use crate::command::status::StatusArgs;
use crate::command::tui::TuiArgs;
use crate::command::uninstall::UninstallArgs;
use crate::command::update::UpdateArgs;
use crate::error::classify_error;

#[derive(Parser)]
#[command(
    name = "grim",
    version,
    about = "An OCI-backed package manager for AI skills and rules",
    long_about = None
)]
pub struct Cli {
    #[command(flatten)]
    pub global: GlobalOptions,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand)]
pub enum Command {
    /// Create a fresh `grimoire.toml`.
    Init(InitArgs),
    /// Resolve declared floating tags to pinned digests in `grimoire.lock`.
    Lock(LockArgs),
    /// Materialize the locked artifacts into the configured AI client(s).
    Install(InstallArgs),
    /// Re-resolve floating tags and re-materialize changed artifacts.
    Update(UpdateArgs),
    /// Report the state of every declared artifact.
    Status(StatusArgs),
    /// Validate and pack a local skill/rule (no push).
    Build(BuildArgs),
    /// Validate, pack, and push a skill/rule with cascade tags.
    Release(ReleaseArgs),
    /// Publish a set of skills/rules/agents/bundles from a manifest.
    Publish(PublishArgs),
    /// Declare a skill/rule in the config and lock it.
    Add(AddArgs),
    /// Undeclare a skill/rule from the config and lock.
    Remove(RemoveArgs),
    /// Fully remove an installed skill/rule: delete files, drop the
    /// install record, and undeclare it from the config and lock.
    Uninstall(UninstallArgs),
    /// Search the registry catalog for skills and rules.
    Search(SearchArgs),
    /// Print the JSON Schema for grimoire.toml or publish.toml.
    Schema(SchemaArgs),
    /// Browse the registry catalog in an interactive TUI.
    Tui(TuiArgs),
    /// Run a local STDIO Model Context Protocol server.
    Mcp(McpArgs),
    /// Authenticate to a registry and store the credential.
    Login(LoginArgs),
    /// Remove a stored registry credential.
    Logout(LogoutArgs),
}

fn main() -> std::process::ExitCode {
    init_tracing();

    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(err) => {
            // Help/version are a successful, intentional invocation; every
            // other parse failure is a usage error → EX_USAGE (64), not
            // clap's default 2.
            let _ = err.print();
            return match err.kind() {
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion => ExitCode::Success.into(),
                _ => ExitCode::UsageError.into(),
            };
        }
    };

    let runtime = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(err) => {
            tracing::error!("failed to start async runtime: {err}");
            return ExitCode::Failure.into();
        }
    };

    match runtime.block_on(app::run(cli)) {
        Ok(code) => code.into(),
        Err(err) => {
            // Full chain via the alternate format, printed exactly once on
            // stderr (a `tracing::error!` here would duplicate the line —
            // the default filter also writes to stderr).
            eprintln!("{err:#}");
            classify_error(&err).into()
        }
    }
}

/// Initialize tracing from the `GRIM_LOG` env var (falls back to `warn`).
///
/// Installs a [`crate::log_switch::SwitchableWriter`] so the TUI can
/// redirect log output to a file while alt-screen is active, then restore
/// stderr on exit. The writer is stored in the process-global
/// [`crate::log_switch::GLOBAL_WRITER`] so TUI code retrieves it without
/// threading it through every call frame.
fn init_tracing() {
    use tracing_subscriber::EnvFilter;
    use tracing_subscriber::prelude::*;

    let filter = EnvFilter::try_from_env("GRIM_LOG").unwrap_or_else(|_| EnvFilter::new("warn"));
    let writer = crate::log_switch::SwitchableWriter::new();
    // Store in the global before installing the subscriber so any code
    // that calls `global_writer()` immediately after init_tracing() finds
    // it. The OnceLock guarantees the assignment happens at most once.
    let stored = crate::log_switch::set_global_writer(writer);

    // Build and install the subscriber. `try_init` is used so a
    // second call (e.g., in a test binary that also calls init_tracing)
    // silently returns the error rather than panicking.
    let _ = tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(stored.clone())
                .with_filter(filter),
        )
        .try_init();
}
