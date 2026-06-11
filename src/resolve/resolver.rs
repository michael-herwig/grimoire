// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! Resolve declared floating tags to pinned digests and assemble a
//! [`GrimoireLock`].
//!
//! Adapted from OCX `project::resolve`, collapsed to the Grimoire scope:
//! no group filter (skills/rules instead of tools/groups), the single
//! [`OciAccess`] seam instead of a chained index, and a declaration-hash
//! staleness gate on the partial path that fires before any I/O.
//!
//! Fully transactional: the first failure aborts every sibling task and
//! the function returns that error — no partial lock is ever produced.

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

use tokio::task::JoinSet;

use crate::oci::bundle::{BUNDLE_LAYER_SIZE_LIMIT, BundleManifest, MAX_BUNDLE_MEMBERS};
use crate::skill::SkillName;

use super::resolve_error::{ResolveError, ResolveErrorKind};
use super::resolve_options::ResolveOptions;
use crate::config::declaration::DesiredSet;
use crate::config::hash::DECLARATION_HASH_VERSION;
use crate::config::scope::ConfigScope;
use crate::lock::grimoire_lock::{GrimoireLock, LockMetadata};
use crate::lock::lock_io::now_rfc3339;
use crate::lock::lock_version::LockVersion;
use crate::lock::locked_artifact::{BundleProvenance, LockedArtifact};
use crate::lock::locked_bundle::LockedBundle;
use crate::oci::access::error::AccessErrorKind;
use crate::oci::access::{OciAccess, Operation};
use crate::oci::reference::ArtifactRef;
use crate::oci::{ArtifactKind, Identifier, PinnedIdentifier};

/// Resolve every declared skill, rule, and agent and assemble a fresh lock.
///
/// `scope` is currently informational (recorded by the caller); the lock
/// document itself is scope-agnostic. Fully transactional — either every
/// artifact resolves or an error is returned with no lock produced.
///
/// # Errors
///
/// Returns the first [`ResolveError`] encountered (tag-not-found, auth,
/// registry-unreachable, timeout). Sibling tasks are aborted on failure.
pub async fn resolve_lock(
    set: &DesiredSet,
    access: &Arc<dyn OciAccess>,
    scope: ConfigScope,
    options: &ResolveOptions,
) -> Result<GrimoireLock, ResolveError> {
    let _ = scope;
    let (work, bundles) = build_work(set, access, options).await?;
    let mut resolved = resolve_work(work, access, options).await?;
    sort_locked(&mut resolved);
    Ok(build_lock(resolved, set, bundles))
}

/// Re-resolve only the named subset, carrying every other previous entry
/// forward verbatim.
///
/// The stale-lock guard fires **first**, before any access call: if the
/// predecessor's `declaration_hash` does not match the current
/// declaration, a fresh resolve is required (a partial would launder a
/// stale lock under a new hash). Each requested name must be declared.
///
/// # Errors
///
/// [`ResolveErrorKind::StaleLock`] when the predecessor is stale;
/// [`ResolveErrorKind::TagNotFound`] when a requested name is not
/// declared; otherwise the same failures as [`resolve_lock`].
pub async fn resolve_lock_partial(
    set: &DesiredSet,
    previous: &GrimoireLock,
    access: &Arc<dyn OciAccess>,
    names: &[String],
    scope: ConfigScope,
    options: &ResolveOptions,
) -> Result<GrimoireLock, ResolveError> {
    let _ = scope;

    // Stale-lock laundering guard — BEFORE any I/O or access call.
    let current = set.declaration_hash_cached();
    if previous.metadata.declaration_hash != current {
        // No single artifact owns this failure; attribute it to the first
        // requested name (or a synthetic placeholder) for context.
        let reference = stale_reference(set, names);
        return Err(ResolveError::new(
            reference,
            ResolveErrorKind::StaleLock {
                previous_hash: previous.metadata.declaration_hash.clone(),
                current_hash: current.to_string(),
            },
        ));
    }

    // Expand bundles + merge so a requested name can be a bundle member.
    let (all_work, bundles) = build_work(set, access, options).await?;

    // Validate every requested name is an effective artifact (direct or a
    // bundle member).
    for name in names {
        if !all_work.iter().any(|w| &w.reference.name == name) {
            let reference = ArtifactRef {
                kind: ArtifactKind::Skill,
                name: name.clone(),
                // A placeholder identifier: the name is undeclared, so no
                // real id exists. `parse` cannot fail on this literal.
                id: Identifier::new_registry(name.clone(), "invalid.localhost"),
            };
            return Err(ResolveError::new(reference, ResolveErrorKind::TagNotFound));
        }
    }

    // Re-resolve only the named subset.
    let selected: Vec<WorkItem> = all_work
        .into_iter()
        .filter(|w| names.iter().any(|n| n == &w.reference.name))
        .collect();
    let resolved = resolve_work(selected, access, options).await?;

    // Carry forward every previous entry not in the re-resolved set,
    // keyed by (kind, name); then merge and sort.
    let new_keys: std::collections::HashSet<(ArtifactKind, String)> =
        resolved.iter().map(|a| (a.kind, a.name.clone())).collect();

    let mut merged: Vec<LockedArtifact> = previous
        .iter_artifacts()
        .filter(|a| !new_keys.contains(&(a.kind, a.name.clone())))
        .cloned()
        .collect();
    merged.extend(resolved);
    sort_locked(&mut merged);

    // The partial path re-expanded every declared bundle above, so the
    // cached snapshots are as fresh as a full resolve's.
    Ok(build_lock(merged, set, bundles))
}

