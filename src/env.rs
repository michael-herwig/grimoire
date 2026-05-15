// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! Typed accessors over Grimoire's environment variables.
//!
//! `GRIM_LOG` is intentionally absent here — log filtering is initialized
//! directly in `main.rs` before anything else runs.

use std::path::PathBuf;

/// Environment variable holding the Grimoire data root.
const GRIM_HOME: &str = "GRIM_HOME";
/// Environment variable holding the default registry for short identifiers.
const GRIM_DEFAULT_REGISTRY: &str = "GRIM_DEFAULT_REGISTRY";
/// Environment variable that, when truthy, disables all network access.
const GRIM_OFFLINE: &str = "GRIM_OFFLINE";
/// Environment variable that, when truthy, routes lookups to the remote.
const GRIM_REMOTE: &str = "GRIM_REMOTE";

/// Resolves the Grimoire data root.
///
/// Uses `$GRIM_HOME` when set and non-empty, otherwise `~/.grimoire`. If
/// the home directory cannot be determined, falls back to `.grimoire`
/// relative to the current directory so the binary still runs.
pub fn grim_home() -> PathBuf {
    if let Some(dir) = non_empty_var(GRIM_HOME) {
        return PathBuf::from(dir);
    }
    match home_dir() {
        Some(home) => home.join(".grimoire"),
        None => PathBuf::from(".grimoire"),
    }
}

/// Returns the configured default registry, if `$GRIM_DEFAULT_REGISTRY`
/// is set and non-empty.
pub fn default_registry() -> Option<String> {
    non_empty_var(GRIM_DEFAULT_REGISTRY)
}

/// Whether offline mode is requested via `$GRIM_OFFLINE`.
pub fn offline() -> bool {
    truthy(GRIM_OFFLINE)
}

/// Whether remote routing is requested via `$GRIM_REMOTE`.
pub fn remote() -> bool {
    truthy(GRIM_REMOTE)
}

fn non_empty_var(key: &str) -> Option<String> {
    std::env::var(key).ok().and_then(non_empty)
}

fn non_empty(value: String) -> Option<String> {
    Some(value).filter(|v| !v.is_empty())
}

/// Treats `1`, `true`, `yes`, `on` (case-insensitive) as enabled.
fn truthy(key: &str) -> bool {
    std::env::var(key).is_ok_and(|v| is_truthy(&v))
}

fn is_truthy(value: &str) -> bool {
    matches!(value.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on")
}

/// Best-effort home directory without an external crate: `$HOME` on Unix,
/// `%USERPROFILE%` on Windows.
fn home_dir() -> Option<PathBuf> {
    let key = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    non_empty_var(key).map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Env mutation is `unsafe` in edition 2024 and the crate forbids
    // unsafe, so the parse logic is tested via the pure inner helpers
    // rather than by mutating the process environment.

    #[test]
    fn is_truthy_recognizes_common_forms() {
        for v in ["1", "true", "YES", "On", " true "] {
            assert!(is_truthy(v), "expected truthy: {v:?}");
        }
        for v in ["0", "false", "", "no", "off", "x"] {
            assert!(!is_truthy(v), "expected falsy: {v:?}");
        }
    }

    #[test]
    fn non_empty_filters_empty_string() {
        assert_eq!(non_empty(String::new()), None);
        assert_eq!(non_empty("x".to_string()), Some("x".to_string()));
    }

    #[test]
    fn grim_home_default_ends_in_dot_grimoire() {
        // When GRIM_HOME is unset (the default for the test process), the
        // resolved path is `<home>/.grimoire` or the relative fallback.
        if std::env::var(GRIM_HOME).is_err() {
            assert!(grim_home().ends_with(".grimoire"));
        }
    }
}
