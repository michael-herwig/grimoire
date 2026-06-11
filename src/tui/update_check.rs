// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! Background update checks for the catalog browser.
//!
//! While the user browses and searches, the TUI runs bounded-concurrency
//! background tasks that (a) refresh the registry catalog so new packages
//! surface live, and (b) re-resolve the floating tag of every
//! installed/locked row to detect a newer pin on the registry and flip its
//! status to `↑ outdated` without a manual refresh.
//!
//! This module mirrors the purity discipline of [`super::state`] /
//! [`super::event`] / [`super::render`]: the **decisions** — which rows are
//! eligible, whether a resolved digest means "outdated", whether enough
//! time has passed to schedule again — are pure functions, unit-tested
//! headlessly with no terminal and no network. The only impurity is
//! confined to the [`UpdateChecker`] spawn helpers, which `tokio::spawn`
//! the actual work, bound it with a [`Semaphore`], and report results back
//! over a bounded [`mpsc`] channel. That makes this module the background-
//! task analog of [`super::app`]'s impure role, while everything testable
//! stays out of the runtime.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::mpsc::Sender;
use tokio::sync::{Semaphore, mpsc};
use tokio::task::JoinSet;

use super::state::{ArtifactState, TuiRow};
use crate::catalog::registry_catalog::Catalog;
use crate::oci::access::{OciAccess, Operation};
use crate::oci::{Digest, Identifier};

/// Maximum concurrent per-row registry re-checks. A polite cap so a browse
/// of a large catalog never opens hundreds of simultaneous connections;
/// hardcoded for v1 (KISS — revisit only if real registries rate-limit).
const ROW_CHECK_CONCURRENCY: usize = 8;

/// Capacity of the results channel. Bounded so a slow UI cannot let results
/// pile up unboundedly (`quality-rust` bans unbounded `mpsc`). A full
/// channel means the UI is behind; the sender drops the stale result rather
/// than block the task, because a fresh check will supersede it.
const RESULT_CHANNEL_CAPACITY: usize = 256;

/// Minimum gap between search-triggered scheduling passes. Per-keystroke
/// search would otherwise spawn `O(visible rows × keystrokes)` registry
/// calls; this coalesces a burst of typing into at most one scheduling pass
/// per window.
const SEARCH_COALESCE: Duration = Duration::from_millis(300);

/// A result flowing from a background check task back into the event loop.
///
/// Keyed by the stable `repo` string, never by a row index: a catalog
/// refresh or a search edit may reorder or refilter rows between the moment
/// a check is scheduled and the moment its result is drained, so an index
/// would dangle.
///
/// Every variant carries the `generation` the work was scheduled under. A
/// scope toggle or refresh bumps the checker's generation; the drain loop
/// discards any result whose stamp is older than the live generation, so work
/// spawned under one scope can never mutate a row set that now belongs to a
/// different scope. For per-row results that prevents flipping the wrong
/// scope's lock/row set; for the catalog refresh it prevents a stale catalog
/// (walked under the previous scope) from merging after a fresh one and
/// resurrecting wrong rows.
///
/// Closed internal enum — matches stay total, no `#[non_exhaustive]`.
#[derive(Debug)]
pub enum CheckMsg {
    /// A background catalog refresh completed, stamped with the generation it
    /// was spawned under. The app reconciles this catalog into the current row
    /// set by `repo` key, preserving marks, selection, and any live per-row
    /// `↑` flags — but only when the stamp matches the live generation; a
    /// catalog walked under a now-superseded scope is discarded on drain.
    CatalogReady { catalog: Box<Catalog>, generation: u64 },
    /// The row's floating tag now resolves to a digest that differs from
    /// its locked pin — a newer version is available. Stamped with the
    /// generation the check was scheduled under (stale stamps are dropped).
    RowOutdated { repo: String, generation: u64 },
    /// The row's floating tag still resolves to its locked digest (or the
    /// tag vanished / offline yielded nothing). No state change. Stamped
    /// with the scheduling generation.
    RowUpToDate { repo: String, generation: u64 },
    /// The per-row check failed (transport/auth). Degrade silently — the
    /// row keeps whatever state it had; the next scheduled check retries.
    /// Stamped with the scheduling generation.
    Failed { repo: String, generation: u64 },
}

