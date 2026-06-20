// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! Background fetch tasks for bundle member lists.
//!
//! Modelled on [`super::update_check::UpdateChecker`] /
//! `spawn_row_checks`: a bounded [`Semaphore`], bounded [`mpsc`] channel,
//! [`JoinSet`], generation stamp, and RAII in-flight dedup slot.
//!
//! The key differences from the update-checker:
//!
//! - Fetches are keyed by `(scope_label, bundle_repo)` rather than bare
//!   `repo`.
//! - The concurrency cap is [`BUNDLE_MEMBER_CONCURRENCY`] (4) instead of
//!   `ROW_CHECK_CONCURRENCY` (8) — member fetches are heavier (manifest +
//!   blob, not just a tag digest resolve).
//! - On `Ready`, the result carries the raw `Vec<BundleMember>` from the
//!   registry; the TUI app translates them into `Vec<MemberNode>` (fail-
//!   soft, dropping unparseable members with a log).
//!
//! Stub bodies are `unimplemented!()` — P2 tests compile against these
//! signatures; P3 fills the bodies.

use std::sync::{Arc, Mutex};

use tokio::sync::{Semaphore, mpsc};
use tokio::task::JoinSet;

use crate::oci::bundle::BundleMember;

/// Maximum concurrent bundle-member fetch tasks.
///
/// Member fetches are heavier than the per-row tag resolves in
/// [`super::update_check`] (manifest fetch + blob fetch + parse), so the
/// concurrency cap is lower: 4 vs 8.
///
/// Named `BUNDLE_MEMBER_CONCURRENCY` per the plan (D3).
pub const BUNDLE_MEMBER_CONCURRENCY: usize = 4;

/// Capacity of the bundle-member results channel. Matches the pattern in
/// `update_check.rs` (`RESULT_CHANNEL_CAPACITY = 256`): bounded so a slow
/// UI tick cannot allow results to pile up unboundedly. A full channel
/// means the UI is behind; the sender drops the stale result (the next
/// drain will process the backlog).
const BUNDLE_MEMBER_CHANNEL_CAPACITY: usize = 256;

/// A result flowing from a background bundle-member fetch task back into
/// the event loop, drained in `app::drain_checks`.
///
/// Every variant carries:
/// - `bundle_repo`: the `registry/repository` reference identifying which
///   bundle's members arrived.
/// - `generation`: the checker's generation when the fetch was spawned.
///   A scope toggle or catalog refresh bumps the generation; stale results
///   are discarded in the drain loop without touching the cache.
///
/// Closed internal enum — matches stay total, no `#[non_exhaustive]`.
#[derive(Debug)]
pub enum BundleMembersMsg {
    /// The fetch succeeded. `members` is the raw, pre-validation list from
    /// the registry (or the lock snapshot). The app translates each entry
    /// into a `MemberNode` fail-soft (dropping unparseable ones).
    Ready {
        /// `registry/repository` reference of the bundle.
        bundle_repo: String,
        /// Raw members from the registry or lock snapshot.
        members: Vec<BundleMember>,
        /// Generation stamp at spawn time.
        generation: u64,
    },
    /// The fetch failed. The app caches `Failed(reason)` — no auto-retry.
    Failed {
        /// `registry/repository` reference of the bundle.
        bundle_repo: String,
        /// Human-readable reason string (will be sanitized before display).
        reason: String,
        /// Generation stamp at spawn time.
        generation: u64,
    },
}

/// Removes a `(scope_label, bundle_repo, generation)` slot from the shared
/// in-flight set on drop — RAII analog of `InFlightGuard` in
/// `update_check.rs`. Frees the slot however the task ends (clean send,
/// dropped send, resolve error, panic).
struct BundleInFlightGuard {
    set: Arc<Mutex<std::collections::HashSet<(String, String, u64)>>>,
    scope_label: String,
    bundle_repo: String,
    generation: u64,
}

impl Drop for BundleInFlightGuard {
    fn drop(&mut self) {
        let mut guard = self.set.lock().unwrap_or_else(|p| p.into_inner());
        guard.remove(&(self.scope_label.clone(), self.bundle_repo.clone(), self.generation));
    }
}

/// Background spawn helper for bundle-member fetches.
///
/// Shaped like [`super::update_check::UpdateChecker`]: owns a `JoinSet`,
/// a `Semaphore`, a bounded `mpsc` sender, a generation counter, and an
/// in-flight dedup set.
///
/// Only the `Sender` half of the channel is held here; the `Receiver` is
/// owned by `app::run` and drained each tick.
pub struct BundleMemberChecker {
    tx: mpsc::Sender<BundleMembersMsg>,
    permits: Arc<Semaphore>,
    in_flight: Arc<Mutex<std::collections::HashSet<(String, String, u64)>>>,
    tasks: JoinSet<()>,
    generation: u64,
    // OCI access seam — stored so P3 can call fetch_bundle_members.
    access: Arc<dyn crate::oci::access::OciAccess>,
}

impl BundleMemberChecker {
    /// Create a new checker, returning the checker and the `Receiver` end
    /// of the results channel.
    pub fn new(access: Arc<dyn crate::oci::access::OciAccess>) -> (Self, mpsc::Receiver<BundleMembersMsg>) {
        let (tx, rx) = mpsc::channel(BUNDLE_MEMBER_CHANNEL_CAPACITY);
        let checker = Self {
            tx,
            permits: Arc::new(Semaphore::new(BUNDLE_MEMBER_CONCURRENCY)),
            in_flight: Arc::new(Mutex::new(std::collections::HashSet::new())),
            tasks: JoinSet::new(),
            generation: 0,
            access,
        };
        (checker, rx)
    }

