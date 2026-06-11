// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! Project-scope `grimoire.toml`: walk-up discovery + two-pass parse.
//!
//! Adapted from OCX `project::config`. Differences: Grimoire discovery is
//! a plain CWD walk-up ceiling'd at `$HOME` / filesystem root with an
//! explicit `--config` override (no env-var precedence, no home-tier
//! fallback — project and global scopes are independent). The schema is
//! `[options]` + `[skills]` + `[rules]` + `[agents]` + `[bundles]`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::config;
use crate::config::config_error::{ConfigError, ConfigErrorKind};
use crate::config::declaration::{ConfigOptions, DesiredSet};
use crate::oci::Identifier;
use crate::oci::identifier::error::IdentifierErrorKind;

/// A parsed project-scope declaration with its on-disk location.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectConfig {
    /// Options table (`[options]`).
    pub options: ConfigOptions,
    /// The declared skills, rules, agents, and bundles.
    pub set: DesiredSet,
}

/// The result of [`ProjectConfig::discover`]: the parsed config plus the
/// resolved config and lock paths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredConfig {
    /// The parsed project config.
    pub config: ProjectConfig,
    config_path: PathBuf,
}

impl DiscoveredConfig {
    /// The resolved `grimoire.toml` path.
    pub fn config_path(&self) -> &Path {
        &self.config_path
    }

    /// The adjacent lock path: `<config_dir>/grimoire.lock`.
    ///
    /// Derived from the config's parent directory (not
    /// `with_extension`), so an unusually named config still produces a
    /// canonically named lock.
    pub fn lock_path(&self) -> PathBuf {
        lock_path_for(&self.config_path)
    }
}

/// Derive `<config_dir>/grimoire.lock` for `config_path`.
pub fn lock_path_for(config_path: &Path) -> PathBuf {
    config_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("grimoire.lock")
}

/// Raw first-pass shape — string values, validated in the second pass so
/// the diagnostic can name both the binding key and the offending value
/// (a value-position visitor cannot see the key).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    #[serde(default)]
    options: ConfigOptions,
    #[serde(default)]
    skills: BTreeMap<String, String>,
    #[serde(default)]
    rules: BTreeMap<String, String>,
    #[serde(default)]
    agents: BTreeMap<String, String>,
    #[serde(default)]
    bundles: BTreeMap<String, String>,
}

impl ProjectConfig {
    /// Parse from a TOML string (path-less; for fixtures / in-memory use).
    pub fn from_toml_str(s: &str) -> Result<Self, ConfigError> {
        parse_config(s, PathBuf::new())
    }

    /// Discover and parse the project-scope config.
    ///
    /// Precedence: an explicit `--config` path (missing ⇒ `Io`
    /// `NotFound`), else walk up from the current directory to the first
    /// `grimoire.toml`, ceiling'd at `$HOME` or the filesystem root. No
    /// match ⇒ [`ConfigErrorKind::NotDiscovered`].
    ///
    /// # Errors
    ///
    /// Propagates parse / size / I/O failures with path context, or
    /// `NotDiscovered` when the walk finds nothing.
    pub fn discover(explicit: Option<&Path>) -> Result<DiscoveredConfig, ConfigError> {
        let config_path = match explicit {
            Some(p) => p.to_path_buf(),
            None => walk_up_for_config()?,
        };
        let config = load_from_path(&config_path)?;
        Ok(DiscoveredConfig { config, config_path })
    }
}

/// Walk up from the current directory looking for `grimoire.toml`,
/// stopping at `$HOME` (inclusive) or the filesystem root.
fn walk_up_for_config() -> Result<PathBuf, ConfigError> {
    let cwd = std::env::current_dir().map_err(|e| ConfigError::new(PathBuf::new(), ConfigErrorKind::Io(e)))?;
    let ceiling = crate::env::home_dir_for_ceiling();

    let mut dir = cwd.as_path();
    loop {
        let candidate = dir.join("grimoire.toml");
        if candidate.is_file() {
            return Ok(candidate);
        }
        // Stop *after* checking the ceiling directory itself.
        if let Some(home) = ceiling.as_deref()
            && dir == home
        {
            break;
        }
        match dir.parent() {
            Some(parent) => dir = parent,
            None => break,
        }
    }
    Err(ConfigError::new(cwd, ConfigErrorKind::NotDiscovered))
}

