// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! The per-vendor frontmatter projection engine.
//!
//! Lifts tool-namespaced `metadata` keys (`claude.<field>: "…"`) out of a
//! canonical, agentskills-pure artifact into the native typed frontmatter
//! each client actually reads. The canonical artifact stays spec-compliant
//! on the wire: `metadata` values are strings, tool capabilities are
//! namespaced keys inside it. The same projection runs for skills and for
//! rule `metadata`, against the registries each [`Vendor`] declares:
//!
//! - a known `<vendor>.<field>` key converts to the field's native YAML
//!   type and is emitted as a native top-level key;
//! - an unknown `<vendor>.<field>` key is a typo guard: **warn + drop**;
//! - a known key with an invalid literal is a hard [`RenderError`]
//!   (fails publish, never silently ships a broken value);
//! - a foreign-namespace key (`opencode.*` while rendering Claude) is
//!   dropped silently;
//! - plain metadata keys (no known tool prefix) pass through unchanged.
//!
//! This module is pure mechanics; vendor field knowledge lives in the
//! [`Vendor`] registries (`vendor_claude` / `vendor_opencode` /
//! `vendor_copilot`). The projection is deterministic: identical input
//! yields byte-identical output, so rendered files can be
//! integrity-hashed like any generated file.

use std::fmt::Write as _;

use serde_yaml::Value;

use crate::skill::agent_frontmatter::ParsedAgent;
use crate::skill::rule_frontmatter::ParsedRule;
use crate::skill::{AgentFrontmatter, RuleFrontmatter, SkillFrontmatter};

use super::client_target::ClientTarget;
use super::vendor::{FieldType, KnownField, Vendor};

/// The known tool namespaces a `metadata` key may carry. Keys prefixed
/// with anything else (`vendor.x`) are plain metadata, not tool keys.
const KNOWN_NAMESPACES: &[&str] = &["claude", "opencode", "copilot", "codex"];

/// A projection failure: a known namespaced key carries a literal that
/// cannot convert to the field's native type. Hard error — publish fails
/// rather than shipping a silently broken value.
#[derive(thiserror::Error, Debug)]
pub enum RenderError {
    /// A known `<vendor>.<field>` key with an unconvertible value.
    #[error("invalid value '{value}' for metadata key '{key}': expected {expected}")]
    InvalidValue {
        /// The full namespaced metadata key (`claude.effort`).
        key: String,
        /// The offending string literal.
        value: String,
        /// Human-readable description of accepted literals.
        expected: String,
    },

    /// Serializing a rendered document to its native on-disk format failed.
    /// In practice unreachable for the flat string tables grim emits (Codex
    /// agent TOML) — surfaced rather than `.expect()`-panicked to keep
    /// library code free of panics across the render boundary.
    #[error("failed to serialize rendered {format} document")]
    Serialization {
        /// The target format (e.g. `TOML`).
        format: &'static str,
        /// The underlying serializer error.
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
}

/// The result of projecting a skill's frontmatter for one vendor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedSkill {
    /// The re-serialized frontmatter YAML (no `---` fences).
    pub frontmatter_yaml: String,
    /// Typo-guard warnings (unknown `<vendor>.*` keys, override notes).
    pub warnings: Vec<String>,
}

/// A fully rendered document (skill or rule index) for one vendor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedDoc {
    /// The complete rendered document.
    pub document: String,
    /// Typo-guard warnings from the projection.
    pub warnings: Vec<String>,
}

/// The generic projection of a metadata map for one vendor.
#[derive(Debug, Clone, PartialEq)]
pub struct RuleProjection {
    /// The vendor's lifted `(native key, value)` pairs, registry order.
    pub lifted: Vec<(&'static str, Value)>,
    /// The frontmatter with every tool-namespaced metadata key removed
    /// (plain metadata, `paths`, and forward-compat extras preserved).
    pub cleaned: RuleFrontmatter,
    /// Typo-guard warnings (unknown own-namespace keys).
    pub warnings: Vec<String>,
    /// Whether any tool-namespaced key was present at all — `false` means
    /// a canonical-style vendor can install the source bytes verbatim.
    pub had_tool_keys: bool,
}

/// Whether `fm` carries any tool-namespaced metadata key — i.e. whether a
/// render would differ from the canonical bytes. `false` means the caller
/// can (and should) copy the file verbatim: byte-identical installs for
/// plain skills.
pub fn has_tool_namespaced_metadata(fm: &SkillFrontmatter) -> bool {
    fm.metadata.keys().any(|k| split_namespaced(k).is_some())
}

/// Split a metadata key into `(known_namespace, field)`; `None` when the
/// key has no known tool prefix (plain metadata).
fn split_namespaced(key: &str) -> Option<(&str, &str)> {
    let (ns, field) = key.split_once('.')?;
    KNOWN_NAMESPACES.contains(&ns).then_some((ns, field))
}

/// Convert a string metadata literal to the native YAML value for `ty`.
fn convert(key: &str, value: &str, ty: FieldType) -> Result<Value, RenderError> {
    match ty {
        FieldType::Bool => match value {
            "true" => Ok(Value::Bool(true)),
            "false" => Ok(Value::Bool(false)),
            other => Err(RenderError::InvalidValue {
                key: key.to_string(),
                value: other.to_string(),
                expected: "'true' or 'false'".to_string(),
            }),
        },
        FieldType::String => Ok(Value::String(value.to_string())),
        FieldType::Enum(allowed) => {
            if allowed.contains(&value) {
                Ok(Value::String(value.to_string()))
            } else {
                Err(RenderError::InvalidValue {
                    key: key.to_string(),
                    value: value.to_string(),
                    expected: format!("one of {}", allowed.join(", ")),
                })
            }
        }
        FieldType::Integer => {
            value
                .parse::<i64>()
                .map(|n| Value::Number(n.into()))
                .map_err(|_| RenderError::InvalidValue {
                    key: key.to_string(),
                    value: value.to_string(),
                    expected: "an integer".to_string(),
                })
        }
        FieldType::Float => match value.parse::<f64>() {
            Ok(f) if f.is_finite() => Ok(Value::Number(serde_yaml::Number::from(f))),
            _ => Err(RenderError::InvalidValue {
                key: key.to_string(),
                value: value.to_string(),
                expected: "a finite number".to_string(),
            }),
        },
        FieldType::CommaList => Ok(comma_list_value(value)),
    }
}

/// A comma-separated string as a native YAML sequence: segments trimmed,
/// empty segments dropped, input order kept. Deterministic, never fails.
pub fn comma_list_value(value: &str) -> Value {
    Value::Sequence(
        value
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| Value::String(s.to_string()))
            .collect(),
    )
}

