// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! A validated [`Identifier`] guaranteed to carry a digest.
//!
//! Adapted from OCX `oci/pinned_identifier.rs`. Used by the lock layer to
//! persist fully resolved artifacts: the digest guarantee means consumers
//! never need fallback resolution logic.

use serde::{Deserialize, Serialize};

use super::{Digest, Identifier};

/// A validated [`Identifier`] guaranteed to carry a digest.
///
/// Equality and hashing include all fields (registry, repository, tag,
/// digest). For content-identity semantics that ignore the advisory tag,
/// use [`eq_content`](Self::eq_content).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PinnedIdentifier(Identifier);

impl PinnedIdentifier {
    /// Returns the digest. Always present by construction.
    pub fn digest(&self) -> Digest {
        // INVARIANT: the only constructors (`TryFrom<Identifier>`,
        // `Deserialize`) reject an identifier without a digest, and
        // `clone_with_digest` always sets one. So this is unreachable.
        match self.0.digest() {
            Some(d) => d,
            None => unreachable!("PinnedIdentifier always has a digest"),
        }
    }

    /// Content-identity comparison: equal if registry, repository, and
    /// digest match. The advisory tag is ignored.
    pub fn eq_content(&self, other: &Self) -> bool {
        self.0.registry() == other.0.registry()
            && self.0.repository() == other.0.repository()
            && self.digest() == other.digest()
    }

    /// Returns a copy with the advisory tag stripped.
    pub fn strip_advisory(&self) -> Self {
        Self(self.0.without_tag())
    }

    /// Returns a copy with the digest replaced. Tag (if any) is preserved.
    pub fn clone_with_digest(&self, digest: Digest) -> Self {
        Self(self.0.clone_with_digest(digest))
    }

    /// Returns a borrow of the inner [`Identifier`].
    pub fn as_identifier(&self) -> &Identifier {
        &self.0
    }
}

impl std::ops::Deref for PinnedIdentifier {
    type Target = Identifier;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl From<PinnedIdentifier> for Identifier {
    fn from(pinned: PinnedIdentifier) -> Self {
        pinned.0
    }
}

impl TryFrom<Identifier> for PinnedIdentifier {
    type Error = PinnedIdentifierError;

    fn try_from(id: Identifier) -> Result<Self, Self::Error> {
        if id.digest().is_none() {
            return Err(PinnedIdentifierError { identifier: id });
        }
        Ok(Self(id))
    }
}

impl std::fmt::Display for PinnedIdentifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl Serialize for PinnedIdentifier {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for PinnedIdentifier {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        let id = Identifier::parse(&s).map_err(serde::de::Error::custom)?;
        Self::try_from(id).map_err(serde::de::Error::custom)
    }
}

/// A pinned identifier requires a digest but none was present.
#[derive(Debug, thiserror::Error)]
#[error("pinned identifier requires a digest: {identifier}")]
#[non_exhaustive]
pub struct PinnedIdentifierError {
    pub identifier: Identifier,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sha256_hex() -> String {
        "a".repeat(64)
    }

    fn id_with_digest() -> Identifier {
        Identifier::new_registry("cmake", "example.com").clone_with_digest(Digest::Sha256(sha256_hex()))
    }

    fn id_with_tag_and_digest() -> Identifier {
        Identifier::new_registry("cmake", "example.com")
            .clone_with_tag("3.28")
            .clone_with_digest(Digest::Sha256(sha256_hex()))
    }

    fn id_without_digest() -> Identifier {
        Identifier::new_registry("cmake", "example.com").clone_with_tag("3.28")
    }

    #[test]
    fn try_from_with_digest_succeeds() {
        let id = id_with_digest();
        let pinned = PinnedIdentifier::try_from(id.clone()).unwrap();
        assert_eq!(pinned.registry(), id.registry());
        assert_eq!(pinned.digest(), id.digest().unwrap());
    }

    #[test]
    fn try_from_without_digest_fails() {
        assert!(PinnedIdentifier::try_from(id_without_digest()).is_err());
        assert!(PinnedIdentifier::try_from(Identifier::new_registry("cmake", "example.com")).is_err());
    }

    #[test]
    fn try_from_preserves_tag() {
        let pinned = PinnedIdentifier::try_from(id_with_tag_and_digest()).unwrap();
        assert_eq!(pinned.tag(), Some("3.28"));
    }

    #[test]
    fn equality_includes_tag_but_eq_content_ignores_it() {
        let with_tag = PinnedIdentifier::try_from(id_with_tag_and_digest()).unwrap();
        let without_tag = PinnedIdentifier::try_from(id_with_digest()).unwrap();
        assert_ne!(with_tag, without_tag);
        assert!(with_tag.eq_content(&without_tag));
    }

    #[test]
    fn strip_advisory_enables_dedup() {
        use std::collections::HashSet;
        let with_tag = PinnedIdentifier::try_from(id_with_tag_and_digest()).unwrap();
        let without_tag = PinnedIdentifier::try_from(id_with_digest()).unwrap();
        assert_eq!(with_tag.strip_advisory(), without_tag.strip_advisory());
        let mut set = HashSet::new();
        set.insert(with_tag.strip_advisory());
        assert!(!set.insert(without_tag.strip_advisory()));
    }

    #[test]
    fn clone_with_digest_replaces_preserves_tag() {
        let pinned = PinnedIdentifier::try_from(id_with_tag_and_digest()).unwrap();
        let new_digest = Digest::Sha256("b".repeat(64));
        let replaced = pinned.clone_with_digest(new_digest.clone());
        assert_eq!(replaced.digest(), new_digest);
        assert_eq!(replaced.tag(), pinned.tag());
    }

    #[test]
    fn deref_and_into_identifier() {
        let id = id_with_digest();
        let pinned = PinnedIdentifier::try_from(id.clone()).unwrap();
        assert_eq!(pinned.repository(), "cmake");
        assert_eq!(pinned.as_identifier().registry(), "example.com");
        let back: Identifier = pinned.into();
        assert_eq!(back, id);
    }

    #[test]
    fn serde_round_trip() {
        let pinned = PinnedIdentifier::try_from(id_with_digest()).unwrap();
        let json = serde_json::to_string(&pinned).unwrap();
        let back: PinnedIdentifier = serde_json::from_str(&json).unwrap();
        assert_eq!(pinned, back);
    }

    #[test]
    fn deserialize_rejects_missing_digest() {
        let err = serde_json::from_str::<PinnedIdentifier>(r#""example.com/cmake""#).unwrap_err();
        assert!(err.to_string().contains("digest"));
    }
}
