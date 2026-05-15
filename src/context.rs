// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! Per-invocation context.
//!
//! Built once per `grim` run. Phase 1 only resolves environment-derived
//! configuration; later phases attach the OCI-access client and local
//! store. Parsed CLI options override the corresponding environment
//! variables (the CLI is authoritative).

use std::path::PathBuf;

use crate::cli::options::GlobalOptions;
use crate::env;

/// Resolved configuration for a single `grim` invocation.
///
/// Fields are resolved eagerly but cheaply (env reads only). The OCI
/// client / local store seam is deferred to Phase 3.
//
// TODO(phase-3): add the resolved OCI-access client + local store here,
// constructed lazily so commands that don't touch the registry pay
// nothing. No stub trait in Phase 1 — the seam lands with the access
// subsystem so its shape is driven by real call sites.
#[derive(Debug, Clone)]
pub struct Context {
    grim_home: PathBuf,
    default_registry: Option<String>,
    offline: bool,
    remote: bool,
}

impl Context {
    /// Builds the context from parsed global options and the environment.
    ///
    /// Resolution-affecting CLI flags take precedence over their
    /// environment-variable counterparts.
    pub fn new(options: &GlobalOptions) -> Self {
        let default_registry = options.registry.clone().or_else(env::default_registry);
        Self {
            grim_home: env::grim_home(),
            default_registry,
            offline: options.offline || env::offline(),
            remote: options.remote || env::remote(),
        }
    }

    /// The resolved Grimoire data root.
    pub fn grim_home(&self) -> &std::path::Path {
        &self.grim_home
    }

    /// The default registry for short identifiers, if configured.
    pub fn default_registry(&self) -> Option<&str> {
        self.default_registry.as_deref()
    }

    /// Whether all network access is disabled for this invocation.
    pub fn offline(&self) -> bool {
        self.offline
    }

    /// Whether mutable lookups route to the remote registry.
    pub fn remote(&self) -> bool {
        self.remote
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::options::OutputFormat;

    fn opts() -> GlobalOptions {
        GlobalOptions {
            format: OutputFormat::Plain,
            offline: false,
            remote: false,
            log_level: None,
            config: None,
            global: false,
            registry: None,
        }
    }

    #[test]
    fn cli_offline_flag_forces_offline_regardless_of_env() {
        let mut o = opts();
        o.offline = true;
        let ctx = Context::new(&o);
        assert!(ctx.offline());
    }

    #[test]
    fn cli_registry_overrides_and_grim_home_resolves() {
        let mut o = opts();
        o.registry = Some("ghcr.io/acme".to_string());
        let ctx = Context::new(&o);
        assert_eq!(ctx.default_registry(), Some("ghcr.io/acme"));
        assert!(ctx.grim_home().is_absolute() || ctx.grim_home().ends_with(".grimoire"));
    }

    #[test]
    fn remote_flag_propagates() {
        let mut o = opts();
        o.remote = true;
        assert!(Context::new(&o).remote());
    }
}