/// Collect the artifacts to resolve in deterministic `BTreeMap` order:
/// every declared skill (kind Skill), then every declared rule (kind
/// Rule), then every declared agent (kind Agent). The final `(kind, name)`
/// sort happens after resolution.
fn collect_work(set: &DesiredSet) -> Vec<ArtifactRef> {
    let mut work = Vec::with_capacity(set.skills.len() + set.rules.len() + set.agents.len());
    for (name, id) in &set.skills {
        work.push(ArtifactRef {
            kind: ArtifactKind::Skill,
            name: name.clone(),
            id: id.clone(),
        });
    }
    for (name, id) in &set.rules {
        work.push(ArtifactRef {
            kind: ArtifactKind::Rule,
            name: name.clone(),
            id: id.clone(),
        });
    }
    for (name, id) in &set.agents {
        work.push(ArtifactRef {
            kind: ArtifactKind::Agent,
            name: name.clone(),
            id: id.clone(),
        });
    }
    work
}

/// A unit of resolution work: the reference to pin plus, for a bundle
/// member, the provenance to stamp on the resulting lock entry.
#[derive(Debug)]
struct WorkItem {
    reference: ArtifactRef,
    /// Every declared bundle this item came from (agreeing bundles
    /// coalesce to one item but ALL contributors are recorded); empty for
    /// a direct `[skills]`/`[rules]` entry.
    bundles: Vec<BundleProvenance>,
}

/// One member produced by expanding a declared bundle.
#[derive(Debug)]
struct ExpandedMember {
    kind: ArtifactKind,
    name: String,
    id: Identifier,
    bundle_repo: String,
    bundle_tag: String,
}

/// Build the full work list: direct declarations plus the members of every
/// declared bundle, after applying the conflict policy.
///
/// Conflict policy, per `(kind, name)`:
/// - a direct `[skills]`/`[rules]` declaration always wins over any bundle;
/// - multiple bundles that agree on the identifier coalesce to one entry;
/// - multiple bundles that disagree fail closed with
///   [`ResolveErrorKind::BundleConflict`].
async fn build_work(
    set: &DesiredSet,
    access: &Arc<dyn OciAccess>,
    options: &ResolveOptions,
) -> Result<(Vec<WorkItem>, Vec<LockedBundle>), ResolveError> {
    let mut work: Vec<WorkItem> = collect_work(set)
        .into_iter()
        .map(|reference| WorkItem {
            reference,
            bundles: Vec::new(),
        })
        .collect();

    // Fast path: no bundles ⇒ no bundle I/O at all.
    if set.bundles.is_empty() {
        return Ok((work, Vec::new()));
    }

    let direct_keys: HashSet<(ArtifactKind, String)> = work
        .iter()
        .map(|w| (w.reference.kind, w.reference.name.clone()))
        .collect();

    let (members, snapshots) = expand_bundles(set, access, options).await?;
    work.extend(merge_bundle_members(&direct_keys, members)?);
    Ok((work, snapshots))
}

/// Apply the bundle conflict policy to expanded members, returning the work
/// items for those that survive. Pure (no I/O) so it is unit-tested
/// directly.
///
/// Per `(kind, name)`:
/// - skip members already declared directly (direct wins);
/// - coalesce members that all share one identifier;
/// - fail closed with [`ResolveErrorKind::BundleConflict`] when two bundles
///   disagree.
// The sibling resolution functions return the same `ResolveError` without
// tripping `result_large_err` because they are `async` (their signature is
// a `Future`, which the lint does not inspect). This is the one sync
// function on the path, so the suppression lives here rather than reshaping
// the shared error type.
#[allow(clippy::result_large_err)]
fn merge_bundle_members(
    direct_keys: &HashSet<(ArtifactKind, String)>,
    members: Vec<ExpandedMember>,
) -> Result<Vec<WorkItem>, ResolveError> {
    // Group by (kind, name); `BTreeMap` keeps iteration deterministic.
    let mut by_key: BTreeMap<(ArtifactKind, String), Vec<ExpandedMember>> = BTreeMap::new();
    for member in members {
        by_key
            .entry((member.kind, member.name.clone()))
            .or_default()
            .push(member);
    }

    let mut work = Vec::new();
    for ((kind, name), group) in by_key {
        // A direct declaration overrides any bundle member.
        if direct_keys.contains(&(kind, name.clone())) {
            continue;
        }
        let first = &group[0];
        let all_agree = group.iter().all(|m| m.id == first.id);
        if !all_agree {
            let mut sources: Vec<String> = group
                .iter()
                .map(|m| format!("{} from {}:{}", m.id, m.bundle_repo, m.bundle_tag))
                .collect();
            sources.sort();
            sources.dedup();
            let reference = ArtifactRef {
                kind,
                name,
                id: first.id.clone(),
            };
            return Err(ResolveError::new(
                reference,
                ResolveErrorKind::BundleConflict {
                    sources: sources.join(", "),
                },
            ));
        }
        // Record EVERY contributing bundle (sorted + deduped, so the lock
        // stays deterministic) — evicting one bundle later must keep a
        // member the others still hold.
        let mut bundles: Vec<BundleProvenance> = group
            .iter()
            .map(|m| BundleProvenance::new(m.bundle_repo.clone(), m.bundle_tag.clone()))
            .collect();
        bundles.sort_by(|a, b| (&a.repo, &a.tag).cmp(&(&b.repo, &b.tag)));
        bundles.dedup();
        work.push(WorkItem {
            reference: ArtifactRef {
                kind,
                name,
                id: first.id.clone(),
            },
            bundles,
        });
    }

    Ok(work)
}