    /// The current generation stamp.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Bump the generation (called on scope toggle or catalog refresh), so
    /// any in-flight fetch is discarded on drain as stale.
    pub fn bump_generation(&mut self) {
        self.generation = self.generation.wrapping_add(1);
    }

    /// Spawn a background fetch for `bundle_repo` under `scope_label`, if
    /// no fetch for this `(scope_label, bundle_repo, generation)` triple is
    /// already in flight.
    ///
    /// Modelled on `UpdateChecker::spawn_row_checks`.
    pub fn spawn_fetch(
        &mut self,
        scope_label: String,
        bundle_repo: String,
        options: &crate::resolve::resolve_options::ResolveOptions,
    ) {
        let generation = self.generation;

        // Test-and-set the dedup slot; hold the lock only for the duration
        // of the check — never across an `.await`.
        {
            let mut guard = self.in_flight.lock().unwrap_or_else(|p| p.into_inner());
            let slot = (scope_label.clone(), bundle_repo.clone(), generation);
            if !guard.insert(slot) {
                // Already in-flight for this (scope, repo, generation) triple — skip.
                return;
            }
        }

        let tx = self.tx.clone();
        let access = Arc::clone(&self.access);
        let permits = Arc::clone(&self.permits);
        let in_flight = Arc::clone(&self.in_flight);
        // Clone options to move into the async task (no borrowing across await).
        let options = options.clone();

        self.tasks.spawn(async move {
            // RAII guard: clears the in-flight slot however the task ends
            // (clean send, dropped send, parse error, panic).
            let _guard = BundleInFlightGuard {
                set: Arc::clone(&in_flight),
                scope_label: scope_label.clone(),
                bundle_repo: bundle_repo.clone(),
                generation,
            };

            // Acquire a semaphore permit for the lifetime of the registry call.
            // `acquire_owned` only fails if the semaphore is closed, which never
            // happens here (we hold the `Arc`).
            let _permit = match permits.acquire_owned().await {
                Ok(p) => p,
                Err(_) => return,
            };

            // Parse the bundle_repo string into an OCI Identifier. A
            // well-formed bundle_repo always contains an explicit registry
            // (the TUI rows carry fully-qualified refs), so parse failures
            // are treated as a fetch error rather than a panic.
            let id = match crate::oci::Identifier::parse(&bundle_repo) {
                Ok(id) => id,
                Err(e) => {
                    let _ = tx.try_send(BundleMembersMsg::Failed {
                        bundle_repo: bundle_repo.clone(),
                        reason: format!("invalid bundle reference: {e}"),
                        generation,
                    });
                    return;
                }
            };

            // Build an ArtifactRef from the parsed identifier.
            let bundle_ref = crate::oci::reference::ArtifactRef {
                kind: crate::oci::ArtifactKind::Bundle,
                name: id.repository().to_string(),
                id: id.clone(),
            };

            let msg = match crate::resolve::resolver::fetch_bundle_members(&bundle_ref, &id, &access, &options).await {
                Ok((members, _pinned)) => BundleMembersMsg::Ready {
                    bundle_repo: bundle_repo.clone(),
                    members,
                    generation,
                },
                Err(e) => BundleMembersMsg::Failed {
                    bundle_repo: bundle_repo.clone(),
                    // Use full error chain (format!("{e:#}")) so the root cause
                    // is preserved in the cached reason string (quality-rust-errors).
                    reason: format!("{e:#}"),
                    generation,
                },
            };

            // If the channel is full, replace the result with a Failed so the
            // UI cache does not remain stuck in Loading forever. The drain loop
            // checks generation freshness before writing Failed to the cache,
            // so a stale drop is still a no-op. On a next Expand the Vacant
            // check will trigger a fresh fetch for a Failed entry (no retry
            // storm: Failed entries are kept; only Loading here is transient).
            if let Err(e) = tx.try_send(msg) {
                // The original msg was consumed; synthesize a Failed so the
                // Loading placeholder resolves.
                let failed = BundleMembersMsg::Failed {
                    bundle_repo: bundle_repo.clone(),
                    reason: format!("result channel full, fetch dropped: {e}"),
                    generation,
                };
                // Best-effort only — if still full, the _guard drop clears the
                // in-flight slot so the next Expand can re-attempt. The Loading
                // entry persists at most until the next expand triggers a
                // re-fetch (which the Vacant check in event.rs would block only
                // for Loading, not Failed — see W3 note in phase2_review_findings.md).
                // We send Failed here to resolve it immediately when possible.
                let _ = tx.try_send(failed);
            }
        });
    }

    /// Reap completed tasks (mirrors `UpdateChecker::reap_finished`).
    ///
    /// Drives the `JoinSet` forward so task panics surface (swallowed in raw
    /// mode, but the `JoinSet` must be polled or completed handles accumulate
    /// for the whole session — a resource leak). Called each tick in the
    /// event loop alongside the `UpdateChecker` reap.
    pub fn reap_finished(&mut self) {
        while self.tasks.try_join_next().is_some() {}
    }

    /// Abort all in-flight tasks. Called on drop.
    pub fn abort_all(&mut self) {
        self.tasks.abort_all();
    }
}

impl Drop for BundleMemberChecker {
    fn drop(&mut self) {
        self.abort_all();
    }
}
