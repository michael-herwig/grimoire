// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! Rolling-release publish-tag computation.
//!
//! A normal release of semver `X.Y.Z` is published once and then the
//! floating tags `X.Y.Z`, `X.Y`, `X`, and `latest` are all pointed at it
//! (most-specific first for crash safety — see `command/release.rs`). A
//! **prerelease** (`X.Y.Z-rc.1`) is intentionally NOT part of the
//! cascade: only its own exact tag is published — no `X.Y`, `X`, or
//! `latest` move, so a release candidate never silently becomes the
//! floating version users pull. Build metadata (`+meta`) does not affect
//! the published tag set (it is not a valid OCI tag character anyway and
//! is dropped from the cascade).
//!
//! A tag that is **not** a semantic version (`canary`, `edge`, `pr-123`,
//! or even a partial `1.2`) is published as a single literal tag with no
//! cascade — there is no version to derive `X.Y`/`X`/`latest` from, so the
//! exact tag is the whole published set. Only a reference with no tag at
//! all is rejected.

use super::Identifier;

/// A release-tier failure (currently only an unparseable version).
///
/// Three-layer shape: top [`crate::error::Error`] → context-bearing
/// [`ReleaseError`] → discriminant [`ReleaseErrorKind`].
#[derive(Debug)]
pub struct ReleaseError {
    /// The release reference the failure is about (when one applies).
    pub reference: Option<Box<Identifier>>,
    /// The specific failure.
    pub kind: ReleaseErrorKind,
}

impl ReleaseError {
    /// Construct without a reference (e.g. a bare version parse failure).
    pub fn without_reference(kind: ReleaseErrorKind) -> Self {
        Self { reference: None, kind }
    }

    /// Attach `reference` context to `kind`.
    pub fn with_reference(reference: Identifier, kind: ReleaseErrorKind) -> Self {
        Self {
            reference: Some(Box::new(reference)),
            kind,
        }
    }
}

impl std::fmt::Display for ReleaseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.reference {
            Some(r) => write!(f, "{r}: {}", self.kind),
            None => write!(f, "{}", self.kind),
        }
    }
}

impl std::error::Error for ReleaseError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.kind)
    }
}

/// Inner discriminant for release-tier failures.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ReleaseErrorKind {
    /// The release version is not valid semver.
    #[error("invalid semantic version '{version}'")]
    InvalidVersion {
        version: String,
        #[source]
        source: semver::Error,
    },

    /// The release reference carried no tag, so there is nothing to
    /// publish under.
    #[error("release reference has no tag; expected registry/repo:tag")]
    MissingTag,

    /// The exact version tag already exists pointing at a different
    /// digest, and `--force` was not given.
    #[error(
        "version tag '{tag}' already exists at a different digest (existing {existing}, new {new}); rerun with --force to move it"
    )]
    TagExists { tag: String, existing: String, new: String },
}

/// Compute the published tag set for `tag`.
///
/// - `1.2.3` → `["1.2.3", "1.2", "1", "latest"]` (full semver cascades)
/// - `2.0.0` → `["2.0.0", "2.0", "2", "latest"]`
/// - `1.2.3-rc.1` (prerelease) → `["1.2.3-rc.1"]` (no cascade, no latest)
/// - `1.2.3+build` → `["1.2.3", "1.2", "1", "latest"]` (build metadata
///   dropped from the tag set)
/// - `canary` / `1.2` / any non-semver → `["canary"]` / `["1.2"]` (the
///   literal tag only — there is no version to cascade, so the cascade is
///   disabled and exactly the requested tag is published)
///
/// The exact tag is always element `0` so the caller can publish it first
/// (crash safety: the specific tag exists before any floating tag is moved
/// to it).
///
/// # Errors
///
/// [`ReleaseErrorKind::MissingTag`] when `tag` is empty (a release
/// reference must carry a tag).
pub fn publish_tags(tag: &str) -> Result<Vec<String>, ReleaseError> {
    if tag.is_empty() {
        return Err(ReleaseError::without_reference(ReleaseErrorKind::MissingTag));
    }

    // A non-semver tag (`canary`, `edge`, or a partial `1.2`) has no
    // version to derive floating pointers from: publish exactly that one
    // literal tag, no cascade. Only a full `X.Y.Z` semver cascades.
    let Ok(parsed) = semver::Version::parse(tag) else {
        return Ok(vec![tag.to_string()]);
    };

    if !parsed.pre.is_empty() {
        // Prerelease: only the exact tag, normalized (build metadata
        // stripped — `major.minor.patch-pre`).
        let exact = format!("{}.{}.{}-{}", parsed.major, parsed.minor, parsed.patch, parsed.pre);
        return Ok(vec![exact]);
    }

    let exact = format!("{}.{}.{}", parsed.major, parsed.minor, parsed.patch);
    Ok(vec![
        exact,
        format!("{}.{}", parsed.major, parsed.minor),
        parsed.major.to_string(),
        "latest".to_string(),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_version_cascades_to_four_tags() {
        assert_eq!(publish_tags("1.2.3").unwrap(), vec!["1.2.3", "1.2", "1", "latest"]);
        assert_eq!(publish_tags("2.0.0").unwrap(), vec!["2.0.0", "2.0", "2", "latest"]);
        assert_eq!(publish_tags("0.10.5").unwrap(), vec!["0.10.5", "0.10", "0", "latest"]);
    }

    #[test]
    fn prerelease_is_exact_only_no_cascade_no_latest() {
        assert_eq!(publish_tags("1.2.3-rc.1").unwrap(), vec!["1.2.3-rc.1"]);
        assert_eq!(publish_tags("2.0.0-alpha").unwrap(), vec!["2.0.0-alpha"]);
        let t = publish_tags("1.0.0-beta.2").unwrap();
        assert_eq!(t.len(), 1);
        assert!(!t.contains(&"latest".to_string()));
    }

    #[test]
    fn build_metadata_dropped_from_tag_set() {
        assert_eq!(
            publish_tags("1.2.3+20260101").unwrap(),
            vec!["1.2.3", "1.2", "1", "latest"]
        );
    }

    #[test]
    fn non_version_tag_publishes_single_tag_no_cascade() {
        // Arbitrary names and partial semver alike: exactly one literal tag,
        // no `X.Y`/`X`/`latest` cascade (cascade is disabled for non-versions).
        for tag in ["canary", "edge", "pr-123", "nightly", "1.2", "1", "v1.2.3"] {
            assert_eq!(publish_tags(tag).unwrap(), vec![tag.to_string()], "tag {tag}");
        }
    }

    #[test]
    fn empty_tag_is_missing_tag_error() {
        let err = publish_tags("").expect_err("a release reference must carry a tag");
        assert!(matches!(err.kind, ReleaseErrorKind::MissingTag));
    }

    #[test]
    fn exact_tag_is_first() {
        assert_eq!(publish_tags("3.4.5").unwrap()[0], "3.4.5");
        assert_eq!(publish_tags("3.4.5-rc.1").unwrap()[0], "3.4.5-rc.1");
        assert_eq!(publish_tags("canary").unwrap()[0], "canary");
    }

    #[test]
    fn missing_tag_error_displays_guidance() {
        let err = publish_tags("").expect_err("reject");
        assert!(err.to_string().contains("no tag"));
    }
}
