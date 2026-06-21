// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! Offline computation of the **effective desired set**: every artifact a
//! declaration implies, direct entries plus the members of every declared
//! bundle, conflict-resolved.
//!
//! Declaration mutations (`grim remove`/`uninstall`, the TUI delete
//! action) compute this set for the declaration *before* and *after* the
//! edit, then act on the difference — drop `E_before \ E_after`, keep the
//! intersection — instead of hand-rolled per-kind lock surgery. The bundle
//! member lists come from the lock's `[[bundle]]` cache, so no network is
//! involved. See `adr_effective_set_mutations.md`.

use std::collections::BTreeMap;

use crate::config::declaration::DesiredSet;
use crate::lock::locked_artifact::BundleProvenance;
use crate::lock::locked_bundle::LockedBundle;
use crate::oci::{ArtifactKind, Identifier};

/// How the effective declaration provides one `(kind, name)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    /// Declared directly in `[skills]`/`[rules]`/`[agents]` (always wins
    /// over bundle members of the same key).
    Direct(Identifier),
    /// Provided only by bundles, all agreeing on one identifier.
    Bundles {
        /// The agreed member identifier.
        id: Identifier,
        /// Every contributing bundle, sorted by `(repo, tag)`, deduped.
        contributors: Vec<BundleProvenance>,
    },
    /// Provided only by bundles that DISAGREE on the identifier (a
    /// conflict the next `grim lock` fails closed on). Mutations treat the
    /// key as still desired but unresolvable offline.
    Conflicted,
}

/// Compute the effective desired set of `set`, expanding declared bundles
/// from the lock's cached snapshots.
///
/// Returns `None` when any declared bundle has no **matching** snapshot in
/// `cached` (pre-cache lock, or the declaration changed since the cache
/// was written) — the caller must fall back to its legacy behavior, since
/// membership is unknowable offline. A snapshot matches when its binding
/// name, `registry/repo`, and provenance tag all equal the declared
/// identifier's.
pub fn effective_set(set: &DesiredSet, cached: &[LockedBundle]) -> Option<BTreeMap<(ArtifactKind, String), Origin>> {
    let mut out: BTreeMap<(ArtifactKind, String), Origin> = BTreeMap::new();

    for (kind, map) in [
        (ArtifactKind::Skill, &set.skills),
        (ArtifactKind::Rule, &set.rules),
        (ArtifactKind::Agent, &set.agents),
    ] {
        for (name, id) in map {
            out.insert((kind, name.clone()), Origin::Direct(id.clone()));
        }
    }

    // Group bundle contributions per (kind, name): the (possibly
    // unparseable) member identifier plus the contributing bundle.
    type Contribution = (Option<Identifier>, BundleProvenance);
    let mut grouped: BTreeMap<(ArtifactKind, String), Vec<Contribution>> = BTreeMap::new();
    for (binding, declared_id) in &set.bundles {
        let snapshot = cached.iter().find(|b| snapshot_matches(binding, declared_id, b))?;
        for member in &snapshot.members {
            let provenance = BundleProvenance::new(snapshot.repo.clone(), snapshot.tag.clone());
            // An unparseable cached id (hand-edited lock) degrades the key
            // to Conflicted below rather than failing the whole mutation.
            let id = Identifier::parse(&member.id).ok();
            grouped
                .entry((member.kind, member.name.clone()))
                .or_default()
                .push((id, provenance));
        }
    }

    for (key, group) in grouped {
        if out.contains_key(&key) {
            continue; // a direct declaration always wins
        }
        let first = &group[0].0;
        let agree = first.is_some() && group.iter().all(|(id, _)| id == first);
        let origin = if agree {
            let mut contributors: Vec<BundleProvenance> = group.iter().map(|(_, p)| p.clone()).collect();
            contributors.sort_by(|a, b| (&a.repo, &a.tag).cmp(&(&b.repo, &b.tag)));
            contributors.dedup();
            Origin::Bundles {
                // `agree` requires `first.is_some()`; the clone cannot panic.
                #[allow(clippy::expect_used)]
                id: first.clone().expect("agree implies a parsed identifier"),
                contributors,
            }
        } else {
            Origin::Conflicted
        };
        out.insert(key, origin);
    }

    Some(out)
}

