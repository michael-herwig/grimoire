// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! Parsed OCI identifiers (`registry/repository[:tag][@digest]`).
//!
//! Adapted from OCX `oci/identifier.rs`. Grimoire has no built-in default
//! registry (it is supplied per-invocation via `GRIM_DEFAULT_REGISTRY`),
//! so [`Identifier::parse`] is strict and `FromStr` delegates to it.
//! Conversion to an `oci_distribution::Reference` is deliberately absent —
//! that belongs to the Phase 3 OCI-access seam, not the domain type.

pub mod error;

use serde::{Deserialize, Serialize};

use super::Digest;
use error::{IdentifierError, IdentifierErrorKind};

const MAX_REPOSITORY_LENGTH: usize = 255;

/// A parsed OCI identifier with registry, repository, optional tag, and
/// optional digest.
///
/// Unlike `oci_spec::Reference`, this type does not inject `"latest"` when
/// no tag is present, does not default to `docker.io`, and provides
/// structured parse errors via [`IdentifierError`].
#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord)]
pub struct Identifier {
    registry: String,
    repository: String,
    tag: Option<String>,
    digest: Option<Digest>,
}

impl Identifier {
    /// Creates an identifier from explicit repository and registry strings.
    ///
    /// No parsing is performed — the values are taken as-is. The resulting
    /// identifier has no tag and no digest.
    pub fn new_registry(repository: impl Into<String>, registry: impl Into<String>) -> Self {
        Self {
            registry: registry.into(),
            repository: repository.into(),
            tag: None,
            digest: None,
        }
    }

    /// Parses an identifier string that must contain an explicit registry.
    ///
    /// # Errors
    ///
    /// Returns [`IdentifierErrorKind::MissingRegistry`] if the input has no
    /// explicit registry (e.g. `"code-review:stable"` or `"org/tool"`),
    /// [`IdentifierErrorKind::DirectoryTraversal`] if any path segment is
    /// `.` or `..`, and the relevant kind for empty / uppercase / overlong
    /// / malformed-digest inputs.
    ///
    /// This parser does not inject `"latest"` when the input has no tag.
    pub fn parse(input: &str) -> Result<Self, IdentifierError> {
        validate_segments(input)?;
        if !has_explicit_registry(input) {
            return Err(IdentifierError {
                input: input.to_string(),
                kind: IdentifierErrorKind::MissingRegistry,
            });
        }
        // The default registry is unused on this path (explicit registry
        // is required above); pass an empty placeholder.
        parse_internal(input, "")
    }

    /// Parses an identifier string, using `default_registry` for inputs
    /// that do not contain an explicit registry.
    ///
    /// # Errors
    ///
    /// Returns the relevant [`IdentifierError`] for empty, traversal,
    /// uppercase, overlong, or malformed-digest inputs.
    pub fn parse_with_default_registry(s: &str, default_registry: &str) -> Result<Self, IdentifierError> {
        validate_segments(s)?;
        parse_internal(s, default_registry)
    }

    /// Returns a new identifier with the given tag, dropping any digest.
    ///
    /// The digest is dropped because changing the tag semantically creates
    /// a different reference. Any `+` in the tag is normalized to `_`
    /// (OCI tags do not allow `+`).
    pub fn clone_with_tag(&self, tag: impl Into<String>) -> Self {
        Self {
            registry: self.registry.clone(),
            repository: self.repository.clone(),
            tag: Some(normalize_tag(tag.into())),
            digest: None,
        }
    }

    /// Clones with the given digest, preserving the existing tag.
    pub fn clone_with_digest(&self, digest: Digest) -> Self {
        Self {
            registry: self.registry.clone(),
            repository: self.repository.clone(),
            tag: self.tag.clone(),
            digest: Some(digest),
        }
    }

    /// Strips the tag, preserving registry, repository, and digest.
    pub fn without_tag(&self) -> Self {
        Self {
            registry: self.registry.clone(),
            repository: self.repository.clone(),
            tag: None,
            digest: self.digest.clone(),
        }
    }

    /// Returns the registry hostname (and optional port).
    pub fn registry(&self) -> &str {
        &self.registry
    }

    /// Returns the repository path within the registry.
    pub fn repository(&self) -> &str {
        &self.repository
    }

    /// Returns the last segment of the repository path as the package name.
    pub fn name(&self) -> &str {
        self.repository.rsplit('/').next().unwrap_or(&self.repository)
    }

