// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! Shared scope plumbing for `lock` / `install` / `update` / `status`.
//!
//! Each of those commands operates on exactly one scope (global or
//! project; never merged) and needs the same four things: the parsed
//! declaration + options, the config-file path (for the advisory flock),
//! the adjacent lock path, and the install-state file path. This module
//! resolves all four from `--global` / `--config` so the commands stay
//! thin.

use std::path::{Path, PathBuf};

use crate::config::config_error::{ConfigError, ConfigErrorKind};
use crate::config::declaration::{ConfigOptions, DesiredSet};
use crate::config::global_config::GlobalConfig;
use crate::config::project_config::ProjectConfig;
use crate::config::scope::ConfigScope;
use crate::context::Context;
use crate::install::install_state::InstallState;

/// A resolved scope: everything the lock/install/update/status commands
/// need to operate on one declaration.
pub struct ResolvedScope {
    /// Which scope this is.
    pub scope: ConfigScope,
    /// The parsed declaration (skills + rules).
    pub set: DesiredSet,
    /// The parsed options table.
    pub options: ConfigOptions,
    /// The config file path (the advisory flock target).
    pub config_path: PathBuf,
    /// The adjacent lock path.
    pub lock_path: PathBuf,
    /// The install-state file path for this scope.
    pub state_path: PathBuf,
    /// The workspace root install targets are rooted at.
    pub workspace: PathBuf,
}

/// Resolve the scope from the global/config flags.
///
/// Global scope reads `$GRIM_HOME/grimoire.toml` (absent ⇒ empty
/// declaration, not an error). Project scope discovers the config by the
/// explicit `--config` path or by walking up from the working directory.
///
/// # Errors
///
/// Propagates any [`ConfigError`] from discovery / parsing.
pub fn resolve(ctx: &Context, global: bool, config: Option<&Path>) -> Result<ResolvedScope, ConfigError> {
    let paths = ctx.paths();
    if global {
        let config_path = paths.global_config();
        let cfg = GlobalConfig::load(&config_path)?;
        Ok(ResolvedScope {
            scope: ConfigScope::Global,
            set: cfg.set,
            options: cfg.options,
            lock_path: paths.global_lock(),
            state_path: InstallState::global_path(&paths.state_dir()),
            // Global artifacts install under `$GRIM_HOME` so a global
            // declaration is client config that follows the user.
            workspace: paths.root().to_path_buf(),
            config_path,
        })
    } else {
        let discovered = ProjectConfig::discover(config)?;
        let config_path = discovered.config_path().to_path_buf();
        let lock_path = discovered.lock_path();
        let workspace = config_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        let canonical = std::fs::canonicalize(&config_path).unwrap_or_else(|_| config_path.clone());
        Ok(ResolvedScope {
            scope: ConfigScope::Project,
            set: discovered.config.set,
            options: discovered.config.options,
            state_path: InstallState::project_path(&paths.state_dir(), &canonical),
            lock_path,
            workspace,
            config_path,
        })
    }
}

/// Whether the config-file flock can be acquired: a global config that
/// does not exist yet has no file to lock, which is benign for read-only
/// commands and for a first `grim lock` (the lock file write is still
/// atomic). Returns the path to lock, or `None` when there is nothing to
/// lock.
pub fn lockable_config_path(scope: &ResolvedScope) -> Option<PathBuf> {
    if scope.config_path.exists() {
        Some(scope.config_path.clone())
    } else {
        None
    }
}

/// Map a missing-explicit-config discovery failure to the user-facing
/// guidance the commands share. Kept here so the wording is single-source.
pub fn config_not_found(err: &ConfigError) -> bool {
    matches!(err.kind, ConfigErrorKind::NotDiscovered | ConfigErrorKind::Io(_))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::options::{GlobalOptions, OutputFormat};
    use crate::config::project_config::lock_path_for;

    fn opts() -> GlobalOptions {
        GlobalOptions {
            format: OutputFormat::Plain,
            offline: false,
            log_level: None,
            config: None,
            global: false,
            registry: None,
        }
    }

    #[test]
    fn global_scope_resolves_under_grim_home() {
        let dir = tempfile::tempdir().unwrap();
        // SAFETY-free env handling: GlobalOptions carries no GRIM_HOME, so
        // build a Context whose grim_home points into the tempdir by
        // routing through the public constructor with the env unset and
        // asserting on the structural path shape instead.
        let mut o = opts();
        o.global = true;
        let _ = dir;
        let ctx = Context::new(&o);
        let scope = resolve(&ctx, true, None).expect("global resolves with empty config");
        assert_eq!(scope.scope, ConfigScope::Global);
        assert!(scope.set.skills.is_empty());
        assert!(scope.lock_path.ends_with("grimoire.lock"));
        assert!(scope.state_path.ends_with("global.json"));
    }

    #[test]
    fn project_scope_explicit_config_resolves_paths() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("grimoire.toml");
        std::fs::write(&cfg, "[skills]\nx = \"localhost:5000/x:latest\"\n").unwrap();
        let ctx = Context::new(&opts());
        let scope = resolve(&ctx, false, Some(&cfg)).expect("project resolves");
        assert_eq!(scope.scope, ConfigScope::Project);
        assert_eq!(scope.config_path, cfg);
        assert_eq!(scope.lock_path, lock_path_for(&cfg));
        assert_eq!(scope.workspace, dir.path());
        assert_eq!(scope.set.skills.len(), 1);
    }
}
