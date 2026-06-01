// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! `grim login` / `grim logout` output.
//!
//! Plain format: a single confirmation table — `Registry | Username` for
//! login, `Registry` for logout.
//!
//! JSON format: a single object (`{"registry","username"}` /
//! `{"registry"}`), not an array — there is exactly one subject.

use std::io::{self, Write};

use serde::Serialize;

use crate::cli::printer::{Printable, print_table};

/// The result of a successful `grim login`.
#[derive(Debug, Serialize)]
pub struct LoginReport {
    /// The registry the credential was stored for (canonical form).
    pub registry: String,
    /// The account name that was authenticated.
    pub username: String,
}

impl LoginReport {
    /// Build from the resolved registry and username.
    pub fn new(registry: impl Into<String>, username: impl Into<String>) -> Self {
        Self {
            registry: registry.into(),
            username: username.into(),
        }
    }
}

impl Printable for LoginReport {
    fn print_plain(&self, w: &mut impl Write) -> io::Result<()> {
        print_table(
            w,
            &["Registry", "Username"],
            &[vec![self.registry.clone(), self.username.clone()]],
        )
    }

    fn print_json(&self, w: &mut impl Write) -> io::Result<()> {
        let json = serde_json::to_string_pretty(self).map_err(io::Error::other)?;
        writeln!(w, "{json}")
    }
}

/// The result of a successful `grim logout`.
#[derive(Debug, Serialize)]
pub struct LogoutReport {
    /// The registry the credential was removed for (canonical form).
    pub registry: String,
}

impl LogoutReport {
    /// Build from the resolved registry.
    pub fn new(registry: impl Into<String>) -> Self {
        Self {
            registry: registry.into(),
        }
    }
}

impl Printable for LogoutReport {
    fn print_plain(&self, w: &mut impl Write) -> io::Result<()> {
        print_table(w, &["Registry"], &[vec![self.registry.clone()]])
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
    fn login_plain_is_single_table_with_header() {
        let r = LoginReport::new("ghcr.io", "alice");
        let mut buf = Vec::new();
        r.print_plain(&mut buf).unwrap();
        let out = String::from_utf8(buf).unwrap();
        let lines: Vec<&str> = out.lines().collect();
        assert!(lines[0].starts_with("Registry"));
        assert!(lines[0].contains("Username"));
        assert!(lines[1].contains("ghcr.io"));
        assert!(lines[1].contains("alice"));
    }

    #[test]
    fn login_json_is_single_object() {
        let r = LoginReport::new("ghcr.io", "alice");
        let mut buf = Vec::new();
        r.print_json(&mut buf).unwrap();
        let v: serde_json::Value = serde_json::from_slice(&buf).unwrap();
        assert!(v.is_object());
        assert_eq!(v["registry"], "ghcr.io");
        assert_eq!(v["username"], "alice");
    }

    #[test]
    fn logout_json_carries_only_registry() {
        let r = LogoutReport::new("ghcr.io");
        let mut buf = Vec::new();
        r.print_json(&mut buf).unwrap();
        let v: serde_json::Value = serde_json::from_slice(&buf).unwrap();
        assert_eq!(v["registry"], "ghcr.io");
        assert!(v.get("username").is_none());
    }
}
