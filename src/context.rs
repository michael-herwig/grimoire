// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! Per-invocation context.
//!
//! Built once per `grim` run. Phase 1 only resolves environment-derived
//! configuration; later phases attach the OCI-access client and local
//! store. Parsed CLI options override the corresponding environment
//! variables (the CLI is authoritative).

use std::path::PathBuf;
use std::sync::Arc;

use crate::cli::options::GlobalOptions;
use crate::env;
use crate::oci::access::cached_access::CachedAccess;
use crate::oci::access::registry_client::RegistryClient;
use crate::oci::access::{AccessMode, OciAccess};
use crate::oci::tag_cache::TagCache;
use crate::store::{BlobStore, GrimPaths};

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
    /// The `--registry` flag value only (highest registry precedence).
    registry_flag: Option<String>,
    /// `$GRIM_DEFAULT_REGISTRY`, captured once at construction.
    registry_env: Option<String>,
    offline: bool,
}

impl Context {
    /// Builds the context from parsed global options and the environment.
    ///
    /// Resolution-affecting CLI flags take precedence over their
    /// environment-variable counterparts. The `--registry` flag and the
    /// `$GRIM_DEFAULT_REGISTRY` env var are stored separately so the
    /// registry-precedence helper can order the flag above the env above
    /// any config default (see `command::resolve_default_registry`).
    pub fn new(options: &GlobalOptions) -> Self {
        Self {
            grim_home: env::grim_home(),
            registry_flag: options.registry.clone(),
            registry_env: env::default_registry(),
            offline: options.offline || env::offline(),
        }
    }

    /// The resolved Grimoire data root.
    pub fn grim_home(&self) -> &std::path::Path {
        &self.grim_home
    }

    /// The `--registry` flag value, if given. Highest registry precedence —
    /// the precedence helper orders this above env and config.
    pub fn registry_flag(&self) -> Option<&str> {
        self.registry_flag.as_deref()
    }

    /// `$GRIM_DEFAULT_REGISTRY`, if set.
    pub fn registry_env(&self) -> Option<&str> {
        self.registry_env.as_deref()
    }

    /// The default registry for short identifiers: the `--registry` flag,
    /// else `$GRIM_DEFAULT_REGISTRY`. Used by `login` / `logout` (which keep
    /// CLI-arg-first then this) — config defaults are layered in by
    /// `command::resolve_default_registry`, not here.
    pub fn default_registry(&self) -> Option<&str> {
        self.registry_flag.as_deref().or(self.registry_env.as_deref())
    }

    /// Whether all network access is disabled for this invocation.
    pub fn offline(&self) -> bool {
        self.offline
    }

    /// The resolved cache-routing mode for this invocation: `Offline` when
    /// the invocation is offline, otherwise the always-fresh `Online`
    /// default. See [`AccessMode`].
    pub fn access_mode(&self) -> AccessMode {
        if self.offline {
            AccessMode::Offline
        } else {
            AccessMode::Online
        }
    }

    /// Typed view of the `$GRIM_HOME` layout for this invocation.
    pub fn paths(&self) -> GrimPaths {
        GrimPaths::new(self.grim_home.clone())
    }

    /// Build the OCI-access seam: a real registry client behind the
    /// persistent tag + blob cache, routed by [`Self::access_mode`].
    ///
    /// `ensure_layout` is called here so the cache directories exist (and
    /// the single-volume invariant is asserted) before the first lookup.
    ///
    /// # Errors
    ///
    /// Returns an [`std::io::Error`] if the `$GRIM_HOME` layout cannot be
    /// created. Callers route it through the install-tier `TargetIo` error
    /// so it classifies as an I/O exit code, not the generic fall-through.
    pub fn access(&self) -> std::io::Result<Arc<dyn OciAccess>> {
        self.access_with_mode(self.access_mode())
    }

    /// Build the OCI-access seam with an explicit routing `mode`.
    ///
    /// # Errors
    ///
    /// Returns an [`std::io::Error`] if the `$GRIM_HOME` layout cannot be
    /// created.
    pub fn access_with_mode(&self, mode: AccessMode) -> std::io::Result<Arc<dyn OciAccess>> {
        let paths = self.paths();
        paths.ensure_layout()?;
        let cached = CachedAccess::new(
            RegistryClient::new(),
            TagCache::new(paths.tags_dir()),
            BlobStore::new(paths.blobs_dir()),
            mode,
        );
        Ok(Arc::new(cached))
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
        assert_eq!(ctx.access_mode(), AccessMode::Offline);
    }

    #[test]
    fn default_invocation_is_online() {
        let ctx = Context::new(&opts());
        assert!(!ctx.offline());
        assert_eq!(ctx.access_mode(), AccessMode::Online);
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
    fn registry_flag_is_split_from_env_and_surfaced_separately() {
        // The `--registry` flag populates `registry_flag()`; `default_registry()`
        // folds flag-or-env for login back-compat. (The env var is not mutated
        // here — that is `unsafe` and the crate forbids it; the env accessor is
        // exercised structurally.)
        let mut o = opts();
        o.registry = Some("ghcr.io/acme".to_string());
        let ctx = Context::new(&o);
        assert_eq!(ctx.registry_flag(), Some("ghcr.io/acme"));
        assert_eq!(ctx.default_registry(), Some("ghcr.io/acme"));
    }
}