/// The work a single per-row check needs: the stable key to report back
/// under, the floating identifier to resolve, and the digest the lock
/// pinned this artifact to (the comparison baseline).
#[derive(Debug, Clone)]
pub struct RowCheck {
    /// The row's `registry/repository` reference — the result key.
    pub repo: String,
    /// The floating identifier (registry/repo + tag) to resolve fresh.
    pub id: Identifier,
    /// The digest the active scope's lock pinned this artifact to.
    pub locked_digest: Digest,
}

/// The pure "is this row worth a registry re-check?" decision.
///
/// Only rows that already have a lock pin to compare against can become
/// "outdated": `Installed` (the common case) and `Outdated` (so a row that
/// was flipped, then had its pin advanced by an install elsewhere, can flip
/// back). A `NotInstalled` row has no pin, so "a newer tag" is meaningless
/// for the `↑` icon — new-package discovery is the catalog-refresh path,
/// not the per-row path. `Modified` / `IntegrityMissing` carry stronger
/// on-disk truth the background check must never override, so they are
/// excluded to avoid wasting a spawn + permit.
pub fn eligible_for_recheck(row: &TuiRow) -> bool {
    matches!(row.state, ArtifactState::Installed | ArtifactState::Outdated)
}

/// The pure registry-aware "outdated" decision.
///
/// `true` ⇒ the registry resolved the floating tag to a digest that differs
/// from the locked pin ⇒ a newer version is available. A resolve of `None`
/// (the tag vanished, or offline returned nothing) is **not** "outdated":
/// absence is never treated as a newer pin, so the icon never lies on a
/// transient miss.
pub fn outdated_from_resolve(locked: &Digest, resolved: Option<&Digest>) -> bool {
    matches!(resolved, Some(d) if d != locked)
}

/// Owns the background-check machinery: the results sender, the concurrency
/// bound, the access seam, and the spawned-task handles.
///
/// Held by [`super::app::run`] for the lifetime of the TUI. Tasks are kept
/// in a [`JoinSet`]; the app drains finished handles non-blockingly each
/// tick (see [`Self::reap_finished`]) and the set aborts any still-running
/// task on drop, so no detached orphan outlives the TUI. The channel sender
/// is dropped with the checker on exit, so any in-flight send fails
/// harmlessly.
pub struct UpdateChecker {
    /// The results sink. Cloned into each spawned task.
    tx: Sender<CheckMsg>,
    /// Bounds how many per-row checks run at once.
    permits: Arc<Semaphore>,
    /// The OCI-access seam (shared, cache-write-through).
    access: Arc<dyn OciAccess>,
    /// The registry whose catalog is refreshed.
    registry: String,
    /// In-flight + finished task handles, reaped each tick and aborted on
    /// drop.
    tasks: JoinSet<()>,
    /// `(repo, generation)` pairs with a per-row check already spawned and not
    /// yet finished, so a re-schedule does not fire a duplicate in-flight check
    /// for the same row *within the same generation*. Keyed by generation as
    /// well as repo so a forced re-arm under a fresh generation is **not**
    /// suppressed by a task still in flight under the previous one: that old
    /// task holds `(repo, old_gen)`, the fresh check inserts `(repo, new_gen)`,
    /// a distinct key — both coexist, the old guard later frees exactly its own
    /// entry, and the set stays bounded by concurrency. Owned by the task
    /// lifecycle: each spawned task removes its own `(repo, generation)` once
    /// its send attempt resolves (success *or* failure), so a dropped result on
    /// a full channel can never strand a repo as permanently in-flight. Shared
    /// with the tasks via [`Arc`].
    in_flight: Arc<Mutex<std::collections::HashSet<(String, u64)>>>,
    /// When the last search-triggered scheduling pass ran, for debounce.
    last_scheduled: Option<Instant>,
    /// Monotonically increasing scope/refresh generation. Stamped onto every
    /// per-row check at spawn; a scope toggle or refresh bumps it so results
    /// scheduled under a now-superseded scope are discarded on drain.
    generation: u64,
}

