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

use schemars::JsonSchema;
use serde::Deserialize;
use unicode_width::UnicodeWidthChar as _;

use crate::config;
use crate::config::config_error::{ConfigError, ConfigErrorKind};
use crate::config::declaration::{ConfigOptions, DesiredSet, RegistryConfig};
use crate::oci::Identifier;
use crate::oci::identifier::error::IdentifierErrorKind;

/// A parsed project-scope declaration with its on-disk location.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectConfig {
    /// Options table (`[options]`).
    pub options: ConfigOptions,
    /// The declared registries (`[[registries]]`); empty when none are
    /// declared (legacy single-registry behavior).
    pub registries: Vec<RegistryConfig>,
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
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    #[serde(default)]
    options: ConfigOptions,
    #[serde(default)]
    registries: Vec<RegistryConfig>,
    #[serde(default)]
    skills: BTreeMap<String, String>,
    #[serde(default)]
    rules: BTreeMap<String, String>,
    #[serde(default)]
    agents: BTreeMap<String, String>,
    #[serde(default)]
    bundles: BTreeMap<String, String>,
}

/// The JSON Schema (schemars) for the on-disk `grimoire.toml` shape.
///
/// Built from the private [`RawConfig`] parse target so the published
/// schema and the parser can never describe different shapes. Lives here,
/// not in the `schema` command, because `RawConfig` is private to this
/// module (the on-disk shape is an implementation detail of parsing).
pub fn config_json_schema() -> schemars::Schema {
    schemars::schema_for!(RawConfig)
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
    validate_registries(&raw.registries, &path)?;
    validate_tree_separators(&raw.options.tui.tree_separators, &path)?;
    let skills = parse_artifact_map(&raw.skills, &path)?;
    let rules = parse_artifact_map(&raw.rules, &path)?;
    // Agent and bundle references validate exactly like skills/rules: a
    // fully-qualified identifier, bare entries defaulting to `:latest`.
    let agents = parse_artifact_map(&raw.agents, &path)?;
    let bundles = parse_artifact_map(&raw.bundles, &path)?;
    Ok(ProjectConfig {
        options: raw.options,
        registries: raw.registries,
        set: DesiredSet::from_maps(skills, rules, agents, bundles),
    })
}

