// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! `grim config` — git-style CLI to read and write `grimoire.toml`.
//!
//! Hybrid surface: explicit `get`/`set`/`unset`/`list` over dotted keys,
//! plus nested `config registry add|rm|use|show|list` for registry
//! lifecycle.  All under one `config` umbrella (see
//! `adr_grim_config_command.md`).
//!
//! Scope is selected by `--global` / `--config` declared on `ConfigArgs`
//! and passed to `scope_resolution::resolve` — the same pattern every
//! scope-aware command (`lock`, `install`, `status`) follows, because
//! `Context` does not expose those flags.

use clap::{Args, Subcommand};
use unicode_width::UnicodeWidthChar as _;

use crate::api::config_report::{
    ConfigEntry, ConfigGetReport, ConfigListReport, ConfigReport, ConfigWriteReport, Origin, RegistryListReport,
    RegistryRow, RegistryShowReport, WriteAction,
};
use crate::cli::exit_code::ExitCode;
use crate::config::declaration::{ConfigOptions, DefaultView, RegistryConfig};
use crate::config::project_config::validate_registries;
use crate::config::scope::ConfigScope;
use crate::context::Context;
use crate::lock::file_lock::ConfigFileLock;

use super::scope_resolution::{self, lockable_config_path};

/// `grim config` arguments.
///
/// Scope flags (`--global`, `--config`) apply to the whole command tree and
/// must precede the subcommand: `grim config --global get <key>`.
#[derive(Debug, Args)]
pub struct ConfigArgs {
    /// Operate on the global config (`$GRIM_HOME/grimoire.toml`) instead of
    /// the discovered project config.
    #[arg(long)]
    pub global: bool,

    /// Explicit project config path.
    #[arg(long)]
    pub config: Option<std::path::PathBuf>,

    #[command(subcommand)]
    pub command: ConfigCommand,
}

/// The `config` subcommand tree.
#[derive(Debug, Subcommand)]
pub enum ConfigCommand {
    /// Print the value of a single dotted key.
    Get {
        /// Dotted key, e.g. `options.clients` or `registry.acme.url`.
        key: String,
    },
    /// Set a dotted key to a value.
    Set {
        /// Dotted key to set.
        key: String,
        /// New value (parsed to the field's type).
        value: String,
    },
    /// Remove a dotted key (or a whole registry entry when the key names
    /// a `registry.<alias>` without a trailing field).
    Unset {
        /// Dotted key to unset.
        key: String,
    },
    /// List all effective key=value pairs for the scope.
    ///
    /// Each invocation reads from exactly one scope, so origin information
    /// is implicit (use `--global` or `--config` to select the scope).
    List,
    /// Manage `[[registries]]` entries.
    #[command(subcommand_value_name = "REGISTRY_COMMAND")]
    Registry(RegistryArgs),
}

/// `grim config registry` arguments.
#[derive(Debug, Args)]
pub struct RegistryArgs {
    #[command(subcommand)]
    pub command: RegistryCommand,
}

/// The `config registry` subcommand tree.
#[derive(Debug, Subcommand)]
pub enum RegistryCommand {
    /// Add a registry or package-index entry (exactly one of --url / --index).
    Add {
        /// Alias to assign (must be non-empty, no `/`, no surrounding whitespace).
        alias: String,
        /// Registry URL (lists packages via the OCI `_catalog` endpoint).
        #[arg(long)]
        url: Option<String>,
        /// Package-index locator (http(s):// static base, or a git repository);
        /// replaces the `_catalog` listing — index entries carry their own
        /// registry refs.
        #[arg(long, conflicts_with = "url")]
        index: Option<String>,
        /// Mark this registry as the default (clears any prior default).
        #[arg(long)]
        default: bool,
    },
    /// Remove a registry entry by alias.
    Rm {
        /// Alias of the registry to remove.
        alias: String,
    },
    /// Mark a registry as the default (clears any prior default).
    Use {
        /// Alias of the registry to make the default.
        alias: String,
    },
    /// Show all fields for a single registry.
    Show {
        /// Alias of the registry to show.
        alias: String,
    },
    /// List all registries in the scope (default marked).
    List,
}

/// Run `grim config`.
///
/// `get` of a valid-but-unset key returns `(ConfigReport::Get, ExitCode::Failure)`
/// with no stdout — git-compatible so `grim config get <key> || default`
/// works in scripts. This is a non-error exit, not a `Result::Err`.
///
/// # Errors
///
/// Unknown key (UsageError 64), invalid value (DataError 65), config parse
/// failure (ConfigError 78), missing config (NotFound 79), write / lock
/// failure (IoError 74), or alias not found (UsageError 64).
pub async fn run(ctx: &Context, args: &ConfigArgs) -> anyhow::Result<(ConfigReport, ExitCode)> {
    match &args.command {
        ConfigCommand::Get { key } => run_get(ctx, args, key),
        ConfigCommand::Set { key, value } => run_set(ctx, args, key, value),
        ConfigCommand::Unset { key } => run_unset(ctx, args, key),
        ConfigCommand::List => run_list(ctx, args),
        ConfigCommand::Registry(r) => match &r.command {
            RegistryCommand::Add {
                alias,
                url,
                index,
                default,
            } => run_registry_add(ctx, args, alias, url.as_deref(), index.as_deref(), *default),
            RegistryCommand::Rm { alias } => run_registry_rm(ctx, args, alias),
            RegistryCommand::Use { alias } => run_registry_use(ctx, args, alias),
            RegistryCommand::Show { alias } => run_registry_show(ctx, args, alias),
            RegistryCommand::List => run_registry_list(ctx, args),
        },
    }
}

// ── Key parsing ──────────────────────────────────────────────────────────────

