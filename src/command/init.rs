// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! `grim init` — create a fresh `grimoire.toml`.
//!
//! Project scope writes `./grimoire.toml`; `--global` writes
//! `$GRIM_HOME/grimoire.toml`. An existing file is never overwritten
//! (exit 64). When `--registry` is given (or the global `--registry` flag
//! / `$GRIM_DEFAULT_REGISTRY` is set), the body includes a `[[registries]]`
//! entry with `default = true` — the canonical on-disk shape that the
//! resolver treats as authoritative. When no registry is supplied the body
//! contains only empty `[skills]` / `[rules]` tables. The built-in fallback
//! registry is never snapshotted — it applies implicitly and must stay
//! floating.

use anyhow::Context as _;
use clap::Args;

use crate::api::artifact_status::InitStatus;
use crate::api::init_report::InitReport;
use crate::cli::exit_code::ExitCode;
use crate::config::config_error::{ConfigError, ConfigErrorKind};
use crate::config::scope::ConfigScope;
use crate::context::Context;

/// `grim init` arguments.
#[derive(Debug, Args)]
pub struct InitArgs {
    /// Create the global config (`$GRIM_HOME/grimoire.toml`) instead of a
    /// project-local one.
    #[arg(long)]
    pub global: bool,

    /// Seed the default registry as a `[[registries]]` entry with
    /// `default = true`.
    #[arg(long)]
    pub registry: Option<String>,
}

/// Run `grim init`.
///
/// # Errors
///
/// Returns a [`ConfigError`] (`ConfigAlreadyExists` ⇒ exit 64, I/O ⇒ 74)
/// if the file exists or cannot be written.
pub async fn run(ctx: &Context, args: &InitArgs) -> anyhow::Result<(InitReport, ExitCode)> {
    let (path, scope) = if args.global {
        (ctx.paths().global_config(), ConfigScope::Global)
    } else {
        let cwd = std::env::current_dir().context("resolving the current directory for `grim init`")?;
        (cwd.join("grimoire.toml"), ConfigScope::Project)
    };

    if path.exists() {
        return Err(crate::error::Error::from(ConfigError::new(path, ConfigErrorKind::ConfigAlreadyExists)).into());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| crate::error::Error::from(ConfigError::new(&path, ConfigErrorKind::Io(e))))?;
    }

    let body = render_config(snapshot_registry(ctx, args.registry.as_deref()));
    std::fs::write(&path, body)
        .map_err(|e| crate::error::Error::from(ConfigError::new(&path, ConfigErrorKind::Io(e))))?;

    let report = InitReport::new(path, scope, InitStatus::Created);
    Ok((report, ExitCode::Success))
}

/// The registry to snapshot into the seed config: `--registry` on `init`
/// wins, then the global `--registry` flag, then `$GRIM_DEFAULT_REGISTRY`.
/// The built-in fallback registry is deliberately NOT snapshotted — pinning
/// it would freeze a default that should keep following the binary.
fn snapshot_registry<'a>(ctx: &'a Context, explicit: Option<&'a str>) -> Option<&'a str> {
    explicit.or_else(|| ctx.registry_flag()).or_else(|| ctx.registry_env())
}