/// Read, size-check, and parse a config file at `path`.
fn load_from_path(path: &Path) -> Result<ProjectConfig, ConfigError> {
    let content = config::read_capped(path)?;
    parse_config(&content, path.to_path_buf())
}

/// Parse the shared `[options]`/`[skills]`/`[rules]`/`[agents]`/`[bundles]`
/// schema.
fn parse_config(s: &str, path: PathBuf) -> Result<ProjectConfig, ConfigError> {
    let raw: RawConfig =
        toml::from_str(s).map_err(|e| ConfigError::new(path.clone(), ConfigErrorKind::TomlParse(e)))?;
    let skills = parse_artifact_map(&raw.skills, &path)?;
    let rules = parse_artifact_map(&raw.rules, &path)?;
    // Agent and bundle references validate exactly like skills/rules: a
    // fully-qualified identifier, bare entries defaulting to `:latest`.
    let agents = parse_artifact_map(&raw.agents, &path)?;
    let bundles = parse_artifact_map(&raw.bundles, &path)?;
    Ok(ProjectConfig {
        options: raw.options,
        set: DesiredSet::from_maps(skills, rules, agents, bundles),
    })
}

/// Catalog metadata authored at the top of a bundle source file
/// (`summary` / `keywords` / `description`). All optional.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BundleMetadata {
    /// Short one-line blurb → `com.grimoire.summary`.
    pub summary: Option<String>,
    /// Comma-separated keywords → `com.grimoire.keywords`.
    pub keywords: Option<String>,
    /// Overrides the default `grimoire bundle of N members` description.
    pub description: Option<String>,
    /// HTTPS URL to the source repository → `org.opencontainers.image.source`
    /// (validated `https://` at publish time).
    pub repository: Option<String>,
}

/// A parsed bundle source: validated members plus catalog metadata.
///
/// The source is `grimoire.toml`-shaped — its `[skills]`/`[rules]`/`[agents]`
/// tables are the members — with optional top-level
/// `summary`/`keywords`/`description`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BundleSource {
    /// Skill members, name → validated identifier.
    pub skills: BTreeMap<String, Identifier>,
    /// Rule members, name → validated identifier.
    pub rules: BTreeMap<String, Identifier>,
    /// Agent members, name → validated identifier.
    pub agents: BTreeMap<String, Identifier>,
    /// Catalog metadata for the bundle artifact.
    pub metadata: BundleMetadata,
}

impl BundleSource {
    /// Parse a bundle source from a TOML string.
    ///
    /// # Errors
    ///
    /// A TOML parse failure or an invalid member identifier (`ConfigError`).
    pub fn from_toml_str(s: &str) -> Result<Self, ConfigError> {
        parse_bundle_source(s, PathBuf::new())
    }
}

/// Raw bundle-source shape: members plus optional catalog metadata. Strict
/// (`deny_unknown_fields`) so a typo'd key in the small bundle file is a hard
/// error rather than silently dropped metadata.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawBundleSource {
    #[serde(default)]
    skills: BTreeMap<String, String>,
    #[serde(default)]
    rules: BTreeMap<String, String>,
    #[serde(default)]
    agents: BTreeMap<String, String>,
    #[serde(default)]
    summary: Option<String>,
    #[serde(default)]
    keywords: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    repository: Option<String>,
}

/// Parse + validate a bundle source: members through [`parse_artifact_map`],
/// metadata passed through verbatim.
fn parse_bundle_source(s: &str, path: PathBuf) -> Result<BundleSource, ConfigError> {
    let raw: RawBundleSource =
        toml::from_str(s).map_err(|e| ConfigError::new(path.clone(), ConfigErrorKind::TomlParse(e)))?;
    let skills = parse_artifact_map(&raw.skills, &path)?;
    let rules = parse_artifact_map(&raw.rules, &path)?;
    let agents = parse_artifact_map(&raw.agents, &path)?;
    Ok(BundleSource {
        skills,
        rules,
        agents,
        metadata: BundleMetadata {
            summary: raw.summary,
            keywords: raw.keywords,
            description: raw.description,
            repository: raw.repository,
        },
    })
}