impl UpdateChecker {
    /// Create a checker and the receiving half of its results channel.
    /// The app holds the [`mpsc::Receiver`] and drains it each tick.
    pub fn new(access: Arc<dyn OciAccess>, registry: String) -> (Self, mpsc::Receiver<CheckMsg>) {
        let (tx, rx) = mpsc::channel(RESULT_CHANNEL_CAPACITY);
        let checker = Self {
            tx,
            permits: Arc::new(Semaphore::new(ROW_CHECK_CONCURRENCY)),
            access,
            registry,
            tasks: JoinSet::new(),
            in_flight: Arc::new(Mutex::new(std::collections::HashSet::new())),
            // (the set is keyed by (repo, generation); see the field doc)
            last_scheduled: None,
            generation: 0,
        };
        (checker, rx)
    }

    /// The pure debounce decision: should a search-triggered scheduling pass
    /// run at `now`, given the last pass time? The first pass always runs;
    /// later passes wait out [`SEARCH_COALESCE`]. Factored out so the
    /// coalescing window is unit-tested without a clock.
    pub fn should_schedule(last_scheduled: Option<Instant>, now: Instant) -> bool {
        match last_scheduled {
            None => true,
            Some(prev) => now.duration_since(prev) >= SEARCH_COALESCE,
        }
    }

    /// Stamp the scheduling clock to `now` (debounce baseline for the next
    /// search-triggered pass). Call after a scheduling pass actually fires.
    pub fn mark_scheduled(&mut self, now: Instant) {
        self.last_scheduled = Some(now);
    }

    /// The last scheduling-pass time, for the debounce decision.
    pub fn last_scheduled(&self) -> Option<Instant> {
        self.last_scheduled
    }

    /// The live scope/refresh generation. A per-row [`CheckMsg`] whose stamp
    /// is older than this is stale (scheduled under a superseded scope) and
    /// the drain loop discards it.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Bump the scope/refresh generation, invalidating every per-row check
    /// already in flight: their results carry the old stamp and are dropped
    /// on drain. Call on a scope toggle or a refresh, *before* re-arming the
    /// fresh per-row sweep, so a check spawned under the previous scope can
    /// never flip a row that now belongs to the new scope's lock/row set.
    pub fn bump_generation(&mut self) {
        self.generation = self.generation.wrapping_add(1);
    }

    /// Non-blockingly reap finished background tasks so panics surface and
    /// the [`JoinSet`] does not accumulate completed handles for the whole
    /// session. Called each event-loop tick.
    ///
    /// A panicking task is *deliberately swallowed*: this is a TUI in raw
    /// mode, so writing to stderr (or any logging that targets the terminal)
    /// would corrupt the alternate screen. The work is best-effort
    /// (idempotent registry re-checks) and the next scheduled pass retries,
    /// so a lost task degrades silently rather than crashing the UI. Task
    /// cancellation (on `abort_all` at drop) is likewise ignored.
    pub fn reap_finished(&mut self) {
        while let Some(_joined) = self.tasks.try_join_next() {
            // Intentionally not inspected: see the doc comment — surfacing a
            // panic here would require terminal output that raw mode forbids.
        }
    }

    /// Spawn a background catalog refresh (force-rebuild of the empty-query
    /// browse window) and report the result as [`CheckMsg::CatalogReady`].
    /// A refresh failure is swallowed: the existing rows stay, and the next
    /// `r`/`--refresh` retries. The catalog write-through is handled inside
    /// [`Catalog::load_or_refresh`].
    pub fn spawn_catalog_refresh(&mut self, catalog_path: std::path::PathBuf) {
        let tx = self.tx.clone();
        let access = Arc::clone(&self.access);
        let registry = self.registry.clone();
        let generation = self.generation;
        self.tasks.spawn(async move {
            // `force = true` rebuilds even a fresh cache; `offline = false`
            // because the app pre-checks `ctx.offline` and never spawns when
            // offline (and `load_or_refresh` would degrade to cache anyway).
            if let Ok(catalog) = Catalog::load_or_refresh(&catalog_path, &registry, "", &access, false, true).await {
                // Drop on a full channel: a stale catalog is superseded by
                // the next refresh; never block the task. Stamped with the
                // scheduling generation so a refresh spawned under a scope the
                // user has since left is discarded on drain rather than merged.
                let _ = tx.try_send(CheckMsg::CatalogReady {
                    catalog: Box::new(catalog),
                    generation,
                });
            }
        });
    }

