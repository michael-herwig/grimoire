// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! A named, kinded reference to an artifact.

use super::{ArtifactKind, Identifier};

/// An artifact as referenced from config: its kind, its config key (name),
/// and the OCI identifier it resolves to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactRef {
    /// Whether this is a skill or a rule.
    pub kind: ArtifactKind,
    /// The config key the artifact is declared under.
    pub name: String,
    /// The OCI identifier the artifact resolves to.
    pub id: Identifier,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constructs_and_compares() {
        let id = Identifier::parse("ghcr.io/acme/code-review:stable").unwrap();
        let a = ArtifactRef {
            kind: ArtifactKind::Skill,
            name: "code-review".to_string(),
            id: id.clone(),
        };
        let b = ArtifactRef {
            kind: ArtifactKind::Skill,
            name: "code-review".to_string(),
            id,
        };
        assert_eq!(a, b);
        assert_eq!(a.name, "code-review");
    }
}
