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
use crate::config::declaration::{ConfigOptions, DesiredSet, RegistryConfig};
use crate::config::project_config::ProjectConfig;
use crate::config::{self};

/// A parsed global-scope declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlobalConfig {
    /// Options table (`[options]`).
    pub options: ConfigOptions,
    /// The declared registries (`[[registries]]`); empty when none declared.
    pub registries: Vec<RegistryConfig>,
    /// The declared skills and rules (empty when the file is absent).
    pub set: DesiredSet,
}

impl GlobalConfig {
    /// Parse from a TOML string (path-less; fixtures / in-memory use).
    pub fn from_toml_str(s: &str) -> Result<Self, ConfigError> {
        let parsed = ProjectConfig::from_toml_str(s)?;
        Ok(Self {
            options: parsed.options,
            registries: parsed.registries,
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
                    registries: Vec::new(),
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
            registries: parsed.registries,
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

    #[test]
    fn global_registries_duplicate_alias_rejected_via_from_toml_str() {
        let err = GlobalConfig::from_toml_str(
            r#"
[[registries]]
alias = "acme"
url = "ghcr.io/acme"

[[registries]]
alias = "acme"
url = "registry.corp/team"
"#,
        )
        .expect_err("duplicate alias must reject for global config");
        assert!(matches!(err.kind, ConfigErrorKind::RegistryInvalid { .. }));
    }

    #[test]
    fn global_registries_duplicate_alias_rejected_via_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("grimoire.toml");
        std::fs::write(
            &path,
            r#"
[[registries]]
alias = "acme"
url = "ghcr.io/acme"

[[registries]]
alias = "acme"
url = "registry.corp/team"
"#,
        )
        .unwrap();
        let err = GlobalConfig::load(&path).expect_err("duplicate alias must reject for global config load");
        assert!(matches!(err.kind, ConfigErrorKind::RegistryInvalid { .. }));
        assert_eq!(err.path, path);
    }

    #[test]
    fn global_registries_alias_with_slash_rejected() {
        let err = GlobalConfig::from_toml_str(
            r#"
[[registries]]
alias = "a/b"
url = "ghcr.io/acme"
"#,
        )
        .expect_err("alias with '/' must reject for global config");
        assert!(matches!(err.kind, ConfigErrorKind::RegistryInvalid { .. }));
    }

    #[test]
    fn global_registries_alias_with_whitespace_rejected() {
        let err = GlobalConfig::from_toml_str(
            r#"
[[registries]]
alias = " acme"
url = "ghcr.io/acme"
"#,
        )
        .expect_err("alias with whitespace must reject for global config");
        assert!(matches!(err.kind, ConfigErrorKind::RegistryInvalid { .. }));
    }

    #[test]
    fn global_registries_valid_multi_registry_accepted() {
        let cfg = GlobalConfig::from_toml_str(
            r#"
[[registries]]
alias = "acme"
url = "ghcr.io/acme"
default = true

[[registries]]
alias = "corp"
url = "registry.corp/team"
"#,
        )
        .expect("valid multi-registry global config must parse");
        assert_eq!(cfg.registries.len(), 2);
        assert_eq!(cfg.registries[0].alias.as_deref(), Some("acme"));
        assert_eq!(cfg.registries[1].alias.as_deref(), Some("corp"));
    }
}