    /// Spawn one bounded per-row check for each item in `checks`, skipping
    /// any whose `(repo, generation)` already has a check in flight. Each task
    /// acquires a [`Semaphore`] permit first (so at most
    /// [`ROW_CHECK_CONCURRENCY`] run at once), resolves the floating tag with
    /// [`Operation::Query`] (a read-only-fresh lookup that never writes a tag
    /// pointer), reports the pure [`outdated_from_resolve`] decision stamped
    /// with the current [`Self::generation`], and clears its own in-flight slot
    /// once the send attempt resolves.
    ///
    /// Dedup is keyed by `(repo, generation)`, not by `repo` alone: a forced
    /// re-arm bumps the generation *before* re-scheduling, so a task still in
    /// flight under the previous generation holds `(repo, old_gen)` and never
    /// blocks the fresh `(repo, new_gen)` check from being scheduled. That
    /// fresh check is what surfaces the row under the new scope; the old task's
    /// result is discarded on drain as stale.
    pub fn spawn_row_checks(&mut self, checks: Vec<RowCheck>) {
        let generation = self.generation;
        for check in checks {
            {
                // Hold the in-flight lock only to test-and-set the dedup slot;
                // never across the await below.
                let mut guard = self.in_flight.lock().unwrap_or_else(|p| p.into_inner());
                if !guard.insert((check.repo.clone(), generation)) {
                    // A check for this repo *in this generation* is already in
                    // flight — do not duplicate it (the spec's "no duplicate
                    // in-flight"). A check under a *prior* generation does not
                    // block this one: its key carries the old generation.
                    continue;
                }
            }
            let tx = self.tx.clone();
            let access = Arc::clone(&self.access);
            let permits = Arc::clone(&self.permits);
            let in_flight = Arc::clone(&self.in_flight);
            self.tasks.spawn(async move {
                // Clear the in-flight slot when this task ends, however it
                // ends (send dropped on a full channel, resolve error, even a
                // panic mid-task). The guard removes *exactly* this task's
                // `(repo, generation)` key, so an old-generation task draining
                // out never evicts the fresh-generation entry. Owning the slot
                // here — not in the drain loop — is what prevents a dropped
                // result from stranding the repo as permanently in-flight and
                // never re-checked.
                let _slot = InFlightGuard {
                    set: Arc::clone(&in_flight),
                    repo: check.repo.clone(),
                    generation,
                };
                // Acquire a permit for the lifetime of the registry call so
                // concurrency stays bounded; the permit drops when the task
                // ends. `acquire_owned` fails only if the semaphore is
                // closed, which never happens here (we hold the `Arc`).
                let _permit = match permits.acquire_owned().await {
                    Ok(p) => p,
                    Err(_) => return,
                };
                let msg = match access.resolve_digest(&check.id, Operation::Query).await {
                    Ok(resolved) => {
                        if outdated_from_resolve(&check.locked_digest, resolved.as_ref()) {
                            CheckMsg::RowOutdated {
                                repo: check.repo,
                                generation,
                            }
                        } else {
                            CheckMsg::RowUpToDate {
                                repo: check.repo,
                                generation,
                            }
                        }
                    }
                    Err(_) => CheckMsg::Failed {
                        repo: check.repo,
                        generation,
                    },
                };
                // Drop on a full channel: a stale per-row result is
                // superseded by the next scheduled check; never block.
                let _ = tx.try_send(msg);
            });
        }
    }
}

