// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! Post-parse application entry point.
//!
//! `main.rs` keeps clap parsing and the `EX_USAGE` (64) mapping; once a
//! [`Cli`] is parsed, all real work happens here: build the per-invocation
//! [`Context`], dispatch the subcommand, render the resulting report
//! through [`Printable`] honouring `--format`, and surface the typed
//! [`ExitCode`].

use std::io::Write;

use crate::cli::exit_code::ExitCode;
use crate::cli::options::OutputFormat;
use crate::cli::printer::Printable;
use crate::context::Context;
use crate::{Cli, Command};

/// Runs the parsed CLI and returns the exit code to surface.
///
/// # Errors
///
/// Returns any error a command produces; `main.rs` logs it with `{err:#}`
/// and classifies it into an exit code.
pub async fn run(cli: Cli) -> anyhow::Result<ExitCode> {
    let ctx = Context::new(&cli.global);
    let format = cli.global.format;

    let Some(command) = cli.command else {
        // Bare `grim` prints help and exits successfully so backend
        // callers get a stable, zero-exit discovery path.
        use clap::CommandFactory;
        let mut cmd = Cli::command();
        cmd.print_help()?;
        println!();
        return Ok(ExitCode::Success);
    };

    // `Printable` has generic methods (not object-safe), so render inside
    // each arm with the concrete report type rather than boxing.
    let code = match command {
        Command::Init(args) => {
            let (r, c) = crate::command::init::run(&ctx, &args).await?;
            render(&r, format)?;
            c
        }
        Command::Lock(args) => {
            let (r, c) = crate::command::lock::run(&ctx, &args).await?;
            render(&r, format)?;
            c
        }
        Command::Install(args) => {
            let (r, c) = crate::command::install::run(&ctx, &args).await?;
            render(&r, format)?;
            c
        }
        Command::Update(args) => {
            let (r, c) = crate::command::update::run(&ctx, &args).await?;
            render(&r, format)?;
            c
        }
        Command::Status(args) => {
            let (r, c) = crate::command::status::run(&ctx, &args).await?;
            render(&r, format)?;
            c
        }
        Command::Build(args) => {
            let (r, c) = crate::command::build::run(&ctx, &args).await?;
            render(&r, format)?;
            c
        }
        Command::Release(args) => {
            let (r, c) = crate::command::release::run(&ctx, &args).await?;
            render(&r, format)?;
            c
        }
        Command::Publish(args) => {
            let (r, c) = crate::command::publish::run(&ctx, &args).await?;
            render(&r, format)?;
            c
        }
        Command::Add(args) => {
            let (r, c) = crate::command::add::run(&ctx, &args).await?;
            render(&r, format)?;
            c
        }
        Command::Remove(args) => {
            let (r, c) = crate::command::remove::run(&ctx, &args).await?;
            render(&r, format)?;
            c
        }
        Command::Uninstall(args) => {
            let (r, c) = crate::command::uninstall::run(&ctx, &args).await?;
            render(&r, format)?;
            c
        }
        Command::Search(args) => {
            let (r, c) = crate::command::search::run(&ctx, &args).await?;
            render(&r, format)?;
            c
        }
        Command::Login(args) => {
            let (r, c) = crate::command::login::run(&ctx, &args).await?;
            render(&r, format)?;
            c
        }
        Command::Logout(args) => {
            let (r, c) = crate::command::logout::run(&ctx, &args).await?;
            render(&r, format)?;
            c
        }
        // `tui` diverges into a full-screen session: it owns the terminal
        // and emits no structured report (exempt from `Printable`).
        Command::Tui(args) => crate::command::tui::run(&ctx, &args).await?,
    };

    Ok(code)
}

/// Render `report` to stdout in the requested format.
fn render<R: Printable>(report: &R, format: OutputFormat) -> std::io::Result<()> {
    let mut out = std::io::stdout().lock();
    match format {
        OutputFormat::Plain => report.print_plain(&mut out)?,
        OutputFormat::Json => report.print_json(&mut out)?,
    }
    out.flush()
}