/// A parsed dotted config key.
enum ParsedKey {
    OptionsClients,
    OptionsDefaultRegistry,
    TuiDefaultView,
    TuiGroupByType,
    TuiTreeSeparators,
    /// `registry.<alias>` — valid only for `unset` (removes the whole entry).
    RegistryAlias {
        alias: String,
    },
    /// `registry.<alias>.<field>`.
    RegistryAliasField {
        alias: String,
        field: RegistryField,
    },
}

enum RegistryField {
    Url,
    Index,
    Default,
}

fn parse_key(key: &str) -> anyhow::Result<ParsedKey> {
    match key {
        "options.clients" => return Ok(ParsedKey::OptionsClients),
        "options.default_registry" => return Ok(ParsedKey::OptionsDefaultRegistry),
        "options.tui.default_view" => return Ok(ParsedKey::TuiDefaultView),
        "options.tui.group_by_type" => return Ok(ParsedKey::TuiGroupByType),
        "options.tui.tree_separators" => return Ok(ParsedKey::TuiTreeSeparators),
        _ => {}
    }
    if let Some(rest) = key.strip_prefix("registry.") {
        // FIX 2: split at the RIGHTMOST dot so aliases containing dots
        // (e.g. `a.b`) are addressable: `registry.a.b.url` → alias=`a.b`,
        // field=`url`.  The field must be exactly `url` or `default`.
        if let Some(dot_pos) = rest.rfind('.') {
            let alias = &rest[..dot_pos];
            let field_str = &rest[dot_pos + 1..];
            if !alias.is_empty() && !field_str.is_empty() {
                let field = match field_str {
                    "url" => RegistryField::Url,
                    "index" => RegistryField::Index,
                    "default" => RegistryField::Default,
                    other => {
                        return Err(super::config_usage(format!(
                            "unknown registry field '{other}'; valid fields: url, index, default"
                        )));
                    }
                };
                // FIX 1: validate alias format at CLI boundary (exit 64) so
                // a bad alias never reaches validate_registries (exit 78).
                validate_alias_format(alias)?;
                return Ok(ParsedKey::RegistryAliasField {
                    alias: alias.to_string(),
                    field,
                });
            }
        } else if !rest.is_empty() {
            return Ok(ParsedKey::RegistryAlias {
                alias: rest.to_string(),
            });
        }
    }
    Err(super::config_usage(format!(
        "unknown config key '{key}'; valid keys: options.clients, \
         options.default_registry, options.tui.default_view, \
         options.tui.group_by_type, options.tui.tree_separators, \
         registry.<alias>.url, registry.<alias>.index, registry.<alias>.default"
    )))
}

fn scope_to_origin(scope: ConfigScope) -> Origin {
    match scope {
        ConfigScope::Global => Origin::Global,
        ConfigScope::Project => Origin::Project,
    }
}

// ── Value getters ─────────────────────────────────────────────────────────────

fn get_value(
    parsed: &ParsedKey,
    options: &ConfigOptions,
    registries: &[RegistryConfig],
) -> anyhow::Result<Option<String>> {
    Ok(match parsed {
        ParsedKey::OptionsClients => {
            if options.clients.is_empty() {
                None
            } else {
                Some(options.clients.join(","))
            }
        }
        ParsedKey::OptionsDefaultRegistry => options.default_registry.clone(),
        ParsedKey::TuiDefaultView => options.tui.default_view.map(|v| match v {
            DefaultView::Flat => "flat".to_string(),
            DefaultView::Tree => "tree".to_string(),
        }),
        ParsedKey::TuiGroupByType => {
            // `false` is the default and indistinguishable from unset on disk —
            // return None so `get` exits 1 and `list` omits the key, consistent
            // with all other default-valued keys.  Setting to `false` removes the
            // key from the written config (see `apply_unset`).
            if options.tui.group_by_type {
                Some("true".to_string())
            } else {
                None
            }
        }
        ParsedKey::TuiTreeSeparators => {
            if options.tui.tree_separators.is_empty() {
                None
            } else {
                Some(options.tui.tree_separators.join(","))
            }
        }
        ParsedKey::RegistryAlias { alias } => {
            return Err(super::config_usage(format!(
                "no registry field specified for '{alias}'; use registry.<alias>.url or registry.<alias>.default"
            )));
        }
        ParsedKey::RegistryAliasField { alias, field } => {
            let rc = find_registry(registries, alias).ok_or_else(|| {
                super::config_usage(format!("no registry '{alias}'; add it with `grim config registry add`"))
            })?;
            match field {
                RegistryField::Url => rc.url.clone(),
                RegistryField::Index => rc.index.clone(),
                RegistryField::Default => Some(rc.default.to_string()),
            }
        }
    })
}

// ── Value setters ─────────────────────────────────────────────────────────────

