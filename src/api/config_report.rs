// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! `grim config` output types.
//!
//! Plain format varies by variant:
//! - `Get`: bare value on a single line (no key, no table — script contract).
//! - `Write`: one-row table (`Action | Key | Value | Scope`) — the shared
//!   confirmation for `set`, `unset`, and `registry add`/`rm`/`use`.
//! - `List`: one table per invocation (`Key | Value`).
//! - `RegistryList`: one table (`Alias | URL | Default`).
//! - `RegistryShow`: one-row table (`Alias | URL | Default`).
//!
//! JSON format:
//! - `Get`: `{"key":"…","value":"…"|null,"set":bool,"scope":"…"}`.
//! - `Write`: single object matching struct fields.
//! - `List`: flat array of `{"key","value"}` objects.
//! - `RegistryList`: flat array of `{"alias","url","default"}` objects.
//! - `RegistryShow`: single object matching struct fields.

use std::fmt;
use std::io::{self, Write};

use serde::{Serialize, Serializer};

use crate::cli::printer::Printable;

/// The top-level dispatch type returned by `grim config` and rendered by
/// the `app.rs` dispatch arm.  Each variant corresponds to one config
/// subcommand group.
pub enum ConfigReport {
    /// Result of `grim config get`.
    Get(ConfigGetReport),
    /// Result of any write — `set`, `unset`, `registry add`/`rm`/`use`.
    Write(ConfigWriteReport),
    /// Result of `grim config list`.
    List(ConfigListReport),
    /// Result of `grim config registry list`.
    RegistryList(RegistryListReport),
    /// Result of `grim config registry show`.
    RegistryShow(RegistryShowReport),
}

impl Printable for ConfigReport {
    fn print_plain(&self, w: &mut impl Write) -> io::Result<()> {
        match self {
            Self::Get(r) => r.print_plain(w),
            Self::Write(r) => r.print_plain(w),
            Self::List(r) => r.print_plain(w),
            Self::RegistryList(r) => r.print_plain(w),
            Self::RegistryShow(r) => r.print_plain(w),
        }
    }

    fn print_json(&self, w: &mut impl Write) -> io::Result<()> {
        match self {
            Self::Get(r) => r.print_json(w),
            Self::Write(r) => r.print_json(w),
            Self::List(r) => r.print_json(w),
            Self::RegistryList(r) => r.print_json(w),
            Self::RegistryShow(r) => r.print_json(w),
        }
    }
}

/// Result of `grim config get <key>`.
///
/// Plain format: bare value on a single line (no key, no table).  `None`
/// means the key is present in the schema but has no value; the command
/// exits with `Failure(1)` and emits no output.
///
/// JSON format: `{"key":"…","value":"…","set":true,"scope":"…"}` when set,
/// or `{"key":"…","value":null,"set":false,"scope":"…"}` when unset.
/// The `set` field enables script-friendly boolean checks without testing
/// `value` for null.
#[derive(Debug)]
pub struct ConfigGetReport {
    /// The dotted key that was queried.
    pub key: String,
    /// The string value, or `None` when the key is unset.
    pub value: Option<String>,
    /// Which scope the value was read from.
    pub scope: Origin,
}

impl Serialize for ConfigGetReport {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(Some(4))?;
        map.serialize_entry("key", &self.key)?;
        map.serialize_entry("value", &self.value)?;
        map.serialize_entry("set", &self.value.is_some())?;
        map.serialize_entry("scope", &self.scope)?;
        map.end()
    }
}

impl Printable for ConfigGetReport {
    fn print_plain(&self, w: &mut impl Write) -> io::Result<()> {
        if let Some(value) = &self.value {
            writeln!(w, "{value}")
        } else {
            Ok(())
        }
    }

    fn print_json(&self, w: &mut impl Write) -> io::Result<()> {
        let json = serde_json::to_string_pretty(self).map_err(io::Error::other)?;
        writeln!(w, "{json}")
    }
}

/// The kind of write a [`ConfigWriteReport`] confirms.
///
/// Typed column rather than a raw string per `subsystem-cli-api.md`.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum WriteAction {
    /// `grim config set <key> <value>`.
    Set,
    /// `grim config unset <key>` (or `registry rm` is reported separately).
    Unset,
    /// `grim config registry add <alias>`.
    RegistryAdded,
    /// `grim config registry rm <alias>`.
    RegistryRemoved,
    /// `grim config registry use <alias>` (made default).
    RegistryDefault,
}

impl fmt::Display for WriteAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Set => "set",
            Self::Unset => "unset",
            Self::RegistryAdded => "registry-added",
            Self::RegistryRemoved => "registry-removed",
            Self::RegistryDefault => "registry-default",
        })
    }
}

