// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! Multi-registry resolution: the ordered browse set and qualified
//! `alias/repo` reference expansion.
//!
//! Two pure functions sit above [`Identifier`] and the single
//! `default_registry` precedence chain (`command::resolve_default_registry`):
//!
//! - [`resolve_registries`] builds the ordered, deduped set of registries a
//!   browse/search spans (`search`/`tui`/`mcp`). Only the `--registry` flag
//!   collapses the set — to exactly the flag's registries (repeatable /
//!   comma-separated, first is primary); `$GRIM_DEFAULT_REGISTRY` is a tier-3
//!   fallback (folded in only when no `[[registries]]` are declared).
//! - [`resolve_reference`] expands a user reference: an explicit registry
//!   parses strictly, a qualified `alias/repo` substitutes the alias's url,
//!   and a bare short id expands against the primary registry.
//!
//! The `alias/repo` form is collision-safe: [`Identifier::parse`] already
//! rejects a bare `alias/repo` with `MissingRegistry`, so alias substitution
//! only rescues inputs that fail today. The colon form `alias:repo` is never
//! interpreted as a qualified reference — it collides with `repo:tag`.

use crate::config::declaration::RegistryConfig;
use crate::oci::Identifier;
use crate::oci::identifier::error::IdentifierError;

/// How a resolved browse source lists its packages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKind {
    /// An OCI registry — listing via the `/v2/_catalog` endpoint.
    Registry,
    /// An HTTP(S) package index serving compiled static files (`all.json`).
    IndexHttp,
    /// A git repository holding `index/**/metadata.json`.
    IndexGit,
}

impl SourceKind {
    /// Whether this source is a package index (any transport).
    pub fn is_index(self) -> bool {
        matches!(self, Self::IndexHttp | Self::IndexGit)
    }
}

/// Classify an `index` locator into its transport, or `None` when the
/// locator matches neither form (validation rejects those at parse time).
///
/// Git forms are checked first so `https://host/repo.git` clones rather
/// than being read as a static-file base.
pub fn classify_index(locator: &str) -> Option<SourceKind> {
    let l = locator.trim();
    if l.is_empty() {
        return None;
    }
    if l.starts_with("git+") || l.starts_with("ssh://") || l.starts_with("git@") || l.ends_with(".git") {
        return Some(SourceKind::IndexGit);
    }
    if l.starts_with("http://") || l.starts_with("https://") {
        return Some(SourceKind::IndexHttp);
    }
    None
}

/// One browse source in the resolved set, in precedence order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedRegistry {
    /// The registry host (and optional namespace) — or, for an index
    /// source, the index locator.
    pub url: String,
    /// The configured alias, when one was declared.
    pub alias: Option<String>,
    /// Whether this is the primary registry short identifiers expand
    /// against. Exactly one entry in a resolved set carries it.
    pub is_default: bool,
    /// How this source lists its packages (`_catalog` vs package index).
    pub kind: SourceKind,
}