fn apply_set(
    parsed: &ParsedKey,
    value_str: &str,
    options: &mut ConfigOptions,
    registries: &mut [RegistryConfig],
) -> anyhow::Result<String> {
    match parsed {
        ParsedKey::OptionsClients => {
            if value_str.is_empty() {
                options.clients.clear();
                Ok(String::new())
            } else {
                let clients: Vec<String> = value_str.split(',').map(|s| s.trim().to_string()).collect();
                for c in &clients {
                    // FIX 3: empty/whitespace-only segment (e.g. "claude, ,opencode"
                    // after split+trim) → exit 65 so the config never holds a blank
                    // client name that silently installs nothing.
                    if c.is_empty() {
                        return Err(super::config_value(
                            "options.clients: empty or whitespace-only segment; \
                             each client name must be non-empty"
                                .to_string(),
                        ));
                    }
                    reject_control_chars(c, "options.clients")?;
                }
                options.clients.clone_from(&clients);
                Ok(clients.join(","))
            }
        }
        ParsedKey::OptionsDefaultRegistry => {
            reject_control_chars(value_str, "options.default_registry")?;
            options.default_registry = Some(value_str.to_string());
            Ok(value_str.to_string())
        }
        ParsedKey::TuiDefaultView => {
            options.tui.default_view = Some(parse_default_view(value_str)?);
            Ok(value_str.to_string())
        }
        ParsedKey::TuiGroupByType => {
            options.tui.group_by_type = parse_bool(value_str, "options.tui.group_by_type")?;
            Ok(value_str.to_string())
        }
        ParsedKey::TuiTreeSeparators => {
            let seps = parse_tree_separators(value_str)?;
            let stored = seps.join(",");
            options.tui.tree_separators = seps;
            Ok(stored)
        }
        ParsedKey::RegistryAlias { alias } => Err(super::config_usage(format!(
            "cannot set registry '{alias}' without a field; \
             use registry.<alias>.url or registry.<alias>.default"
        ))),
        ParsedKey::RegistryAliasField { alias, field } => {
            if find_registry(registries, alias).is_none() {
                return Err(super::config_usage(format!(
                    "no registry '{alias}'; add it with `grim config registry add`"
                )));
            }
            match field {
                RegistryField::Url => {
                    reject_control_chars(value_str, &format!("registry.{alias}.url"))?;
                    if find_registry(registries, alias).is_some_and(|rc| rc.index.is_some()) {
                        return Err(super::config_value(format!(
                            "registry '{alias}' is an index entry; url and index are mutually \
                             exclusive — unset registry.{alias}.index first"
                        )));
                    }
                    set_registry_field(registries, alias, |rc| rc.url = Some(value_str.to_string()));
                    Ok(value_str.to_string())
                }
                RegistryField::Index => {
                    reject_control_chars(value_str, &format!("registry.{alias}.index"))?;
                    if find_registry(registries, alias).is_some_and(|rc| rc.url.is_some()) {
                        return Err(super::config_value(format!(
                            "registry '{alias}' is a registry entry; url and index are mutually \
                             exclusive — unset registry.{alias}.url first"
                        )));
                    }
                    if crate::config::registry_resolve::classify_index(value_str).is_none() {
                        return Err(super::config_value(format!(
                            "invalid index locator '{value_str}': must be an http(s):// base or a \
                             git repository (git+…, ssh://, git@…, or ending in .git)"
                        )));
                    }
                    set_registry_field(registries, alias, |rc| rc.index = Some(value_str.to_string()));
                    Ok(value_str.to_string())
                }
                RegistryField::Default => {
                    let b = parse_bool(value_str, &format!("registry.{alias}.default"))?;
                    if b {
                        clear_all_defaults(registries);
                    }
                    set_registry_default(registries, alias, b);
                    Ok(value_str.to_string())
                }
            }
        }
    }
}

fn apply_unset(
    parsed: &ParsedKey,
    options: &mut ConfigOptions,
    registries: &mut Vec<RegistryConfig>,
) -> anyhow::Result<()> {
    match parsed {
        ParsedKey::OptionsClients => {
            options.clients.clear();
            Ok(())
        }
        ParsedKey::OptionsDefaultRegistry => {
            options.default_registry = None;
            Ok(())
        }
        ParsedKey::TuiDefaultView => {
            options.tui.default_view = None;
            Ok(())
        }
        ParsedKey::TuiGroupByType => {
            options.tui.group_by_type = false;
            Ok(())
        }
        ParsedKey::TuiTreeSeparators => {
            options.tui.tree_separators.clear();
            Ok(())
        }
        ParsedKey::RegistryAlias { alias } => {
            if !registries.iter().any(|r| r.alias.as_deref() == Some(alias.as_str())) {
                return Err(super::config_usage(format!(
                    "no registry '{alias}'; cannot remove a registry that does not exist"
                )));
            }
            registries.retain(|r| r.alias.as_deref() != Some(alias.as_str()));
            Ok(())
        }
        ParsedKey::RegistryAliasField { alias, field } => match field {
            RegistryField::Url => {
                let Some(rc) = find_registry(registries, alias) else {
                    return Err(super::config_usage(format!(
                        "no registry '{alias}'; cannot unset a field on a registry that does not exist"
                    )));
                };
                if rc.index.is_none() {
                    return Err(super::config_usage(format!(
                        "cannot unset registry.{alias}.url: the entry would have no source; \
                         set registry.{alias}.index first or use `grim config registry rm {alias}`"
                    )));
                }
                set_registry_field(registries, alias, |rc| rc.url = None);
                Ok(())
            }
            RegistryField::Index => {
                let Some(rc) = find_registry(registries, alias) else {
                    return Err(super::config_usage(format!(
                        "no registry '{alias}'; cannot unset a field on a registry that does not exist"
                    )));
                };
                if rc.url.is_none() {
                    return Err(super::config_usage(format!(
                        "cannot unset registry.{alias}.index: the entry would have no source; \
                         set registry.{alias}.url first or use `grim config registry rm {alias}`"
                    )));
                }
                set_registry_field(registries, alias, |rc| rc.index = None);
                Ok(())
            }
            RegistryField::Default => {
                if find_registry(registries, alias).is_none() {
                    return Err(super::config_usage(format!(
                        "no registry '{alias}'; cannot unset default on a registry that does not exist"
                    )));
                }
                set_registry_default(registries, alias, false);
                Ok(())
            }
        },
    }
}

// ── List collector ────────────────────────────────────────────────────────────