/// Fetch and parse every declared bundle, returning its flattened members
/// plus one [`LockedBundle`] snapshot per declared binding (the lock's
/// `[[bundle]]` cache enabling offline effective-set mutations).
///
/// Each bundle is resolved fresh (its tag → digest), its manifest and
/// single members-layer fetched, and the JSON members document parsed.
/// A nested bundle member or an unparseable member id is rejected.
async fn expand_bundles(
    set: &DesiredSet,
    access: &Arc<dyn OciAccess>,
    options: &ResolveOptions,
) -> Result<(Vec<ExpandedMember>, Vec<LockedBundle>), ResolveError> {
    let mut out = Vec::new();
    let mut snapshots = Vec::new();
    for (cfg_name, bundle_id) in &set.bundles {
        let bundle_ref = ArtifactRef {
            kind: ArtifactKind::Bundle,
            name: cfg_name.clone(),
            id: bundle_id.clone(),
        };

        // Bound the WHOLE fetch chain (tag resolve + manifest + blob) by the
        // per-artifact timeout, so a hung registry on any leg cannot stall
        // the resolve indefinitely (the per-artifact-timeout contract).
        let (blob, pinned) = match tokio::time::timeout(
            options.per_artifact_timeout,
            fetch_bundle_layer(&bundle_ref, bundle_id, access, options),
        )
        .await
        {
            Ok(result) => result?,
            Err(_elapsed) => return Err(ResolveError::new(bundle_ref, ResolveErrorKind::ResolveTimeout)),
        };

        let bundle_manifest = BundleManifest::from_layer_bytes(&blob)
            .map_err(|e| ResolveError::new(bundle_ref.clone(), ResolveErrorKind::BundleInvalid(e.to_string())))?;

        if bundle_manifest.members.len() > MAX_BUNDLE_MEMBERS {
            return Err(ResolveError::new(
                bundle_ref.clone(),
                ResolveErrorKind::BundleInvalid(format!(
                    "bundle declares {} members, exceeds the limit of {MAX_BUNDLE_MEMBERS}",
                    bundle_manifest.members.len()
                )),
            ));
        }

        let bundle_repo = bundle_id.registry_repository();
        let bundle_tag = bundle_provenance_tag(bundle_id);
        // Snapshot BEFORE the member validation loop consumes the list; a
        // validation failure aborts the whole resolve, so an invalid list
        // never reaches the lock.
        snapshots.push(LockedBundle {
            name: cfg_name.clone(),
            repo: bundle_repo.clone(),
            tag: bundle_tag.clone(),
            pinned,
            members: bundle_manifest.members.clone(),
        });

        for member in bundle_manifest.members {
            if member.kind == ArtifactKind::Bundle {
                return Err(ResolveError::new(
                    bundle_ref.clone(),
                    ResolveErrorKind::BundleInvalid(format!("nested bundle member '{}' is not supported", member.name)),
                ));
            }
            // The member name is registry-controlled and flows into a
            // filesystem install path; validate it against the same charset
            // as a declared name so it cannot traverse out of the workspace
            // (CWE-22).
            SkillName::parse(&member.name).map_err(|e| {
                ResolveError::new(
                    bundle_ref.clone(),
                    ResolveErrorKind::BundleInvalid(format!("member name '{}' is invalid: {e}", member.name)),
                )
            })?;
            let id = Identifier::parse(&member.id).map_err(|_| {
                ResolveError::new(
                    bundle_ref.clone(),
                    ResolveErrorKind::BundleInvalid(format!(
                        "member '{}' has an invalid identifier '{}'",
                        member.name, member.id
                    )),
                )
            })?;
            out.push(ExpandedMember {
                kind: member.kind,
                name: member.name,
                id,
                bundle_repo: bundle_repo.clone(),
                bundle_tag: bundle_tag.clone(),
            });
        }
    }
    Ok((out, snapshots))
}

/// Fetch and integrity-check a bundle's members-layer blob: resolve the tag
/// to a digest, fetch the manifest, then the single layer — rejecting an
/// oversized layer by its descriptor before any bytes transfer, and again
/// after, to bound memory against a hostile registry (CWE-770). Returns
/// the blob together with the bundle's pinned identifier (manifest
/// digest), recorded in the lock's `[[bundle]]` snapshot.
async fn fetch_bundle_layer(
    bundle_ref: &ArtifactRef,
    bundle_id: &Identifier,
    access: &Arc<dyn OciAccess>,
    options: &ResolveOptions,
) -> Result<(Vec<u8>, PinnedIdentifier), ResolveError> {
    let digest = match retry_chain(bundle_ref, access, options).await {
        Ok(digest) => digest,
        Err(mut e) => {
            // A missing bundle tag reads clearer as BundleNotFound.
            if matches!(e.kind, ResolveErrorKind::TagNotFound) {
                e.kind = ResolveErrorKind::BundleNotFound;
            }
            return Err(e);
        }
    };

    let pinned_id = bundle_id.clone_with_digest(digest);
    #[allow(clippy::expect_used)]
    let pinned = PinnedIdentifier::try_from(pinned_id)
        .expect("clone_with_digest unconditionally sets the digest; PinnedIdentifier cannot fail here");

    let manifest = access
        .fetch_manifest(&pinned)
        .await
        .map_err(|e| ResolveError::new(bundle_ref.clone(), ResolveErrorKind::RegistryUnreachable(e)))?
        .ok_or_else(|| ResolveError::new(bundle_ref.clone(), ResolveErrorKind::BundleNotFound))?;

    let layer = manifest.single_layer().ok_or_else(|| {
        ResolveError::new(
            bundle_ref.clone(),
            ResolveErrorKind::BundleInvalid("expected exactly one members layer".to_string()),
        )
    })?;

    // Pre-reject by the (untrusted) descriptor size before transferring.
    if layer.size > BUNDLE_LAYER_SIZE_LIMIT {
        return Err(oversize_error(bundle_ref, layer.size));
    }

    let blob = access
        .fetch_blob(bundle_id, &layer.digest)
        .await
        .map_err(|e| ResolveError::new(bundle_ref.clone(), ResolveErrorKind::RegistryUnreachable(e)))?
        .ok_or_else(|| ResolveError::new(bundle_ref.clone(), ResolveErrorKind::BundleNotFound))?;

    // Re-check the actual bytes: the descriptor size is not authoritative.
    if blob.len() as u64 > BUNDLE_LAYER_SIZE_LIMIT {
        return Err(oversize_error(bundle_ref, blob.len() as u64));
    }
    Ok((blob, pinned))
}