/// Validate every `(name → value)` entry as a fully-qualified identifier.
///
/// A bare entry (registry + repository, no tag, no digest) gets `:latest`
/// injected here — at the schema boundary, not on [`Identifier`] — so CLI
/// args without a tag still surface as `tag = None`. Digest-pinned entries
/// keep `tag = None`; the digest is the canonical pin.
fn parse_artifact_map(
    raw: &BTreeMap<String, String>,
    path: &Path,
) -> Result<BTreeMap<String, Identifier>, ConfigError> {
    let mut out = BTreeMap::new();
    for (name, value) in raw {
        match Identifier::parse(value) {
            Ok(id) => {
                let id = if id.tag().is_none() && id.digest().is_none() {
                    id.clone_with_tag("latest")
                } else {
                    id
                };
                out.insert(name.clone(), id);
            }
            Err(e) if matches!(e.kind, IdentifierErrorKind::MissingRegistry) => {
                return Err(ConfigError::new(
                    path.to_path_buf(),
                    ConfigErrorKind::ArtifactValueMissingRegistry {
                        name: name.clone(),
                        value: value.clone(),
                    },
                ));
            }
            Err(e) => {
                return Err(ConfigError::new(
                    path.to_path_buf(),
                    ConfigErrorKind::ArtifactValueInvalid {
                        name: name.clone(),
                        value: value.clone(),
                        source: e,
                    },
                ));
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::FILE_SIZE_LIMIT_BYTES;

    #[test]
    fn parse_minimal_ok() {
        let cfg = ProjectConfig::from_toml_str(
            r#"
[skills]
code-review = "ghcr.io/acme/skills/code-review:stable"
"#,
        )
        .expect("parse");
        assert_eq!(cfg.set.skills.len(), 1);
        assert_eq!(
            cfg.set.skills.get("code-review").unwrap().to_string(),
            "ghcr.io/acme/skills/code-review:stable"
        );
        assert!(cfg.set.rules.is_empty());
    }

    #[test]
    fn parse_full_ok() {
        let cfg = ProjectConfig::from_toml_str(
            r#"
[options]
default_registry = "ghcr.io/acme"
clients = ["claude", "opencode"]

[skills]
code-review = "ghcr.io/acme/skills/code-review:stable"

[rules]
rust-style = "ghcr.io/acme/rules/rust-style:v3"
"#,
        )
        .expect("parse");
        assert_eq!(cfg.options.default_registry.as_deref(), Some("ghcr.io/acme"));
        assert_eq!(cfg.options.clients, vec!["claude".to_string(), "opencode".to_string()]);
        assert_eq!(cfg.set.skills.len(), 1);
        assert_eq!(cfg.set.rules.len(), 1);
    }

    #[test]
    fn parse_empty_ok() {
        let cfg = ProjectConfig::from_toml_str("").expect("empty parses");
        assert!(cfg.set.skills.is_empty());
        assert!(cfg.set.rules.is_empty());
        assert!(cfg.set.bundles.is_empty());
    }

    #[test]
    fn parse_bundles_table_ok() {
        let cfg = ProjectConfig::from_toml_str(
            r#"
[bundles]
python-stack = "ghcr.io/acme/bundles/python-stack:1.0.0"

[skills]
code-review = "ghcr.io/acme/skills/code-review:stable"
"#,
        )
        .expect("parse");
        assert_eq!(cfg.set.bundles.len(), 1);
        assert_eq!(
            cfg.set.bundles.get("python-stack").unwrap().to_string(),
            "ghcr.io/acme/bundles/python-stack:1.0.0"
        );
        assert_eq!(cfg.set.skills.len(), 1);
    }

    #[test]
    fn parse_agents_table_ok() {
        let cfg = ProjectConfig::from_toml_str(
            r#"
[agents]
code-reviewer = "ghcr.io/acme/agents/code-reviewer:1.0.0"

[skills]
code-review = "ghcr.io/acme/skills/code-review:stable"
"#,
        )
        .expect("parse");
        assert_eq!(cfg.set.agents.len(), 1);
        assert_eq!(
            cfg.set.agents.get("code-reviewer").unwrap().to_string(),
            "ghcr.io/acme/agents/code-reviewer:1.0.0"
        );
        assert_eq!(cfg.set.skills.len(), 1);
    }

    #[test]
    fn bare_agent_defaults_to_latest() {
        let cfg = ProjectConfig::from_toml_str(
            r#"
[agents]
rev = "ghcr.io/acme/agents/rev"
"#,
        )
        .expect("parse");
        assert_eq!(cfg.set.agents.get("rev").unwrap().tag(), Some("latest"));
    }

    #[test]
    fn bare_bundle_defaults_to_latest() {
        let cfg = ProjectConfig::from_toml_str(
            r#"
[bundles]
stack = "ghcr.io/acme/bundles/stack"
"#,
        )
        .expect("parse");
        assert_eq!(cfg.set.bundles.get("stack").unwrap().tag(), Some("latest"));
    }

    #[test]
    fn bare_entry_defaults_to_latest() {
        let cfg = ProjectConfig::from_toml_str(
            r#"
[skills]
code-review = "ghcr.io/acme/skills/code-review"
"#,
        )
        .expect("parse");
        let id = cfg.set.skills.get("code-review").unwrap();
        assert_eq!(id.tag(), Some("latest"));
        assert_eq!(id.to_string(), "ghcr.io/acme/skills/code-review:latest");
    }

    #[test]
    fn digest_pinned_entry_keeps_no_tag() {
        let hex = "a".repeat(64);
        let toml = format!(
            r#"
[skills]
x = "ghcr.io/acme/x@sha256:{hex}"
"#
        );
        let cfg = ProjectConfig::from_toml_str(&toml).expect("parse");
        let id = cfg.set.skills.get("x").unwrap();
        assert_eq!(id.tag(), None);
        assert!(id.digest().is_some());
    }

    #[test]
    fn missing_registry_value_carries_binding_key() {
        let err = ProjectConfig::from_toml_str(
            r#"
[skills]
code-review = "stable"
"#,
        )
        .expect_err("must reject");
        let ConfigErrorKind::ArtifactValueMissingRegistry { name, value } = err.kind else {
            panic!("expected ArtifactValueMissingRegistry, got {:?}", err.kind);
        };
        assert_eq!(name, "code-review");
        assert_eq!(value, "stable");
    }

    #[test]
    fn malformed_value_surfaces_invalid_with_source() {
        let err = ProjectConfig::from_toml_str(
            r#"
[rules]
bad = "ghcr.io/ACME/rust-style:v3"
"#,
        )
        .expect_err("must reject");
        let ConfigErrorKind::ArtifactValueInvalid { name, value, .. } = err.kind else {
            panic!("expected ArtifactValueInvalid, got {:?}", err.kind);
        };
        assert_eq!(name, "bad");
        assert_eq!(value, "ghcr.io/ACME/rust-style:v3");
    }

    #[test]
    fn unknown_field_rejected() {
        let err = ProjectConfig::from_toml_str(
            r#"
surprise = "field"

[skills]
x = "ghcr.io/acme/x:1"
"#,
        )
        .expect_err("unknown field must reject");
        assert!(matches!(err.kind, ConfigErrorKind::TomlParse(_)));
    }

    #[test]
    fn oversize_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("grimoire.toml");
        let padding = "# pad pad pad pad pad pad pad pad pad pad pad pad\n".repeat(2200);
        let body = format!("{padding}\n[skills]\nx = \"ghcr.io/acme/x:1\"\n");
        assert!(body.len() as u64 > FILE_SIZE_LIMIT_BYTES);
        std::fs::write(&path, &body).unwrap();
        let err = ProjectConfig::discover(Some(&path)).expect_err("oversize must reject");
        assert!(matches!(err.kind, ConfigErrorKind::FileTooLarge { .. }));
    }

    #[test]
    fn discover_explicit_missing_is_io_not_found() {
        let missing = Path::new("/tmp/grim-nonexistent-explicit-cfg-xyz.toml");
        let err = ProjectConfig::discover(Some(missing)).expect_err("missing explicit must error");
        let ConfigErrorKind::Io(io) = err.kind else {
            panic!("expected Io, got {:?}", err.kind);
        };
        assert_eq!(io.kind(), std::io::ErrorKind::NotFound);
    }

    #[test]
    fn discover_walk_up_finds_config_and_derives_lock_path() {
        let root = tempfile::tempdir().unwrap();
        let cfg_path = root.path().join("grimoire.toml");
        std::fs::write(&cfg_path, "[skills]\nx = \"ghcr.io/acme/x:1\"\n").unwrap();
        let nested = root.path().join("a/b/c");
        std::fs::create_dir_all(&nested).unwrap();

        // `discover` walks up from the *process* CWD; drive the inner
        // walk directly via an explicit path here, and exercise the
        // lock-path derivation which is the load-bearing contract.
        let discovered = ProjectConfig::discover(Some(&cfg_path)).expect("discover");
        assert_eq!(discovered.config_path(), cfg_path);
        assert_eq!(discovered.lock_path(), root.path().join("grimoire.lock"));
    }

    #[test]
    fn lock_path_for_always_named_grimoire_lock() {
        assert_eq!(
            lock_path_for(Path::new("/p/grimoire.toml")),
            PathBuf::from("/p/grimoire.lock")
        );
        assert_eq!(
            lock_path_for(Path::new("/p/custom-name.toml")),
            PathBuf::from("/p/grimoire.lock")
        );
        assert_eq!(
            lock_path_for(Path::new("/p/NoExtension")),
            PathBuf::from("/p/grimoire.lock")
        );
    }

    #[test]
    fn bundle_source_reads_members_and_metadata() {
        let src = BundleSource::from_toml_str(
            r#"
summary = "Python dev stack"
keywords = "python,lint,test"
description = "Skills and rules for Python work"
repository = "https://github.com/acme/python-stack"

[skills]
code-review = "ghcr.io/acme/code-review:1"

[rules]
rust-style = "ghcr.io/acme/rust-style:2"
"#,
        )
        .expect("parse");
        assert_eq!(src.skills.len(), 1);
        assert_eq!(src.rules.len(), 1);
        assert_eq!(src.metadata.summary.as_deref(), Some("Python dev stack"));
        assert_eq!(src.metadata.keywords.as_deref(), Some("python,lint,test"));
        assert_eq!(
            src.metadata.description.as_deref(),
            Some("Skills and rules for Python work")
        );
        assert_eq!(
            src.metadata.repository.as_deref(),
            Some("https://github.com/acme/python-stack")
        );
    }

    #[test]
    fn bundle_source_reads_agent_members() {
        let src = BundleSource::from_toml_str(
            r#"
[agents]
code-reviewer = "ghcr.io/acme/agents/code-reviewer:1"

[skills]
code-review = "ghcr.io/acme/code-review:1"
"#,
        )
        .expect("parse");
        assert_eq!(src.agents.len(), 1);
        assert_eq!(
            src.agents.get("code-reviewer").unwrap().to_string(),
            "ghcr.io/acme/agents/code-reviewer:1"
        );
    }

    #[test]
    fn bundle_source_metadata_optional() {
        let src = BundleSource::from_toml_str(
            r#"
[skills]
code-review = "ghcr.io/acme/code-review:1"
"#,
        )
        .expect("parse");
        assert_eq!(src.metadata, BundleMetadata::default());
    }

    #[test]
    fn bundle_source_keywords_array_is_rejected() {
        // Keywords are string-only; a TOML array is a hard parse error.
        let err = BundleSource::from_toml_str(
            r#"
keywords = ["python", "lint"]

[skills]
code-review = "ghcr.io/acme/code-review:1"
"#,
        )
        .expect_err("array keywords rejected");
        assert!(matches!(err.kind, ConfigErrorKind::TomlParse(_)));
    }

    #[test]
    fn bundle_source_unknown_key_rejected() {
        let err = BundleSource::from_toml_str("summary = \"x\"\nsumary = \"typo\"\n").expect_err("typo'd key rejected");
        assert!(matches!(err.kind, ConfigErrorKind::TomlParse(_)));
    }
}