/// Render the seed config. When a registry is given, emit a `[[registries]]`
/// entry with `default = true` — the canonical on-disk shape the resolver
/// treats as authoritative. When none is given, emit only the empty
/// `[skills]` / `[rules]` tables (no `[[registries]]`, no `[options]`).
///
/// The registry URL is TOML-escaped via `toml::Value::String` to handle any
/// embedded quotes or backslashes in the URL (e.g. from unusual registry
/// configurations).
fn render_config(registry: Option<&str>) -> String {
    let mut out = String::new();
    if let Some(reg) = registry {
        // TOML-escape the url value so quotes or backslashes in the registry
        // string produce valid TOML, consistent with how `write_config` in
        // `add.rs` escapes tree_separators.
        let escaped_url = toml::Value::String(reg.to_string()).to_string();
        out.push_str("[[registries]]\n");
        out.push_str(&format!("url = {escaped_url}\n"));
        out.push_str("default = true\n\n");
    }
    out.push_str("[skills]\n\n[rules]\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_includes_registries_array_when_present() {
        // Contract (b): render_config(Some(…)) emits the [[registries]] shape,
        // NOT [options]/default_registry.
        let body = render_config(Some("ghcr.io/acme"));
        assert!(body.contains("[[registries]]"), "must contain [[registries]]");
        assert!(body.contains("url = \"ghcr.io/acme\""), "must contain url");
        assert!(body.contains("default = true"), "must contain default = true");
        assert!(
            !body.contains("default_registry ="),
            "must NOT contain legacy default_registry"
        );
        assert!(!body.contains("[options]"), "must NOT contain [options]");
        assert!(body.contains("[skills]"));
        assert!(body.contains("[rules]"));
    }

    #[test]
    fn render_output_parses_and_resolves_primary() {
        // Contract (b) round-trip: the shape init writes is the shape the
        // resolver treats as authoritative. Parse the rendered body and verify
        // primary_registry == the seeded url.
        use crate::config::registry_resolve::primary_registry;
        use crate::config::resolve_registries;
        let url = "ghcr.io/acme";
        let body = render_config(Some(url));
        let cfg =
            crate::config::project_config::ProjectConfig::from_toml_str(&body).expect("rendered config must parse");
        let set = resolve_registries(
            &[],
            &cfg.registries,
            cfg.options.default_registry.as_deref(),
            &[],
            None,
            crate::command::FALLBACK_REGISTRY,
            None,
        );
        assert_eq!(primary_registry(&set), url, "primary must equal the seeded url");
    }

    #[test]
    fn snapshot_registry_prefers_explicit_then_flag_then_env() {
        // Explicit `init --registry` wins over the context tiers.
        let tmp = tempfile::tempdir().unwrap();
        let hermetic = Context::hermetic(tmp.path().to_path_buf());
        assert_eq!(snapshot_registry(&hermetic, Some("init.example")), Some("init.example"));
        // Nothing anywhere ⇒ no snapshot (the built-in fallback stays
        // implicit, never written to disk).
        assert_eq!(snapshot_registry(&hermetic, None), None);

        // The global `--registry` flag is snapshotted when `init` has none.
        let opts = crate::cli::options::GlobalOptions {
            format: crate::cli::options::OutputFormat::Plain,
            offline: false,
            log_level: None,
            config: None,
            global: false,
            registry: vec!["flag.example".to_string()],
        };
        let ctx = Context::new(&opts);
        assert_eq!(snapshot_registry(&ctx, None), Some("flag.example"));
        assert_eq!(snapshot_registry(&ctx, Some("init.example")), Some("init.example"));
    }

    #[test]
    fn render_config_toml_escapes_url_with_special_chars() {
        // S1 (CWE-116): a registry url containing a backslash or quote must
        // produce valid TOML that round-trips to the same string — not break
        // the TOML parser or silently truncate the url.
        let url_with_backslash = r"example.io/org\repo";
        let url_with_quote = r#"example.io/org"repo"#;

        for url in &[url_with_backslash, url_with_quote] {
            let body = render_config(Some(url));
            let cfg = crate::config::project_config::ProjectConfig::from_toml_str(&body)
                .unwrap_or_else(|e| panic!("render_config({url:?}) produced invalid TOML: {e}"));
            assert_eq!(
                cfg.registries.len(),
                1,
                "must have exactly one [[registries]] entry for url={url:?}"
            );
            assert_eq!(
                cfg.registries[0].url.as_deref(),
                Some(&**url),
                "url must round-trip through TOML escaping for url={url:?}"
            );
        }
    }

    #[test]
    fn render_omits_options_table_without_registry() {
        let body = render_config(None);
        assert!(!body.contains("[options]"));
        assert!(body.starts_with("[skills]"));
        assert!(body.contains("[rules]"));
        // The seed must parse back as a valid (empty) config.
        let cfg = crate::config::project_config::ProjectConfig::from_toml_str(&body).unwrap();
        assert!(cfg.set.skills.is_empty());
        assert!(cfg.set.rules.is_empty());
    }
}