fn collect_entries(options: &ConfigOptions, registries: &[RegistryConfig]) -> Vec<ConfigEntry> {
    let mut entries = Vec::new();
    if let Some(r) = &options.default_registry {
        entries.push(ConfigEntry {
            key: "options.default_registry".to_string(),
            value: r.clone(),
        });
    }
    if !options.clients.is_empty() {
        entries.push(ConfigEntry {
            key: "options.clients".to_string(),
            value: options.clients.join(","),
        });
    }
    if let Some(dv) = options.tui.default_view {
        entries.push(ConfigEntry {
            key: "options.tui.default_view".to_string(),
            value: match dv {
                DefaultView::Flat => "flat",
                DefaultView::Tree => "tree",
            }
            .to_string(),
        });
    }
    if options.tui.group_by_type {
        entries.push(ConfigEntry {
            key: "options.tui.group_by_type".to_string(),
            value: "true".to_string(),
        });
    }
    if !options.tui.tree_separators.is_empty() {
        entries.push(ConfigEntry {
            key: "options.tui.tree_separators".to_string(),
            value: options.tui.tree_separators.join(","),
        });
    }
    for rc in registries {
        if let Some(alias) = &rc.alias {
            if let Some(url) = &rc.url {
                entries.push(ConfigEntry {
                    key: format!("registry.{alias}.url"),
                    value: url.clone(),
                });
            }
            if let Some(index) = &rc.index {
                entries.push(ConfigEntry {
                    key: format!("registry.{alias}.index"),
                    value: index.clone(),
                });
            }
            entries.push(ConfigEntry {
                key: format!("registry.{alias}.default"),
                value: rc.default.to_string(),
            });
        }
    }
    entries
}

// ── Value-parsing helpers ─────────────────────────────────────────────────────

fn parse_default_view(s: &str) -> anyhow::Result<DefaultView> {
    match s {
        "flat" => Ok(DefaultView::Flat),
        "tree" => Ok(DefaultView::Tree),
        _ => Err(super::config_value(format!(
            "invalid value for options.tui.default_view: '{s}'; valid values: flat, tree"
        ))),
    }
}

fn parse_bool(s: &str, key: &str) -> anyhow::Result<bool> {
    match s {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(super::config_value(format!(
            "invalid value for {key}: '{s}'; must be true or false"
        ))),
    }
}

fn parse_tree_separators(s: &str) -> anyhow::Result<Vec<String>> {
    let seps: Vec<String> = s.split(',').map(str::to_string).collect();
    for sep in &seps {
        // Mirror validate_tree_separators exactly: require exactly one char,
        // non-control, non-whitespace, and display width == 1.
        // The width check rejects zero-width chars (U+200B, U+202E, U+FEFF,
        // Default_Ignorable) that pass the control/whitespace tests but would
        // cause every subsequent config load to fail (ConfigError 78) with no
        // CLI recovery path.
        let valid = {
            let mut chars = sep.chars();
            match (chars.next(), chars.next()) {
                (Some(ch), None) => !ch.is_control() && !ch.is_whitespace() && ch.width() == Some(1),
                _ => false,
            }
        };
        if !valid {
            return Err(super::config_value(format!(
                "invalid tree separator '{sep}': must be exactly one \
                 non-control, non-whitespace, single-column character"
            )));
        }
    }
    Ok(seps)
}

/// Reject values containing control characters (including newline) at exit 65.
///
/// All string values written into TOML are TOML-escaped in `write_config`, but
/// control characters produce confusing invisible input; reject them early so
/// the TOML layer never sees them.
fn reject_control_chars(value: &str, key: &str) -> anyhow::Result<()> {
    if value.chars().any(char::is_control) {
        return Err(super::config_value(format!(
            "value for {key} must not contain control characters (including newline)"
        )));
    }
    Ok(())
}

// ── Registry mutation helpers ─────────────────────────────────────────────────

/// Validate a registry alias at the CLI boundary (exit 64).
///
/// Rules mirror [`validate_registries`] in `project_config.rs`: non-empty,
/// no leading/trailing whitespace, no `/`, `"`, `\`, or control characters.
/// Called in `run_registry_add` and `parse_key` so bad aliases exit 64 rather
/// than reaching `validate_registries` → exit 78 (config error).
fn validate_alias_format(alias: &str) -> anyhow::Result<()> {
    if alias.is_empty() {
        return Err(super::config_usage("registry alias must not be empty".to_string()));
    }
    if alias != alias.trim() {
        return Err(super::config_usage(format!(
            "registry alias '{alias}' must not have leading or trailing whitespace"
        )));
    }
    if alias.contains('/') {
        return Err(super::config_usage(format!(
            "registry alias '{alias}' must not contain '/'"
        )));
    }
    if alias.contains('"') || alias.contains('\\') {
        return Err(super::config_usage(format!(
            "registry alias '{alias}' must not contain '\"' or '\\'"
        )));
    }
    if alias.chars().any(char::is_control) {
        return Err(super::config_usage(format!(
            "registry alias '{alias}' must not contain control characters"
        )));
    }
    Ok(())
}

fn find_registry<'a>(registries: &'a [RegistryConfig], alias: &str) -> Option<&'a RegistryConfig> {
    registries.iter().find(|r| r.alias.as_deref() == Some(alias))
}

fn set_registry_field(registries: &mut [RegistryConfig], alias: &str, mutate: impl FnOnce(&mut RegistryConfig)) {
    if let Some(rc) = registries.iter_mut().find(|r| r.alias.as_deref() == Some(alias)) {
        mutate(rc);
    }
}

fn clear_all_defaults(registries: &mut [RegistryConfig]) {
    for r in registries.iter_mut() {
        r.default = false;
    }
}

fn set_registry_default(registries: &mut [RegistryConfig], alias: &str, value: bool) {
    if let Some(rc) = registries.iter_mut().find(|r| r.alias.as_deref() == Some(alias)) {
        rc.default = value;
    }
}

// ── Shared write helpers ──────────────────────────────────────────────────────