/// The provenance tag recorded for a bundle member: the bundle's own tag,
/// or its short digest when the bundle is declared digest-only — never a
/// fabricated `latest`.
fn bundle_provenance_tag(bundle_id: &Identifier) -> String {
    match bundle_id.tag() {
        Some(tag) => tag.to_string(),
        None => bundle_id
            .digest()
            .map(|d| d.to_short_string())
            .unwrap_or_else(|| "latest".to_string()),
    }
}

fn oversize_error(bundle_ref: &ArtifactRef, size: u64) -> ResolveError {
    ResolveError::new(
        bundle_ref.clone(),
        ResolveErrorKind::BundleInvalid(format!(
            "members layer is {size} bytes, exceeds the limit of {BUNDLE_LAYER_SIZE_LIMIT} bytes"
        )),
    )
}

/// Spawn one task per work item, each wrapped in a per-artifact timeout.
/// `JoinSet` provides fail-fast: the first error aborts the rest.
async fn resolve_work(
    work: Vec<WorkItem>,
    access: &Arc<dyn OciAccess>,
    options: &ResolveOptions,
) -> Result<Vec<LockedArtifact>, ResolveError> {
    if work.is_empty() {
        return Ok(Vec::new());
    }

    let mut set: JoinSet<Result<LockedArtifact, ResolveError>> = JoinSet::new();
    for item in work {
        let access = Arc::clone(access);
        let options = options.clone();
        set.spawn(async move { resolve_one(item, access, options).await });
    }

    let mut resolved = Vec::new();
    while let Some(join) = set.join_next().await {
        // `.expect` at the join boundary documents "a resolver task
        // panicked"; the inner Result is always propagated (quality-rust.md
        // explicitly permits `.expect()` to surface a task panic at the
        // join boundary — it is not a fallible-op swallow).
        #[allow(clippy::expect_used)]
        let outcome = join.expect("resolver task panicked");
        match outcome {
            Ok(artifact) => resolved.push(artifact),
            Err(err) => {
                set.abort_all();
                return Err(err);
            }
        }
    }
    Ok(resolved)
}

/// Resolve one work item, wrapping the retry chain in a timeout and
/// stamping any bundle provenance onto the resulting lock entry.
async fn resolve_one(
    item: WorkItem,
    access: Arc<dyn OciAccess>,
    options: ResolveOptions,
) -> Result<LockedArtifact, ResolveError> {
    let WorkItem { reference, bundles } = item;
    let timeout = options.per_artifact_timeout;
    let digest = match tokio::time::timeout(timeout, retry_chain(&reference, &access, &options)).await {
        Ok(Ok(digest)) => digest,
        Ok(Err(err)) => return Err(err),
        Err(_elapsed) => {
            return Err(ResolveError::new(reference, ResolveErrorKind::ResolveTimeout));
        }
    };

    let pinned_id = reference.id.clone_with_digest(digest);
    // `clone_with_digest` unconditionally sets the digest, so the
    // conversion cannot fail. quality-rust.md permits `.expect()` for an
    // invariant proven by preceding logic; the message names it.
    #[allow(clippy::expect_used)]
    let pinned = PinnedIdentifier::try_from(pinned_id)
        .expect("clone_with_digest unconditionally sets the digest; PinnedIdentifier cannot fail here");

    Ok(LockedArtifact {
        name: reference.name,
        kind: reference.kind,
        pinned,
        bundles,
    })
}

/// Run the retry chain for one artifact.
///
/// Match on [`AccessErrorKind`] is **exhaustive** so a new access-error
/// variant is a compile error here until it is explicitly routed.
async fn retry_chain(
    reference: &ArtifactRef,
    access: &Arc<dyn OciAccess>,
    options: &ResolveOptions,
) -> Result<crate::oci::Digest, ResolveError> {
    let mut attempt: u32 = 0;
    let mut backoff = options.base_backoff;

    loop {
        match access.resolve_digest(&reference.id, Operation::Resolve).await {
            Ok(Some(digest)) => return Ok(digest),
            Ok(None) => {
                return Err(ResolveError::new(reference.clone(), ResolveErrorKind::TagNotFound));
            }
            Err(err) => {
                // Decide retry vs terminal by the error kind. The match is
                // exhaustive: adding an `AccessErrorKind` variant forces a
                // routing decision here.
                let retryable = match &err.kind {
                    AccessErrorKind::Registry(_) | AccessErrorKind::Io { .. } => true,
                    AccessErrorKind::Authentication(_) => {
                        return Err(ResolveError::new(reference.clone(), ResolveErrorKind::AuthFailure(err)));
                    }
                    AccessErrorKind::OfflineMiss
                    | AccessErrorKind::ManifestNotFound
                    | AccessErrorKind::BlobNotFound
                    | AccessErrorKind::InvalidManifest(_)
                    | AccessErrorKind::DigestMismatch { .. } => {
                        return Err(ResolveError::new(
                            reference.clone(),
                            ResolveErrorKind::RegistryUnreachable(err),
                        ));
                    }
                };

                if retryable && attempt < options.max_retries {
                    tokio::time::sleep(backoff).await;
                    backoff = backoff.saturating_mul(2);
                    attempt += 1;
                    continue;
                }
                // Retry budget exhausted (or zero retries configured).
                return Err(ResolveError::new(
                    reference.clone(),
                    ResolveErrorKind::RegistryUnreachable(err),
                ));
            }
        }
    }
}

/// Deterministic ordering for lock entries: `(kind, name)`.
fn sort_locked(items: &mut [LockedArtifact]) {
    items.sort_by(|a, b| (a.kind, a.name.as_str()).cmp(&(b.kind, b.name.as_str())));
}