    /// Returns the tag if one was explicitly provided, or `None`.
    pub fn tag(&self) -> Option<&str> {
        self.tag.as_deref()
    }

    /// Returns the tag if present, or `"latest"` as a default.
    pub fn tag_or_latest(&self) -> &str {
        self.tag.as_deref().unwrap_or("latest")
    }

    /// Content-addressed digest, if pinned.
    pub fn digest(&self) -> Option<Digest> {
        self.digest.clone()
    }
}

impl std::fmt::Display for Identifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.registry, self.repository)?;
        if let Some(tag) = &self.tag {
            write!(f, ":{tag}")?;
        }
        if let Some(digest) = &self.digest {
            write!(f, "@{digest}")?;
        }
        Ok(())
    }
}

impl std::str::FromStr for Identifier {
    type Err = IdentifierError;

    fn from_str(value: &str) -> Result<Self, IdentifierError> {
        Self::parse(value)
    }
}

impl Serialize for Identifier {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for Identifier {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Self::parse(&s).map_err(serde::de::Error::custom)
    }
}

// ── Parser ───────────────────────────────────────────────────────────

/// Validates that no path segment is `.` or `..` (traversal defence).
fn validate_segments(input: &str) -> Result<(), IdentifierError> {
    let name_part = input.split_once('@').map_or(input, |(name, _)| name);
    for segment in name_part.split('/') {
        let dir_name = segment.split_once(':').map_or(segment, |(name, _)| name);
        if dir_name == "." || dir_name == ".." {
            return Err(IdentifierError {
                input: input.to_string(),
                kind: IdentifierErrorKind::DirectoryTraversal,
            });
        }
    }
    Ok(())
}

/// Whether the input contains an explicit registry in the first segment.
fn has_explicit_registry(input: &str) -> bool {
    let name_part = input.split_once('@').map_or(input, |(name, _)| name);
    match name_part.split_once('/') {
        None => false,
        Some((first, _)) => first.contains('.') || first.contains(':') || first == "localhost",
    }
}

fn parse_internal(input: &str, default_registry: &str) -> Result<Identifier, IdentifierError> {
    if input.is_empty() {
        return Err(IdentifierError {
            input: String::new(),
            kind: IdentifierErrorKind::Empty,
        });
    }

    let (name_part, digest) = match input.split_once('@') {
        Some((name, digest_str)) => (name, Some(parse_digest(input, digest_str)?)),
        None => (input, None),
    };

    let (name_without_tag, tag) = split_tag(name_part);
    let full_name = prepend_domain(name_without_tag, default_registry);

    let (registry, repository) = split_registry_repository(&full_name).ok_or_else(|| IdentifierError {
        input: input.to_string(),
        kind: IdentifierErrorKind::InvalidFormat,
    })?;

    if repository.chars().any(|c| c.is_ascii_uppercase()) {
        return Err(IdentifierError {
            input: input.to_string(),
            kind: IdentifierErrorKind::UppercaseRepository,
        });
    }
    if repository.len() > MAX_REPOSITORY_LENGTH {
        return Err(IdentifierError {
            input: input.to_string(),
            kind: IdentifierErrorKind::RepositoryTooLong,
        });
    }

    Ok(Identifier {
        registry,
        repository,
        tag,
        digest,
    })
}

fn parse_digest(input: &str, digest_str: &str) -> Result<Digest, IdentifierError> {
    Digest::try_from(digest_str).map_err(|_| IdentifierError {
        input: input.to_string(),
        kind: IdentifierErrorKind::DigestInvalidFormat,
    })
}

/// Splits the tag from the name portion. Only looks for a `:` in the last
/// path segment so registry ports like `localhost:5000` are not mistaken
/// for tags.
fn split_tag(name: &str) -> (&str, Option<String>) {
    let last_slash = name.rfind('/');
    let last_segment = match last_slash {
        Some(pos) => &name[pos + 1..],
        None => name,
    };

    match last_segment.find(':') {
        Some(colon_in_segment) => {
            let colon_pos = match last_slash {
                Some(slash_pos) => slash_pos + 1 + colon_in_segment,
                None => colon_in_segment,
            };
            let tag = &name[colon_pos + 1..];
            (&name[..colon_pos], Some(normalize_tag(tag.to_string())))
        }
        None => (name, None),
    }
}