/// Removes a `(repo, generation)` slot from the shared in-flight set on drop,
/// so the slot is freed when the owning task ends *regardless of how it ends*
/// — a clean send, a dropped send on a full channel, a resolve error, or a
/// panic. The key carries the generation so a task draining out under a
/// superseded generation removes *exactly* its own entry and never evicts a
/// fresh-generation check scheduled for the same repo after a re-arm. This
/// RAII tie to the task lifecycle (not the drain loop) is what fixes the
/// dedup-leak where a dropped result would otherwise strand a repo as
/// permanently in-flight.
struct InFlightGuard {
    set: Arc<Mutex<std::collections::HashSet<(String, u64)>>>,
    repo: String,
    generation: u64,
}

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        // Recover from a poisoned lock: the set is a plain dedup cache, so a
        // prior panic while holding it leaves no broken invariant — just
        // take the inner set and remove our slot.
        let mut guard = self.set.lock().unwrap_or_else(|p| p.into_inner());
        guard.remove(&(self.repo.clone(), self.generation));
    }
}

impl Drop for UpdateChecker {
    fn drop(&mut self) {
        // Abort every still-running background task on exit — no detached
        // orphan outlives the TUI. Finished tasks are reaped each tick by
        // [`Self::reap_finished`]; this catches whatever is mid-flight at
        // quit. The tasks are short-lived and side-effect-free beyond the
        // channel send and the catalog write-through, so an abort mid-flight
        // is safe.
        self.tasks.abort_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oci::Algorithm;

    fn digest(seed: &[u8]) -> Digest {
        Algorithm::Sha256.hash(seed)
    }

    fn row(repo: &str, state: ArtifactState) -> TuiRow {
        TuiRow {
            kind: "skill".to_string(),
            repo: repo.to_string(),
            description: String::new(),
            summary: String::new(),
            keywords: Vec::new(),
            latest_tag: "latest".to_string(),
            version: "1.0.0".to_string(),
            pinned_version: None,
            state,
        }
    }

    // ── outdated_from_resolve truth table ────────────────────────────────

    #[test]
    fn outdated_when_resolved_differs_from_locked() {
        let locked = digest(b"locked");
        let newer = digest(b"newer");
        assert!(
            outdated_from_resolve(&locked, Some(&newer)),
            "different digest ⇒ outdated"
        );
    }

    #[test]
    fn not_outdated_when_resolved_equals_locked() {
        let locked = digest(b"same");
        let same = digest(b"same");
        assert!(
            !outdated_from_resolve(&locked, Some(&same)),
            "identical digest ⇒ up to date"
        );
    }

    #[test]
    fn not_outdated_when_resolve_is_none() {
        let locked = digest(b"locked");
        assert!(
            !outdated_from_resolve(&locked, None),
            "a vanished/offline tag is never treated as a newer pin"
        );
    }

    // ── eligible_for_recheck row selection ───────────────────────────────

    #[test]
    fn only_installed_and_outdated_rows_are_eligible() {
        assert!(eligible_for_recheck(&row("r/a", ArtifactState::Installed)));
        assert!(eligible_for_recheck(&row("r/b", ArtifactState::Outdated)));
        assert!(!eligible_for_recheck(&row("r/c", ArtifactState::NotInstalled)));
        assert!(!eligible_for_recheck(&row("r/d", ArtifactState::Modified)));
        assert!(!eligible_for_recheck(&row("r/e", ArtifactState::IntegrityMissing)));
    }

    // ── should_schedule debounce window ──────────────────────────────────

    #[test]
    fn first_schedule_always_runs() {
        let now = Instant::now();
        assert!(UpdateChecker::should_schedule(None, now), "no prior pass ⇒ run");
    }

    #[test]
    fn schedule_suppressed_inside_coalesce_window() {
        let prev = Instant::now();
        let inside = prev + SEARCH_COALESCE - Duration::from_millis(1);
        assert!(
            !UpdateChecker::should_schedule(Some(prev), inside),
            "within the coalesce window ⇒ suppressed (no storm)"
        );
    }

    #[test]
    fn schedule_runs_after_coalesce_window() {
        let prev = Instant::now();
        let after = prev + SEARCH_COALESCE;
        assert!(
            UpdateChecker::should_schedule(Some(prev), after),
            "at or past the window boundary ⇒ run"
        );
        let well_after = prev + SEARCH_COALESCE + Duration::from_millis(50);
        assert!(UpdateChecker::should_schedule(Some(prev), well_after));
    }

    // ── in-flight dedup + leak-free re-check (no duplicate per-row checks) ─

    use crate::oci::PinnedIdentifier;
    use crate::oci::access::error::AccessError;
    use crate::oci::manifest::OciManifest;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::Notify;

    /// A mock that counts `resolve_digest` calls and, until released, blocks
    /// inside `resolve_digest` so the calling task stays in flight (holding
    /// its dedup slot). Returns a fixed "newer" digest so a completed check
    /// flips the row to outdated.
    struct GatedAccess {
        calls: AtomicUsize,
        newer: Digest,
        /// When set, `resolve_digest` waits on `gate` before returning, so a
        /// test can pin a task in flight to exercise dedup deterministically.
        gate: Notify,
        gated: std::sync::atomic::AtomicBool,
    }

    impl GatedAccess {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                calls: AtomicUsize::new(0),
                newer: Algorithm::Sha256.hash(b"newer"),
                gate: Notify::new(),
                gated: std::sync::atomic::AtomicBool::new(false),
            })
        }
        fn release(&self) {
            self.gated.store(false, Ordering::SeqCst);
            self.gate.notify_waiters();
        }
    }

    #[async_trait::async_trait]
    impl OciAccess for GatedAccess {
        async fn resolve_digest(&self, _id: &Identifier, _op: Operation) -> Result<Option<Digest>, AccessError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.gated.load(Ordering::SeqCst) {
                self.gate.notified().await;
            }
            Ok(Some(self.newer.clone()))
        }
        async fn fetch_manifest(&self, _id: &PinnedIdentifier) -> Result<Option<OciManifest>, AccessError> {
            Ok(None)
        }
        async fn fetch_blob(&self, _r: &Identifier, _d: &Digest) -> Result<Option<Vec<u8>>, AccessError> {
            Ok(None)
        }
        async fn list_tags(&self, _id: &Identifier) -> Result<Option<Vec<String>>, AccessError> {
            Ok(None)
        }
        async fn list_catalog(&self, _registry: &str) -> Result<Vec<String>, AccessError> {
            Ok(Vec::new())
        }
        async fn push_blob(&self, _r: &Identifier, b: &[u8]) -> Result<Digest, AccessError> {
            Ok(Algorithm::Sha256.hash(b))
        }
        async fn push_manifest(&self, _r: &Identifier, _m: &OciManifest) -> Result<Digest, AccessError> {
            Ok(Algorithm::Sha256.hash(b"m"))
        }
        async fn put_tag(&self, _r: &Identifier, _t: &str, _d: &Digest) -> Result<(), AccessError> {
            Ok(())
        }
    }

    fn sample_check() -> RowCheck {
        RowCheck {
            repo: "localhost:5000/acme/code-review".to_string(),
            id: Identifier::new_registry("acme/code-review", "localhost:5000").clone_with_tag("latest"),
            locked_digest: digest(b"locked"),
        }
    }

    #[tokio::test]
    async fn duplicate_in_flight_row_checks_are_deduped() {
        let access = GatedAccess::new();
        // Gate the first task so it stays in flight while we re-schedule.
        access.gated.store(true, Ordering::SeqCst);
        let dyn_access: Arc<dyn OciAccess> = access.clone();
        let (mut checker, mut rx) = UpdateChecker::new(dyn_access, "localhost:5000".to_string());
        let check = sample_check();

        // First schedule spawns a task that blocks inside resolve_digest.
        checker.spawn_row_checks(vec![check.clone()]);
        // Let the spawned task reach the gated await so its slot is held.
        while access.calls.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
        // Re-schedule twice while the first is gated: dedup must suppress both.
        checker.spawn_row_checks(vec![check.clone()]);
        checker.spawn_row_checks(vec![check.clone()]);
        tokio::task::yield_now().await;
        assert_eq!(
            access.calls.load(Ordering::SeqCst),
            1,
            "duplicate in-flight checks for one repo collapse to a single registry call"
        );

        // Release the gate; the result arrives and the slot frees itself.
        access.release();
        let first = rx.recv().await.expect("one result");
        assert!(
            matches!(first, CheckMsg::RowOutdated { .. }),
            "newer digest flips the row"
        );
    }

    #[tokio::test]
    async fn finished_task_frees_its_in_flight_slot_for_re_check() {
        let access = GatedAccess::new(); // not gated ⇒ completes immediately
        let dyn_access: Arc<dyn OciAccess> = access.clone();
        let (mut checker, mut rx) = UpdateChecker::new(dyn_access, "localhost:5000".to_string());
        let check = sample_check();

        checker.spawn_row_checks(vec![check.clone()]);
        assert!(matches!(rx.recv().await, Some(CheckMsg::RowOutdated { .. })));
        // The task cleared its own slot on completion — no explicit clear.
        tokio::task::yield_now().await;
        checker.spawn_row_checks(vec![check.clone()]);
        assert!(
            matches!(rx.recv().await, Some(CheckMsg::RowOutdated { .. })),
            "a finished repo may be re-checked without an external clear"
        );
        tokio::task::yield_now().await;
        assert_eq!(
            access.calls.load(Ordering::SeqCst),
            2,
            "both scheduling passes reached the registry once the slot freed"
        );
    }

    #[tokio::test]
    async fn dropped_result_on_full_channel_does_not_strand_in_flight() {
        // Regression for the dedup-leak: with the in-flight slot freed in the
        // drain loop, a result dropped on a full channel would leave the repo
        // marked in-flight forever and never re-checked. Now the task frees
        // its own slot, so a re-schedule still reaches the registry.
        let access = GatedAccess::new();
        let dyn_access: Arc<dyn OciAccess> = access.clone();
        let (mut checker, mut rx) = UpdateChecker::new(dyn_access, "localhost:5000".to_string());
        let check = sample_check();

        // Saturate the bounded results channel so the task's `try_send` drops
        // its result (the UI is "behind"). The channel holds RESULT_CHANNEL_
        // CAPACITY filler messages; the real per-row result is then dropped.
        for _ in 0..RESULT_CHANNEL_CAPACITY {
            checker
                .tx
                .try_send(CheckMsg::CatalogReady {
                    catalog: Box::new(Catalog::empty("localhost:5000")),
                    generation: 0,
                })
                .expect("filler fits");
        }
        checker.spawn_row_checks(vec![check.clone()]);
        // Let the task run, hit the full channel, drop its result, and (the
        // fix) free its in-flight slot on the way out.
        while access.calls.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
        tokio::task::yield_now().await;

        // Drain the filler so the channel has room for the next result.
        while let Ok(CheckMsg::CatalogReady { .. }) = rx.try_recv() {}

        // Re-schedule: the slot was freed despite the dropped result, so this
        // pass reaches the registry and a result actually lands.
        checker.spawn_row_checks(vec![check.clone()]);
        let second = rx.recv().await.expect("re-check after a dropped result");
        assert!(matches!(second, CheckMsg::RowOutdated { .. }));
        assert_eq!(
            access.calls.load(Ordering::SeqCst),
            2,
            "a repo whose result was dropped on a full channel is re-checkable"
        );
    }

    // ── generation stamping (stale per-row results are invalidated) ───────

    #[test]
    fn bump_generation_increments_and_stamps_fresh_checks() {
        let access = GatedAccess::new();
        let dyn_access: Arc<dyn OciAccess> = access.clone();
        let (mut checker, _rx) = UpdateChecker::new(dyn_access, "localhost:5000".to_string());
        assert_eq!(checker.generation(), 0, "starts at generation 0");
        checker.bump_generation();
        assert_eq!(checker.generation(), 1, "a scope toggle / refresh advances it");
        checker.bump_generation();
        assert_eq!(checker.generation(), 2);
    }

    #[tokio::test]
    async fn per_row_result_carries_scheduling_generation() {
        let access = GatedAccess::new();
        let dyn_access: Arc<dyn OciAccess> = access.clone();
        let (mut checker, mut rx) = UpdateChecker::new(dyn_access, "localhost:5000".to_string());
        checker.bump_generation(); // now at generation 1
        checker.spawn_row_checks(vec![sample_check()]);
        let msg = rx.recv().await.expect("one result");
        match msg {
            CheckMsg::RowOutdated { generation, .. } => {
                assert_eq!(generation, 1, "result is stamped with the live generation at spawn");
            }
            other => panic!("expected RowOutdated, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn catalog_refresh_carries_scheduling_generation() {
        // A catalog refresh is stamped with the generation it was spawned
        // under, so the drain path can discard one walked under a superseded
        // scope. `load_or_refresh` with an empty `list_catalog` yields an empty
        // catalog, which is enough to observe the stamp.
        let access = GatedAccess::new();
        let dyn_access: Arc<dyn OciAccess> = access.clone();
        let (mut checker, mut rx) = UpdateChecker::new(dyn_access, "localhost:5000".to_string());
        checker.bump_generation(); // now at generation 1
        let tmp = std::env::temp_dir().join(format!("grim-catalog-gen-{}.json", std::process::id()));
        checker.spawn_catalog_refresh(tmp.clone());
        match rx.recv().await.expect("a catalog result") {
            CheckMsg::CatalogReady { generation, .. } => {
                assert_eq!(generation, 1, "the catalog result carries the spawn generation");
            }
            other => panic!("expected CatalogReady, got {other:?}"),
        }
        let _ = std::fs::remove_file(&tmp);
    }

    #[tokio::test]
    async fn re_arm_under_fresh_generation_is_not_suppressed_by_in_flight_old_task() {
        // Finding-1 regression: a forced re-arm bumps the generation, but a
        // task from the previous generation is still in flight holding the
        // repo's slot. Keying the in-flight set by (repo, generation) — not by
        // repo alone — must let the fresh-generation check schedule anyway, so
        // the row is re-checked under the new scope instead of being stranded
        // until some later trigger.
        let access = GatedAccess::new();
        // Gate every call so both the gen-0 and gen-1 tasks stay in flight.
        access.gated.store(true, Ordering::SeqCst);
        let dyn_access: Arc<dyn OciAccess> = access.clone();
        let (mut checker, mut rx) = UpdateChecker::new(dyn_access, "localhost:5000".to_string());
        let check = sample_check();

        // Generation 0: spawn a task that blocks inside resolve_digest.
        checker.spawn_row_checks(vec![check.clone()]);
        while access.calls.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }

        // A forced re-arm: bump the generation, then re-schedule the same repo
        // *while the gen-0 task is still gated in flight*.
        checker.bump_generation();
        checker.spawn_row_checks(vec![check.clone()]);
        // The fresh (repo, gen=1) key is distinct from the in-flight
        // (repo, gen=0) key, so this pass must reach the registry: a second
        // call lands even though the first task has not yet returned.
        while access.calls.load(Ordering::SeqCst) < 2 {
            tokio::task::yield_now().await;
        }
        assert_eq!(
            access.calls.load(Ordering::SeqCst),
            2,
            "the fresh-generation re-arm is not suppressed by the in-flight old-generation task"
        );

        // Release both tasks; each frees exactly its own (repo, generation)
        // slot, and two stamped results arrive (gen 0 and gen 1).
        access.release();
        let a = rx.recv().await.expect("first result");
        let b = rx.recv().await.expect("second result");
        let gens: std::collections::BTreeSet<u64> = [&a, &b]
            .iter()
            .map(|m| match m {
                CheckMsg::RowOutdated { generation, .. }
                | CheckMsg::RowUpToDate { generation, .. }
                | CheckMsg::Failed { generation, .. } => *generation,
                CheckMsg::CatalogReady { generation, .. } => *generation,
            })
            .collect();
        assert_eq!(
            gens,
            std::collections::BTreeSet::from([0, 1]),
            "both the old- and fresh-generation checks produced a result"
        );

        // Both (repo, gen) slots freed: the set is empty, so a third re-arm at
        // the live generation can schedule again without stranding.
        {
            let guard = checker.in_flight.lock().unwrap_or_else(|p| p.into_inner());
            assert!(guard.is_empty(), "every (repo, generation) slot freed on completion");
        }
    }
}
