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

use std::sync::Arc;

use tokio::task::JoinSet;

use super::resolve_error::{ResolveError, ResolveErrorKind};
use super::resolve_options::ResolveOptions;
use crate::config::declaration::DesiredSet;
use crate::config::hash::DECLARATION_HASH_VERSION;
use crate::config::scope::ConfigScope;
use crate::lock::grimoire_lock::{GrimoireLock, LockMetadata};
use crate::lock::lock_io::now_rfc3339;
use crate::lock::lock_version::LockVersion;
use crate::lock::locked_artifact::LockedArtifact;
use crate::oci::access::error::AccessErrorKind;
use crate::oci::access::{OciAccess, Operation};
use crate::oci::reference::ArtifactRef;
use crate::oci::{ArtifactKind, Identifier, PinnedIdentifier};

/// Resolve every declared skill and rule and assemble a fresh lock.
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
    let work = collect_work(set);
    let mut resolved = resolve_work(work, access, options).await?;
    sort_locked(&mut resolved);
    Ok(build_lock(resolved, set))
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

    let all_work = collect_work(set);

    // Validate every requested name is declared.
    for name in names {
        if !all_work.iter().any(|r| &r.name == name) {
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
    let selected: Vec<ArtifactRef> = all_work
        .into_iter()
        .filter(|r| names.iter().any(|n| n == &r.name))
        .collect();
    let resolved = resolve_work(selected, access, options).await?;

    // Carry forward every previous entry not in the re-resolved set,
    // keyed by (kind, name); then merge and sort.
    let new_keys: std::collections::HashSet<(ArtifactKind, String)> =
        resolved.iter().map(|a| (a.kind, a.name.clone())).collect();

    let mut merged: Vec<LockedArtifact> = previous
        .skills
        .iter()
        .chain(previous.rules.iter())
        .filter(|a| !new_keys.contains(&(a.kind, a.name.clone())))
        .cloned()
        .collect();
    merged.extend(resolved);
    sort_locked(&mut merged);

    Ok(build_lock(merged, set))
}

/// Collect the artifacts to resolve in deterministic `BTreeMap` order:
/// every declared skill (kind Skill), then every declared rule (kind
/// Rule). The final `(kind, name)` sort happens after resolution.
fn collect_work(set: &DesiredSet) -> Vec<ArtifactRef> {
    let mut work = Vec::with_capacity(set.skills.len() + set.rules.len());
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
    work
}

/// Spawn one task per artifact, each wrapped in a per-artifact timeout.
/// `JoinSet` provides fail-fast: the first error aborts the rest.
async fn resolve_work(
    work: Vec<ArtifactRef>,
    access: &Arc<dyn OciAccess>,
    options: &ResolveOptions,
) -> Result<Vec<LockedArtifact>, ResolveError> {
    if work.is_empty() {
        return Ok(Vec::new());
    }

    let mut set: JoinSet<Result<LockedArtifact, ResolveError>> = JoinSet::new();
    for reference in work {
        let access = Arc::clone(access);
        let options = options.clone();
        set.spawn(async move { resolve_one(reference, access, options).await });
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

/// Resolve one artifact, wrapping the retry chain in a timeout.
async fn resolve_one(
    reference: ArtifactRef,
    access: Arc<dyn OciAccess>,
    options: ResolveOptions,
) -> Result<LockedArtifact, ResolveError> {
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

/// Build a [`GrimoireLock`] from resolved entries and the source set.
fn build_lock(resolved: Vec<LockedArtifact>, set: &DesiredSet) -> GrimoireLock {
    let (skills, rules): (Vec<_>, Vec<_>) = resolved.into_iter().partition(|a| a.kind == ArtifactKind::Skill);

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
        let prev_b = LockedArtifact {
            name: "b".to_string(),
            kind: ArtifactKind::Skill,
            pinned: PinnedIdentifier::try_from(prev_b_id).unwrap(),
        };
        let prev_a_id = Identifier::parse("ghcr.io/acme/a:1")
            .unwrap()
            .clone_with_digest(Algorithm::Sha256.hash(b"old-a"));
        let prev_a = LockedArtifact {
            name: "a".to_string(),
            kind: ArtifactKind::Skill,
            pinned: PinnedIdentifier::try_from(prev_a_id).unwrap(),
        };
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
}