/// Acquire the config-file advisory lock, or return `None` when the file does
/// not yet exist (new global config). The returned guard must remain alive for
/// the entire read-modify-write sequence.
fn acquire_config_lock(scope: &scope_resolution::ResolvedScope) -> anyhow::Result<Option<ConfigFileLock>> {
    match lockable_config_path(scope) {
        Some(path) => Ok(Some(super::grim(ConfigFileLock::try_acquire(&path))?)),
        None => Ok(None),
    }
}

/// Validate then atomically write the config for the given scope. Callers
/// must hold the lock returned by [`acquire_config_lock`] for the duration.
fn commit_config(
    scope: &scope_resolution::ResolvedScope,
    options: &ConfigOptions,
    registries: &[RegistryConfig],
) -> anyhow::Result<()> {
    super::grim(validate_registries(registries, &scope.config_path))?;
    super::grim(crate::command::add::write_config(
        &scope.config_path,
        options,
        registries,
        &scope.set,
    ))
}

// ── Sub-command handlers ──────────────────────────────────────────────────────

fn run_get(ctx: &Context, args: &ConfigArgs, key: &str) -> anyhow::Result<(ConfigReport, ExitCode)> {
    let parsed = parse_key(key)?;
    if matches!(parsed, ParsedKey::RegistryAlias { .. }) {
        return Err(super::config_usage(
            "cannot get registry without a field; \
             use registry.<alias>.url or registry.<alias>.default",
        ));
    }
    let scope = super::grim(scope_resolution::resolve(ctx, args.global, args.config.as_deref()))?;
    let value = get_value(&parsed, &scope.options, &scope.registries)?;
    let exit_code = if value.is_some() {
        ExitCode::Success
    } else {
        ExitCode::Failure
    };
    Ok((
        ConfigReport::Get(ConfigGetReport {
            key: key.to_string(),
            value,
            scope: scope_to_origin(scope.scope),
        }),
        exit_code,
    ))
}

fn run_set(ctx: &Context, args: &ConfigArgs, key: &str, value: &str) -> anyhow::Result<(ConfigReport, ExitCode)> {
    let parsed = parse_key(key)?;
    let scope = super::grim(scope_resolution::resolve(ctx, args.global, args.config.as_deref()))?;
    let origin = scope_to_origin(scope.scope);

    let _guard = acquire_config_lock(&scope)?;

    let mut options = scope.options.clone();
    let mut registries = scope.registries.clone();
    let stored = apply_set(&parsed, value, &mut options, &mut registries)?;
    commit_config(&scope, &options, &registries)?;

    Ok((
        ConfigReport::Write(ConfigWriteReport {
            action: WriteAction::Set,
            key: key.to_string(),
            value: Some(stored),
            scope: origin,
        }),
        ExitCode::Success,
    ))
}

fn run_unset(ctx: &Context, args: &ConfigArgs, key: &str) -> anyhow::Result<(ConfigReport, ExitCode)> {
    let parsed = parse_key(key)?;
    let scope = super::grim(scope_resolution::resolve(ctx, args.global, args.config.as_deref()))?;
    let origin = scope_to_origin(scope.scope);

    let _guard = acquire_config_lock(&scope)?;

    let mut options = scope.options.clone();
    let mut registries = scope.registries.clone();
    apply_unset(&parsed, &mut options, &mut registries)?;
    commit_config(&scope, &options, &registries)?;

    Ok((
        ConfigReport::Write(ConfigWriteReport {
            action: WriteAction::Unset,
            key: key.to_string(),
            value: None,
            scope: origin,
        }),
        ExitCode::Success,
    ))
}

fn run_list(ctx: &Context, args: &ConfigArgs) -> anyhow::Result<(ConfigReport, ExitCode)> {
    let scope = super::grim(scope_resolution::resolve(ctx, args.global, args.config.as_deref()))?;
    let entries = collect_entries(&scope.options, &scope.registries);
    Ok((ConfigReport::List(ConfigListReport { entries }), ExitCode::Success))
}

fn run_registry_add(
    ctx: &Context,
    args: &ConfigArgs,
    alias: &str,
    url: Option<&str>,
    index: Option<&str>,
    make_default: bool,
) -> anyhow::Result<(ConfigReport, ExitCode)> {
    // FIX 1: pre-validate alias at the CLI boundary (exit 64) so a bad alias
    // exits UsageError rather than ConfigError after write → validate_registries.
    validate_alias_format(alias)?;

    // Exactly one source locator (clap already rejects both via
    // `conflicts_with`; neither is checked here).
    let (locator, is_index) = match (url, index) {
        (Some(u), None) => (u, false),
        (None, Some(i)) => (i, true),
        _ => {
            return Err(super::config_usage(
                "exactly one of --url / --index must be given".to_string(),
            ));
        }
    };
    reject_control_chars(locator, if is_index { "registry.index" } else { "registry.url" })?;
    if is_index && crate::config::registry_resolve::classify_index(locator).is_none() {
        return Err(super::config_value(format!(
            "invalid index locator '{locator}': must be an http(s):// base or a \
             git repository (git+…, ssh://, git@…, or ending in .git)"
        )));
    }

    let scope = super::grim(scope_resolution::resolve(ctx, args.global, args.config.as_deref()))?;
    let origin = scope_to_origin(scope.scope);

    let _guard = acquire_config_lock(&scope)?;

    let mut registries = scope.registries.clone();

    if registries.iter().any(|r| r.alias.as_deref() == Some(alias)) {
        return Err(super::config_usage(format!(
            "registry '{alias}' already exists; use `grim config set registry.{alias}.url <url>` \
             to update or `grim config registry rm {alias}` to remove"
        )));
    }

    if make_default {
        clear_all_defaults(&mut registries);
    }
    registries.push(RegistryConfig {
        alias: Some(alias.to_string()),
        url: (!is_index).then(|| locator.to_string()),
        index: is_index.then(|| locator.to_string()),
        default: make_default,
    });

    commit_config(&scope, &scope.options, &registries)?;

    Ok((
        ConfigReport::Write(ConfigWriteReport {
            action: WriteAction::RegistryAdded,
            key: format!("registry.{alias}"),
            value: Some(locator.to_string()),
            scope: origin,
        }),
        ExitCode::Success,
    ))
}