/// Confirmation for any config write: `set`, `unset`, and the registry
/// lifecycle verbs (`add`, `rm`, `use`).
///
/// Plain format: one-row table — `Action | Key | Value | Scope`.
///
/// JSON format: `{"action": "…", "key": "…", "value": "…"|null, "scope": "…"}`.
#[derive(Debug, Serialize)]
pub struct ConfigWriteReport {
    /// What kind of write this confirms.
    pub action: WriteAction,
    /// The dotted key or `registry.<alias>` affected.
    pub key: String,
    /// The new value (e.g. the URL for `registry add`), or `None` for
    /// `unset` / `rm` / `use`.
    pub value: Option<String>,
    /// Which scope was written.
    pub scope: Origin,
}

impl Printable for ConfigWriteReport {
    fn print_plain(&self, w: &mut impl Write) -> io::Result<()> {
        use crate::cli::printer::print_table;
        let value_str = self.value.as_deref().unwrap_or("");
        print_table(
            w,
            &["Action", "Key", "Value", "Scope"],
            &[vec![
                self.action.to_string(),
                self.key.clone(),
                value_str.to_string(),
                self.scope.to_string(),
            ]],
        )
    }

    fn print_json(&self, w: &mut impl Write) -> io::Result<()> {
        let json = serde_json::to_string_pretty(self).map_err(io::Error::other)?;
        writeln!(w, "{json}")
    }
}

/// Result of `grim config list`.
///
/// Plain format: one table — `Key | Value`. The list reads from exactly one
/// scope per invocation, so an Origin column would be constant-valued and
/// is omitted.
///
/// JSON format: flat array of `{"key":"…","value":"…"}` objects. Never
/// wrapped in a parent object — per `subsystem-cli-api.md` custom-Serialize
/// rule.
#[derive(Debug)]
pub struct ConfigListReport {
    /// All effective key=value pairs for the scope.
    pub entries: Vec<ConfigEntry>,
}

impl Serialize for ConfigListReport {
    /// Flatten to a bare array per `subsystem-cli-api.md`.
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        // ponytail: delegate directly — entries already derive Serialize
        self.entries.serialize(serializer)
    }
}

impl Printable for ConfigListReport {
    fn print_plain(&self, w: &mut impl Write) -> io::Result<()> {
        use crate::cli::printer::print_table;
        let rows: Vec<Vec<String>> = self
            .entries
            .iter()
            .map(|e| vec![e.key.clone(), e.value.clone()])
            .collect();
        print_table(w, &["Key", "Value"], &rows)
    }

    fn print_json(&self, w: &mut impl Write) -> io::Result<()> {
        let json = serde_json::to_string_pretty(self).map_err(io::Error::other)?;
        writeln!(w, "{json}")
    }
}

/// One key=value line from `grim config list`.
#[derive(Debug, Serialize)]
pub struct ConfigEntry {
    /// The dotted key.
    pub key: String,
    /// The string representation of the value.
    pub value: String,
}

/// The scope a config value originated from.
///
/// Used as a typed column in `grim config list --show-origin` — never a
/// raw string per `subsystem-cli-api.md` typed-enum rule.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Origin {
    /// From the project `grimoire.toml`.
    Project,
    /// From `$GRIM_HOME/grimoire.toml`.
    Global,
}

impl fmt::Display for Origin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Project => "project",
            Self::Global => "global",
        })
    }
}

/// Result of `grim config registry list`.
///
/// Plain format: one table — `Alias | URL | Default`.
///
/// JSON format: flat array of `{"alias":"…"|null,"url":"…","default":bool}`
/// objects. Never wrapped in a parent object — per `subsystem-cli-api.md`
/// custom-Serialize rule.
#[derive(Debug)]
pub struct RegistryListReport {
    /// All registries declared in the scope's `[[registries]]`.
    pub rows: Vec<RegistryRow>,
}

impl Serialize for RegistryListReport {
    /// Flatten to a bare array per `subsystem-cli-api.md`.
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.rows.serialize(serializer)
    }
}

impl Printable for RegistryListReport {
    fn print_plain(&self, w: &mut impl Write) -> io::Result<()> {
        use crate::cli::printer::print_table;
        let rows: Vec<Vec<String>> = self
            .rows
            .iter()
            .map(|r| {
                let (ty, source) = type_and_source(r.url.as_deref(), r.index.as_deref());
                vec![
                    r.alias.as_deref().unwrap_or("").to_string(),
                    ty.to_string(),
                    source.to_string(),
                    r.default.to_string(),
                ]
            })
            .collect();
        print_table(w, &["Alias", "Type", "Source", "Default"], &rows)
    }

    fn print_json(&self, w: &mut impl Write) -> io::Result<()> {
        let json = serde_json::to_string_pretty(self).map_err(io::Error::other)?;
        writeln!(w, "{json}")
    }
}