/// Partition a metadata map for one vendor: plain keys into `plain`,
/// the vendor's own known keys converted into `lifted` (registry order),
/// own unknown keys into `warnings`, foreign tool keys dropped.
#[allow(clippy::type_complexity)]
fn partition_metadata(
    metadata: &std::collections::BTreeMap<String, String>,
    registry: &'static [KnownField],
    vendor_name: &str,
) -> Result<
    (
        std::collections::BTreeMap<String, String>,
        Vec<(&'static str, Value)>,
        Vec<String>,
        bool,
    ),
    RenderError,
> {
    let mut plain = std::collections::BTreeMap::new();
    let mut lifted: Vec<(&'static str, Value)> = Vec::new();
    let mut warnings = Vec::new();
    let mut had_tool_keys = false;

    for (key, value) in metadata {
        match split_namespaced(key) {
            None => {
                plain.insert(key.clone(), value.clone());
            }
            Some((ns, field)) if ns == vendor_name => {
                had_tool_keys = true;
                match registry.iter().find(|f| f.field == field) {
                    Some(known) => {
                        let converted = convert(key, value, known.ty)?;
                        lifted.push((known.native, converted));
                    }
                    None => {
                        warnings.push(format!(
                            "unknown metadata key '{key}' for client '{vendor_name}': dropped (typo?)"
                        ));
                    }
                }
            }
            // Foreign tool namespace: not for this vendor, drop silently.
            Some(_) => {
                had_tool_keys = true;
            }
        }
    }

    // Keep the registry's declared order regardless of BTreeMap iteration
    // order, so the emitted YAML is stable when fields are added.
    lifted.sort_by_key(|(native, _)| registry.iter().position(|f| f.native == *native).unwrap_or(usize::MAX));

    Ok((plain, lifted, warnings, had_tool_keys))
}

/// Project a skill's frontmatter for `target`: lift the vendor's
/// namespaced metadata keys into native typed top-level fields, drop
/// foreign-namespace keys, keep plain metadata, and re-serialize
/// deterministically.
///
/// # Errors
///
/// [`RenderError::InvalidValue`] when a known `<vendor>.<field>` key
/// carries a literal that does not convert to the field's native type.
pub fn project_skill(fm: &SkillFrontmatter, vendor: &dyn Vendor) -> Result<RenderedSkill, RenderError> {
    let (plain_metadata, lifted, mut warnings, _) =
        partition_metadata(&fm.metadata, vendor.skill_fields(), vendor.name())?;

    let mut plain = fm.clone();
    plain.metadata = plain_metadata;

    // Serialize the cleaned frontmatter, then append the lifted native
    // keys. serde_yaml's Mapping preserves insertion order, so the output
    // is: struct fields (declaration order), `extra` keys (BTreeMap
    // order), lifted keys (registry order) — fully deterministic.
    let mut mapping = to_mapping(&plain);
    append_lifted(&mut mapping, lifted, vendor.name(), &[], &mut warnings);

    Ok(RenderedSkill {
        frontmatter_yaml: serialize_mapping(&mapping),
        warnings,
    })
}

/// Project a rule's `metadata` map for `vendor`. Pure partition — the
/// vendor decides how (and whether) to emit the result.
///
/// # Errors
///
/// [`RenderError::InvalidValue`] for a known key with a bad literal.
pub fn project_rule(fm: &RuleFrontmatter, vendor: &dyn Vendor) -> Result<RuleProjection, RenderError> {
    let (plain_metadata, lifted, warnings, had_tool_keys) =
        partition_metadata(&fm.metadata, vendor.rule_fields(), vendor.name())?;
    let mut cleaned = fm.clone();
    cleaned.metadata = plain_metadata;
    Ok(RuleProjection {
        lifted,
        cleaned,
        warnings,
        had_tool_keys,
    })
}

/// The generic projection of an agent's metadata map for one vendor.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentProjection {
    /// The vendor's lifted `(native key, value)` pairs, registry order.
    pub lifted: Vec<(&'static str, Value)>,
    /// The frontmatter with every tool-namespaced metadata key removed
    /// (common fields, plain metadata, and forward-compat extras kept).
    pub cleaned: AgentFrontmatter,
    /// Typo-guard warnings (unknown own-namespace keys).
    pub warnings: Vec<String>,
    /// Whether any tool-namespaced key was present at all — `false` means
    /// a canonical-style vendor can install the source bytes verbatim.
    pub had_tool_keys: bool,
}

/// Project an agent's `metadata` map for `vendor`. Pure partition — the
/// vendor decides which common fields to emit and how (the per-vendor
/// emit matrix lives in the `agent_index` impls).
///
/// # Errors
///
/// [`RenderError::InvalidValue`] for a known key with a bad literal.
pub fn project_agent(fm: &AgentFrontmatter, vendor: &dyn Vendor) -> Result<AgentProjection, RenderError> {
    let (plain_metadata, lifted, warnings, had_tool_keys) =
        partition_metadata(&fm.metadata, vendor.agent_fields(), vendor.name())?;
    let mut cleaned = fm.clone();
    cleaned.metadata = plain_metadata;
    Ok(AgentProjection {
        lifted,
        cleaned,
        warnings,
        had_tool_keys,
    })
}

/// Render an agent document in **canonical style** for a vendor whose
/// native format equals the canonical one (Claude): `None` when the agent
/// carries no tool-namespaced metadata (verbatim install), else the
/// cleaned full frontmatter (own keys lifted — a collision on a native in
/// `expected_overrides` replaces the projected common field silently —
/// foreign keys dropped, plain metadata kept) re-serialized over the
/// verbatim body.
///
/// # Errors
///
/// [`RenderError::InvalidValue`] for a known key with a bad literal.
pub fn render_agent_canonical(
    parsed: &ParsedAgent,
    vendor: &dyn Vendor,
    expected_overrides: &[&str],
) -> Result<Option<RenderedDoc>, RenderError> {
    let projection = project_agent(&parsed.frontmatter, vendor)?;
    if !projection.had_tool_keys {
        return Ok(None);
    }

    let mut warnings = projection.warnings;
    let mut mapping = to_mapping(&projection.cleaned);
    append_lifted(
        &mut mapping,
        projection.lifted,
        vendor.name(),
        expected_overrides,
        &mut warnings,
    );

    let mut document = String::new();
    if !mapping.is_empty() {
        document.push_str("---\n");
        document.push_str(&serialize_mapping(&mapping));
        document.push_str("---\n");
    }
    document.push_str(&parsed.body);
    Ok(Some(RenderedDoc { document, warnings }))
}

/// Build an agent frontmatter block (`---\n…---\n`, or empty when nothing
/// is emitted) from explicit native `(key, value)` pairs plus the vendor's
/// lifted keys — the shared mechanics behind the transforming vendors'
/// `agent_index` impls (OpenCode, Copilot). Override-aware: a lifted key
/// whose native name is in `expected_overrides` replaces the projected
/// common pair silently; any other collision warns.
pub fn agent_frontmatter_block(
    natives: Vec<(&'static str, Value)>,
    lifted: Vec<(&'static str, Value)>,
    vendor_name: &str,
    expected_overrides: &[&str],
    warnings: &mut Vec<String>,
) -> String {
    let mut mapping = serde_yaml::Mapping::new();
    for (key, value) in natives {
        mapping.insert(Value::String(key.to_string()), value);
    }
    append_lifted(&mut mapping, lifted, vendor_name, expected_overrides, warnings);

    if mapping.is_empty() {
        return String::new();
    }
    let mut block = String::from("---\n");
    block.push_str(&serialize_mapping(&mapping));
    block.push_str("---\n");
    block
}

/// Render a rule index in **canonical style** for a vendor that reads the
/// canonical frontmatter natively (Claude): `None` when the rule carries
/// no tool-namespaced metadata (verbatim install), else the cleaned
/// frontmatter (own keys lifted, foreign keys dropped, plain keys kept)
/// re-serialized over the verbatim body.
///
/// # Errors
///
/// [`RenderError::InvalidValue`] for a known key with a bad literal.
pub fn render_rule_canonical(parsed: &ParsedRule, vendor: &dyn Vendor) -> Result<Option<RenderedDoc>, RenderError> {
    let projection = project_rule(&parsed.frontmatter, vendor)?;
    if !projection.had_tool_keys {
        return Ok(None);
    }

    let mut warnings = projection.warnings;
    let mut mapping = to_mapping(&projection.cleaned);
    append_lifted(&mut mapping, projection.lifted, vendor.name(), &[], &mut warnings);

    let mut document = String::new();
    if !mapping.is_empty() {
        document.push_str("---\n");
        document.push_str(&serialize_mapping(&mapping));
        document.push_str("---\n");
    }
    document.push_str(&parsed.body);
    Ok(Some(RenderedDoc { document, warnings }))
}

/// Render a full `SKILL.md` document for `target`, or `None` when the
/// canonical bytes should be installed verbatim: the document carries no
/// tool-namespaced metadata, or it does not parse as a skill at all (a
/// foreign artifact is copied untouched).
///
/// # Errors
///
/// [`RenderError::InvalidValue`] when a known `<vendor>.<field>` key
/// carries an unconvertible literal — never silently install a broken
/// projection.
pub fn render_skill_doc(doc: &str, vendor: &dyn Vendor) -> Result<Option<RenderedDoc>, RenderError> {
    let path = std::path::Path::new("SKILL.md");
    // Split once; `parse_doc` would re-run the same frontmatter scan.
    let Ok((fm_yaml, body)) = SkillFrontmatter::split(doc, path) else {
        return Ok(None);
    };
    let Ok(fm) = SkillFrontmatter::from_yaml(&fm_yaml, path) else {
        return Ok(None);
    };
    if !has_tool_namespaced_metadata(&fm) {
        return Ok(None);
    }
    let rendered = project_skill(&fm, vendor)?;

    let mut document = String::with_capacity(rendered.frontmatter_yaml.len() + body.len() + 8);
    document.push_str("---\n");
    document.push_str(&rendered.frontmatter_yaml);
    document.push_str("---\n");
    document.push_str(&body);
    Ok(Some(RenderedDoc {
        document,
        warnings: rendered.warnings,
    }))
}

/// Serialize a struct to a YAML mapping (a struct always serializes to a
/// mapping; the fallback keeps the arm total without panicking).
fn to_mapping<T: serde::Serialize>(value: &T) -> serde_yaml::Mapping {
    match serde_yaml::to_value(value) {
        Ok(Value::Mapping(m)) => m,
        Ok(_) | Err(_) => serde_yaml::Mapping::new(),
    }
}

/// Append lifted native keys to `mapping`, warning when a lifted key
/// overrides an existing (legacy top-level) key. The namespaced metadata
/// value always wins. A collision on a native named in
/// `expected_overrides` is **documented precedence** (a vendor key
/// overriding a projected common field, e.g. `claude.model` over `model`)
/// and replaces silently — no warning.
fn append_lifted(
    mapping: &mut serde_yaml::Mapping,
    lifted: Vec<(&'static str, Value)>,
    vendor_name: &str,
    expected_overrides: &[&str],
    warnings: &mut Vec<String>,
) {
    for (native, value) in lifted {
        let native_key = Value::String(native.to_string());
        if mapping.contains_key(&native_key) && !expected_overrides.contains(&native) {
            warnings.push(format!(
                "metadata key '{vendor_name}.{native}' overrides the top-level '{native}' frontmatter key"
            ));
        }
        mapping.insert(native_key, value);
    }
}

/// Serialize a YAML mapping to a deterministic string. `serde_yaml`
/// serialization of scalar/string/sequence values is itself
/// deterministic; this wrapper only exists to keep the unreachable error
/// arm in one place.
fn serialize_mapping(mapping: &serde_yaml::Mapping) -> String {
    serde_yaml::to_string(mapping).unwrap_or_else(|_| {
        // Serializing an in-memory mapping of plain values cannot fail;
        // return an empty document rather than panicking in library code.
        let mut s = String::new();
        let _ = writeln!(s, "{{}}");
        s
    })
}

/// Validate the namespaced metadata of a skill against **every** supported
/// target: a publish-time gate. Returns the union of per-target warnings
/// (deduplicated, in target order).
///
/// # Errors
///
/// The first [`RenderError`] from any target — a known key with a bad
/// literal must fail the publish before the artifact reaches a registry.
pub fn validate_namespaced_metadata(fm: &SkillFrontmatter) -> Result<Vec<String>, RenderError> {
    let mut warnings = Vec::new();
    for target in ClientTarget::ALL {
        let rendered = project_skill(fm, target.vendor())?;
        for w in rendered.warnings {
            if !warnings.contains(&w) {
                warnings.push(w);
            }
        }
    }
    // Migration nudge: a known tool-specific field authored as a top-level
    // frontmatter key (it landed in `extra`) should move into namespaced
    // metadata.
    for key in fm.extra.keys() {
        let claude_fields = ClientTarget::Claude.vendor().skill_fields();
        // Match either spelling (registry key or native key), but always
        // advise the canonical registry key — `field` and `native` diverge
        // for `when-to-use`, and only `claude.<field>` is recognized.
        if let Some(f) = claude_fields
            .iter()
            .find(|f| f.native == key.as_str() || f.field == key.as_str())
        {
            warnings.push(format!(
                "top-level frontmatter key '{key}' is not an agentskills field; author it as metadata 'claude.{}' instead",
                f.field
            ));
        }
    }
    Ok(warnings)
}

/// Validate an agent's tool-namespaced `metadata` keys against every
/// supported target: a publish-time gate. Returns the union of per-target
/// typo-guard warnings plus a migration nudge for vendor-namespaced keys
/// authored top-level (in `extra` — the modeled common fields `model` /
/// `tools` are legitimate top-level keys and never nudged).
///
/// # Errors
///
/// The first [`RenderError`] from any target — a known key with a bad
/// literal must fail the publish before the artifact reaches a registry.
pub fn validate_agent_metadata(fm: &AgentFrontmatter) -> Result<Vec<String>, RenderError> {
    let mut warnings = Vec::new();
    for target in ClientTarget::ALL {
        let projection = project_agent(fm, target.vendor())?;
        for w in projection.warnings {
            if !warnings.contains(&w) {
                warnings.push(w);
            }
        }
    }
    // Migration nudge: a tool-namespaced key authored top-level in the
    // agent frontmatter is never projected — it belongs inside `metadata`.
    for key in fm.extra.keys() {
        if split_namespaced(key).is_some() {
            warnings.push(format!(
                "top-level agent frontmatter key '{key}' is not projected; author it inside 'metadata' instead"
            ));
        }
    }
    Ok(warnings)
}

/// Validate a rule's tool-namespaced `metadata` keys against every
/// supported target: a publish-time gate. Returns typo-guard warnings
/// plus a migration nudge for vendor keys authored top-level.
///
/// # Errors
///
/// [`RenderError::InvalidValue`] for a known key with a bad literal
/// (today only `copilot.exclude-agent`).
pub fn validate_rule_metadata(fm: &RuleFrontmatter) -> Result<Vec<String>, RenderError> {
    let mut warnings = Vec::new();
    for target in ClientTarget::ALL {
        let projection = project_rule(fm, target.vendor())?;
        for w in projection.warnings {
            if !warnings.contains(&w) {
                warnings.push(w);
            }
        }
    }
    // Migration nudge: a tool-namespaced key authored top-level in the
    // rule frontmatter is never projected — it belongs inside `metadata`.
    for key in fm.extra.keys() {
        if split_namespaced(key).is_some() {
            warnings.push(format!(
                "top-level rule frontmatter key '{key}' is not projected; author it inside 'metadata' instead"
            ));
        }
    }
    Ok(warnings)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn fm(doc: &str) -> SkillFrontmatter {
        SkillFrontmatter::parse_doc(doc, Path::new("SKILL.md")).expect("parse")
    }

    fn rule(doc: &str) -> ParsedRule {
        RuleFrontmatter::parse_doc(doc, Path::new("r.md")).expect("parse")
    }

    const NAMESPACED: &str = r#"---
name: next
description: Suggest the next command.
metadata:
  keywords: workflow,planning
  claude.disable-model-invocation: "true"
  claude.model: opus
  opencode.future-flag: "x"
---
# body
"#;

    #[test]
    fn claude_lifts_native_typed_fields() {
        let r = project_skill(&fm(NAMESPACED), ClientTarget::Claude.vendor()).expect("render");
        // Native bool, not the string "true".
        assert!(r.frontmatter_yaml.contains("disable-model-invocation: true"));
        assert!(!r.frontmatter_yaml.contains("disable-model-invocation: 'true'"));
        assert!(r.frontmatter_yaml.contains("model: opus"));
        // The namespaced keys are gone from metadata; plain metadata stays.
        assert!(!r.frontmatter_yaml.contains("claude."));
        assert!(r.frontmatter_yaml.contains("keywords: workflow,planning"));
        // The foreign opencode key is dropped silently — no warning.
        assert!(!r.frontmatter_yaml.contains("future-flag"));
        assert!(r.warnings.is_empty(), "no warnings expected: {:?}", r.warnings);
    }

    #[test]
    fn opencode_render_is_clean_universal_with_warning_for_own_unknown_key() {
        let r = project_skill(&fm(NAMESPACED), ClientTarget::OpenCode.vendor()).expect("render");
        // No tool key survives; opencode's registry is empty so its own
        // namespaced key warns (typo guard).
        assert!(!r.frontmatter_yaml.contains("claude."));
        assert!(!r.frontmatter_yaml.contains("opencode."));
        assert!(!r.frontmatter_yaml.contains("disable-model-invocation"));
        assert!(r.frontmatter_yaml.contains("keywords: workflow,planning"));
        assert_eq!(r.warnings.len(), 1);
        assert!(r.warnings[0].contains("opencode.future-flag"));
    }

    #[test]
    fn copilot_skill_render_matches_opencode_universal_shape() {
        // OpenCode and Copilot both read only universal fields: identical
        // rendered frontmatter (the unified universal render).
        let oc = project_skill(&fm(NAMESPACED), ClientTarget::OpenCode.vendor()).expect("render");
        let cp = project_skill(&fm(NAMESPACED), ClientTarget::Copilot.vendor()).expect("render");
        assert_eq!(oc.frontmatter_yaml, cp.frontmatter_yaml);
        assert!(cp.warnings.is_empty(), "foreign namespaces drop silently");
    }

    #[test]
    fn bad_bool_literal_is_render_error() {
        let doc = "---\nname: s\ndescription: d\nmetadata:\n  claude.user-invocable: \"yes\"\n---\n";
        let err = project_skill(&fm(doc), ClientTarget::Claude.vendor()).expect_err("bad bool");
        let msg = err.to_string();
        assert!(msg.contains("claude.user-invocable"), "{msg}");
        assert!(msg.contains("'true' or 'false'"), "{msg}");
    }

    #[test]
    fn bad_enum_literals_are_render_errors() {
        for (key, value) in [
            ("claude.effort", "ultra"),
            ("claude.context", "thread"),
            ("claude.shell", "zsh"),
        ] {
            let doc = format!("---\nname: s\ndescription: d\nmetadata:\n  {key}: \"{value}\"\n---\n");
            let err = project_skill(&fm(&doc), ClientTarget::Claude.vendor()).expect_err("bad enum");
            assert!(err.to_string().contains(key), "{err}");
        }
        // The valid literals pass.
        let doc = "---\nname: s\ndescription: d\nmetadata:\n  claude.effort: xhigh\n  claude.context: fork\n  claude.shell: bash\n---\n";
        let r = project_skill(&fm(doc), ClientTarget::Claude.vendor()).expect("valid enums");
        assert!(r.frontmatter_yaml.contains("effort: xhigh"));
        assert!(r.frontmatter_yaml.contains("context: fork"));
        assert!(r.frontmatter_yaml.contains("shell: bash"));
    }

    #[test]
    fn unknown_target_key_warns_and_drops() {
        let doc = "---\nname: s\ndescription: d\nmetadata:\n  claude.modle: opus\n---\n";
        let r = project_skill(&fm(doc), ClientTarget::Claude.vendor()).expect("render");
        assert!(!r.frontmatter_yaml.contains("modle"));
        assert_eq!(r.warnings.len(), 1);
        assert!(r.warnings[0].contains("claude.modle"));
    }

    #[test]
    fn when_to_use_lifts_to_native_underscore_key() {
        let doc = "---\nname: s\ndescription: d\nmetadata:\n  claude.when-to-use: planning time\n---\n";
        let r = project_skill(&fm(doc), ClientTarget::Claude.vendor()).expect("render");
        assert!(r.frontmatter_yaml.contains("when_to_use: planning time"));
        assert!(!r.frontmatter_yaml.contains("when-to-use"));
    }

    #[test]
    fn render_is_deterministic_and_identity_detection_works() {
        let f = fm(NAMESPACED);
        let a = project_skill(&f, ClientTarget::Claude.vendor()).expect("render");
        let b = project_skill(&f, ClientTarget::Claude.vendor()).expect("render");
        assert_eq!(a, b, "re-render must be byte-identical");
        assert!(has_tool_namespaced_metadata(&f));

        let plain = fm("---\nname: s\ndescription: d\nmetadata:\n  keywords: a,b\n  vendor.x: y\n---\n");
        // `vendor.` is not a known tool namespace ⇒ plain metadata.
        assert!(!has_tool_namespaced_metadata(&plain));
    }

    #[test]
    fn rendered_skill_doc_reparses() {
        let doc = render_skill_doc(NAMESPACED, ClientTarget::Claude.vendor())
            .expect("render")
            .expect("namespaced metadata present");
        assert!(doc.document.starts_with("---\n"));
        let again = SkillFrontmatter::parse_doc(&doc.document, Path::new("SKILL.md")).expect("reparse");
        assert_eq!(again.name.as_str(), "next");
        // Plain skill ⇒ identity.
        assert!(
            render_skill_doc(
                "---\nname: s\ndescription: d\n---\nbody\n",
                ClientTarget::Claude.vendor()
            )
            .expect("render")
            .is_none()
        );
    }

    #[test]
    fn top_level_override_warns_but_namespaced_wins() {
        // `model` authored top-level (lands in extra) AND namespaced.
        let doc = "---\nname: s\ndescription: d\nmodel: haiku\nmetadata:\n  claude.model: opus\n---\n";
        let r = project_skill(&fm(doc), ClientTarget::Claude.vendor()).expect("render");
        assert!(r.frontmatter_yaml.contains("model: opus"));
        assert!(!r.frontmatter_yaml.contains("haiku"));
        assert_eq!(r.warnings.len(), 1);
        assert!(r.warnings[0].contains("overrides"));
    }

    #[test]
    fn validate_namespaced_metadata_unions_warnings_and_fails_on_bad_literal() {
        let ok = fm(NAMESPACED);
        let warnings = validate_namespaced_metadata(&ok).expect("valid");
        // opencode.future-flag is unknown for opencode (its registry is
        // empty) ⇒ exactly one warning.
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("opencode.future-flag"));

        let bad = fm("---\nname: s\ndescription: d\nmetadata:\n  claude.effort: warp\n---\n");
        assert!(validate_namespaced_metadata(&bad).is_err());
    }

    #[test]
    fn validate_lints_legacy_top_level_claude_keys() {
        let legacy = fm("---\nname: s\ndescription: d\nuser-invocable: true\n---\n");
        let warnings = validate_namespaced_metadata(&legacy).expect("valid");
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("claude.user-invocable"), "{:?}", warnings);
    }

    #[test]
    fn migration_nudge_suggests_canonical_registry_key_not_native_spelling() {
        // `when_to_use` is the *native* spelling; the registry key is
        // `when-to-use`. The nudge must advise the key the renderer
        // actually knows, or following it lands in the typo-drop path.
        let legacy = fm("---\nname: s\ndescription: d\nwhen_to_use: planning\n---\n");
        let warnings = validate_namespaced_metadata(&legacy).expect("valid");
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("claude.when-to-use"), "{:?}", warnings);
    }

    // ── Agent projection ─────────────────────────────────────────────────

    fn agent(doc: &str) -> ParsedAgent {
        crate::skill::AgentFrontmatter::parse_doc(doc, Path::new("rev.md")).expect("parse")
    }

    #[test]
    fn comma_list_trims_and_drops_empties_in_input_order() {
        let v = comma_list_value(" b , a ,, c,");
        let Value::Sequence(items) = v else { panic!("sequence") };
        let strs: Vec<&str> = items.iter().filter_map(|i| i.as_str()).collect();
        assert_eq!(strs, vec!["b", "a", "c"], "input order kept, empties dropped");
        assert_eq!(comma_list_value(""), Value::Sequence(vec![]));
    }

    #[test]
    fn integer_and_float_conversion_and_errors() {
        assert_eq!(
            convert("k", "12", FieldType::Integer).unwrap(),
            Value::Number(12.into())
        );
        assert!(convert("k", "12.5", FieldType::Integer).is_err());
        assert!(convert("k", "0.25", FieldType::Float).is_ok());
        assert!(convert("k", "NaN", FieldType::Float).is_err(), "non-finite rejected");
        assert!(convert("k", "inf", FieldType::Float).is_err());
        assert!(convert("k", "warm", FieldType::Float).is_err());
    }

    #[test]
    fn project_agent_partitions_and_detects_tool_keys() {
        let p = agent(
            "---\nname: rev\ndescription: d\nmodel: sonnet\nmetadata:\n  keywords: a,b\n  claude.memory: project\n  copilot.tools: \"x\"\n---\nbody\n",
        );
        let claude = project_agent(&p.frontmatter, ClientTarget::Claude.vendor()).expect("project");
        assert!(claude.had_tool_keys);
        assert_eq!(claude.lifted.len(), 1, "only the claude key lifts for claude");
        assert_eq!(claude.lifted[0].0, "memory");
        assert_eq!(
            claude.cleaned.metadata.get("keywords").map(String::as_str),
            Some("a,b"),
            "plain metadata survives the clean"
        );
        assert!(!claude.cleaned.metadata.contains_key("claude.memory"));

        let plain = agent("---\nname: rev\ndescription: d\n---\nbody\n");
        let proj = project_agent(&plain.frontmatter, ClientTarget::Claude.vendor()).expect("project");
        assert!(!proj.had_tool_keys, "no tool keys ⇒ verbatim-capable");
    }

    #[test]
    fn render_agent_canonical_verbatim_and_override_paths() {
        // No tool keys ⇒ None (verbatim).
        let plain = agent("---\nname: rev\ndescription: d\nmodel: sonnet\n---\nbody\n");
        assert!(
            render_agent_canonical(&plain, ClientTarget::Claude.vendor(), &["model", "tools"])
                .expect("render")
                .is_none()
        );

        // claude.model overrides the common model — silently (expected).
        let over = agent("---\nname: rev\ndescription: d\nmodel: sonnet\nmetadata:\n  claude.model: opus\n---\nbody\n");
        let out = render_agent_canonical(&over, ClientTarget::Claude.vendor(), &["model", "tools"])
            .expect("render")
            .expect("tool keys present");
        assert!(out.document.contains("model: opus"));
        assert!(!out.document.contains("sonnet"));
        assert!(
            out.warnings.is_empty(),
            "expected override is silent: {:?}",
            out.warnings
        );

        // A collision NOT in expected_overrides still warns (extra key).
        let legacy =
            agent("---\nname: rev\ndescription: d\nmemory: user\nmetadata:\n  claude.memory: project\n---\nbody\n");
        let out = render_agent_canonical(&legacy, ClientTarget::Claude.vendor(), &["model", "tools"])
            .expect("render")
            .expect("rendered");
        assert!(out.document.contains("memory: project"), "namespaced wins");
        assert_eq!(out.warnings.len(), 1);
        assert!(out.warnings[0].contains("overrides"));
    }

    #[test]
    fn agent_frontmatter_block_is_deterministic_and_override_aware() {
        let natives = vec![
            ("description", Value::String("d".to_string())),
            ("model", Value::String("sonnet".to_string())),
        ];
        let lifted = vec![("model", Value::String("anthropic/claude-sonnet-4-5".to_string()))];
        let mut warnings = Vec::new();
        let a = agent_frontmatter_block(natives.clone(), lifted.clone(), "opencode", &["model"], &mut warnings);
        assert!(a.starts_with("---\n") && a.ends_with("---\n"));
        assert!(a.contains("model: anthropic/claude-sonnet-4-5"));
        assert!(!a.contains("sonnet\n"), "common value replaced");
        assert!(warnings.is_empty(), "{warnings:?}");
        let b = agent_frontmatter_block(natives, lifted, "opencode", &["model"], &mut warnings);
        assert_eq!(a, b, "re-render byte-identical");
        // Empty mapping ⇒ no block at all.
        assert_eq!(
            agent_frontmatter_block(vec![], vec![], "opencode", &[], &mut warnings),
            ""
        );
    }

    #[test]
    fn validate_agent_metadata_unions_and_nudges() {
        // Unknown own-namespace key warns; common top-level fields never nudge.
        let ok = agent(
            "---\nname: rev\ndescription: d\nmodel: sonnet\ntools: Read\nmetadata:\n  opencode.future: \"x\"\n---\nbody\n",
        );
        let warnings = validate_agent_metadata(&ok.frontmatter).expect("valid");
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("opencode.future"));

        // Bad literal fails the gate.
        let bad = agent("---\nname: rev\ndescription: d\nmetadata:\n  claude.effort: warp\n---\nbody\n");
        assert!(validate_agent_metadata(&bad.frontmatter).is_err());

        // Vendor key authored top-level: migration nudge.
        let legacy = agent("---\nname: rev\ndescription: d\nclaude.memory: project\n---\nbody\n");
        let warnings = validate_agent_metadata(&legacy.frontmatter).expect("valid");
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("author it inside 'metadata'"));
    }

    // ── Rule projection ──────────────────────────────────────────────────

    #[test]
    fn plain_rule_is_identity_for_canonical_vendor() {
        let parsed = rule("---\npaths: [\"**/*.rs\"]\nkeywords: rust\n---\n# R\nbody\n");
        let out = render_rule_canonical(&parsed, ClientTarget::Claude.vendor()).expect("render");
        assert!(out.is_none(), "no tool-namespaced metadata ⇒ verbatim");
    }

    #[test]
    fn rule_with_foreign_vendor_key_renders_cleaned_for_claude() {
        let parsed = rule(
            "---\npaths: [\"**/*.rs\"]\nkeywords: rust\nmetadata:\n  copilot.exclude-agent: code-review\n---\n# R\nbody\n",
        );
        let out = render_rule_canonical(&parsed, ClientTarget::Claude.vendor())
            .expect("render")
            .expect("tool keys present ⇒ rendered");
        // Foreign vendor key dropped; canonical scoping + plain keys kept.
        assert!(!out.document.contains("copilot.exclude-agent"));
        assert!(out.document.contains("**/*.rs"));
        assert!(out.document.contains("keywords: rust"));
        assert!(out.document.ends_with("# R\nbody\n"));
        assert!(out.warnings.is_empty(), "{:?}", out.warnings);
        // Deterministic re-render.
        let again = render_rule_canonical(&parsed, ClientTarget::Claude.vendor())
            .expect("render")
            .expect("rendered");
        assert_eq!(out, again);
    }

    #[test]
    fn rule_with_only_foreign_metadata_and_no_other_frontmatter_drops_the_block() {
        let parsed = rule("---\nmetadata:\n  copilot.exclude-agent: code-review\n---\nbody\n");
        let out = render_rule_canonical(&parsed, ClientTarget::Claude.vendor())
            .expect("render")
            .expect("rendered");
        assert_eq!(out.document, "body\n", "empty frontmatter block is omitted");
    }

    #[test]
    fn unknown_own_namespace_rule_key_warns_for_claude() {
        let parsed = rule("---\nmetadata:\n  claude.unknown-thing: x\n---\nbody\n");
        let out = render_rule_canonical(&parsed, ClientTarget::Claude.vendor())
            .expect("render")
            .expect("rendered");
        assert_eq!(out.warnings.len(), 1);
        assert!(out.warnings[0].contains("claude.unknown-thing"));
        assert!(!out.document.contains("unknown-thing"));
    }

    #[test]
    fn validate_rule_metadata_checks_all_vendors_and_lints_top_level_keys() {
        // Valid metadata key for copilot ⇒ no error; unknown key warns once.
        let ok = rule("---\nmetadata:\n  copilot.exclude-agent: cloud-agent\n  claude.foo: x\n---\nbody\n");
        let warnings = validate_rule_metadata(&ok.frontmatter).expect("valid");
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("claude.foo"));

        // Bad literal fails.
        let bad = rule("---\nmetadata:\n  copilot.exclude-agent: everything\n---\nbody\n");
        assert!(validate_rule_metadata(&bad.frontmatter).is_err());

        // Vendor key authored top-level: migration nudge.
        let legacy = rule("---\ncopilot.exclude-agent: code-review\n---\nbody\n");
        let warnings = validate_rule_metadata(&legacy.frontmatter).expect("valid");
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("author it inside 'metadata'"));
    }
}