fn run_registry_rm(ctx: &Context, args: &ConfigArgs, alias: &str) -> anyhow::Result<(ConfigReport, ExitCode)> {
    let scope = super::grim(scope_resolution::resolve(ctx, args.global, args.config.as_deref()))?;
    let origin = scope_to_origin(scope.scope);

    let _guard = acquire_config_lock(&scope)?;

    let mut registries = scope.registries.clone();
    if !registries.iter().any(|r| r.alias.as_deref() == Some(alias)) {
        return Err(super::config_usage(format!(
            "no registry '{alias}'; cannot remove a registry that does not exist"
        )));
    }
    registries.retain(|r| r.alias.as_deref() != Some(alias));

    commit_config(&scope, &scope.options, &registries)?;

    Ok((
        ConfigReport::Write(ConfigWriteReport {
            action: WriteAction::RegistryRemoved,
            key: format!("registry.{alias}"),
            value: None,
            scope: origin,
        }),
        ExitCode::Success,
    ))
}

fn run_registry_use(ctx: &Context, args: &ConfigArgs, alias: &str) -> anyhow::Result<(ConfigReport, ExitCode)> {
    let scope = super::grim(scope_resolution::resolve(ctx, args.global, args.config.as_deref()))?;
    let origin = scope_to_origin(scope.scope);

    let _guard = acquire_config_lock(&scope)?;

    let mut registries = scope.registries.clone();
    if !registries.iter().any(|r| r.alias.as_deref() == Some(alias)) {
        return Err(super::config_usage(format!(
            "no registry '{alias}'; add it with `grim config registry add`"
        )));
    }
    clear_all_defaults(&mut registries);
    set_registry_default(&mut registries, alias, true);

    commit_config(&scope, &scope.options, &registries)?;

    Ok((
        ConfigReport::Write(ConfigWriteReport {
            action: WriteAction::RegistryDefault,
            key: format!("registry.{alias}"),
            value: None,
            scope: origin,
        }),
        ExitCode::Success,
    ))
}

fn run_registry_show(ctx: &Context, args: &ConfigArgs, alias: &str) -> anyhow::Result<(ConfigReport, ExitCode)> {
    let scope = super::grim(scope_resolution::resolve(ctx, args.global, args.config.as_deref()))?;
    let rc = find_registry(&scope.registries, alias)
        .ok_or_else(|| super::config_usage(format!("no registry '{alias}'; add it with `grim config registry add`")))?;
    Ok((
        ConfigReport::RegistryShow(RegistryShowReport {
            alias: alias.to_string(),
            url: rc.url.clone(),
            index: rc.index.clone(),
            default: rc.default,
        }),
        ExitCode::Success,
    ))
}