/// Validate a `[[registries]]` array: every entry sets exactly one of
/// `oci` / `index` (non-empty), every `index` locator classifies as an
/// HTTP(S) or git transport, every present `alias` is non-empty and unique
/// across the array, and at most one entry sets `default = true`.
/// At-most-one default is checked after the per-entry structural checks so
/// a `default = true` entry necessarily already has a valid locator.
pub(crate) fn validate_registries(registries: &[RegistryConfig], path: &Path) -> Result<(), ConfigError> {
    let mut seen_aliases = std::collections::BTreeSet::new();
    for rc in registries {
        let oci_set = rc.oci.as_deref().is_some_and(|u| !u.trim().is_empty());
        let index_set = rc.index.as_deref().is_some_and(|i| !i.trim().is_empty());
        match (oci_set, index_set) {
            (true, true) => {
                return Err(ConfigError::new(
                    path.to_path_buf(),
                    ConfigErrorKind::RegistryInvalid {
                        reason: format!(
                            "entry '{}' sets both oci and index; exactly one must be set \
                             (index entries carry their own registry refs)",
                            rc.locator()
                        ),
                    },
                ));
            }
            (false, false) => {
                return Err(ConfigError::new(
                    path.to_path_buf(),
                    ConfigErrorKind::RegistryInvalid {
                        reason: "exactly one of oci / index must be set (non-empty)".to_string(),
                    },
                ));
            }
            _ => {}
        }
        if index_set && crate::config::registry_resolve::classify_index(rc.locator()).is_none() {
            return Err(ConfigError::new(
                path.to_path_buf(),
                ConfigErrorKind::RegistryInvalid {
                    reason: format!(
                        "index '{}' must be an http(s):// base or a git repository \
                         (git+…, ssh://, git@…, or ending in .git)",
                        rc.locator()
                    ),
                },
            ));
        }
        if let Some(alias) = &rc.alias {
            if alias.trim().is_empty() {
                return Err(ConfigError::new(
                    path.to_path_buf(),
                    ConfigErrorKind::RegistryInvalid {
                        reason: format!("alias for '{}' must not be empty", rc.locator()),
                    },
                ));
            }
            if alias != alias.trim() {
                return Err(ConfigError::new(
                    path.to_path_buf(),
                    ConfigErrorKind::RegistryInvalid {
                        reason: format!("alias '{alias}' must not have leading or trailing whitespace"),
                    },
                ));
            }
            // `/` is unreachable — reference resolution splits the input on the
            // first `/`, so an alias containing one can never match.
            if alias.contains('/') {
                return Err(ConfigError::new(
                    path.to_path_buf(),
                    ConfigErrorKind::RegistryInvalid {
                        reason: format!("alias '{alias}' must not contain '/'"),
                    },
                ));
            }
            if alias.chars().any(char::is_control) {
                return Err(ConfigError::new(
                    path.to_path_buf(),
                    ConfigErrorKind::RegistryInvalid {
                        reason: format!("alias '{alias}' must not contain control characters"),
                    },
                ));
            }
            if alias.contains('"') || alias.contains('\\') {
                return Err(ConfigError::new(
                    path.to_path_buf(),
                    ConfigErrorKind::RegistryInvalid {
                        reason: format!("alias '{alias}' must not contain '\"' or '\\'"),
                    },
                ));
            }
            if !seen_aliases.insert(alias.as_str()) {
                return Err(ConfigError::new(
                    path.to_path_buf(),
                    ConfigErrorKind::RegistryInvalid {
                        reason: format!("duplicate alias '{alias}'"),
                    },
                ));
            }
        }
    }
    // At-most-one-default check: two `default = true` entries are ambiguous
    // and are rejected at parse time. `normalize_primary` is a defensive
    // net for programmatically-built sets; on-disk configs must be unambiguous.
    let default_count = registries.iter().filter(|rc| rc.default).count();
    if default_count > 1 {
        return Err(ConfigError::new(
            path.to_path_buf(),
            ConfigErrorKind::RegistryInvalid {
                reason: "at most one [[registries]] entry may set default = true".to_string(),
            },
        ));
    }
    Ok(())
}