/// Normalizes `+` to `_` in a tag string.
///
/// OCI tags do not allow `+` (`[a-zA-Z0-9_][a-zA-Z0-9._-]{0,127}`). This
/// is the earliest boundary where user input enters the system.
fn normalize_tag(tag: String) -> String {
    tag.replace('+', "_")
}

/// Splits `full_name` into `(registry, repository)`.
fn split_registry_repository(full_name: &str) -> Option<(String, String)> {
    let (first, rest) = full_name.split_once('/')?;
    Some((first.to_string(), rest.to_string()))
}

fn prepend_domain(name: &str, domain: &str) -> String {
    match name.split_once('/') {
        None => format!("{domain}/{name}"),
        Some((left, _)) => {
            if !(left.contains('.') || left.contains(':')) && left != "localhost" {
                format!("{domain}/{name}")
            } else {
                name.into()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Strict parse (explicit registry required) ────────────────────

    #[test]
    fn parse_accepts_explicit_registry() {
        let id = Identifier::parse("ghcr.io/acme/code-review:stable").unwrap();
        assert_eq!(id.registry(), "ghcr.io");
        assert_eq!(id.repository(), "acme/code-review");
        assert_eq!(id.tag(), Some("stable"));
        assert_eq!(id.name(), "code-review");
    }

    #[test]
    fn parse_accepts_localhost_and_port() {
        assert_eq!(Identifier::parse("localhost/repo:tag").unwrap().registry(), "localhost");
        assert_eq!(
            Identifier::parse("localhost:5000/repo:tag").unwrap().registry(),
            "localhost:5000"
        );
    }

    #[test]
    fn parse_rejects_bare_name() {
        let err = Identifier::parse("code-review:stable").unwrap_err();
        assert!(matches!(err.kind, IdentifierErrorKind::MissingRegistry));
    }

    #[test]
    fn parse_rejects_org_repo_without_registry() {
        let err = Identifier::parse("myorg/tool").unwrap_err();
        assert!(matches!(err.kind, IdentifierErrorKind::MissingRegistry));
    }

    #[test]
    fn parse_rejects_uppercase_repository() {
        let err = Identifier::parse("ghcr.io/Foo").unwrap_err();
        assert!(matches!(err.kind, IdentifierErrorKind::UppercaseRepository));
    }

    #[test]
    fn parse_rejects_dotdot_traversal() {
        let err = Identifier::parse("ghcr.io/../evil").unwrap_err();
        assert!(matches!(err.kind, IdentifierErrorKind::DirectoryTraversal));
    }

    #[test]
    fn parse_rejects_dot_traversal() {
        let err = Identifier::parse("ghcr.io/org/./evil").unwrap_err();
        assert!(matches!(err.kind, IdentifierErrorKind::DirectoryTraversal));
    }

    #[test]
    fn parse_rejects_empty_as_missing_registry() {
        // Strict `parse` checks for an explicit registry before reaching
        // the empty-input guard, so "" is reported as MissingRegistry.
        let err = Identifier::parse("").unwrap_err();
        assert!(matches!(err.kind, IdentifierErrorKind::MissingRegistry));
    }

    #[test]
    fn default_registry_path_reports_empty() {
        let err = Identifier::parse_with_default_registry("", "ghcr.io").unwrap_err();
        assert!(matches!(err.kind, IdentifierErrorKind::Empty));
    }

    #[test]
    fn parse_rejects_bad_digest() {
        let err = Identifier::parse("ghcr.io/repo@md5:abc").unwrap_err();
        assert!(matches!(err.kind, IdentifierErrorKind::DigestInvalidFormat));
        let err = Identifier::parse("ghcr.io/repo@sha256:abc").unwrap_err();
        assert!(matches!(err.kind, IdentifierErrorKind::DigestInvalidFormat));
    }

    // ── parse_with_default_registry (CLI path) ───────────────────────

    #[test]
    fn default_registry_used_for_bare_name() {
        // The default registry is a bare host: split_registry_repository
        // splits on the first '/', so the host is registry and the rest
        // is the repository (OCX-adapted prepend_domain semantics).
        let id = Identifier::parse_with_default_registry("code-review:stable", "ghcr.io").unwrap();
        assert_eq!(id.registry(), "ghcr.io");
        assert_eq!(id.repository(), "code-review");
        assert_eq!(id.tag(), Some("stable"));
    }

    #[test]
    fn default_registry_preserves_tag_absence() {
        let bare = Identifier::parse_with_default_registry("code-review", "localhost:5000").unwrap();
        assert_eq!(bare.tag(), None);
        assert_eq!(bare.tag_or_latest(), "latest");
        assert_eq!(bare.registry(), "localhost:5000");
    }

    #[test]
    fn default_registry_ignored_when_registry_present() {
        let id = Identifier::parse_with_default_registry("ghcr.io/org/tool:1.0", "localhost:5000").unwrap();
        assert_eq!(id.registry(), "ghcr.io");
        assert_eq!(id.repository(), "org/tool");
    }

    #[test]
    fn default_registry_rejects_traversal() {
        let err = Identifier::parse_with_default_registry("../evil/tool", "ghcr.io").unwrap_err();
        assert!(matches!(err.kind, IdentifierErrorKind::DirectoryTraversal));
    }

    // ── Tag normalization (+ → _) ────────────────────────────────────

    #[test]
    fn parse_normalizes_plus_to_underscore() {
        let id = Identifier::parse("ghcr.io/repo:3.28.1+20260216").unwrap();
        assert_eq!(id.tag(), Some("3.28.1_20260216"));
    }

    #[test]
    fn parse_normalizes_plus_with_registry_port() {
        let id = Identifier::parse("localhost:5000/repo:1.0+build").unwrap();
        assert_eq!(id.registry(), "localhost:5000");
        assert_eq!(id.tag(), Some("1.0_build"));
    }

    #[test]
    fn clone_with_tag_normalizes_plus_and_drops_digest() {
        let hex = "a".repeat(64);
        let base = Identifier::parse(&format!("ghcr.io/repo:t@sha256:{hex}")).unwrap();
        assert!(base.digest().is_some());
        let tagged = base.clone_with_tag("3.28.1+b1");
        assert_eq!(tagged.tag(), Some("3.28.1_b1"));
        assert_eq!(tagged.digest(), None);
    }

    #[test]
    fn clone_with_digest_preserves_tag() {
        let id = Identifier::parse("ghcr.io/repo:tag").unwrap();
        let digest = Digest::Sha256("a".repeat(64));
        let with = id.clone_with_digest(digest.clone());
        assert_eq!(with.tag(), Some("tag"));
        assert_eq!(with.digest(), Some(digest));
    }

    #[test]
    fn without_tag_keeps_digest() {
        let hex = "a".repeat(64);
        let id = Identifier::parse(&format!("ghcr.io/repo:tag@sha256:{hex}")).unwrap();
        let stripped = id.without_tag();
        assert_eq!(stripped.tag(), None);
        assert!(stripped.digest().is_some());
    }

    // ── Display / serde round-trip ───────────────────────────────────

    #[test]
    fn display_round_trip() {
        let hex = "abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890";
        for input in [
            "ghcr.io/repo:tag",
            "localhost:5000/org/repo",
            "sub.foo.com/bar/baz/quux",
            &format!("localhost:5000/repo:tag@sha256:{hex}"),
        ] {
            let id = Identifier::parse(input).unwrap();
            let reparsed = Identifier::parse(&id.to_string()).unwrap();
            assert_eq!(id, reparsed, "round-trip failed for {input}");
        }
    }

    #[test]
    fn serde_round_trip() {
        let id = Identifier::parse("ghcr.io/repo:tag").unwrap();
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, "\"ghcr.io/repo:tag\"");
        let back: Identifier = serde_json::from_str(&json).unwrap();
        assert_eq!(id, back);
    }

    #[test]
    fn deserialize_rejects_bare_name() {
        let err = serde_json::from_str::<Identifier>(r#""code-review:stable""#).unwrap_err();
        assert!(err.to_string().contains("explicit registry"));
    }

    #[test]
    fn from_str_is_strict() {
        let err = "code-review".parse::<Identifier>().unwrap_err();
        assert!(matches!(err.kind, IdentifierErrorKind::MissingRegistry));
    }

    #[test]
    fn new_registry_constructs_directly() {
        let id = Identifier::new_registry("code-review", "ghcr.io/acme");
        assert_eq!(id.registry(), "ghcr.io/acme");
        assert_eq!(id.repository(), "code-review");
        assert_eq!(id.tag(), None);
        assert_eq!(id.digest(), None);
    }
}