/// Whether a cached snapshot corresponds to the declared bundle binding:
/// same binding name, same `registry/repo`, and the snapshot's provenance
/// tag equals the declared tag (or the short digest for a digest-only
/// declaration — mirroring how the resolver stamps it).
pub fn snapshot_matches(binding: &str, declared_id: &Identifier, snapshot: &LockedBundle) -> bool {
    if snapshot.name != binding || snapshot.repo != declared_id.registry_repository() {
        return false;
    }
    let declared_tag = match declared_id.tag() {
        Some(tag) => tag.to_string(),
        None => declared_id
            .digest()
            .map(|d| d.to_short_string())
            .unwrap_or_else(|| "latest".to_string()),
    };
    snapshot.tag == declared_tag
}

/// Whether removing the **direct** declaration of `(kind, name)` would leave
/// the artifact still provided by a declared bundle — so its materialized
/// files must NOT be deleted on uninstall, because it stays in the effective
/// desired set.
///
/// This is the *file-retention* gate, deliberately broader than the
/// lock-retention rule in [`crate::command::remove::drop_from_lock`]: the lock
/// entry survives only on an exact-identifier flip, but the files survive
/// whenever a declared bundle names the artifact at **any** identifier (the
/// version is reconciled later by `grim lock` + update). Deleting still-desired
/// files is the surprising, destructive behavior this guard prevents.
///
/// Fires only for an artifact that is *currently* declared directly — so
/// removing its direct entry is meaningful. A bundle-only member is never gated
/// here: uninstalling one is the explicit "delete this member's files" action
/// (the TUI member-delete feature). `Bundle` kind is never bundle-held.
///
/// Pure + offline: expands the lock's `[[bundle]]` snapshots via
/// [`effective_set`]. Returns `false` when the snapshots are incomplete
/// (pre-cache lock) — bundle membership is then unknowable offline, so the
/// caller falls back to the pre-effective-set behavior (delete the files).
pub fn bundle_holds_after_direct_removal(
    set: &DesiredSet,
    cached: &[LockedBundle],
    kind: ArtifactKind,
    name: &str,
) -> bool {
    let directly_declared = match kind {
        ArtifactKind::Skill => set.skills.contains_key(name),
        ArtifactKind::Rule => set.rules.contains_key(name),
        ArtifactKind::Agent => set.agents.contains_key(name),
        ArtifactKind::Bundle => false,
    };
    if !directly_declared {
        return false;
    }
    let mut after = set.clone();
    let _removed = match kind {
        ArtifactKind::Skill => after.skills.remove(name).is_some(),
        ArtifactKind::Rule => after.rules.remove(name).is_some(),
        ArtifactKind::Agent => after.agents.remove(name).is_some(),
        ArtifactKind::Bundle => false,
    };
    after.invalidate_declaration_hash_cache();
    effective_set(&after, cached)
        .map(|e| e.contains_key(&(kind, name.to_string())))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oci::bundle::BundleMember;
    use crate::oci::{Digest, PinnedIdentifier};
    use std::collections::BTreeMap as Map;

    fn id(s: &str) -> Identifier {
        Identifier::parse(s).unwrap()
    }

    fn snapshot(binding: &str, repo: &str, tag: &str, members: &[(ArtifactKind, &str, &str)]) -> LockedBundle {
        let pinned_id = id(&format!("{repo}:{tag}")).clone_with_digest(Digest::Sha256("a".repeat(64)));
        LockedBundle {
            name: binding.to_string(),
            repo: repo.to_string(),
            tag: tag.to_string(),
            pinned: PinnedIdentifier::try_from(pinned_id).unwrap(),
            members: members
                .iter()
                .map(|(kind, name, mid)| BundleMember {
                    kind: *kind,
                    name: (*name).to_string(),
                    id: (*mid).to_string(),
                })
                .collect(),
        }
    }

    fn set_with(skills: &[(&str, &str)], bundles: &[(&str, &str)]) -> DesiredSet {
        let skills: Map<String, Identifier> = skills.iter().map(|(n, i)| ((*n).to_string(), id(i))).collect();
        let mut set = DesiredSet::from_parts(skills, Map::new());
        for (n, i) in bundles {
            set.bundles.insert((*n).to_string(), id(i));
        }
        set.invalidate_declaration_hash_cache();
        set
    }

    #[test]
    fn direct_wins_over_bundle_member() {
        let set = set_with(
            &[("cr", "ghcr.io/acme/cr:direct")],
            &[("stack", "ghcr.io/acme/stack:1")],
        );
        let cache = [snapshot(
            "stack",
            "ghcr.io/acme/stack",
            "1",
            &[(ArtifactKind::Skill, "cr", "ghcr.io/acme/cr:bundled")],
        )];
        let e = effective_set(&set, &cache).expect("cache complete");
        assert_eq!(
            e[&(ArtifactKind::Skill, "cr".to_string())],
            Origin::Direct(id("ghcr.io/acme/cr:direct"))
        );
    }

    #[test]
    fn bundle_only_member_with_contributors() {
        let set = set_with(&[], &[("a", "ghcr.io/acme/a:1"), ("b", "ghcr.io/acme/b:1")]);
        let cache = [
            snapshot(
                "a",
                "ghcr.io/acme/a",
                "1",
                &[(ArtifactKind::Skill, "cr", "ghcr.io/acme/cr:1")],
            ),
            snapshot(
                "b",
                "ghcr.io/acme/b",
                "1",
                &[(ArtifactKind::Skill, "cr", "ghcr.io/acme/cr:1")],
            ),
        ];
        let e = effective_set(&set, &cache).expect("cache complete");
        match &e[&(ArtifactKind::Skill, "cr".to_string())] {
            Origin::Bundles { id: mid, contributors } => {
                assert_eq!(mid, &id("ghcr.io/acme/cr:1"));
                assert_eq!(contributors.len(), 2);
            }
            other => panic!("expected Bundles, got {other:?}"),
        }
    }

    #[test]
    fn disagreeing_bundles_mark_conflicted() {
        let set = set_with(&[], &[("a", "ghcr.io/acme/a:1"), ("b", "ghcr.io/acme/b:1")]);
        let cache = [
            snapshot(
                "a",
                "ghcr.io/acme/a",
                "1",
                &[(ArtifactKind::Skill, "cr", "ghcr.io/acme/cr:1")],
            ),
            snapshot(
                "b",
                "ghcr.io/acme/b",
                "1",
                &[(ArtifactKind::Skill, "cr", "ghcr.io/acme/cr:2")],
            ),
        ];
        let e = effective_set(&set, &cache).expect("cache complete");
        assert_eq!(e[&(ArtifactKind::Skill, "cr".to_string())], Origin::Conflicted);
    }

    #[test]
    fn missing_snapshot_returns_none() {
        let set = set_with(&[], &[("stack", "ghcr.io/acme/stack:1")]);
        assert!(effective_set(&set, &[]).is_none(), "incomplete cache must be signalled");
    }

    #[test]
    fn retagged_declaration_invalidates_snapshot() {
        // The declaration moved to :2 since the cache recorded :1 — the
        // snapshot no longer matches, so the set is incomputable offline.
        let set = set_with(&[], &[("stack", "ghcr.io/acme/stack:2")]);
        let cache = [snapshot(
            "stack",
            "ghcr.io/acme/stack",
            "1",
            &[(ArtifactKind::Skill, "cr", "ghcr.io/acme/cr:1")],
        )];
        assert!(effective_set(&set, &cache).is_none());
    }

    #[test]
    fn no_bundles_needs_no_cache() {
        let set = set_with(&[("cr", "ghcr.io/acme/cr:1")], &[]);
        let e = effective_set(&set, &[]).expect("no bundles, no cache needed");
        assert_eq!(e.len(), 1);
    }

    // ── bundle_holds_after_direct_removal (file-retention gate) ─────────────

    #[test]
    fn holds_when_bundle_provides_same_identifier() {
        // Direct + a bundle naming the SAME id: removing the direct leaves the
        // bundle holding it → files must be kept.
        let set = set_with(&[("cr", "ghcr.io/acme/cr:1")], &[("stack", "ghcr.io/acme/stack:1")]);
        let cache = [snapshot(
            "stack",
            "ghcr.io/acme/stack",
            "1",
            &[(ArtifactKind::Skill, "cr", "ghcr.io/acme/cr:1")],
        )];
        assert!(bundle_holds_after_direct_removal(
            &set,
            &cache,
            ArtifactKind::Skill,
            "cr"
        ));
    }

    #[test]
    fn holds_when_bundle_provides_different_identifier() {
        // The maintainer's real case: standalone at :latest, bundle pins the
        // same name at :0. The bundle still HOLDS it (a different version it
        // reconciles later) → files must be kept even though the lock entry
        // itself goes stale.
        let set = set_with(
            &[("cr", "ghcr.io/acme/cr:latest")],
            &[("stack", "ghcr.io/acme/stack:1")],
        );
        let cache = [snapshot(
            "stack",
            "ghcr.io/acme/stack",
            "1",
            &[(ArtifactKind::Skill, "cr", "ghcr.io/acme/cr:0")],
        )];
        assert!(bundle_holds_after_direct_removal(
            &set,
            &cache,
            ArtifactKind::Skill,
            "cr"
        ));
    }

    #[test]
    fn does_not_hold_when_no_bundle_provides_it() {
        // Direct only, no bundle names it → normal uninstall deletes.
        let set = set_with(&[("cr", "ghcr.io/acme/cr:1")], &[]);
        assert!(!bundle_holds_after_direct_removal(&set, &[], ArtifactKind::Skill, "cr"));
    }

    #[test]
    fn does_not_hold_for_bundle_only_member() {
        // Not directly declared — a pure bundle member. Uninstalling it is the
        // explicit member-delete action, so the gate must NOT fire (files are
        // deleted, the member becomes re-installable).
        let set = set_with(&[], &[("stack", "ghcr.io/acme/stack:1")]);
        let cache = [snapshot(
            "stack",
            "ghcr.io/acme/stack",
            "1",
            &[(ArtifactKind::Skill, "cr", "ghcr.io/acme/cr:1")],
        )];
        assert!(!bundle_holds_after_direct_removal(
            &set,
            &cache,
            ArtifactKind::Skill,
            "cr"
        ));
    }

    #[test]
    fn does_not_hold_when_cache_incomplete() {
        // A declared bundle with no matching snapshot (pre-cache lock):
        // membership is unknowable offline → fall back to deleting.
        let set = set_with(&[("cr", "ghcr.io/acme/cr:1")], &[("stack", "ghcr.io/acme/stack:1")]);
        assert!(!bundle_holds_after_direct_removal(&set, &[], ArtifactKind::Skill, "cr"));
    }

    #[test]
    fn does_not_hold_for_bundle_kind() {
        // A bundle is never "held by a bundle".
        let set = set_with(&[], &[("stack", "ghcr.io/acme/stack:1")]);
        let cache = [snapshot("stack", "ghcr.io/acme/stack", "1", &[])];
        assert!(!bundle_holds_after_direct_removal(
            &set,
            &cache,
            ArtifactKind::Bundle,
            "stack"
        ));
    }
}
