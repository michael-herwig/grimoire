// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! Global-scope `$GRIM_HOME/grimoire.toml`.
//!
//! Same schema as the project scope (`[options]` + `[skills]` +
//! `[rules]`). The two scopes are independent and never merged. An absent
//! global config is **not** an error — it yields an empty declaration so
//! a fresh install behaves like a config declaring nothing.

use std::path::Path;

use crate::config::config_error::{ConfigError, ConfigErrorKind};
use crate::config::declaration::{ConfigOptions, DesiredSet};
use crate::config::project_config::ProjectConfig;
use crate::config::{self};

/// A parsed global-scope declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlobalConfig {
    /// Options table (`[options]`).
    pub options: ConfigOptions,
    /// The declared skills and rules (empty when the file is absent).
    pub set: DesiredSet,
}

impl GlobalConfig {
    /// Parse from a TOML string (path-less; fixtures / in-memory use).
    pub fn from_toml_str(s: &str) -> Result<Self, ConfigError> {
        let parsed = ProjectConfig::from_toml_str(s)?;
        Ok(Self {
            options: parsed.options,
            set: parsed.set,
        })
    }

    /// Load `$GRIM_HOME/grimoire.toml`.
    ///
    /// An absent file yields an empty config (not an error). Any other
    /// I/O failure, a size-cap violation, or a parse error surfaces as
    /// [`ConfigError`].
    ///
    /// # Errors
    ///
    /// Propagates parse / size / non-not-found I/O failures with path
    /// context.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let content = match config::read_capped(path) {
            Ok(c) => c,
            Err(ConfigError {
                kind: ConfigErrorKind::Io(io),
                ..
            }) if io.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self {
                    options: ConfigOptions::default(),
                    set: DesiredSet::default(),
                });
            }
            Err(other) => return Err(other),
        };
        // Reparse with path context by routing through the shared parser
        // via a path-aware load on the project type, then re-key.
        let parsed = ProjectConfig::from_toml_str(&content).map_err(|e| {
            // `from_toml_str` carries an empty path; re-attach the real
            // file so the diagnostic names the global config.
            ConfigError::new(path, e.kind)
        })?;
        Ok(Self {
            options: parsed.options,
            set: parsed.set,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_file_yields_empty_config() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("grimoire.toml");
        let cfg = GlobalConfig::load(&path).expect("absent ⇒ empty, not error");
        assert!(cfg.set.skills.is_empty());
        assert!(cfg.set.rules.is_empty());
        assert_eq!(cfg.options, ConfigOptions::default());
    }

    #[test]
    fn present_file_parses() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("grimoire.toml");
        std::fs::write(
            &path,
            r#"
[options]
clients = ["claude"]

[rules]
rust-style = "ghcr.io/acme/rules/rust-style:v3"
"#,
        )
        .unwrap();
        let cfg = GlobalConfig::load(&path).expect("parse");
        assert_eq!(cfg.options.clients, vec!["claude".to_string()]);
        assert_eq!(cfg.set.rules.len(), 1);
    }

    #[test]
    fn parse_error_carries_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("grimoire.toml");
        std::fs::write(&path, "surprise = true\n").unwrap();
        let err = GlobalConfig::load(&path).expect_err("unknown field rejects");
        assert!(matches!(err.kind, ConfigErrorKind::TomlParse(_)));
        assert_eq!(err.path, path);
    }
}