/// The `Type | Source` cell pair for a registry/index entry: which kind of
/// browse source it is and its locator. Empty pair only for an invalid
/// entry that validation would reject.
fn type_and_source<'a>(url: Option<&'a str>, index: Option<&'a str>) -> (&'static str, &'a str) {
    match (url, index) {
        (Some(url), _) => ("registry", url),
        (None, Some(index)) => ("index", index),
        (None, None) => ("", ""),
    }
}

/// One row in `grim config registry list`.
#[derive(Debug, Serialize)]
pub struct RegistryRow {
    /// The registry alias, or `None` for alias-less (locator-only) entries.
    pub alias: Option<String>,
    /// The registry URL (`None` for index entries).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// The package-index locator (`None` for registry entries).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub index: Option<String>,
    /// Whether this is the default registry.
    pub default: bool,
}

/// Result of `grim config registry show <alias>`.
///
/// Plain format: one-row table — `Alias | Type | Source | Default`.
///
/// JSON format: `{"alias": "…", "url"|"index": "…", "default": bool}`.
#[derive(Debug, Serialize)]
pub struct RegistryShowReport {
    /// The registry alias.
    pub alias: String,
    /// The registry URL (`None` for index entries).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// The package-index locator (`None` for registry entries).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub index: Option<String>,
    /// Whether this is the default registry.
    pub default: bool,
}

impl Printable for RegistryShowReport {
    fn print_plain(&self, w: &mut impl Write) -> io::Result<()> {
        use crate::cli::printer::print_table;
        let (ty, source) = type_and_source(self.url.as_deref(), self.index.as_deref());
        print_table(
            w,
            &["Alias", "Type", "Source", "Default"],
            &[vec![
                self.alias.clone(),
                ty.to_string(),
                source.to_string(),
                self.default.to_string(),
            ]],
        )
    }