/// Build the ordered, deduped registry browse set.
///
/// Precedence:
/// 1. The `--registry` flag(s) (`forced`) collapse the set to exactly those
///    registries, in order and deduped (first is primary), preserving the
///    historical "flag-registries are the only ones searched" behavior. Only
///    the flag collapses; `$GRIM_DEFAULT_REGISTRY` (`env_default`) is a
///    default for tier 3, not a collapse trigger.
/// 2. Otherwise the declared `[[registries]]` are authoritative — project
///    entries then global entries, deduped by url (first occurrence wins).
///    Exactly one is marked primary: the first `default = true`, else the
///    first entry. When `[[registries]]` is non-empty the `env_default` is
///    NOT added to the browse set (it stays the short-id default only).
/// 3. When no `[[registries]]` are declared anywhere, the legacy single
///    `default_registry` chain applies: `env_default`
///    (`$GRIM_DEFAULT_REGISTRY`) then project, then global, then the
///    built-in `fallback`.
pub fn resolve_registries(
    forced: &[String],
    project: &[RegistryConfig],
    project_default: Option<&str>,
    global: &[RegistryConfig],
    global_default: Option<&str>,
    fallback: &str,
    env_default: Option<&str>,
) -> Vec<ResolvedRegistry> {
    // 1. The --registry flag(s) force exactly those registries, in order and
    // deduped (first is primary). The env var is NOT a collapse trigger — it
    // is folded in only at tier 3 below.
    let mut forced_set: Vec<ResolvedRegistry> = Vec::new();
    let mut forced_seen = std::collections::BTreeSet::new();
    for url in forced.iter().filter(|s| !s.is_empty()) {
        if forced_seen.insert(url.as_str()) {
            forced_set.push(ResolvedRegistry {
                url: url.clone(),
                alias: None,
                is_default: forced_set.is_empty(),
                kind: SourceKind::Registry,
            });
        }
    }
    if !forced_set.is_empty() {
        return forced_set;
    }

    // 2. Declared `[[registries]]` are authoritative when present. Each
    // entry is either a registry (`url`) or an index (`index`) source —
    // validation enforces exactly-one-of; an unclassifiable programmatic
    // index locator degrades to the HTTP transport rather than panicking.
    let mut out: Vec<ResolvedRegistry> = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for rc in project.iter().chain(global.iter()) {
        let (locator, kind) = match (&rc.url, &rc.index) {
            (Some(url), _) if !url.trim().is_empty() => (url.clone(), SourceKind::Registry),
            (_, Some(index)) if !index.trim().is_empty() => {
                (index.clone(), classify_index(index).unwrap_or(SourceKind::IndexHttp))
            }
            // Invalid entry (validation rejects it on parsed configs) —
            // skip rather than fabricate an empty source.
            _ => continue,
        };
        if seen.insert(locator.clone()) {
            out.push(ResolvedRegistry {
                url: locator,
                alias: rc.alias.clone(),
                is_default: rc.default,
                kind,
            });
        }
    }
    if !out.is_empty() {
        normalize_primary(&mut out);
        return out;
    }

    // 3. Legacy single default registry: env > project > global > fallback.
    // The env var is a default (tier 3) and is NOT consulted when
    // `[[registries]]` are declared (tier 2 wins above).
    let url = env_default
        .or(project_default)
        .or(global_default)
        .filter(|s| !s.is_empty())
        .unwrap_or(fallback)
        .to_string();
    vec![ResolvedRegistry {
        url,
        alias: None,
        is_default: true,
        kind: SourceKind::Registry,
    }]
}

/// Ensure exactly one entry is marked primary: keep the first `is_default`,
/// clear the rest; if none is marked, promote the first entry.
fn normalize_primary(registries: &mut [ResolvedRegistry]) {
    let first_default = registries.iter().position(|r| r.is_default);
    let primary = first_default.unwrap_or(0);
    for (i, r) in registries.iter_mut().enumerate() {
        r.is_default = i == primary;
    }
}

/// The primary registry's url: the `is_default` entry, else the first, else
/// the empty string (an empty set is never produced by [`resolve_registries`]).
pub fn primary_registry(registries: &[ResolvedRegistry]) -> &str {
    registries
        .iter()
        .find(|r| r.is_default)
        .or_else(|| registries.first())
        .map(|r| r.url.as_str())
        .unwrap_or("")
}