/// Build a [`GrimoireLock`] from resolved entries, the source set, and the
/// declared bundles' cached expansion snapshots.
fn build_lock(resolved: Vec<LockedArtifact>, set: &DesiredSet, bundles: Vec<LockedBundle>) -> GrimoireLock {
    let mut skills = Vec::new();
    let mut rules = Vec::new();
    let mut agents = Vec::new();
    for artifact in resolved {
        match artifact.kind {
            ArtifactKind::Skill => skills.push(artifact),
            ArtifactKind::Rule => rules.push(artifact),
            ArtifactKind::Agent => agents.push(artifact),
            // Bundles never produce lock entries — only their members do.
            ArtifactKind::Bundle => {}
        }
    }

    GrimoireLock {
        metadata: LockMetadata {
            lock_version: LockVersion::V1,
            declaration_hash_version: DECLARATION_HASH_VERSION,
            declaration_hash: set.declaration_hash_cached().to_string(),
            generated_by: LockMetadata::generated_by_current(),
            // Stamp "now"; `lock_io::save` preserves it on a content-equal
            // re-lock at write time.
            generated_at: now_rfc3339(),
        },
        skills,
        rules,
        agents,
        bundles,
    }
}

/// A best-effort artifact reference for the stale-lock error: the first
/// requested name if it is declared, else a synthetic placeholder.
fn stale_reference(set: &DesiredSet, names: &[String]) -> ArtifactRef {
    if let Some(first) = names.first() {
        let work = collect_work(set);
        if let Some(found) = work.into_iter().find(|r| &r.name == first) {
            return found;
        }
        return ArtifactRef {
            kind: ArtifactKind::Skill,
            name: first.clone(),
            id: Identifier::new_registry(first.clone(), "invalid.localhost"),
        };
    }
    ArtifactRef {
        kind: ArtifactKind::Skill,
        name: "<partial>".to_string(),
        id: Identifier::new_registry("partial", "invalid.localhost"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeMap, VecDeque};
    use std::sync::Mutex;
    use std::time::Duration;

    use crate::oci::access::error::AccessError;
    use crate::oci::manifest::OciManifest;
    use crate::oci::{Algorithm, Digest};

    // ── Scripted mock OciAccess ──────────────────────────────────────────

    /// One scripted `resolve_digest` outcome.
    enum Scripted {
        Ok(Option<Digest>),
        Err(AccessErrorKind),
        /// Sleep this long, then yield the digest (timeout exercise).
        Hang(Duration, Digest),
    }

    /// Deterministic mock with a FIFO script and a shared call counter.
    /// Cloning shares the script + counter via `Arc` so spawned tasks and
    /// the test observe the same state. No network.
    #[derive(Clone)]
    struct MockAccess {
        script: Arc<Mutex<VecDeque<Scripted>>>,
        calls: Arc<Mutex<usize>>,
    }

    impl MockAccess {
        fn new(script: Vec<Scripted>) -> Self {
            Self {
                script: Arc::new(Mutex::new(script.into_iter().collect())),
                calls: Arc::new(Mutex::new(0)),
            }
        }

        fn calls(&self) -> usize {
            *self.calls.lock().unwrap()
        }
    }

    #[async_trait::async_trait]
    impl OciAccess for MockAccess {
        async fn resolve_digest(&self, id: &Identifier, _op: Operation) -> Result<Option<Digest>, AccessError> {
            let next = self.script.lock().unwrap().pop_front();
            // Sleep (if any) is computed before bumping the counter so a
            // timeout test never records the hung call as completed.
            if let Some(Scripted::Hang(d, digest)) = &next {
                let d = *d;
                let digest = digest.clone();
                tokio::time::sleep(d).await;
                *self.calls.lock().unwrap() += 1;
                return Ok(Some(digest));
            }
            *self.calls.lock().unwrap() += 1;
            match next {
                Some(Scripted::Ok(v)) => Ok(v),
                Some(Scripted::Err(kind)) => Err(AccessError::with_identifier(id.clone(), kind)),
                Some(Scripted::Hang(..)) => unreachable!("handled above"),
                None => panic!("MockAccess script exhausted — unexpected extra resolve_digest call"),
            }
        }

        async fn fetch_manifest(&self, _id: &PinnedIdentifier) -> Result<Option<OciManifest>, AccessError> {
            Ok(None)
        }
        async fn fetch_blob(&self, _repo: &Identifier, _digest: &Digest) -> Result<Option<Vec<u8>>, AccessError> {
            Ok(None)
        }
        async fn list_tags(&self, _id: &Identifier) -> Result<Option<Vec<String>>, AccessError> {
            Ok(None)
        }
        async fn list_catalog(&self, _registry: &str) -> Result<Vec<String>, AccessError> {
            Ok(Vec::new())
        }
        async fn push_blob(&self, _repo: &Identifier, bytes: &[u8]) -> Result<Digest, AccessError> {
            Ok(Algorithm::Sha256.hash(bytes))
        }
        async fn push_manifest(&self, _repo: &Identifier, _m: &OciManifest) -> Result<Digest, AccessError> {
            Ok(Algorithm::Sha256.hash(b"manifest"))
        }
        async fn put_tag(&self, _repo: &Identifier, _tag: &str, _d: &Digest) -> Result<(), AccessError> {
            Ok(())
        }
    }

    fn digest() -> Digest {
        Algorithm::Sha256.hash(b"resolved")
    }

    fn single_skill_set() -> DesiredSet {
        let mut skills = BTreeMap::new();
        skills.insert(
            "code-review".to_string(),
            Identifier::parse("ghcr.io/acme/code-review:stable").unwrap(),
        );
        DesiredSet::from_parts(skills, BTreeMap::new())
    }

    fn fast_options() -> ResolveOptions {
        ResolveOptions {
            per_artifact_timeout: Duration::from_secs(2),
            max_retries: 3,
            base_backoff: Duration::from_millis(1),
        }
    }

    fn arc(mock: MockAccess) -> Arc<dyn OciAccess> {
        Arc::new(mock)
    }

    #[tokio::test]
    async fn happy_path_single_skill() {
        let set = single_skill_set();
        let mock = MockAccess::new(vec![Scripted::Ok(Some(digest()))]);
        let access = arc(mock.clone());
        let lock = resolve_lock(&set, &access, ConfigScope::Project, &fast_options())
            .await
            .expect("happy path resolves");
        assert_eq!(lock.skills.len(), 1);
        assert_eq!(lock.skills[0].name, "code-review");
        assert_eq!(lock.skills[0].pinned.digest(), digest());
        assert_eq!(mock.calls(), 1);
    }

    #[tokio::test]
    async fn retry_then_success() {
        let set = single_skill_set();
        let mock = MockAccess::new(vec![
            Scripted::Err(AccessErrorKind::Registry(Box::new(std::io::Error::other("503")))),
            Scripted::Ok(Some(digest())),
        ]);
        let access = arc(mock.clone());
        let lock = resolve_lock(&set, &access, ConfigScope::Project, &fast_options())
            .await
            .expect("recovers on second attempt");
        assert_eq!(lock.skills[0].pinned.digest(), digest());
        assert_eq!(mock.calls(), 2);
    }

    #[tokio::test]
    async fn retries_exhausted_is_registry_unreachable() {
        let set = single_skill_set();
        let script = (0..4)
            .map(|_| Scripted::Err(AccessErrorKind::Registry(Box::new(std::io::Error::other("down")))))
            .collect();
        let mock = MockAccess::new(script);
        let access = arc(mock.clone());
        let err = resolve_lock(&set, &access, ConfigScope::Project, &fast_options())
            .await
            .expect_err("exhaustion must surface");
        assert!(matches!(err.kind, ResolveErrorKind::RegistryUnreachable(_)));
        assert_eq!(mock.calls(), 4, "1 initial + 3 retries");
    }

    #[tokio::test]
    async fn auth_failure_no_retry() {
        let set = single_skill_set();
        let mock = MockAccess::new(vec![Scripted::Err(AccessErrorKind::Authentication(Box::new(
            std::io::Error::other("401"),
        )))]);
        let access = arc(mock.clone());
        let err = resolve_lock(&set, &access, ConfigScope::Project, &fast_options())
            .await
            .expect_err("auth must surface");
        assert!(matches!(err.kind, ResolveErrorKind::AuthFailure(_)));
        assert_eq!(mock.calls(), 1, "auth must not retry");
    }

    #[tokio::test]
    async fn none_is_tag_not_found() {
        let set = single_skill_set();
        let mock = MockAccess::new(vec![Scripted::Ok(None)]);
        let access = arc(mock.clone());
        let err = resolve_lock(&set, &access, ConfigScope::Project, &fast_options())
            .await
            .expect_err("None must surface as an error");
        assert!(matches!(err.kind, ResolveErrorKind::TagNotFound));
        assert_eq!(mock.calls(), 1, "not-found must not retry");
    }

    #[tokio::test]
    async fn timeout_is_resolve_timeout() {
        let set = single_skill_set();
        let mock = MockAccess::new(vec![Scripted::Hang(Duration::from_millis(500), digest())]);
        let access = arc(mock.clone());
        let options = ResolveOptions {
            per_artifact_timeout: Duration::from_millis(40),
            max_retries: 0,
            base_backoff: Duration::from_millis(1),
        };
        let start = std::time::Instant::now();
        let err = resolve_lock(&set, &access, ConfigScope::Project, &options)
            .await
            .expect_err("hang must time out");
        assert!(start.elapsed() < Duration::from_millis(400), "timeout fired early");
        assert!(matches!(err.kind, ResolveErrorKind::ResolveTimeout));
    }

    #[tokio::test]
    async fn first_failure_aborts_siblings_transactional() {
        // Two skills, two rules. One scripted failure plus successes; the
        // failure must abort the rest and yield no lock.
        let mut skills = BTreeMap::new();
        skills.insert("a".to_string(), Identifier::parse("ghcr.io/acme/a:1").unwrap());
        skills.insert("b".to_string(), Identifier::parse("ghcr.io/acme/b:1").unwrap());
        let mut rules = BTreeMap::new();
        rules.insert("c".to_string(), Identifier::parse("ghcr.io/acme/c:1").unwrap());
        let set = DesiredSet::from_parts(skills, rules);

        // Every scripted entry is a hard auth failure; whichever task pops
        // first fails and the result must be an error (no lock).
        let mock = MockAccess::new(vec![
            Scripted::Err(AccessErrorKind::Authentication(Box::new(std::io::Error::other("401")))),
            Scripted::Ok(Some(digest())),
            Scripted::Ok(Some(digest())),
        ]);
        let access = arc(mock.clone());
        let err = resolve_lock(&set, &access, ConfigScope::Project, &fast_options())
            .await
            .expect_err("a failing sibling must fail the whole resolve");
        assert!(matches!(err.kind, ResolveErrorKind::AuthFailure(_)));
    }

    #[tokio::test]
    async fn output_order_is_kind_then_name() {
        let mut skills = BTreeMap::new();
        skills.insert("zeta".to_string(), Identifier::parse("ghcr.io/acme/zeta:1").unwrap());
        skills.insert("alpha".to_string(), Identifier::parse("ghcr.io/acme/alpha:1").unwrap());
        let mut rules = BTreeMap::new();
        rules.insert("rho".to_string(), Identifier::parse("ghcr.io/acme/rho:1").unwrap());
        let set = DesiredSet::from_parts(skills, rules);
        let mock = MockAccess::new(vec![
            Scripted::Ok(Some(digest())),
            Scripted::Ok(Some(digest())),
            Scripted::Ok(Some(digest())),
        ]);
        let access = arc(mock);
        let lock = resolve_lock(&set, &access, ConfigScope::Project, &fast_options())
            .await
            .expect("resolves");
        let skill_names: Vec<&str> = lock.skills.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(skill_names, vec!["alpha", "zeta"], "skills sorted by name");
        assert_eq!(lock.rules.len(), 1);
        assert_eq!(lock.rules[0].name, "rho");
    }

    #[tokio::test]
    async fn agents_resolve_into_the_agent_list() {
        let mut agents = BTreeMap::new();
        agents.insert(
            "code-reviewer".to_string(),
            Identifier::parse("ghcr.io/acme/code-reviewer:1").unwrap(),
        );
        let set = DesiredSet::from_maps(BTreeMap::new(), BTreeMap::new(), agents, BTreeMap::new());
        let mock = MockAccess::new(vec![Scripted::Ok(Some(digest()))]);
        let access = arc(mock.clone());
        let lock = resolve_lock(&set, &access, ConfigScope::Project, &fast_options())
            .await
            .expect("agent resolves");
        assert!(lock.skills.is_empty());
        assert!(lock.rules.is_empty());
        assert_eq!(lock.agents.len(), 1);
        assert_eq!(lock.agents[0].name, "code-reviewer");
        assert_eq!(lock.agents[0].kind, ArtifactKind::Agent);
        assert_eq!(lock.agents[0].pinned.digest(), digest());
    }

    // ── Partial ──────────────────────────────────────────────────────────

    #[tokio::test]
    async fn partial_stale_hash_gate_fires_before_any_access() {
        let set = single_skill_set();
        let previous = GrimoireLock {
            metadata: LockMetadata {
                lock_version: LockVersion::V1,
                declaration_hash_version: DECLARATION_HASH_VERSION,
                declaration_hash: "sha256:0000000000000000000000000000000000000000000000000000000000000000".to_string(),
                generated_by: LockMetadata::generated_by_current(),
                generated_at: "2026-01-01T00:00:00Z".to_string(),
            },
            skills: vec![],
            rules: vec![],
            agents: vec![],
            bundles: vec![],
        };
        let mock = MockAccess::new(vec![Scripted::Ok(Some(digest()))]);
        let access = arc(mock.clone());
        let err = resolve_lock_partial(
            &set,
            &previous,
            &access,
            &["code-review".to_string()],
            ConfigScope::Project,
            &fast_options(),
        )
        .await
        .expect_err("stale predecessor must be rejected");
        assert!(matches!(err.kind, ResolveErrorKind::StaleLock { .. }));
        assert_eq!(mock.calls(), 0, "gate must fire before any access call");
    }

    #[tokio::test]
    async fn partial_carries_forward_unselected_entries() {
        // Declare two skills; the lock already has both pinned. Re-resolve
        // only "a"; "b" must be carried forward verbatim.
        let mut skills = BTreeMap::new();
        skills.insert("a".to_string(), Identifier::parse("ghcr.io/acme/a:1").unwrap());
        skills.insert("b".to_string(), Identifier::parse("ghcr.io/acme/b:1").unwrap());
        let set = DesiredSet::from_parts(skills, BTreeMap::new());
        let current_hash = set.declaration_hash_cached().to_string();

        let old_b_digest = Algorithm::Sha256.hash(b"old-b");
        let prev_b_id = Identifier::parse("ghcr.io/acme/b:1")
            .unwrap()
            .clone_with_digest(old_b_digest.clone());
        let prev_b = LockedArtifact::direct(
            "b".to_string(),
            ArtifactKind::Skill,
            PinnedIdentifier::try_from(prev_b_id).unwrap(),
        );
        let prev_a_id = Identifier::parse("ghcr.io/acme/a:1")
            .unwrap()
            .clone_with_digest(Algorithm::Sha256.hash(b"old-a"));
        let prev_a = LockedArtifact::direct(
            "a".to_string(),
            ArtifactKind::Skill,
            PinnedIdentifier::try_from(prev_a_id).unwrap(),
        );
        let previous = GrimoireLock {
            metadata: LockMetadata {
                lock_version: LockVersion::V1,
                declaration_hash_version: DECLARATION_HASH_VERSION,
                declaration_hash: current_hash,
                generated_by: LockMetadata::generated_by_current(),
                generated_at: "2026-01-01T00:00:00Z".to_string(),
            },
            skills: vec![prev_a, prev_b],
            rules: vec![],
            agents: vec![],
            bundles: vec![],
        };

        let new_a_digest = Algorithm::Sha256.hash(b"new-a");
        let mock = MockAccess::new(vec![Scripted::Ok(Some(new_a_digest.clone()))]);
        let access = arc(mock.clone());
        let lock = resolve_lock_partial(
            &set,
            &previous,
            &access,
            &["a".to_string()],
            ConfigScope::Project,
            &fast_options(),
        )
        .await
        .expect("partial resolve succeeds");

        assert_eq!(mock.calls(), 1, "only the named subset is re-resolved");
        let a = lock.skills.iter().find(|s| s.name == "a").expect("a present");
        let b = lock.skills.iter().find(|s| s.name == "b").expect("b carried forward");
        assert_eq!(a.pinned.digest(), new_a_digest, "a re-resolved to the new digest");
        assert_eq!(b.pinned.digest(), old_b_digest, "b carried forward verbatim");
    }

    #[tokio::test]
    async fn partial_rejects_undeclared_name() {
        let set = single_skill_set();
        let current = set.declaration_hash_cached().to_string();
        let previous = GrimoireLock {
            metadata: LockMetadata {
                lock_version: LockVersion::V1,
                declaration_hash_version: DECLARATION_HASH_VERSION,
                declaration_hash: current,
                generated_by: LockMetadata::generated_by_current(),
                generated_at: "2026-01-01T00:00:00Z".to_string(),
            },
            skills: vec![],
            rules: vec![],
            agents: vec![],
            bundles: vec![],
        };
        let mock = MockAccess::new(vec![]);
        let access = arc(mock);
        let err = resolve_lock_partial(
            &set,
            &previous,
            &access,
            &["does-not-exist".to_string()],
            ConfigScope::Project,
            &fast_options(),
        )
        .await
        .expect_err("undeclared name must be rejected");
        assert!(matches!(err.kind, ResolveErrorKind::TagNotFound));
    }

    // ── Bundle conflict engine (pure merge) ──────────────────────────────

    fn member(kind: ArtifactKind, name: &str, id: &str, bundle: &str, tag: &str) -> ExpandedMember {
        ExpandedMember {
            kind,
            name: name.to_string(),
            id: Identifier::parse(id).unwrap(),
            bundle_repo: bundle.to_string(),
            bundle_tag: tag.to_string(),
        }
    }

    fn no_direct() -> HashSet<(ArtifactKind, String)> {
        HashSet::new()
    }

    #[test]
    fn merge_single_member_becomes_work_with_provenance() {
        let members = vec![member(
            ArtifactKind::Skill,
            "code-review",
            "ghcr.io/acme/code-review:stable",
            "ghcr.io/acme/python-stack",
            "1.0.0",
        )];
        let work = merge_bundle_members(&no_direct(), members).expect("merge ok");
        assert_eq!(work.len(), 1);
        assert_eq!(work[0].reference.name, "code-review");
        assert_eq!(
            work[0].bundles,
            vec![BundleProvenance::new("ghcr.io/acme/python-stack", "1.0.0")]
        );
    }

    #[test]
    fn merge_direct_declaration_wins_over_bundle() {
        let mut direct = HashSet::new();
        direct.insert((ArtifactKind::Skill, "code-review".to_string()));
        let members = vec![member(
            ArtifactKind::Skill,
            "code-review",
            "ghcr.io/acme/code-review:other",
            "ghcr.io/acme/stack",
            "1",
        )];
        let work = merge_bundle_members(&direct, members).expect("merge ok");
        assert!(work.is_empty(), "the direct declaration overrides the bundle member");
    }

    #[test]
    fn merge_agreeing_bundles_coalesce() {
        let members = vec![
            member(
                ArtifactKind::Skill,
                "code-review",
                "ghcr.io/acme/code-review:stable",
                "ghcr.io/acme/stack-a",
                "1",
            ),
            member(
                ArtifactKind::Skill,
                "code-review",
                "ghcr.io/acme/code-review:stable",
                "ghcr.io/acme/stack-b",
                "2",
            ),
        ];
        let work = merge_bundle_members(&no_direct(), members).expect("merge ok");
        assert_eq!(work.len(), 1, "identical members coalesce to one entry");
        assert_eq!(
            work[0].bundles,
            vec![
                BundleProvenance::new("ghcr.io/acme/stack-a", "1"),
                BundleProvenance::new("ghcr.io/acme/stack-b", "2"),
            ],
            "every contributing bundle is recorded, sorted"
        );
    }

    #[test]
    fn merge_duplicate_provenance_dedupes() {
        // The same bundle listed twice (e.g. two bindings at one repo+tag
        // both expanded) must not double-record its provenance.
        let members = vec![
            member(
                ArtifactKind::Skill,
                "code-review",
                "ghcr.io/acme/code-review:stable",
                "ghcr.io/acme/stack",
                "1",
            ),
            member(
                ArtifactKind::Skill,
                "code-review",
                "ghcr.io/acme/code-review:stable",
                "ghcr.io/acme/stack",
                "1",
            ),
        ];
        let work = merge_bundle_members(&no_direct(), members).expect("merge ok");
        assert_eq!(work.len(), 1);
        assert_eq!(work[0].bundles, vec![BundleProvenance::new("ghcr.io/acme/stack", "1")]);
    }

    #[test]
    fn merge_disagreeing_bundles_fail_closed() {
        let members = vec![
            member(
                ArtifactKind::Skill,
                "code-review",
                "ghcr.io/acme/code-review:stable",
                "ghcr.io/acme/stack-a",
                "1",
            ),
            member(
                ArtifactKind::Skill,
                "code-review",
                "ghcr.io/acme/code-review:1.4",
                "ghcr.io/acme/stack-b",
                "2",
            ),
        ];
        let err = merge_bundle_members(&no_direct(), members).expect_err("disagreement must fail closed");
        assert!(matches!(err.kind, ResolveErrorKind::BundleConflict { .. }));
        assert_eq!(err.reference.name, "code-review");
    }

    #[test]
    fn merge_agent_member_becomes_work_with_provenance() {
        // Bundle expansion is kind-generic: an agent member flows through
        // the same merge with its provenance stamped.
        let members = vec![member(
            ArtifactKind::Agent,
            "code-reviewer",
            "ghcr.io/acme/code-reviewer:1",
            "ghcr.io/acme/stack",
            "1.0.0",
        )];
        let work = merge_bundle_members(&no_direct(), members).expect("merge ok");
        assert_eq!(work.len(), 1);
        assert_eq!(work[0].reference.kind, ArtifactKind::Agent);
        assert_eq!(
            work[0].bundles,
            vec![BundleProvenance::new("ghcr.io/acme/stack", "1.0.0")]
        );
    }

    #[test]
    fn merge_distinguishes_skill_and_rule_with_same_name() {
        let members = vec![
            member(ArtifactKind::Skill, "x", "ghcr.io/acme/x:1", "ghcr.io/acme/s", "1"),
            member(ArtifactKind::Rule, "x", "ghcr.io/acme/x-rule:1", "ghcr.io/acme/s", "1"),
        ];
        let work = merge_bundle_members(&no_direct(), members).expect("merge ok");
        assert_eq!(work.len(), 2, "skill x and rule x are distinct keys");
    }

    #[test]
    fn provenance_tag_uses_digest_when_bundle_is_digest_pinned() {
        let tagged = Identifier::parse("ghcr.io/acme/stack:1.0").unwrap();
        assert_eq!(bundle_provenance_tag(&tagged), "1.0");

        let hex = "a".repeat(64);
        let pinned = Identifier::parse(&format!("ghcr.io/acme/stack@sha256:{hex}")).unwrap();
        let tag = bundle_provenance_tag(&pinned);
        assert!(
            tag.starts_with("sha256:"),
            "digest-pinned bundle records its digest, got {tag}"
        );
        assert_ne!(tag, "latest", "must not fabricate a `latest` tag");
    }
}