fn run_registry_list(ctx: &Context, args: &ConfigArgs) -> anyhow::Result<(ConfigReport, ExitCode)> {
    let scope = super::grim(scope_resolution::resolve(ctx, args.global, args.config.as_deref()))?;
    let rows = scope
        .registries
        .iter()
        .map(|rc| RegistryRow {
            alias: rc.alias.clone(),
            url: rc.url.clone(),
            index: rc.index.clone(),
            default: rc.default,
        })
        .collect();
    Ok((
        ConfigReport::RegistryList(RegistryListReport { rows }),
        ExitCode::Success,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser as _;

    /// Minimal parse harness so the arg tree can be exercised in isolation.
    #[derive(clap::Parser)]
    struct Harness {
        #[command(subcommand)]
        cmd: Sub,
    }

    #[derive(clap::Subcommand)]
    enum Sub {
        Config(ConfigArgs),
    }

    fn parse(args: &[&str]) -> Result<ConfigArgs, clap::Error> {
        let mut argv = vec!["grim", "config"];
        argv.extend_from_slice(args);
        Harness::try_parse_from(argv).map(|h| match h.cmd {
            Sub::Config(a) => a,
        })
    }

    #[test]
    fn get_subcommand_parses() {
        let a = parse(&["get", "options.clients"]).expect("get parses");
        assert!(matches!(a.command, ConfigCommand::Get { key } if key == "options.clients"));
    }

    #[test]
    fn set_subcommand_parses() {
        let a = parse(&["set", "options.clients", "claude,opencode"]).expect("set parses");
        assert!(matches!(
            a.command,
            ConfigCommand::Set { key, value }
            if key == "options.clients" && value == "claude,opencode"
        ));
    }

    #[test]
    fn unset_subcommand_parses() {
        parse(&["unset", "options.clients"]).expect("unset parses");
    }

    #[test]
    fn list_without_flags_parses() {
        // --show-origin was removed (FIX 4: dead surface — list reads one scope,
        // origin would always be the same constant value).
        let a = parse(&["list"]).expect("list parses");
        assert!(matches!(a.command, ConfigCommand::List));
    }

    #[test]
    fn registry_add_parses() {
        let a = parse(&["registry", "add", "acme", "--url", "ghcr.io/acme"]).expect("registry add parses");
        match a.command {
            ConfigCommand::Registry(r) => match r.command {
                RegistryCommand::Add {
                    alias,
                    url,
                    index,
                    default,
                } => {
                    assert_eq!(alias, "acme");
                    assert_eq!(url.as_deref(), Some("ghcr.io/acme"));
                    assert_eq!(index, None);
                    assert!(!default);
                }
                _ => panic!("expected Add"),
            },
            _ => panic!("expected Registry"),
        }
    }

    #[test]
    fn registry_add_with_default_flag_parses() {
        let a = parse(&["registry", "add", "acme", "--url", "ghcr.io/acme", "--default"]).expect("parses");
        match a.command {
            ConfigCommand::Registry(r) => match r.command {
                RegistryCommand::Add { default, .. } => assert!(default),
                _ => panic!("expected Add"),
            },
            _ => panic!("expected Registry"),
        }
    }

    #[test]
    fn registry_rm_parses() {
        parse(&["registry", "rm", "acme"]).expect("registry rm parses");
    }

    #[test]
    fn registry_use_parses() {
        parse(&["registry", "use", "acme"]).expect("registry use parses");
    }

    #[test]
    fn registry_show_parses() {
        parse(&["registry", "show", "acme"]).expect("registry show parses");
    }

    #[test]
    fn registry_list_parses() {
        parse(&["registry", "list"]).expect("registry list parses");
    }

    #[test]
    fn get_missing_key_arg_fails() {
        assert!(parse(&["get"]).is_err());
    }

    #[test]
    fn set_missing_value_arg_fails() {
        assert!(parse(&["set", "options.clients"]).is_err());
    }

    #[test]
    fn registry_add_source_arg_combinations() {
        // Neither --url nor --index parses at the clap level (exactly-one is
        // a runtime usage error, 64, so the message can explain the choice);
        // both together conflict at the clap level; each alone parses.
        assert!(parse(&["registry", "add", "acme"]).is_ok());
        assert!(
            parse(&[
                "registry",
                "add",
                "acme",
                "--url",
                "ghcr.io/acme",
                "--index",
                "https://idx"
            ])
            .is_err()
        );
        assert!(parse(&["registry", "add", "acme", "--url", "ghcr.io/acme"]).is_ok());
        let a = parse(&["registry", "add", "hub", "--index", "https://index.grimoire.rs"]).expect("parses");
        match a.command {
            ConfigCommand::Registry(r) => match r.command {
                RegistryCommand::Add { url, index, .. } => {
                    assert_eq!(url, None);
                    assert_eq!(index.as_deref(), Some("https://index.grimoire.rs"));
                }
                _ => panic!("expected Add"),
            },
            _ => panic!("expected Registry"),
        }
    }

    // ── F3: parse_key, value-parser, and registry mutation unit tests ────────

    #[test]
    fn parse_key_all_seven_valid_keys() {
        assert!(matches!(parse_key("options.clients"), Ok(ParsedKey::OptionsClients)));
        assert!(matches!(
            parse_key("options.default_registry"),
            Ok(ParsedKey::OptionsDefaultRegistry)
        ));
        assert!(matches!(
            parse_key("options.tui.default_view"),
            Ok(ParsedKey::TuiDefaultView)
        ));
        assert!(matches!(
            parse_key("options.tui.group_by_type"),
            Ok(ParsedKey::TuiGroupByType)
        ));
        assert!(matches!(
            parse_key("options.tui.tree_separators"),
            Ok(ParsedKey::TuiTreeSeparators)
        ));
        assert!(matches!(
            parse_key("registry.acme.url"),
            Ok(ParsedKey::RegistryAliasField { alias, field: RegistryField::Url })
            if alias == "acme"
        ));
        assert!(matches!(
            parse_key("registry.acme.default"),
            Ok(ParsedKey::RegistryAliasField { alias, field: RegistryField::Default })
            if alias == "acme"
        ));
    }

    #[test]
    fn parse_key_registry_alias_without_field() {
        assert!(matches!(
            parse_key("registry.acme"),
            Ok(ParsedKey::RegistryAlias { alias }) if alias == "acme"
        ));
    }

    #[test]
    fn parse_key_unknown_returns_err() {
        assert!(parse_key("unknown.key").is_err());
        assert!(parse_key("optins.clients").is_err());
    }

    #[test]
    fn parse_default_view_valid_and_invalid() {
        use crate::config::declaration::DefaultView;
        assert!(matches!(parse_default_view("flat"), Ok(DefaultView::Flat)));
        assert!(matches!(parse_default_view("tree"), Ok(DefaultView::Tree)));
        assert!(parse_default_view("bogus").is_err());
        assert!(parse_default_view("Flat").is_err());
    }

    #[test]
    fn parse_bool_valid_and_invalid() {
        assert!(matches!(parse_bool("true", "k"), Ok(true)));
        assert!(matches!(parse_bool("false", "k"), Ok(false)));
        assert!(parse_bool("yes", "k").is_err());
        assert!(parse_bool("1", "k").is_err());
        assert!(parse_bool("True", "k").is_err());
    }

    #[test]
    fn parse_tree_separators_valid_and_invalid() {
        let r = parse_tree_separators("/,-").unwrap();
        assert_eq!(r, vec!["/", "-"]);
        // Multi-character entry rejected.
        assert!(parse_tree_separators("::").is_err());
        // Empty entry rejected.
        assert!(parse_tree_separators("").is_err());
        // Control character rejected.
        assert!(parse_tree_separators("\n").is_err());
    }

    #[test]
    fn parse_tree_separators_zero_width_char_rejected() {
        // FIX A regression lock: U+200B ZERO WIDTH SPACE passes the single-char
        // and control/whitespace checks but has display width 0, not 1. Without
        // the width check the CLI accepts it, writes a config that fails every
        // load (ConfigError 78), and `unset` also fails — complete lockout.
        // This mirrors validate_tree_separators in project_config.rs exactly.
        assert!(
            parse_tree_separators("\u{200b}").is_err(),
            "U+200B ZWSP must be rejected"
        );
        // Bidi override and BOM also have width 0.
        assert!(
            parse_tree_separators("\u{202e}").is_err(),
            "U+202E RLO must be rejected"
        );
        assert!(
            parse_tree_separators("\u{feff}").is_err(),
            "U+FEFF BOM must be rejected"
        );
        // Existing valid single-column chars still pass.
        assert!(parse_tree_separators("/").is_ok());
        assert!(parse_tree_separators("-").is_ok());
        assert!(parse_tree_separators("/,-").is_ok());
    }

    #[test]
    fn registry_use_enforces_at_most_one_default() {
        use crate::config::declaration::RegistryConfig;
        let mut registries = vec![
            RegistryConfig {
                alias: Some("a".to_string()),
                url: Some("u1".to_string()),
                index: None,
                default: true,
            },
            RegistryConfig {
                alias: Some("b".to_string()),
                url: Some("u2".to_string()),
                index: None,
                default: false,
            },
        ];
        // Simulate `registry use b`.
        clear_all_defaults(&mut registries);
        set_registry_default(&mut registries, "b", true);
        let defaults: Vec<_> = registries.iter().filter(|r| r.default).collect();
        assert_eq!(defaults.len(), 1, "exactly one default after use");
        assert_eq!(defaults[0].alias.as_deref(), Some("b"));
    }

    // ── FIX 1: alias pre-validation at CLI boundary ──────────────────────────

    #[test]
    fn validate_alias_format_rejects_slash() {
        assert!(
            validate_alias_format("a/b").is_err(),
            "alias with '/' must be rejected (exit 64)"
        );
    }

    #[test]
    fn validate_alias_format_rejects_empty() {
        assert!(validate_alias_format("").is_err(), "empty alias must be rejected");
    }

    #[test]
    fn validate_alias_format_rejects_leading_whitespace() {
        assert!(
            validate_alias_format(" acme").is_err(),
            "alias with leading whitespace must be rejected"
        );
    }

    #[test]
    fn validate_alias_format_rejects_control_char() {
        assert!(
            validate_alias_format("a\nb").is_err(),
            "alias with control char must be rejected"
        );
    }

    #[test]
    fn validate_alias_format_allows_dots() {
        // Dots are allowed in aliases (FIX 2 addressability).
        assert!(validate_alias_format("a.b").is_ok(), "alias with dot must be allowed");
        assert!(
            validate_alias_format("a.b.c").is_ok(),
            "alias with multiple dots must be allowed"
        );
    }

    // ── FIX 2: parse_key uses rightmost dot ──────────────────────────────────

    #[test]
    fn parse_key_dotted_alias_url() {
        // `registry.a.b.url` → alias=`a.b`, field=Url
        let result = parse_key("registry.a.b.url");
        assert!(result.is_ok(), "parse_key registry.a.b.url must succeed");
        match result.unwrap() {
            ParsedKey::RegistryAliasField {
                alias,
                field: RegistryField::Url,
            } => assert_eq!(alias, "a.b"),
            _ => panic!("expected RegistryAliasField(a.b, Url)"),
        }
    }

    #[test]
    fn parse_key_dotted_alias_default() {
        // `registry.a.b.default` → alias=`a.b`, field=Default
        let result = parse_key("registry.a.b.default");
        assert!(result.is_ok(), "parse_key registry.a.b.default must succeed");
        match result.unwrap() {
            ParsedKey::RegistryAliasField {
                alias,
                field: RegistryField::Default,
            } => assert_eq!(alias, "a.b"),
            _ => panic!("expected RegistryAliasField(a.b, Default)"),
        }
    }

    #[test]
    fn parse_key_slash_in_alias_exits_64() {
        // FIX 1: `registry.a/b.url` → alias `a/b` is invalid → usage error.
        // The error message must reference the bad character, confirming the
        // alias was caught at the CLI boundary (not at validate_registries).
        let result = parse_key("registry.a/b.url");
        assert!(result.is_err(), "slash in alias must be rejected");
        let msg = result.err().unwrap().to_string();
        assert!(
            msg.contains("'/'") || msg.contains('/'),
            "error must name the offending character; got: {msg}"
        );
    }

    // ── FIX 3: empty/whitespace segment in options.clients ───────────────────

    #[test]
    fn apply_set_clients_rejects_whitespace_segment() {
        use crate::config::declaration::{ConfigOptions, TuiOptions};
        let mut options = ConfigOptions {
            clients: vec![],
            default_registry: None,
            tui: TuiOptions::default(),
        };
        let mut registries = vec![];
        let result = apply_set(
            &ParsedKey::OptionsClients,
            "claude, ,opencode",
            &mut options,
            &mut registries,
        );
        // FIX 3: empty segment must be rejected with an error (exit 65).
        assert!(result.is_err(), "whitespace segment in clients must be rejected");
        let msg = result.err().unwrap().to_string();
        assert!(
            msg.contains("empty") || msg.contains("segment"),
            "error must describe the empty segment; got: {msg}"
        );
    }

    #[test]
    fn set_registry_alias_default_true_at_most_one() {
        use crate::config::declaration::RegistryConfig;
        let mut registries = vec![
            RegistryConfig {
                alias: Some("x".to_string()),
                url: Some("u1".to_string()),
                index: None,
                default: true,
            },
            RegistryConfig {
                alias: Some("y".to_string()),
                url: Some("u2".to_string()),
                index: None,
                default: false,
            },
        ];
        // Simulate `set registry.y.default true`.
        clear_all_defaults(&mut registries);
        set_registry_default(&mut registries, "y", true);
        assert_eq!(registries.iter().filter(|r| r.default).count(), 1);
    }
}