/// Expand a user reference into a fully-qualified [`Identifier`].
///
/// - A qualified `alias/repo[...]` whose leading `/`-segment matches a
///   configured alias substitutes that alias's url as the registry.
/// - Any other input expands against the primary registry exactly as a
///   single-registry `add` does today: an explicit registry parses
///   strictly, a bare short id gets the primary registry injected.
///
/// # Errors
///
/// Propagates [`IdentifierError`] for malformed input (invalid characters,
/// bad digest, uppercase repo, traversal segments).
pub fn resolve_reference(input: &str, registries: &[ResolvedRegistry]) -> Result<Identifier, IdentifierError> {
    if let Some((first, rest)) = input.split_once('/')
        && !rest.is_empty()
        && let Some(reg) = registries.iter().find(|r| r.alias.as_deref() == Some(first))
    {
        // Substitute the alias's url as an explicit registry, then parse
        // strictly (the substituted form always carries a registry).
        let substituted = format!("{}/{}", reg.url, rest);
        return Identifier::parse(&substituted);
    }
    Identifier::parse_with_default_registry(input, primary_registry(registries))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rc(alias: Option<&str>, url: &str, default: bool) -> RegistryConfig {
        RegistryConfig {
            alias: alias.map(str::to_string),
            url: Some(url.to_string()),
            index: None,
            default,
        }
    }

    fn rc_index(alias: Option<&str>, index: &str, default: bool) -> RegistryConfig {
        RegistryConfig {
            alias: alias.map(str::to_string),
            url: None,
            index: Some(index.to_string()),
            default,
        }
    }

    // ── Index-source classification and resolution ──────────────────────────

    #[test]
    fn classify_index_detects_transports() {
        assert_eq!(classify_index("https://index.grimoire.rs"), Some(SourceKind::IndexHttp));
        assert_eq!(classify_index("http://localhost:8080"), Some(SourceKind::IndexHttp));
        assert_eq!(
            classify_index("https://github.com/acme/index.git"),
            Some(SourceKind::IndexGit)
        );
        assert_eq!(
            classify_index("git+https://github.com/acme/index"),
            Some(SourceKind::IndexGit)
        );
        assert_eq!(
            classify_index("git@github.com:acme/index.git"),
            Some(SourceKind::IndexGit)
        );
        assert_eq!(classify_index("ssh://git@host/index"), Some(SourceKind::IndexGit));
        assert_eq!(
            classify_index("ghcr.io/acme"),
            None,
            "a bare registry url is not an index"
        );
        assert_eq!(classify_index(""), None);
    }

    #[test]
    fn index_entry_resolves_with_index_kind() {
        let set = resolve_registries(
            &[],
            &[
                rc_index(Some("hub"), "https://index.grimoire.rs", true),
                rc(Some("corp"), "registry.corp/team", false),
            ],
            None,
            &[],
            None,
            "grim.ocx.sh",
            None,
        );
        assert_eq!(set.len(), 2);
        assert_eq!(set[0].url, "https://index.grimoire.rs");
        assert_eq!(set[0].kind, SourceKind::IndexHttp);
        assert!(set[0].is_default);
        assert_eq!(set[1].kind, SourceKind::Registry);
    }

    #[test]
    fn invalid_entry_with_neither_url_nor_index_is_skipped() {
        // Programmatic (unvalidated) configs never fabricate an empty source.
        let empty = RegistryConfig::default();
        let set = resolve_registries(&[], &[empty], None, &[], None, "grim.ocx.sh", None);
        assert_eq!(set.len(), 1, "falls through to the legacy fallback tier");
        assert_eq!(set[0].url, "grim.ocx.sh");
    }

    #[test]
    fn forced_registry_collapses_to_single() {
        let set = resolve_registries(
            &["flag.example".to_string()],
            &[rc(Some("acme"), "ghcr.io/acme", true)],
            Some("proj.example"),
            &[],
            None,
            "grim.ocx.sh",
            None,
        );
        assert_eq!(set.len(), 1);
        assert_eq!(set[0].url, "flag.example");
        assert!(set[0].is_default);
    }

    #[test]
    fn multiple_forced_registries_collapse_to_all_in_order_first_primary() {
        // Several `--registry` values browse all of them at once, in order,
        // with the first as primary. `[[registries]]` and env are ignored.
        let set = resolve_registries(
            &["a.example".to_string(), "b.example".to_string()],
            &[rc(Some("acme"), "ghcr.io/acme", true)],
            Some("proj.example"),
            &[],
            None,
            "grim.ocx.sh",
            Some("env.example"),
        );
        assert_eq!(set.len(), 2);
        assert_eq!(set[0].url, "a.example");
        assert_eq!(set[1].url, "b.example");
        assert!(set[0].is_default, "first forced registry is primary");
        assert!(!set[1].is_default);
    }

    #[test]
    fn forced_registries_dedupe_keeping_first_and_skip_empty() {
        // Duplicate urls collapse to one (first wins); empty strings are
        // ignored so they never produce a blank registry entry.
        let set = resolve_registries(
            &[
                "".to_string(),
                "a.example".to_string(),
                "a.example".to_string(),
                "b.example".to_string(),
            ],
            &[],
            None,
            &[],
            None,
            "grim.ocx.sh",
            None,
        );
        assert_eq!(set.len(), 2);
        assert_eq!(set[0].url, "a.example");
        assert_eq!(set[1].url, "b.example");
        assert!(set[0].is_default);
    }

    #[test]
    fn all_empty_forced_registries_fall_through_to_registries_array() {
        // A `forced` slice of only empty strings is not a collapse trigger —
        // resolution falls through to the declared `[[registries]]`.
        let set = resolve_registries(
            &["".to_string()],
            &[rc(Some("acme"), "ghcr.io/acme", true)],
            None,
            &[],
            None,
            "grim.ocx.sh",
            None,
        );
        assert_eq!(set.len(), 1);
        assert_eq!(set[0].url, "ghcr.io/acme");
    }

    #[test]
    fn registries_array_is_authoritative_project_then_global() {
        let set = resolve_registries(
            &[],
            &[rc(Some("acme"), "ghcr.io/acme", false)],
            Some("proj.example"), // ignored when [[registries]] present
            &[rc(Some("corp"), "registry.corp/team", false)],
            None,
            "grim.ocx.sh",
            None,
        );
        assert_eq!(set.len(), 2);
        assert_eq!(set[0].url, "ghcr.io/acme");
        assert_eq!(set[1].url, "registry.corp/team");
        // No `default = true` ⇒ the first entry is primary.
        assert!(set[0].is_default);
        assert!(!set[1].is_default);
    }

    #[test]
    fn explicit_default_flag_picks_primary() {
        let set = resolve_registries(
            &[],
            &[
                rc(Some("acme"), "ghcr.io/acme", false),
                rc(Some("corp"), "registry.corp/team", true),
            ],
            None,
            &[],
            None,
            "grim.ocx.sh",
            None,
        );
        assert_eq!(primary_registry(&set), "registry.corp/team");
        assert!(set[1].is_default);
        assert!(!set[0].is_default);
    }

    #[test]
    fn duplicate_url_deduped_first_wins() {
        let set = resolve_registries(
            &[],
            &[rc(Some("a"), "ghcr.io/acme", false)],
            None,
            &[rc(Some("b"), "ghcr.io/acme", true)], // same url, global tier
            None,
            "grim.ocx.sh",
            None,
        );
        assert_eq!(set.len(), 1);
        assert_eq!(set[0].alias.as_deref(), Some("a"));
    }

    #[test]
    fn no_registries_folds_legacy_default() {
        let set = resolve_registries(
            &[],
            &[],
            Some("proj.example"),
            &[],
            Some("glob.example"),
            "grim.ocx.sh",
            None,
        );
        assert_eq!(set.len(), 1);
        assert_eq!(set[0].url, "proj.example");
        assert!(set[0].is_default);
    }

    #[test]
    fn no_registries_no_default_uses_fallback() {
        let set = resolve_registries(&[], &[], None, &[], None, "grim.ocx.sh", None);
        assert_eq!(set.len(), 1);
        assert_eq!(set[0].url, "grim.ocx.sh");
    }

    #[test]
    fn reference_explicit_registry_parses_as_is() {
        let set = resolve_registries(&[], &[], Some("ghcr.io/acme"), &[], None, "grim.ocx.sh", None);
        let id = resolve_reference("ghcr.io/other/x:1", &set).expect("explicit parses");
        assert_eq!(id.registry(), "ghcr.io");
        assert_eq!(id.to_string(), "ghcr.io/other/x:1");
    }

    #[test]
    fn reference_short_id_expands_against_primary() {
        let set = resolve_registries(&[], &[], Some("ghcr.io/acme"), &[], None, "grim.ocx.sh", None);
        let id = resolve_reference("code-review:stable", &set).expect("short id expands");
        assert_eq!(id.to_string(), "ghcr.io/acme/code-review:stable");
    }

    #[test]
    fn reference_qualified_alias_substitutes_url() {
        let set = resolve_registries(
            &[],
            &[rc(Some("corp"), "registry.corp/team", false)],
            None,
            &[],
            None,
            "grim.ocx.sh",
            None,
        );
        let id = resolve_reference("corp/internal-tool:1", &set).expect("alias substitutes");
        assert_eq!(id.registry(), "registry.corp");
        assert_eq!(id.to_string(), "registry.corp/team/internal-tool:1");
    }

    #[test]
    fn reference_repo_tag_is_not_treated_as_alias() {
        // `code-review:stable` must NOT be read as `alias:repo` even when a
        // `code-review` alias exists: the colon form has no `/`, so it
        // expands against the primary registry as a bare `repo:tag`. The
        // primary here is a *different* registry (`ghcr.io/acme`), so a hijack
        // would be visible as the alias's url leaking in.
        let set = resolve_registries(
            &[],
            &[
                rc(Some("code-review"), "registry.corp/team", false),
                rc(None, "ghcr.io/acme", true),
            ],
            None,
            &[],
            None,
            "grim.ocx.sh",
            None,
        );
        let id = resolve_reference("code-review:stable", &set).expect("repo:tag expands against primary");
        assert_eq!(id.to_string(), "ghcr.io/acme/code-review:stable");
    }

    #[test]
    fn reference_unknown_alias_prefix_expands_against_primary() {
        // `acme/x` where `acme` is not a configured alias is a multi-segment
        // repository path under the primary registry, exactly as today.
        let set = resolve_registries(&[], &[], Some("ghcr.io"), &[], None, "grim.ocx.sh", None);
        let id = resolve_reference("acme/x:1", &set).expect("repo path expands");
        assert_eq!(id.to_string(), "ghcr.io/acme/x:1");
    }

    // ── New tests for env_default semantics ────────────────────────────────

    #[test]
    fn env_default_does_not_collapse_when_registries_present() {
        // `$GRIM_DEFAULT_REGISTRY` must NOT interpose when `[[registries]]`
        // are declared — tier 2 (`[[registries]]`) wins; env is NOT added to
        // the browse set.
        let set = resolve_registries(
            &[],
            &[
                rc(Some("acme"), "ghcr.io/acme", true),
                rc(Some("corp"), "registry.corp/team", false),
            ],
            None,
            &[],
            None,
            "grim.ocx.sh",
            Some("env.example"), // env_default must be ignored when [[registries]] present
        );
        assert_eq!(
            set.len(),
            2,
            "[[registries]] declared → browse set must be the full array"
        );
        assert_eq!(set[0].url, "ghcr.io/acme");
        assert_eq!(set[1].url, "registry.corp/team");
        assert!(
            set.iter().all(|r| r.url != "env.example"),
            "env_default must NOT appear in browse set when [[registries]] is non-empty"
        );
    }

    #[test]
    fn env_default_is_single_default_when_no_registries() {
        // When no `[[registries]]` exist, `$GRIM_DEFAULT_REGISTRY` is used as
        // the single-default (tier 3 head), ahead of project and global config.
        let set = resolve_registries(
            &[],
            &[],
            Some("proj.example"),
            &[],
            Some("glob.example"),
            "grim.ocx.sh",
            Some("env.example"),
        );
        assert_eq!(set.len(), 1);
        assert_eq!(
            set[0].url, "env.example",
            "env_default must head tier-3 when no [[registries]] are declared"
        );
    }

    #[test]
    fn flag_collapses_even_with_registries_and_env() {
        // The `--registry` flag (`forced`) is the sole collapse trigger.
        // Both `[[registries]]` and `env_default` are present; the flag
        // still wins.
        let set = resolve_registries(
            &["flag.example".to_string()],
            &[rc(Some("acme"), "ghcr.io/acme", true)],
            None,
            &[],
            None,
            "grim.ocx.sh",
            Some("env.example"),
        );
        assert_eq!(set.len(), 1);
        assert_eq!(set[0].url, "flag.example");
    }

    #[test]
    fn env_default_beats_project_and_global_in_tier3() {
        // In tier 3 the precedence chain is env > project > global > fallback.
        let set = resolve_registries(
            &[],
            &[],
            Some("proj.example"),
            &[],
            Some("glob.example"),
            "grim.ocx.sh",
            Some("env.example"),
        );
        assert_eq!(
            set[0].url, "env.example",
            "env_default must beat project and global in tier 3"
        );
    }
}
