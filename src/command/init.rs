// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! `grim init` — create a fresh `grimoire.toml`.
//!
//! Project scope writes `./grimoire.toml`; `--global` writes
//! `$GRIM_HOME/grimoire.toml`. An existing file is never overwritten
//! (exit 64). The body is an `[options]` table (carrying
//! `default_registry` only when `--registry` is given) plus empty
//! `[skills]` / `[rules]` tables.

use anyhow::Context as _;
use clap::Args;

use crate::api::artifact_status::InitStatus;
use crate::api::init_report::InitReport;
use crate::cli::exit_code::ExitCode;
use crate::config::config_error::{ConfigError, ConfigErrorKind};
use crate::config::scope::ConfigScope;
use crate::context::Context;

/// `grim init` arguments.
#[derive(Debug, Args)]
pub struct InitArgs {
    /// Create the global config (`$GRIM_HOME/grimoire.toml`) instead of a
    /// project-local one.
    #[arg(long)]
    pub global: bool,

    /// Seed `[options].default_registry` with this value.
    #[arg(long)]
    pub registry: Option<String>,
}

/// Run `grim init`.
///
/// # Errors
///
/// Returns a [`ConfigError`] (`ConfigAlreadyExists` ⇒ exit 64, I/O ⇒ 74)
/// if the file exists or cannot be written.
pub async fn run(ctx: &Context, args: &InitArgs) -> anyhow::Result<(InitReport, ExitCode)> {
    let (path, scope) = if args.global {
        (ctx.paths().global_config(), ConfigScope::Global)
    } else {
        let cwd = std::env::current_dir().context("resolving the current directory for `grim init`")?;
        (cwd.join("grimoire.toml"), ConfigScope::Project)
    };

    if path.exists() {
        return Err(crate::error::Error::from(ConfigError::new(path, ConfigErrorKind::ConfigAlreadyExists)).into());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| crate::error::Error::from(ConfigError::new(&path, ConfigErrorKind::Io(e))))?;
    }

    let body = render_config(args.registry.as_deref());
    std::fs::write(&path, body)
        .map_err(|e| crate::error::Error::from(ConfigError::new(&path, ConfigErrorKind::Io(e))))?;

    let report = InitReport::new(path, scope, InitStatus::Created);
    Ok((report, ExitCode::Success))
}

/// Render the seed config. `[options]` is emitted only when there is
/// something to put in it (a registry); the clients list stays unset so the
/// install default (`claude`) applies.
fn render_config(registry: Option<&str>) -> String {
    let mut out = String::new();
    if let Some(reg) = registry {
        out.push_str("[options]\n");
        out.push_str(&format!("default_registry = \"{reg}\"\n\n"));
    }
    out.push_str("[skills]\n\n[rules]\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_includes_registry_when_present() {
        let body = render_config(Some("ghcr.io/acme"));
        assert!(body.contains("[options]"));
        assert!(body.contains("default_registry = \"ghcr.io/acme\""));
        assert!(body.contains("[skills]"));
        assert!(body.contains("[rules]"));
    }

    #[test]
    fn render_omits_options_table_without_registry() {
        let body = render_config(None);
        assert!(!body.contains("[options]"));
        assert!(body.starts_with("[skills]"));
        assert!(body.contains("[rules]"));
        // The seed must parse back as a valid (empty) config.
        let cfg = crate::config::project_config::ProjectConfig::from_toml_str(&body).unwrap();
        assert!(cfg.set.skills.is_empty());
        assert!(cfg.set.rules.is_empty());
    }
}