    fn print_json(&self, w: &mut impl Write) -> io::Result<()> {
        let json = serde_json::to_string_pretty(self).map_err(io::Error::other)?;
        writeln!(w, "{json}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origin_display_matches_serde_rename() {
        assert_eq!(Origin::Project.to_string(), "project");
        assert_eq!(Origin::Global.to_string(), "global");
    }

    #[test]
    fn config_get_report_serializes_with_value() {
        let r = ConfigGetReport {
            key: "options.clients".to_string(),
            value: Some("claude".to_string()),
            scope: Origin::Project,
        };
        let v: serde_json::Value = serde_json::from_str(&serde_json::to_string(&r).unwrap()).unwrap();
        assert_eq!(v["key"], "options.clients");
        assert_eq!(v["value"], "claude");
        assert_eq!(v["set"], true);
        assert_eq!(v["scope"], "project");
    }

    #[test]
    fn config_get_report_serializes_none_as_null_with_set_false() {
        let r = ConfigGetReport {
            key: "options.clients".to_string(),
            value: None,
            scope: Origin::Global,
        };
        let v: serde_json::Value = serde_json::from_str(&serde_json::to_string(&r).unwrap()).unwrap();
        assert!(v["value"].is_null());
        assert_eq!(v["set"], false);
        assert_eq!(v["scope"], "global");
    }

    #[test]
    fn config_get_report_plain_prints_bare_value_when_set() {
        // ADR: plain `get` emits bare value — no key, no table — so that
        // `$(grim config get options.clients)` works in scripts.
        let r = ConfigGetReport {
            key: "options.clients".to_string(),
            value: Some("claude,opencode".to_string()),
            scope: Origin::Project,
        };
        let mut buf: Vec<u8> = Vec::new();
        r.print_plain(&mut buf).unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(
            out.contains("claude,opencode"),
            "plain get must emit the bare value; got: {out:?}"
        );
        // Must NOT echo the key — callers rely on value-only stdout.
        assert!(
            !out.contains("options.clients"),
            "plain get must not echo the key; got: {out:?}"
        );
    }

    #[test]
    fn config_get_report_plain_emits_nothing_when_unset() {
        // ADR: `get` of an unset key exits Failure(1) with no stdout.
        // The Printable impl must not write anything to `w` when value is None.
        let r = ConfigGetReport {
            key: "options.clients".to_string(),
            value: None,
            scope: Origin::Project,
        };
        let mut buf: Vec<u8> = Vec::new();
        r.print_plain(&mut buf).unwrap();
        assert!(
            buf.is_empty(),
            "plain get of unset key must write nothing; got: {buf:?}"
        );
    }

    #[test]
    fn config_write_report_json_carries_action_key_value_scope() {
        // ADR: ConfigWriteReport JSON shape {"action":"…","key":"…","value":"…","scope":"…"}.
        let r = ConfigWriteReport {
            action: WriteAction::Set,
            key: "options.clients".to_string(),
            value: Some("claude".to_string()),
            scope: Origin::Project,
        };
        let mut buf: Vec<u8> = Vec::new();
        r.print_json(&mut buf).unwrap();
        let v: serde_json::Value = serde_json::from_slice(&buf).unwrap();
        assert!(v["action"].is_string(), "action must be a string; got: {v}");
        assert_eq!(v["key"], "options.clients", "key field must match");
        assert_eq!(v["scope"], "project", "scope must be 'project'");
        let val = v["value"].as_str().unwrap_or("");
        assert!(val.contains("claude"), "value field must contain 'claude'");
    }

    #[test]
    fn config_write_report_plain_emits_table_with_action_columns() {
        // subsystem-cli-api.md: single-table rule — exactly one print_table call.
        // The table must contain action, key, value, and scope data.
        let r = ConfigWriteReport {
            action: WriteAction::Set,
            key: "options.clients".to_string(),
            value: Some("claude".to_string()),
            scope: Origin::Project,
        };
        let mut buf: Vec<u8> = Vec::new();
        r.print_plain(&mut buf).unwrap();
        let text = String::from_utf8(buf).unwrap();
        assert!(!text.is_empty(), "plain write-confirmation must not be empty");
        // All four column values must appear in the output.
        assert!(
            text.contains("options.clients"),
            "key must appear in table; got: {text:?}"
        );
        assert!(text.contains("claude"), "value must appear in table; got: {text:?}");
        assert!(text.contains("project"), "scope must appear in table; got: {text:?}");
    }

    #[test]
    fn config_list_report_plain_shows_key_value_entries() {
        // ADR: list plain format — key=value lines, one table per invocation.
        let r = ConfigListReport {
            entries: vec![ConfigEntry {
                key: "options.clients".to_string(),
                value: "claude".to_string(),
            }],
        };
        let mut buf: Vec<u8> = Vec::new();
        r.print_plain(&mut buf).unwrap();
        let text = String::from_utf8(buf).unwrap();
        assert!(
            text.contains("options.clients"),
            "key must appear in list output; got: {text:?}"
        );
        assert!(
            text.contains("claude"),
            "value must appear in list output; got: {text:?}"
        );
    }

    #[test]
    fn config_list_report_json_is_flat_array() {
        // W2: Serialize must flatten to bare array, not wrap in {"entries":[...]}.
        let r = ConfigListReport {
            entries: vec![ConfigEntry {
                key: "options.clients".to_string(),
                value: "claude".to_string(),
            }],
        };
        let mut buf: Vec<u8> = Vec::new();
        r.print_json(&mut buf).unwrap();
        let v: serde_json::Value = serde_json::from_slice(&buf).unwrap();
        assert!(v.is_array(), "JSON list must be a bare array; got: {v}");
        assert_eq!(v[0]["key"], "options.clients");
        assert_eq!(v[0]["value"], "claude");
    }

    #[test]
    fn registry_list_report_plain_shows_alias_url_default() {
        // ADR: registry list — one table (Alias | URL | Default).
        let r = RegistryListReport {
            rows: vec![RegistryRow {
                alias: Some("acme".to_string()),
                url: Some("ghcr.io/acme".to_string()),
                index: None,
                default: true,
            }],
        };
        let mut buf: Vec<u8> = Vec::new();
        r.print_plain(&mut buf).unwrap();
        let text = String::from_utf8(buf).unwrap();
        assert!(text.contains("acme"), "alias must appear; got: {text:?}");
        assert!(text.contains("ghcr.io/acme"), "URL must appear; got: {text:?}");
    }

    #[test]
    fn registry_list_report_json_is_flat_array() {
        // W2: Serialize must flatten to bare array, not wrap in {"rows":[...]}.
        let r = RegistryListReport {
            rows: vec![RegistryRow {
                alias: Some("acme".to_string()),
                url: Some("ghcr.io/acme".to_string()),
                index: None,
                default: false,
            }],
        };
        let mut buf: Vec<u8> = Vec::new();
        r.print_json(&mut buf).unwrap();
        let v: serde_json::Value = serde_json::from_slice(&buf).unwrap();
        assert!(v.is_array(), "registry list JSON must be a bare array; got: {v}");
        assert_eq!(v[0]["alias"], "acme");
        assert_eq!(v[0]["url"], "ghcr.io/acme");
    }

    #[test]
    fn registry_show_report_plain_is_one_row_table() {
        // ADR: registry show — one-row table (Alias | URL | Default).
        let r = RegistryShowReport {
            alias: "acme".to_string(),
            url: Some("ghcr.io/acme".to_string()),
            index: None,
            default: false,
        };
        let mut buf: Vec<u8> = Vec::new();
        r.print_plain(&mut buf).unwrap();
        let text = String::from_utf8(buf).unwrap();
        assert!(text.contains("acme"), "alias must appear; got: {text:?}");
        assert!(text.contains("ghcr.io/acme"), "URL must appear; got: {text:?}");
    }
}