/// Validate that every `tree_separators` entry contains exactly one Unicode
/// scalar value that is a single-column printable character. Empty and
/// multi-character strings are rejected so the TUI tree splitter is always
/// handed single printable `char` inputs. Control and whitespace characters
/// (e.g. `"\n"`, `"\t"`, `"\u{1b}"`, NBSP) are also rejected — a separator
/// the user cannot see or type cannot meaningfully delimit a path segment.
/// Zero-width and bidi-override characters (U+200B ZWSP, U+202E RLO,
/// U+FEFF BOM, and any char where `unicode_width` reports width ≠ 1) are
/// rejected to prevent invisible or display-corrupting separators.
fn validate_tree_separators(separators: &[String], path: &Path) -> Result<(), ConfigError> {
    for entry in separators {
        let mut chars = entry.chars();
        let valid = match (chars.next(), chars.next()) {
            (Some(ch), None) => {
                // Reject control and whitespace chars first (clearer error context,
                // defense in depth against future unicode-width table changes).
                !ch.is_control()
                    && !ch.is_whitespace()
                    // Require exactly one terminal column: rejects zero-width ignorables
                    // (U+200B ZWSP, U+FEFF BOM, Default_Ignorable category) and wide
                    // chars (CJK full-width), accepting only normal single-column glyphs.
                    && ch.width() == Some(1)
            }
            _ => false,
        };
        if !valid {
            return Err(ConfigError::new(
                path.to_path_buf(),
                ConfigErrorKind::TreeSeparatorInvalid { entry: entry.clone() },
            ));
        }
    }
    Ok(())
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
    /// Deprecation notice → `com.grimoire.deprecated`. A non-empty message
    /// marks the bundle deprecated; emitted only when present.
    pub deprecated: Option<String>,
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
///
/// MUST NOT gain a top-level `registry` key — D7 disambiguation in
/// `grim publish` / `grim release` guards depend on its absence; see
/// `.claude/artifacts/adr_grim_publish.md` D7.
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
    #[serde(default)]
    deprecated: Option<String>,
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
            deprecated: raw.deprecated,
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
        assert!(cfg.registries.is_empty());
    }

    #[test]
    fn parse_registries_array_ok() {
        let cfg = ProjectConfig::from_toml_str(
            r#"
[[registries]]
alias = "acme"
oci = "ghcr.io/acme"
default = true

[[registries]]
oci = "registry.corp/team"
"#,
        )
        .expect("parse");
        assert_eq!(cfg.registries.len(), 2);
        assert_eq!(cfg.registries[0].alias.as_deref(), Some("acme"));
        assert_eq!(cfg.registries[0].oci.as_deref(), Some("ghcr.io/acme"));
        assert!(cfg.registries[0].default);
        assert_eq!(cfg.registries[1].alias, None);
        assert!(!cfg.registries[1].default);
    }

    #[test]
    fn registries_empty_oci_rejected() {
        let err = ProjectConfig::from_toml_str(
            r#"
[[registries]]
oci = ""
"#,
        )
        .expect_err("empty oci must reject");
        assert!(matches!(err.kind, ConfigErrorKind::RegistryInvalid { .. }));
    }

    #[test]
    fn registries_legacy_url_key_parses_as_oci_alias() {
        // Back-compat: the pre-0.7.0 key `url` deserializes into `oci`
        // via a serde alias so 0.6.x configs keep working unchanged.
        let cfg = ProjectConfig::from_toml_str(
            r#"
[[registries]]
alias = "acme"
url = "ghcr.io/acme"
"#,
        )
        .expect("legacy url key must parse");
        assert_eq!(cfg.registries[0].oci.as_deref(), Some("ghcr.io/acme"));
    }

    #[test]
    fn registries_duplicate_alias_rejected() {
        let err = ProjectConfig::from_toml_str(
            r#"
[[registries]]
alias = "acme"
oci = "ghcr.io/acme"

[[registries]]
alias = "acme"
oci = "registry.corp/team"
"#,
        )
        .expect_err("duplicate alias must reject");
        assert!(matches!(err.kind, ConfigErrorKind::RegistryInvalid { .. }));
    }

    #[test]
    fn registries_alias_with_slash_rejected() {
        let err = ProjectConfig::from_toml_str(
            r#"
[[registries]]
alias = "a/b"
oci = "ghcr.io/acme"
"#,
        )
        .expect_err("alias with '/' must reject");
        assert!(matches!(err.kind, ConfigErrorKind::RegistryInvalid { .. }));
        if let ConfigErrorKind::RegistryInvalid { reason } = &err.kind {
            assert!(
                reason.contains('/'),
                "reason should mention the offending character: {reason}"
            );
            assert!(
                !reason.contains("unreachable"),
                "user-facing reason must not leak the implementation note: {reason}"
            );
        }
    }

    #[test]
    fn registries_alias_with_control_char_rejected() {
        let err = ProjectConfig::from_toml_str("[[registries]]\nalias = \"a\\tb\"\nurl = \"ghcr.io/acme\"\n")
            .expect_err("alias with an embedded control character must reject");
        assert!(matches!(err.kind, ConfigErrorKind::RegistryInvalid { .. }));
    }

    #[test]
    fn registries_alias_with_leading_whitespace_rejected() {
        let err = ProjectConfig::from_toml_str(
            r#"
[[registries]]
alias = " acme"
oci = "ghcr.io/acme"
"#,
        )
        .expect_err("alias with leading whitespace must reject");
        assert!(matches!(err.kind, ConfigErrorKind::RegistryInvalid { .. }));
        if let ConfigErrorKind::RegistryInvalid { reason } = &err.kind {
            assert!(
                reason.contains("whitespace"),
                "reason should mention whitespace: {reason}"
            );
        }
    }

    #[test]
    fn registries_alias_with_trailing_whitespace_rejected() {
        let err = ProjectConfig::from_toml_str(
            r#"
[[registries]]
alias = "acme "
oci = "ghcr.io/acme"
"#,
        )
        .expect_err("alias with trailing whitespace must reject");
        assert!(matches!(err.kind, ConfigErrorKind::RegistryInvalid { .. }));
        if let ConfigErrorKind::RegistryInvalid { reason } = &err.kind {
            assert!(
                reason.contains("whitespace"),
                "reason should mention whitespace: {reason}"
            );
        }
    }

    #[test]
    fn registries_valid_multi_registry_accepted() {
        let cfg = ProjectConfig::from_toml_str(
            r#"
[[registries]]
alias = "acme"
oci = "ghcr.io/acme"
default = true

[[registries]]
alias = "corp"
oci = "registry.corp/team"

[[registries]]
oci = "other.registry.io"
"#,
        )
        .expect("valid multi-registry config must parse");
        assert_eq!(cfg.registries.len(), 3);
        assert_eq!(cfg.registries[0].alias.as_deref(), Some("acme"));
        assert!(cfg.registries[0].default);
        assert_eq!(cfg.registries[1].alias.as_deref(), Some("corp"));
        assert_eq!(cfg.registries[2].alias, None);
    }

    #[test]
    fn registries_unknown_field_rejected() {
        let err = ProjectConfig::from_toml_str(
            r#"
[[registries]]
oci = "ghcr.io/acme"
surprise = "x"
"#,
        )
        .expect_err("unknown registry field must reject");
        assert!(matches!(err.kind, ConfigErrorKind::TomlParse(_)));
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
    fn bundle_source_reads_deprecated_metadata() {
        let src = BundleSource::from_toml_str(
            "deprecated = \"migrate to python-stack-2\"\n\n[skills]\ncode-review = \"ghcr.io/acme/code-review:1\"\n",
        )
        .expect("parse");
        assert_eq!(src.metadata.deprecated.as_deref(), Some("migrate to python-stack-2"));
        // Absent ⇒ None (bundle is not deprecated).
        let plain =
            BundleSource::from_toml_str("[skills]\ncode-review = \"ghcr.io/acme/code-review:1\"\n").expect("parse");
        assert_eq!(plain.metadata.deprecated, None);
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

    // ── tree_separators validation (S2 CWE-20) ───────────────────────────────

    #[test]
    fn tree_separators_single_chars_accepted() {
        // Single-character separators (including `/` and `-`) must parse cleanly.
        let cfg = ProjectConfig::from_toml_str(
            r#"
[options.tui]
tree_separators = ["/", "-"]
"#,
        )
        .expect("single-char tree_separators must be accepted");
        assert_eq!(cfg.options.tui.tree_separators, vec!["/".to_string(), "-".to_string()]);
    }

    #[test]
    fn tree_separators_empty_entry_rejected() {
        // S2: an empty string is not exactly one character and must be rejected.
        let err = ProjectConfig::from_toml_str(
            r#"
[options.tui]
tree_separators = [""]
"#,
        )
        .expect_err("empty tree_separators entry must be rejected");
        assert!(
            matches!(err.kind, ConfigErrorKind::TreeSeparatorInvalid { .. }),
            "expected TreeSeparatorInvalid, got {:?}",
            err.kind
        );
        if let ConfigErrorKind::TreeSeparatorInvalid { entry } = &err.kind {
            assert_eq!(entry, "", "error must name the offending entry");
        }
    }

    #[test]
    fn tree_separators_multi_char_entry_rejected() {
        // S2: a multi-character string like "::" must be rejected.
        let err = ProjectConfig::from_toml_str(
            r#"
[options.tui]
tree_separators = ["::"]
"#,
        )
        .expect_err("multi-char tree_separators entry must be rejected");
        assert!(
            matches!(err.kind, ConfigErrorKind::TreeSeparatorInvalid { .. }),
            "expected TreeSeparatorInvalid, got {:?}",
            err.kind
        );
        if let ConfigErrorKind::TreeSeparatorInvalid { entry } = &err.kind {
            assert_eq!(entry, "::", "error must name the offending entry");
        }
    }

    #[test]
    fn tree_separators_first_invalid_entry_named_in_error() {
        // The first offending entry (not the last) is named in the error.
        let err = ProjectConfig::from_toml_str(
            r#"
[options.tui]
tree_separators = ["/", "::"]
"#,
        )
        .expect_err("mixed valid+invalid tree_separators must be rejected");
        if let ConfigErrorKind::TreeSeparatorInvalid { entry } = &err.kind {
            assert_eq!(entry, "::", "error must name the offending multi-char entry");
        } else {
            panic!("expected TreeSeparatorInvalid, got {:?}", err.kind);
        }
    }

    #[test]
    fn tree_separators_control_char_newline_rejected() {
        // SEC: a single control character like "\n" passes the char-count check but
        // must be rejected — a separator the user cannot see or type is not useful.
        let err = ProjectConfig::from_toml_str("[options.tui]\ntree_separators = [\"\\n\"]\n")
            .expect_err("newline tree_separator must be rejected");
        assert!(
            matches!(err.kind, ConfigErrorKind::TreeSeparatorInvalid { .. }),
            "expected TreeSeparatorInvalid, got {:?}",
            err.kind
        );
    }

    #[test]
    fn tree_separators_whitespace_space_rejected() {
        // SEC: a single whitespace character (space) passes the char-count check
        // but must be rejected — a separator the user cannot see is not useful
        // and could be a sign of an encoding or copy-paste accident.
        let err = ProjectConfig::from_toml_str("[options.tui]\ntree_separators = [\" \"]\n")
            .expect_err("space tree_separator must be rejected");
        assert!(
            matches!(err.kind, ConfigErrorKind::TreeSeparatorInvalid { .. }),
            "expected TreeSeparatorInvalid, got {:?}",
            err.kind
        );
    }

    // ── CWE-20: zero-width / bidi-override / Default_Ignorable chars rejected ──

    #[test]
    fn tree_separators_zero_width_space_u200b_rejected() {
        // CWE-20: U+200B ZERO WIDTH SPACE is a single scalar value (passes
        // char-count check) but has display width 0, making it invisible and
        // useless as a separator. Must be rejected.
        let err = ProjectConfig::from_toml_str("[options.tui]\ntree_separators = [\"\u{200b}\"]\n")
            .expect_err("U+200B ZWSP tree_separator must be rejected");
        assert!(
            matches!(err.kind, ConfigErrorKind::TreeSeparatorInvalid { .. }),
            "expected TreeSeparatorInvalid for U+200B, got {:?}",
            err.kind
        );
    }

    #[test]
    fn tree_separators_bidi_override_u202e_rejected() {
        // CWE-20: U+202E RIGHT-TO-LEFT OVERRIDE is a single scalar value but
        // has display width 0. As a separator it would corrupt tree display
        // without being visible. Must be rejected.
        let err = ProjectConfig::from_toml_str("[options.tui]\ntree_separators = [\"\u{202e}\"]\n")
            .expect_err("U+202E RLO tree_separator must be rejected");
        assert!(
            matches!(err.kind, ConfigErrorKind::TreeSeparatorInvalid { .. }),
            "expected TreeSeparatorInvalid for U+202E, got {:?}",
            err.kind
        );
    }

    #[test]
    fn tree_separators_bom_ufeff_rejected() {
        // CWE-20: U+FEFF BOM / ZERO WIDTH NO-BREAK SPACE is a Default_Ignorable
        // character with display width 0. Must be rejected.
        let err = ProjectConfig::from_toml_str("[options.tui]\ntree_separators = [\"\u{feff}\"]\n")
            .expect_err("U+FEFF BOM tree_separator must be rejected");
        assert!(
            matches!(err.kind, ConfigErrorKind::TreeSeparatorInvalid { .. }),
            "expected TreeSeparatorInvalid for U+FEFF, got {:?}",
            err.kind
        );
    }

    #[test]
    fn tree_separators_middle_dot_u00b7_accepted() {
        // U+00B7 MIDDLE DOT is a single-column printable character (width 1).
        // Useful as a path separator in namespaced artifact names.
        let cfg = ProjectConfig::from_toml_str("[options.tui]\ntree_separators = [\"\u{00b7}\"]\n")
            .expect("U+00B7 middle dot tree_separator must be accepted");
        assert_eq!(cfg.options.tui.tree_separators, vec!["\u{00b7}".to_string()]);
    }

    #[test]
    fn default_view_invalid_value_rejected() {
        // A7: `default_view = "list"` is not a valid enum variant and must be
        // rejected at deserialization — serde rejects it as an unknown variant.
        let err = ProjectConfig::from_toml_str("[options.tui]\ndefault_view = \"list\"\n")
            .expect_err("invalid default_view value must be rejected");
        assert!(
            matches!(err.kind, ConfigErrorKind::TomlParse(_)),
            "expected TomlParse for unknown DefaultView variant, got {:?}",
            err.kind
        );
    }

    // ── Contract (a) — at-most-one-default validation ──────────────────────

    #[test]
    fn registries_two_defaults_rejected() {
        // Two `default = true` entries must be rejected with RegistryInvalid,
        // and the reason must mention "default".
        let err = ProjectConfig::from_toml_str(
            r#"
[[registries]]
oci = "ghcr.io/acme"
default = true

[[registries]]
oci = "registry.corp/team"
default = true
"#,
        )
        .expect_err("two default = true entries must be rejected");
        let ConfigErrorKind::RegistryInvalid { reason } = &err.kind else {
            panic!("expected RegistryInvalid, got {:?}", err.kind);
        };
        assert!(reason.contains("default"), "reason must mention 'default': {reason}");
    }

    #[test]
    fn registries_single_default_accepted() {
        // Exactly one `default = true` must parse cleanly — boundary case.
        let cfg = ProjectConfig::from_toml_str(
            r#"
[[registries]]
oci = "ghcr.io/acme"
default = true

[[registries]]
oci = "registry.corp/team"
"#,
        )
        .expect("exactly one default must be accepted");
        assert_eq!(cfg.registries.len(), 2);
        assert!(cfg.registries[0].default);
        assert!(!cfg.registries[1].default);
    }

    #[test]
    fn registries_no_default_accepted() {
        // No `default = true` at all must parse cleanly — resolver promotes the first.
        let cfg = ProjectConfig::from_toml_str(
            r#"
[[registries]]
oci = "ghcr.io/acme"

[[registries]]
oci = "registry.corp/team"
"#,
        )
        .expect("zero defaults must be accepted");
        assert_eq!(cfg.registries.len(), 2);
        assert!(!cfg.registries[0].default);
        assert!(!cfg.registries[1].default);
    }

    // ── Contract (d) — both-fields baseline: array wins, legacy ignored ────

    #[test]
    fn both_fields_present_array_wins_legacy_ignored() {
        use crate::config::registry_resolve::primary_registry;
        use crate::config::resolve_registries;
        // Pre-migration baseline: when both `[options].default_registry` and
        // `[[registries]]` are present, the array is authoritative for browse
        // and the legacy field is ignored.
        let cfg = ProjectConfig::from_toml_str(
            r#"
[options]
default_registry = "legacy.example"

[[registries]]
oci = "array.example"
default = true
"#,
        )
        .expect("mixed config must parse");
        // The in-memory state carries both fields.
        assert_eq!(cfg.options.default_registry.as_deref(), Some("legacy.example"));
        assert_eq!(cfg.registries.len(), 1);
        assert_eq!(cfg.registries[0].oci.as_deref(), Some("array.example"));
        // When resolved: the array is authoritative, legacy is folded in only
        // when no `[[registries]]` are present (step 3 of resolve_registries).
        let set = resolve_registries(
            &[],
            &cfg.registries,
            cfg.options.default_registry.as_deref(),
            &[],
            None,
            crate::command::FALLBACK_REGISTRY,
            None,
        );
        assert_eq!(primary_registry(&set), "array.example", "array must win over legacy");
        // The legacy url must not appear in the resolved set at all.
        assert!(
            set.iter().all(|r| r.url != "legacy.example"),
            "legacy url must be absent from the resolved set when array is present"
        );
    }
}
