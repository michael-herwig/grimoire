// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! Multi-registry resolution: the ordered browse set and qualified
//! `alias/repo` reference expansion.
//!
//! Two pure functions sit above [`Identifier`] and the single
//! `default_registry` precedence chain (`command::resolve_default_registry`):
//!
//! - [`resolve_registries`] builds the ordered, deduped set of registries a
//!   browse/search spans (`search`/`tui`/`mcp`), folding the legacy single
//!   `default_registry` in only when no `[[registries]]` are declared.
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

/// One registry in the resolved browse set, in precedence order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedRegistry {
    /// The registry host (and optional namespace).
    pub url: String,
    /// The configured alias, when one was declared.
    pub alias: Option<String>,
    /// Whether this is the primary registry short identifiers expand
    /// against. Exactly one entry in a resolved set carries it.
    pub is_default: bool,
}

/// Build the ordered, deduped registry browse set.
///
/// Precedence:
/// 1. A forced single registry (the `--registry` flag or
///    `$GRIM_DEFAULT_REGISTRY`) collapses the set to exactly that registry,
///    preserving the historical "`--registry` searches only this one"
///    behavior.
/// 2. Otherwise the declared `[[registries]]` are authoritative — project
///    entries then global entries, deduped by url (first occurrence wins).
///    Exactly one is marked primary: the first `default = true`, else the
///    first entry.
/// 3. When no `[[registries]]` are declared anywhere, the legacy single
///    `default_registry` chain applies (project, then global, then the
///    built-in `fallback`).
pub fn resolve_registries(
    forced: Option<&str>,
    project: &[RegistryConfig],
    project_default: Option<&str>,
    global: &[RegistryConfig],
    global_default: Option<&str>,
    fallback: &str,
) -> Vec<ResolvedRegistry> {
    // 1. An explicit flag / env forces exactly one registry.
    if let Some(url) = forced.filter(|s| !s.is_empty()) {
        return vec![ResolvedRegistry {
            url: url.to_string(),
            alias: None,
            is_default: true,
        }];
    }

    // 2. Declared `[[registries]]` are authoritative when present.
    let mut out: Vec<ResolvedRegistry> = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for rc in project.iter().chain(global.iter()) {
        if seen.insert(rc.url.clone()) {
            out.push(ResolvedRegistry {
                url: rc.url.clone(),
                alias: rc.alias.clone(),
                is_default: rc.default,
            });
        }
    }
    if !out.is_empty() {
        normalize_primary(&mut out);
        return out;
    }

    // 3. Legacy single default registry.
    let url = project_default
        .or(global_default)
        .filter(|s| !s.is_empty())
        .unwrap_or(fallback)
        .to_string();
    vec![ResolvedRegistry {
        url,
        alias: None,
        is_default: true,
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
            url: url.to_string(),
            default,
        }
    }

    #[test]
    fn forced_registry_collapses_to_single() {
        let set = resolve_registries(
            Some("flag.example"),
            &[rc(Some("acme"), "ghcr.io/acme", true)],
            Some("proj.example"),
            &[],
            None,
            "grim.ocx.sh",
        );
        assert_eq!(set.len(), 1);
        assert_eq!(set[0].url, "flag.example");
        assert!(set[0].is_default);
    }

    #[test]
    fn registries_array_is_authoritative_project_then_global() {
        let set = resolve_registries(
            None,
            &[rc(Some("acme"), "ghcr.io/acme", false)],
            Some("proj.example"), // ignored when [[registries]] present
            &[rc(Some("corp"), "registry.corp/team", false)],
            None,
            "grim.ocx.sh",
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
            None,
            &[
                rc(Some("acme"), "ghcr.io/acme", false),
                rc(Some("corp"), "registry.corp/team", true),
            ],
            None,
            &[],
            None,
            "grim.ocx.sh",
        );
        assert_eq!(primary_registry(&set), "registry.corp/team");
        assert!(set[1].is_default);
        assert!(!set[0].is_default);
    }

    #[test]
    fn duplicate_url_deduped_first_wins() {
        let set = resolve_registries(
            None,
            &[rc(Some("a"), "ghcr.io/acme", false)],
            None,
            &[rc(Some("b"), "ghcr.io/acme", true)], // same url, global tier
            None,
            "grim.ocx.sh",
        );
        assert_eq!(set.len(), 1);
        assert_eq!(set[0].alias.as_deref(), Some("a"));
    }

    #[test]
    fn no_registries_folds_legacy_default() {
        let set = resolve_registries(
            None,
            &[],
            Some("proj.example"),
            &[],
            Some("glob.example"),
            "grim.ocx.sh",
        );
        assert_eq!(set.len(), 1);
        assert_eq!(set[0].url, "proj.example");
        assert!(set[0].is_default);
    }

    #[test]
    fn no_registries_no_default_uses_fallback() {
        let set = resolve_registries(None, &[], None, &[], None, "grim.ocx.sh");
        assert_eq!(set.len(), 1);
        assert_eq!(set[0].url, "grim.ocx.sh");
    }

    #[test]
    fn reference_explicit_registry_parses_as_is() {
        let set = resolve_registries(None, &[], Some("ghcr.io/acme"), &[], None, "grim.ocx.sh");
        let id = resolve_reference("ghcr.io/other/x:1", &set).expect("explicit parses");
        assert_eq!(id.registry(), "ghcr.io");
        assert_eq!(id.to_string(), "ghcr.io/other/x:1");
    }

    #[test]
    fn reference_short_id_expands_against_primary() {
        let set = resolve_registries(None, &[], Some("ghcr.io/acme"), &[], None, "grim.ocx.sh");
        let id = resolve_reference("code-review:stable", &set).expect("short id expands");
        assert_eq!(id.to_string(), "ghcr.io/acme/code-review:stable");
    }

    #[test]
    fn reference_qualified_alias_substitutes_url() {
        let set = resolve_registries(
            None,
            &[rc(Some("corp"), "registry.corp/team", false)],
            None,
            &[],
            None,
            "grim.ocx.sh",
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
            None,
            &[
                rc(Some("code-review"), "registry.corp/team", false),
                rc(None, "ghcr.io/acme", true),
            ],
            None,
            &[],
            None,
            "grim.ocx.sh",
        );
        let id = resolve_reference("code-review:stable", &set).expect("repo:tag expands against primary");
        assert_eq!(id.to_string(), "ghcr.io/acme/code-review:stable");
    }

    #[test]
    fn reference_unknown_alias_prefix_expands_against_primary() {
        // `acme/x` where `acme` is not a configured alias is a multi-segment
        // repository path under the primary registry, exactly as today.
        let set = resolve_registries(None, &[], Some("ghcr.io"), &[], None, "grim.ocx.sh");
        let id = resolve_reference("acme/x:1", &set).expect("repo path expands");
        assert_eq!(id.to_string(), "ghcr.io/acme/x:1");
    }
}
