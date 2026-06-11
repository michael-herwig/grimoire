// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! The TUI runtime: the one place the terminal, raw mode, the async
//! catalog load, and the event loop live.
//!
//! Everything decision-shaped is delegated to the pure
//! [`super::state`] / [`super::event`] / [`super::render`] modules; this
//! file only does the impure work: enter/leave raw mode (via an RAII
//! guard that restores the terminal even on panic), read crossterm
//! events, map them to the abstract [`TuiInput`], apply the pure
//! transition, and on `Install` / `Update` reuse the **same** resolve →
//! lock → materialize path the `install`/`update` commands use (no forked
//! logic). This module is excluded from acceptance tests; its logic is
//! covered headlessly by the pure modules' unit tests.

use std::io::{self};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind};
use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

use crate::catalog::registry_catalog::Catalog;
use crate::command::add::{declare, relock_declared, write_config};
use crate::command::grim;
use crate::command::uninstall::undeclare_and_unlock;
use crate::config::declaration::{ConfigOptions, DesiredSet};
use crate::config::global_config::GlobalConfig;
use crate::config::project_config::ProjectConfig;
use crate::config::scope::ConfigScope;
use crate::install::install_state::InstallState;
use crate::install::installer::{InstallOutcome, install_all};
use crate::install::materializer::DefaultMaterializer;
use crate::install::target::InstallTarget;
use crate::lock::file_lock::ConfigFileLock;
use crate::lock::grimoire_lock::GrimoireLock;
use crate::lock::lock_io;
use crate::lock::locked_artifact::LockedArtifact;
use crate::oci::access::OciAccess;
use crate::oci::{ArtifactKind, Identifier};

use super::event::{BatchOp, TuiAction, TuiInput, handle};
use super::render::{draw, frame};
use super::state::{ArtifactState, Mode, TuiRow, TuiState};
use super::update_check::{CheckMsg, RowCheck, UpdateChecker, eligible_for_recheck};

use std::time::Instant;

use tokio::sync::mpsc::Receiver;

/// Everything the TUI needs to load the catalog and reuse the install
/// path, resolved once by `command/tui.rs` before raw mode is entered.
pub struct TuiContext {
    /// The registry whose catalog is browsed. Also the effective default
    /// registry: its host is elided as the tree root so names stay short.
    pub registry: String,
    /// The catalog cache file (`$GRIM_HOME/catalog.json`).
    pub catalog_path: std::path::PathBuf,
    /// The OCI-access seam (shared with the resolve/install path).
    pub access: Arc<dyn OciAccess>,
    /// Whether this invocation is offline (degrade, never crash).
    pub offline: bool,
    /// Whether the initial catalog load force-rebuilds even a fresh cache
    /// (the `--refresh` flag). The interactive `r` key always forces a
    /// reload regardless of this; this governs only the first load.
    pub force_refresh: bool,
    /// The scope install/update materialize into.
    pub scope: ConfigScope,
    /// The workspace root targets are rooted at.
    pub workspace: std::path::PathBuf,
    /// The scope's lock path (badge derivation + the per-action relock).
    pub lock_path: std::path::PathBuf,
    /// The scope's install-state path.
    pub state_path: std::path::PathBuf,
    /// The scope's config path (`grimoire.toml`). The TUI declares an
    /// install into it through the same seam `grim add` uses, and the
    /// delete action undeclares through the `grim uninstall` seam.
    pub config_path: std::path::PathBuf,
    /// The AI client target(s) to materialize into (the raw config `clients`
    /// option; empty triggers detection at install time). Still needed for
    /// the `InstallTarget::parse` fallback in [`perform`].
    pub clients_default: Vec<String>,
    /// The *effective* selected clients for the active scope (config clients
    /// when set, else detected) — surfaced in the status area for display.
    pub clients_selected: Vec<crate::install::client_target::ClientTarget>,
    /// Human label for the active scope (`project` / `global`), shown in
    /// the title.
    pub scope_label: String,
    /// The *other* scope, if one is resolvable — enables the runtime
    /// Global ⇄ Project toggle. `None` ⇒ toggle is a no-op (e.g. no
    /// project config discoverable).
    pub alt: Option<ScopeSwap>,
}

/// The scope-dependent fields that swap when the user toggles scope.
/// Everything else in [`TuiContext`] (registry, catalog, access) is
/// scope-independent.
pub struct ScopeSwap {
    /// Which scope this is.
    pub scope: ConfigScope,
    /// The workspace root targets are rooted at.
    pub workspace: std::path::PathBuf,
    /// The scope's lock path.
    pub lock_path: std::path::PathBuf,
    /// The scope's install-state path.
    pub state_path: std::path::PathBuf,
    /// The scope's config path (`grimoire.toml`).
    pub config_path: std::path::PathBuf,
    /// The AI client target(s) to materialize into (raw config clients).
    pub clients_default: Vec<String>,
    /// The effective selected clients for this scope (config or detected).
    pub clients_selected: Vec<crate::install::client_target::ClientTarget>,
    /// Human label (`project` / `global`).
    pub label: String,
}

impl TuiContext {
    /// Swap the active scope-dependent fields with [`Self::alt`]. A no-op
    /// when no alternate scope was resolvable. The previously-active
    /// fields become the new `alt`, so toggling again returns.
    fn toggle_scope(&mut self) -> bool {
        let Some(alt) = self.alt.take() else {
            return false;
        };
        let now_alt = ScopeSwap {
            scope: self.scope,
            workspace: std::mem::replace(&mut self.workspace, alt.workspace),
            lock_path: std::mem::replace(&mut self.lock_path, alt.lock_path),
            state_path: std::mem::replace(&mut self.state_path, alt.state_path),
            config_path: std::mem::replace(&mut self.config_path, alt.config_path),
            clients_default: std::mem::replace(&mut self.clients_default, alt.clients_default),
            clients_selected: std::mem::replace(&mut self.clients_selected, alt.clients_selected),
            label: std::mem::replace(&mut self.scope_label, alt.label),
        };
        self.scope = alt.scope;
        self.alt = Some(now_alt);
        true
    }
}

/// Restores the terminal on drop — even if the body panics or returns an
/// error — so a crash never leaves the user's shell in raw mode.
struct TerminalGuard;

impl TerminalGuard {
    fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        crossterm::execute!(io::stdout(), EnterAlternateScreen)?;
        Ok(Self)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = crossterm::execute!(io::stdout(), LeaveAlternateScreen);
        let _ = disable_raw_mode();
    }
}

/// Run the TUI to a clean quit.
///
/// # Errors
///
/// A terminal-setup or draw I/O failure. Catalog-load and install/update
/// failures are surfaced *in* the status line, not as a hard error — the
/// TUI degrades rather than crashing (offline included).
pub async fn run(mut ctx: TuiContext) -> anyhow::Result<()> {
    let _guard = TerminalGuard::enter()?;
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend)?;

    let mut state = TuiState::new();
    state.set_offline(ctx.offline);
    state.set_scope_label(&ctx.scope_label);
    state.set_clients(client_names(&ctx));
    // The browsed registry is the effective default: eliding its host
    // from the tree root keeps leaf names short (the user's ask).
    state.set_default_registry(Some(ctx.registry.clone()));

    // Initial async catalog load: show `loading`, then populate.
    terminal.draw(|f| draw(f, &frame(&state)))?;
    load_into(&ctx, &mut state).await;
    terminal.draw(|f| draw(f, &frame(&state)))?;

    // The background-update-check machinery: a bounded set of tokio tasks
    // that refresh the catalog and re-resolve installed rows' floating tags
    // while the user browses, feeding results back over `rx`. Offline
    // disables it entirely (no network); the checker is still created so the
    // event loop is shape-stable, it just never gets primed.
    let (mut checker, mut rx) = UpdateChecker::new(Arc::clone(&ctx.access), ctx.registry.clone());
    arm_background_checks(&ctx, &state, &mut checker);

    loop {
        // Reap finished background tasks so panics surface (deliberately
        // swallowed in raw mode — see `UpdateChecker::reap_finished`) and the
        // JoinSet does not accumulate completed handles for the whole session.
        checker.reap_finished();
        // Drain any background results that arrived since the last tick and
        // redraw if state changed — the 200ms poll below doubles as the
        // result-drain tick (no event needed to surface a flipped icon).
        if drain_checks(&ctx, &mut state, &mut checker, &mut rx) {
            terminal.draw(|f| draw(f, &frame(&state)))?;
        }

        // Poll so a slow terminal does not spin; on timeout, loop back to
        // drain again (so results surface within ~200ms even while idle).
        if !event::poll(Duration::from_millis(200))? {
            continue;
        }
        let ev = event::read()?;
        // A terminal resize must redraw immediately — the layout is
        // recomputed every `draw`, but only key events reached it before.
        if let Event::Resize(..) = ev {
            terminal.draw(|f| draw(f, &frame(&state)))?;
            continue;
        }
        let Event::Key(key) = ev else {
            continue;
        };
        // Only act on key *press* (Windows emits press+release).
        if key.kind == KeyEventKind::Release {
            continue;
        }
        let Some(input) = map_key(key) else {
            continue;
        };

        // A search edit may surface new installed rows — schedule debounced
        // per-row checks after the transition applies (below).
        let was_searching = state.mode == Mode::Search;
        match handle(&mut state, input) {
            TuiAction::Quit => break,
            TuiAction::None => {}
            TuiAction::Refresh => {
                state.set_loading(true);
                state.set_status("refreshing catalog…");
                terminal.draw(|f| draw(f, &frame(&state)))?;
                reload_into(&ctx, &mut state, true).await;
                // Re-arm the background checks against the freshly-loaded
                // rows (the `r` key is an explicit "check again" too).
                arm_background_checks(&ctx, &state, &mut checker);
            }
            TuiAction::Batch { op, rows } => {
                run_batch(&ctx, &mut state, &rows, op).await;
                // An install/update may have just pinned a version older
                // than the registry's floating tag (the user picked an old
                // version in the picker) — re-check exactly those rows now
                // so the badge flips to `↑ outdated` immediately, not at
                // the next manual refresh.
                if op != BatchOp::Uninstall {
                    recheck_rows(&ctx, &state, &mut checker, &rows);
                }
            }
            TuiAction::LoadVersions { row } => {
                load_versions(&ctx, &mut state, row).await;
            }
            TuiAction::ToggleScope => {
                if ctx.toggle_scope() {
                    state.set_scope_label(&ctx.scope_label);
                    state.set_clients(client_names(&ctx));
                    recompute_states(&ctx, &mut state);
                    // The new scope has a different lock/state — re-check its
                    // installed rows against the registry.
                    arm_background_checks(&ctx, &state, &mut checker);
                    // The colored MODE box already shows the active scope
                    // — no redundant title-bar status.
                    state.set_status("");
                } else {
                    state.set_status("no alternate scope to switch to");
                }
            }
        }

        // While searching, a query edit can reveal installed rows that were
        // filtered out — schedule debounced per-row checks for them.
        if was_searching && state.mode == Mode::Search {
            schedule_row_checks(&ctx, &state, &mut checker, Instant::now());
        }

        terminal.draw(|f| draw(f, &frame(&state)))?;
    }
    Ok(())
}

/// Spawn the launch/refresh/scope-toggle round of background checks against
/// the current rows: a catalog refresh (new packages) plus a per-row
/// floating-tag re-check for every eligible (installed/outdated) row. A
/// no-op when offline (zero network). Called after the first load, after
/// `Refresh`, and after a scope toggle.
///
/// This is the **forced** entry point: it bypasses the search-debounce
/// window (`r` / `--refresh` / a scope flip are explicit "check again now"
/// gestures the user expects to act immediately) and bumps the checker
/// generation first, so any per-row check still in flight under the previous
/// scope/refresh has its result discarded on drain. The per-keystroke search
/// path uses [`schedule_row_checks`] instead, which *does* debounce.
fn arm_background_checks(ctx: &TuiContext, state: &TuiState, checker: &mut UpdateChecker) {
    if ctx.offline {
        return;
    }
    // Invalidate results from the previous scope/refresh before re-arming.
    checker.bump_generation();
    checker.spawn_catalog_refresh(ctx.catalog_path.clone());
    // Force past the debounce: this is the launch/refresh/scope check, not a
    // per-keystroke storm.
    schedule_row_checks_forced(ctx, state, checker, Instant::now(), true);
}

/// Schedule bounded per-row registry re-checks for the eligible rows,
/// debounced so per-keystroke search never spawns a storm. Each eligible
/// row contributes one [`RowCheck`] (its floating identifier + locked
/// digest); the checker dedups any repo already in flight. A no-op when
/// offline. This is the **debounced** path (the search edit); the forced
/// re-arm path is [`arm_background_checks`].
fn schedule_row_checks(ctx: &TuiContext, state: &TuiState, checker: &mut UpdateChecker, now: Instant) {
    schedule_row_checks_forced(ctx, state, checker, now, false);
}

/// The shared body behind [`schedule_row_checks`] (debounced) and
/// [`arm_background_checks`] (forced). When `force` is `true` the
/// [`SEARCH_COALESCE`] debounce window is bypassed entirely, so a refresh or
/// scope toggle that lands inside the window of a recent search keystroke
/// still arms its per-row sweep instead of being silently swallowed. When
/// `force` is `false` the pass is suppressed inside the coalesce window.
fn schedule_row_checks_forced(
    ctx: &TuiContext,
    state: &TuiState,
    checker: &mut UpdateChecker,
    now: Instant,
    force: bool,
) {
    if ctx.offline {
        return;
    }
    if !force && !UpdateChecker::should_schedule(checker.last_scheduled(), now) {
        return;
    }
    let (lock, _install_state) = load_scope_for_badges(ctx);
    let Some(lock) = lock else {
        return; // No lock ⇒ no pins to compare against.
    };
    let checks: Vec<RowCheck> = state
        .rows
        .iter()
        .filter(|r| eligible_for_recheck(r))
        .filter_map(|r| build_row_check(&lock, r))
        .collect();
    if checks.is_empty() {
        return;
    }
    checker.spawn_row_checks(checks);
    checker.mark_scheduled(now);
}

/// Build the [`RowCheck`] for one eligible row: pair its floating identifier
/// (registry/repo + the representative/`latest` tag) with the digest the
/// scope's lock pinned it to. `None` when the row carries no lock entry
/// (then "newer tag" has no baseline) or its repo is malformed.
fn build_row_check(lock: &GrimoireLock, row: &TuiRow) -> Option<RowCheck> {
    let (registry, repository) = split_repo(&row.repo)?;
    let locked = lock
        .skills
        .iter()
        .chain(lock.rules.iter())
        .find(|a| a.pinned.registry() == registry && a.pinned.repository() == repository)?;
    // Resolve the same floating tag the badge derivation pins against: the
    // representative tag, else the conventional `latest`.
    let tag = if row.latest_tag.is_empty() {
        "latest".to_string()
    } else {
        row.latest_tag.clone()
    };
    let id = Identifier::new_registry(repository, registry).clone_with_tag(tag);
    Some(RowCheck {
        repo: row.repo.clone(),
        id,
        locked_digest: locked.pinned.digest(),
    })
}

/// Spawn immediate per-row re-checks for the rows a batch just installed
/// or updated (no debounce — a finished batch is an explicit gesture, like
/// `r`). The checker's `(repo, generation)` in-flight dedup absorbs any
/// overlap with a scheduled sweep. This is what flips a just-installed old
/// version to `↑ outdated` without waiting for a manual refresh: the lock
/// now pins the old digest, and the floating-tag re-check observes the
/// registry's newer one.
fn recheck_rows(ctx: &TuiContext, state: &TuiState, checker: &mut UpdateChecker, rows: &[usize]) {
    if ctx.offline {
        return;
    }
    let (lock, _install_state) = load_scope_for_badges(ctx);
    let Some(lock) = lock else {
        return; // No lock ⇒ no pins to compare against.
    };
    let checks = post_batch_checks(&lock, &state.rows, rows);
    if !checks.is_empty() {
        checker.spawn_row_checks(checks);
    }
}

/// The pure post-batch selection: the [`RowCheck`]s for exactly the
/// acted-on row indices that are eligible (installed/outdated) and carry a
/// lock pin. Out-of-range indices and ineligible rows are skipped.
fn post_batch_checks(lock: &GrimoireLock, rows: &[TuiRow], indices: &[usize]) -> Vec<RowCheck> {
    indices
        .iter()
        .filter_map(|&i| rows.get(i))
        .filter(|r| eligible_for_recheck(r))
        .filter_map(|r| build_row_check(lock, r))
        .collect()
}

/// Whether a [`CheckMsg`] stamped with `msg_generation` is still fresh at
/// `live_generation`. A stamp older than the live generation means the work
/// was scheduled under a scope/refresh the user has since left (a scope toggle
/// or `r` bumped the generation); applying it would mutate the wrong scope's
/// view, so the drain path discards it. Pure so the discard rule is one
/// unit-testable predicate shared by the per-row and catalog drain arms.
fn is_generation_fresh(msg_generation: u64, live_generation: u64) -> bool {
    msg_generation == live_generation
}

/// Apply a per-row "outdated" result to `state` **only** when its stamp is
/// fresh (see [`is_generation_fresh`]). Returns `true` when a flip happened.
/// Pure over `state` so the discard is unit-testable without a [`TuiContext`].
fn apply_outdated_if_fresh(state: &mut TuiState, repo: &str, msg_generation: u64, live_generation: u64) -> bool {
    is_generation_fresh(msg_generation, live_generation) && state.mark_outdated_if_installed(repo)
}

/// Drain every pending [`CheckMsg`] non-blockingly and apply it to `state`.
/// Returns `true` when anything changed (so the caller redraws). This is the
/// only place background results touch the screen model — through the pure
/// setters, keeping `state.rs` the single source of row truth.
fn drain_checks(
    ctx: &TuiContext,
    state: &mut TuiState,
    checker: &mut UpdateChecker,
    rx: &mut Receiver<CheckMsg>,
) -> bool {
    let mut changed = false;
    let live_generation = checker.generation();
    // `try_recv` never blocks; loop until the channel is momentarily empty.
    while let Ok(msg) = rx.try_recv() {
        match msg {
            CheckMsg::CatalogReady { catalog, generation } => {
                // Discard a catalog walked under a superseded scope: a refresh
                // spawned before a scope toggle / `r` carries the old stamp,
                // and merging it after a fresh one would resurrect the wrong
                // scope's rows. Only a stamp matching the live generation is
                // reconciled.
                if is_generation_fresh(generation, live_generation) {
                    // Re-derive rows from the fresh catalog against the active
                    // scope, then reconcile preserving marks, cursor, live ↑ /
                    // pins + the kind-sort and filter. The scope load is cheap
                    // (advisory).
                    drain_catalog_ready(ctx, state, &catalog);
                    changed = true;
                }
            }
            // A per-row result is honored only when its stamp matches the
            // live generation: a scope toggle or refresh bumped the
            // generation, so a check spawned under the previous scope would
            // flip the wrong row (different lock / row set) and is dropped.
            // The in-flight slot is freed by the task itself on completion,
            // so no bookkeeping is needed here.
            CheckMsg::RowOutdated { repo, generation } => {
                if apply_outdated_if_fresh(state, &repo, generation, live_generation) {
                    changed = true;
                }
            }
            CheckMsg::RowUpToDate { generation, .. } | CheckMsg::Failed { generation, .. } => {
                // No state change either way; the stamp is irrelevant beyond
                // the (intentional) no-op. Stale stamps are simply ignored.
                let _ = generation;
            }
        }
    }
    if changed {
        update_idle_breadcrumb(state);
    }
    changed
}

/// Apply a [`CheckMsg::CatalogReady`]: project the fresh catalog into rows
/// (badges derived from the active scope's lock + install record, reusing
/// the same path the initial load uses) and merge them, preserving live
/// per-row `↑` flags, pins, and re-applying the kind-sort + filter.
fn drain_catalog_ready(ctx: &TuiContext, state: &mut TuiState, catalog: &Catalog) {
    let (lock, install_state) = load_scope_for_badges(ctx);
    let fresh = rows_from_catalog(catalog, lock.as_ref(), &install_state);
    state.merge_catalog_rows(fresh);
    // The background refresh re-walks the same browse window, so its
    // truncation verdict supersedes the initial load's (the cap may now be
    // hit or cleared as the registry grows/shrinks).
    state.set_truncated(catalog.truncated());
}

/// Set a quiet tally breadcrumb ("N update(s) available") **only** when the
/// status line is otherwise idle, so a transient batch-result or refresh
/// message is never clobbered by the background checker. Cleared to empty
/// when no updates are outstanding and the line is idle.
fn update_idle_breadcrumb(state: &mut TuiState) {
    // Only speak into an idle line: a non-empty status is a transient
    // message (batch result, error, refresh) that must win.
    if !state.status_line.is_empty() {
        return;
    }
    let n = state.outdated_count();
    if n > 0 {
        state.set_status(format!("{n} update{} available", if n == 1 { "" } else { "s" }));
    }
}

/// Map a crossterm key to the abstract [`TuiInput`]. The *only*
/// crossterm-aware decision in the codebase; the alphabet it targets is
/// pure and fully unit-tested in `event.rs`.
fn map_key(key: KeyEvent) -> Option<TuiInput> {
    Some(match key.code {
        KeyCode::Up => TuiInput::Up,
        KeyCode::Down => TuiInput::Down,
        KeyCode::Enter => TuiInput::Enter,
        KeyCode::Esc => TuiInput::Esc,
        KeyCode::Backspace => TuiInput::Backspace,
        KeyCode::Char(c) => TuiInput::Char(c),
        _ => return None,
    })
}

/// Load the catalog into `state` for the initial render, honouring the
/// `--refresh` flag (`ctx.force_refresh`) so a fresh cache is rebuilt when
/// asked. Degrades on any failure.
async fn load_into(ctx: &TuiContext, state: &mut TuiState) {
    reload_into(ctx, state, ctx.force_refresh).await;
}

/// Load or rebuild the catalog into `state`. `force` rebuilds even a
/// fresh cache. Any failure (offline included) degrades to a status-line
/// message and whatever rows are already known — never a crash.
async fn reload_into(ctx: &TuiContext, state: &mut TuiState, force: bool) {
    // The TUI browses a capped window (empty name-scope) and narrows
    // in-memory via the pure state filter; a registry-wide walk is a
    // cut-line shared with `search`.
    match Catalog::load_or_refresh(&ctx.catalog_path, &ctx.registry, "", &ctx.access, ctx.offline, force).await {
        Ok(catalog) => {
            let (lock, install_state) = load_scope_for_badges(ctx);
            let rows = rows_from_catalog(&catalog, lock.as_ref(), &install_state);
            let n = rows.len();
            state.set_rows(rows);
            // Surface whether the browse window hit the cap so the row list
            // is not read as exhaustive (CLI search warns on stderr; the TUI
            // shows a quiet legend-line hint).
            state.set_truncated(catalog.truncated());
            state.set_status(if ctx.offline {
                format!("offline — {n} cached entr{} ", if n == 1 { "y" } else { "ies" })
            } else if n == 0 {
                // An online build that yields nothing is most often a
                // registry whose `_catalog` listing is unsupported or
                // access-restricted (GHCR, Docker Hub) — say so rather than
                // showing a silent blank list, and point at targeted search.
                "0 entries — registry catalog listing may be unsupported or restricted; try `grim search <name>`"
                    .to_string()
            } else {
                format!("{n} entr{}", if n == 1 { "y" } else { "ies" })
            });
        }
        Err(e) => {
            state.set_loading(false);
            state.set_status(format!("catalog unavailable: {e}"));
        }
    }
}

/// Project a catalog into TUI rows, deriving each state from the scope's
/// lock + install-state.
fn rows_from_catalog(catalog: &Catalog, lock: Option<&GrimoireLock>, state: &InstallState) -> Vec<TuiRow> {
    catalog
        .entries()
        .map(|e| {
            let kind = e.kind.clone().unwrap_or_else(|| "-".to_string());
            let row_state = derive_row_state(&kind, &e.registry, &e.repository, lock, state);
            TuiRow {
                kind,
                repo: e.repo(),
                description: e.description.clone().unwrap_or_default(),
                summary: e.summary.clone().unwrap_or_default(),
                keywords: e.keywords.clone(),
                latest_tag: e.latest_tag.clone().unwrap_or_default(),
                // Show the explicit highest version; fall back to the
                // representative tag when no semver tag exists.
                version: e.version.clone().or_else(|| e.latest_tag.clone()).unwrap_or_default(),
                pinned_version: None,
                state: row_state,
            }
        })
        .collect()
}

/// Kind-aware row state: a bundle row aggregates the states of the
/// members its lock provenance names; everything else derives directly
/// from its own lock entry + install record.
fn derive_row_state(
    kind: &str,
    registry: &str,
    repository: &str,
    lock: Option<&GrimoireLock>,
    state: &InstallState,
) -> ArtifactState {
    if row_kind(kind) == ArtifactKind::Bundle {
        derive_bundle_state(&format!("{registry}/{repository}"), lock, state)
    } else {
        derive_artifact_state(registry, repository, lock, state)
    }
}

/// Derive a bundle row's state from the members it contributed to the
/// lock (matched by provenance repo): worst-of aggregation so one broken
/// member surfaces on the bundle row. No members in the lock ⇒ the bundle
/// is not installed.
fn derive_bundle_state(bundle_repo: &str, lock: Option<&GrimoireLock>, state: &InstallState) -> ArtifactState {
    let Some(lock) = lock else {
        return ArtifactState::NotInstalled;
    };
    /// Severity rank for worst-of aggregation: a healthier state never
    /// masks a degraded member.
    fn rank(s: ArtifactState) -> u8 {
        match s {
            ArtifactState::Installed => 0,
            ArtifactState::Outdated => 1,
            ArtifactState::NotInstalled => 2,
            ArtifactState::Modified => 3,
            ArtifactState::IntegrityMissing => 4,
        }
    }
    lock.skills
        .iter()
        .chain(lock.rules.iter())
        .filter(|a| a.bundle.as_deref() == Some(bundle_repo))
        .map(|m| derive_artifact_state(m.pinned.registry(), m.pinned.repository(), Some(lock), state))
        .max_by_key(|s| rank(*s))
        .unwrap_or(ArtifactState::NotInstalled)
}

/// Derive the richer TUI [`ArtifactState`] for `registry/repository`.
///
/// Precedence mirrors `status.rs::derive_state` and
/// `status_badge::derive_badge` — the *only* divergence is that a present
/// install record whose client outputs are missing or unreadable is
/// surfaced as [`ArtifactState::IntegrityMissing`] rather than collapsed
/// into `NotInstalled`, so a broken/tampered install is distinguishable
/// from a never-installed entry. No lock entry or no record at all is
/// still `NotInstalled`.
fn derive_artifact_state(
    registry: &str,
    repository: &str,
    lock: Option<&GrimoireLock>,
    state: &InstallState,
) -> ArtifactState {
    let Some(locked) = lock.and_then(|l| {
        l.skills
            .iter()
            .chain(l.rules.iter())
            .find(|a| a.pinned.registry() == registry && a.pinned.repository() == repository)
    }) else {
        return ArtifactState::NotInstalled;
    };
    let Some(record) = state
        .iter_records()
        .find(|r| r.pinned.registry() == registry && r.pinned.repository() == repository)
    else {
        return ArtifactState::NotInstalled;
    };

    let outputs = record.client_outputs();
    if outputs.iter().any(|o| !o.target.exists()) {
        return ArtifactState::IntegrityMissing;
    }
    for out in &outputs {
        match out.current_hash() {
            Ok(actual) if actual != out.content_hash => return ArtifactState::Modified,
            Ok(_) => {}
            Err(_) => return ArtifactState::IntegrityMissing,
        }
    }
    if record.pinned.eq_content(&locked.pinned) {
        ArtifactState::Installed
    } else {
        ArtifactState::Outdated
    }
}

/// Recompute every row's [`ArtifactState`] against the currently-active
/// scope's lock + install-state (used after a scope toggle — the catalog
/// itself is scope-independent, only the per-row state changes).
fn recompute_states(ctx: &TuiContext, state: &mut TuiState) {
    let (lock, install_state) = load_scope_for_badges(ctx);
    for r in &mut state.rows {
        if let Some((registry, repository)) = split_repo(&r.repo) {
            r.state = derive_row_state(&r.kind, &registry, &repository, lock.as_ref(), &install_state);
        }
    }
}

/// Best-effort scope load for badges (advisory — never fails the TUI).
fn load_scope_for_badges(ctx: &TuiContext) -> (Option<GrimoireLock>, InstallState) {
    let lock = lock_io::load(&ctx.lock_path).ok();
    let state = InstallState::load(&ctx.state_path).unwrap_or_else(|_| InstallState::empty(&ctx.state_path));
    (lock, state)
}

/// Lazily fetch the tag list for `row` and feed it to the open picker.
/// Degrades to a status-line message (and a closed picker) on any failure
/// — never a crash, offline included.
async fn load_versions(ctx: &TuiContext, state: &mut TuiState, row: usize) {
    let Some(r) = state.rows.get(row).cloned() else {
        state.cancel_version();
        return;
    };
    let Some((registry, repository)) = split_repo(&r.repo) else {
        state.set_status(format!("malformed catalog repo: {}", r.repo));
        state.cancel_version();
        return;
    };
    let id = Identifier::new_registry(repository, registry);
    match ctx.access.list_tags(&id).await {
        Ok(Some(tags)) if !tags.is_empty() => state.set_picker_tags(order_tags(tags)),
        Ok(_) => {
            state.set_status(format!("no tags for {}", r.repo));
            state.cancel_version();
        }
        Err(e) => {
            state.set_status(format!("tag lookup failed: {e}"));
            state.cancel_version();
        }
    }
}

/// Order tags for the picker: the moving `latest` pointer first (if
/// present), then concrete semver descending, then everything else
/// lexicographically — so the newest explicit version is near the top.
fn order_tags(tags: Vec<String>) -> Vec<String> {
    let mut latest = Vec::new();
    let mut semver: Vec<(semver::Version, String)> = Vec::new();
    let mut other = Vec::new();
    for t in tags {
        if t == "latest" {
            latest.push(t);
        } else if let Ok(v) = semver::Version::parse(&t.replace('_', "+")) {
            semver.push((v, t));
        } else {
            other.push(t);
        }
    }
    semver.sort_by(|a, b| b.0.cmp(&a.0));
    other.sort();
    latest
        .into_iter()
        .chain(semver.into_iter().map(|(_, t)| t))
        .chain(other)
        .collect()
}

/// Run a batch [`BatchOp`] over `rows` indices (the marked set, or the
/// single selection). Install/update reuse the **same** resolve → lock →
/// materialize path the commands use; uninstall reuses the shared
/// [`crate::install::uninstall`] seam — no forked logic either way. Each
/// row's state is refreshed; the status line aggregates `n ok, m failed`.
async fn run_batch(ctx: &TuiContext, state: &mut TuiState, rows: &[usize], op: BatchOp) {
    // Install/update need the network; uninstall is purely local.
    if ctx.offline && op != BatchOp::Uninstall {
        state.set_status("offline — cannot install/update");
        return;
    }
    let (verb, verbed) = match op {
        BatchOp::Install => ("install", "installed"),
        BatchOp::Update => ("update", "updated"),
        BatchOp::Uninstall => ("uninstall", "uninstalled"),
    };
    let total = rows.len();
    let (mut ok, mut failed) = (0usize, 0usize);
    let mut last_err: Option<String> = None;

    for (n, &i) in rows.iter().enumerate() {
        let Some(row) = state.rows.get(i).cloned() else {
            continue;
        };
        state.set_status(format!("{verb} {}/{total}: {}…", n + 1, row.repo));
        let outcome = match op {
            BatchOp::Install => perform(ctx, &row, false).await.map(|_| ()),
            BatchOp::Update => perform(ctx, &row, true).await.map(|_| ()),
            BatchOp::Uninstall => perform_uninstall(ctx, &row),
        };
        match outcome {
            Ok(()) => {
                ok += 1;
                let (lock, install_state) = load_scope_for_badges(ctx);
                if let Some((registry, repository)) = split_repo(&row.repo)
                    && let Some(r) = state.rows.get_mut(i)
                {
                    r.state = derive_row_state(&r.kind, &registry, &repository, lock.as_ref(), &install_state);
                }
            }
            Err(e) => {
                failed += 1;
                last_err = Some(format!("{}: {e}", row.repo));
            }
        }
    }

    // A completed batch consumes the marks (they describe past intent).
    state.clear_marks();
    state.set_status(match (total, failed, last_err) {
        (1, 0, _) => format!("{verbed} ({ok} ok)"),
        (_, 0, _) => format!("{verbed} {ok}/{total}"),
        (_, _, Some(err)) => format!("{verbed} {ok}/{total}, {failed} failed — {err}"),
        (_, _, None) => format!("{verbed} {ok}/{total}, {failed} failed"),
    });
}

/// Uninstall one catalog row through the shared seams: delete the
/// materialized files and drop the install-state record
/// ([`crate::install::uninstall`]), then undeclare the entry from the
/// config + lock ([`undeclare_and_unlock`]) — the full inverse of the
/// install action, which declares like `grim add`. Lock entries written
/// by the TUI before it declared installs are dropped the same way. A
/// bundle row expands into the member records its lock provenance names;
/// the undeclare seam then drops the `[bundles]` entry and evicts the
/// members from the lock.
fn perform_uninstall(ctx: &TuiContext, row: &TuiRow) -> anyhow::Result<()> {
    let (_registry, repository) =
        split_repo(&row.repo).ok_or_else(|| anyhow::anyhow!("malformed catalog repo: {}", row.repo))?;
    let kind = row_kind(&row.kind);
    let name = repository.rsplit('/').next().unwrap_or(&repository).to_string();

    // The install-state records this row owns: itself for a skill/rule;
    // for a bundle, every lock member stamped with this repo's provenance
    // (computed BEFORE the undeclare below evicts them from the lock).
    let targets: Vec<(ArtifactKind, String)> = match kind {
        ArtifactKind::Bundle => lock_io::load(&ctx.lock_path)
            .map(|lock| {
                lock.skills
                    .iter()
                    .chain(lock.rules.iter())
                    .filter(|a| a.bundle.as_deref() == Some(row.repo.as_str()))
                    .map(|a| (a.kind, a.name.clone()))
                    .collect()
            })
            .unwrap_or_default(),
        _ => vec![(kind, name.clone())],
    };

    let mut install_state =
        InstallState::load(&ctx.state_path).map_err(|e| anyhow::anyhow!("install-state load failed: {e}"))?;
    let mut involved_clients: Vec<crate::install::client_target::ClientTarget> = Vec::new();
    let mut any_removed = false;
    for (target_kind, target_name) in &targets {
        for client in install_state
            .get(*target_kind, target_name)
            .map(|r| {
                r.client_outputs()
                    .iter()
                    .filter_map(|c| c.client.parse().ok())
                    .collect::<Vec<crate::install::client_target::ClientTarget>>()
            })
            .unwrap_or_default()
        {
            if !involved_clients.contains(&client) {
                involved_clients.push(client);
            }
        }
        let result = crate::install::uninstall::uninstall(&mut install_state, *target_kind, target_name)
            .map_err(|e| anyhow::anyhow!("uninstall failed: {e}"))?;
        any_removed |= result.outcome == crate::install::uninstall::UninstallOutcome::Removed;
    }
    if any_removed {
        install_state
            .save()
            .map_err(|e| anyhow::anyhow!("install-state save failed: {e}"))?;
    }
    // Converge vendor-owned config for every client the removed record
    // carried, mirroring `command::uninstall`. `with_context` keeps the
    // `io::Error` source chain (kind, config path) intact for the popup.
    for client in involved_clients {
        client
            .vendor()
            .sync_config(&install_state, &ctx.workspace, ctx.scope)
            .with_context(|| format!("vendor config sync failed for client '{client}'"))?;
    }

    // Undeclare from the config + lock through the `grim uninstall` seam
    // (config flock held for the read-modify-write), so the badge no
    // longer derives "installed" and a later `grim install` does not
    // silently bring the entry back.
    let _guard = match ctx.config_path.exists() {
        true => Some(grim(ConfigFileLock::try_acquire(&ctx.config_path))?),
        false => None,
    };
    let (options, mut set) = load_scope_declaration(ctx)?;
    undeclare_and_unlock(&ctx.config_path, &ctx.lock_path, &options, &mut set, kind, &name)?;
    Ok(())
}

/// Human label for an install outcome (status-line only).
fn outcome_label(o: &InstallOutcome) -> &'static str {
    match o {
        InstallOutcome::Installed => "installed",
        InstallOutcome::Updated => "updated",
        InstallOutcome::AlreadyInstalled => "unchanged",
        InstallOutcome::Skipped(_) => "skipped",
        InstallOutcome::Refused { .. } => "refused (locally modified)",
    }
}

/// Resolve + materialize one catalog repo through the shared path.
///
/// Mirrors `grim add` + a single-artifact `grim install`: the entry is
/// **declared** in the scope's `grimoire.toml` under the config flock,
/// relocked through the same partial-relock seam `add` uses (so the lock's
/// declaration hash always matches the config — a TUI install is never an
/// undeclared lock entry), and then only the acted-on artifact is
/// materialized.
async fn perform(ctx: &TuiContext, row: &TuiRow, is_update: bool) -> anyhow::Result<String> {
    let (registry, repository) =
        split_repo(&row.repo).ok_or_else(|| anyhow::anyhow!("malformed catalog repo: {}", row.repo))?;

    let kind = row_kind(&row.kind);
    let name = repository.rsplit('/').next().unwrap_or(&repository).to_string();
    // A user-pinned version (chosen in the picker) wins; otherwise the
    // representative tag, otherwise the conventional `latest`.
    let tag = row
        .pinned_version
        .clone()
        .filter(|t| !t.is_empty())
        .or_else(|| Some(row.latest_tag.clone()).filter(|t| !t.is_empty()))
        .unwrap_or_else(|| "latest".to_string());
    let id = Identifier::new_registry(repository.clone(), registry).clone_with_tag(tag.clone());

    // Declare + relock under the config flock, exactly like `grim add`
    // (the declaration is re-read fresh — the config can change while the
    // TUI runs). A repeated install of the same entry is an idempotent
    // overwrite. The shared `declare` seam routes a bundle into
    // `[bundles]`, and `relock_declared` full-resolves it so its members
    // expand into the lock.
    let _guard = match ctx.config_path.exists() {
        true => Some(grim(ConfigFileLock::try_acquire(&ctx.config_path))?),
        false => None,
    };
    let (options, mut set) = load_scope_declaration(ctx)?;
    declare(&mut set, kind, name.clone(), id);
    grim(write_config(&ctx.config_path, &options, &set))?;

    let previous = lock_io::load(&ctx.lock_path).ok();
    let new_lock = grim(relock_declared(&set, previous.as_ref(), kind, &name, &ctx.access, ctx.scope).await)?;
    grim(lock_io::save(&ctx.lock_path, &new_lock, previous.as_ref()))?;

    // Materialize only the acted-on artifact — the rest of the (now
    // complete) lock belongs to `grim install`, not a single-row action.
    // A bundle materializes exactly the members it contributed (matched by
    // lock provenance), never a blob of its own.
    let single = match kind {
        ArtifactKind::Bundle => bundle_members_lock(&new_lock, &row.repo, &tag),
        _ => single_entry_lock(&new_lock, kind, &name)
            .ok_or_else(|| anyhow::anyhow!("resolved lock is missing '{name}'"))?,
    };

    let target = InstallTarget::parse(&ctx.workspace, ctx.scope, &[], &ctx.clients_default)
        .map_err(|e| anyhow::Error::from(crate::error::Error::from(e)))?;
    let mut install_state =
        InstallState::load(&ctx.state_path).map_err(|e| anyhow::anyhow!("install-state load failed: {e}"))?;
    let materializer = DefaultMaterializer;

    // `update` forces re-materialization (rolling-release contract),
    // matching `command::update`; `install` honours the integrity gate.
    let outcomes = install_all(
        &single,
        &ctx.access,
        &materializer,
        &target,
        &mut install_state,
        is_update,
    )
    .await;
    install_state
        .save()
        .map_err(|e| anyhow::anyhow!("install-state save failed: {e}"))?;

    // Converge vendor-owned config on the new state, mirroring
    // `command::install`. `with_context` keeps the `io::Error` source
    // chain (kind, config path) intact for the popup.
    for client in target.clients() {
        client
            .vendor()
            .sync_config(&install_state, &ctx.workspace, ctx.scope)
            .with_context(|| format!("vendor config sync failed for client '{client}'"))?;
    }

    let mut label = "unchanged".to_string();
    for o in outcomes {
        match o.result {
            Ok(outcome) => label = outcome_label(&outcome).to_string(),
            Err(e) => return Err(anyhow::Error::from(e)),
        }
    }
    Ok(label)
}

/// Load the active scope's declaration fresh from disk: the config can
/// change while the TUI runs (another `grim add`, an editor), so each
/// install/uninstall re-reads rather than caching a parse from startup.
/// A missing global config is an empty declaration (mirroring
/// `scope_resolution::resolve`); a missing project config is an error.
fn load_scope_declaration(ctx: &TuiContext) -> anyhow::Result<(ConfigOptions, DesiredSet)> {
    match ctx.scope {
        ConfigScope::Global => {
            let cfg = grim(GlobalConfig::load(&ctx.config_path))?;
            Ok((cfg.options, cfg.set))
        }
        ConfigScope::Project => {
            let discovered = grim(ProjectConfig::discover(Some(&ctx.config_path)))?;
            Ok((discovered.config.options, discovered.config.set))
        }
    }
}

/// Map a catalog row's kind string (`skill`/`rule`/`bundle`) onto the
/// typed artifact kind. Unknown / `-` defaults to skill (a directory
/// artifact); the materializer validates the actual payload shape.
fn row_kind(kind: &str) -> ArtifactKind {
    ArtifactKind::from_kind_str(kind).unwrap_or(ArtifactKind::Skill)
}

/// Project the members the bundle `bundle_repo:bundle_tag` contributed out
/// of `lock` as a members-only lock (same metadata), so the shared
/// `install_all` path materializes exactly the acted-on bundle's members.
/// Members are matched by the provenance the resolver stamps
/// ([`LockedArtifact::bundle`] / [`LockedArtifact::bundle_tag`]); an empty
/// projection means the bundle resolved to zero members (or every member
/// was overridden by a direct declaration).
fn bundle_members_lock(lock: &GrimoireLock, bundle_repo: &str, bundle_tag: &str) -> GrimoireLock {
    let is_member =
        |a: &LockedArtifact| a.bundle.as_deref() == Some(bundle_repo) && a.bundle_tag.as_deref() == Some(bundle_tag);
    GrimoireLock {
        metadata: lock.metadata.clone(),
        skills: lock.skills.iter().filter(|a| is_member(a)).cloned().collect(),
        rules: lock.rules.iter().filter(|a| is_member(a)).cloned().collect(),
    }
}

/// Project the single `kind`/`name` entry out of `lock` as a one-artifact
/// lock (same metadata), so the shared `install_all` path materializes
/// exactly the acted-on row and nothing else. `None` when the entry is
/// absent from the resolved lock (defensive — not expected). Bundle rows
/// go through [`bundle_members_lock`] instead.
fn single_entry_lock(lock: &GrimoireLock, kind: ArtifactKind, name: &str) -> Option<GrimoireLock> {
    let entry = lock
        .skills
        .iter()
        .chain(lock.rules.iter())
        .find(|a| a.kind == kind && a.name == name)
        .cloned()?;
    let (skills, rules) = match kind {
        ArtifactKind::Skill => (vec![entry], Vec::new()),
        ArtifactKind::Rule => (Vec::new(), vec![entry]),
        ArtifactKind::Bundle => return None,
    };
    Some(GrimoireLock {
        metadata: lock.metadata.clone(),
        skills,
        rules,
    })
}

/// Split `registry/repository` at the first `/`.
fn split_repo(repo: &str) -> Option<(String, String)> {
    repo.split_once('/').map(|(r, p)| (r.to_string(), p.to_string()))
}

/// The display names of the active scope's effective selected clients
/// (`claude`, `opencode`, …), in [`crate::install::client_target::ClientTarget::ALL`]
/// order, for the status area.
fn client_names(ctx: &TuiContext) -> Vec<String> {
    ctx.clients_selected.iter().map(ToString::to_string).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_repo_splits_first_slash_only() {
        assert_eq!(
            split_repo("localhost:5000/acme/code-review"),
            Some(("localhost:5000".to_string(), "acme/code-review".to_string()))
        );
        assert_eq!(split_repo("noslash"), None);
    }

    #[test]
    fn map_key_covers_the_alphabet() {
        let mk = |code| KeyEvent::new(code, crossterm::event::KeyModifiers::NONE);
        assert_eq!(map_key(mk(KeyCode::Up)), Some(TuiInput::Up));
        assert_eq!(map_key(mk(KeyCode::Down)), Some(TuiInput::Down));
        assert_eq!(map_key(mk(KeyCode::Enter)), Some(TuiInput::Enter));
        assert_eq!(map_key(mk(KeyCode::Esc)), Some(TuiInput::Esc));
        assert_eq!(map_key(mk(KeyCode::Backspace)), Some(TuiInput::Backspace));
        assert_eq!(map_key(mk(KeyCode::Char('i'))), Some(TuiInput::Char('i')));
        assert_eq!(map_key(mk(KeyCode::Tab)), None);
    }

    fn installed_row(repo: &str) -> TuiRow {
        TuiRow {
            kind: "skill".to_string(),
            repo: repo.to_string(),
            description: String::new(),
            summary: String::new(),
            keywords: Vec::new(),
            latest_tag: "latest".to_string(),
            version: "1.0.0".to_string(),
            pinned_version: None,
            state: ArtifactState::Installed,
        }
    }

    #[test]
    fn fresh_generation_flips_row_stale_is_discarded() {
        let mut s = TuiState::new();
        s.set_rows(vec![installed_row("r/a")]);

        // A stale-stamped result (scheduled under generation 0 but the live
        // generation has advanced to 1) must NOT flip the row.
        assert!(
            !apply_outdated_if_fresh(&mut s, "r/a", 0, 1),
            "a stale-generation result is discarded"
        );
        assert_eq!(
            s.rows[0].state,
            ArtifactState::Installed,
            "the row keeps its state across a stale result"
        );

        // A matching-generation result flips the row.
        assert!(
            apply_outdated_if_fresh(&mut s, "r/a", 1, 1),
            "a fresh-generation result flips the row"
        );
        assert_eq!(s.rows[0].state, ArtifactState::Outdated);
    }

    #[test]
    fn catalog_ready_stamp_freshness_gates_the_merge() {
        // The same predicate that guards per-row flips guards the CatalogReady
        // drain arm: a catalog walked under a superseded generation is stale
        // and must be discarded so it cannot resurrect the wrong scope's rows
        // after a scope toggle / refresh; a matching stamp is reconciled.
        assert!(
            !is_generation_fresh(0, 1),
            "a catalog stamped under the old generation is discarded"
        );
        assert!(
            is_generation_fresh(1, 1),
            "a catalog stamped under the live generation is reconciled"
        );
    }

    fn sha(byte: char) -> String {
        std::iter::repeat_n(byte, 64).collect()
    }

    /// A locked artifact pinned under registry `r` with `repository == name`,
    /// matching the `installed_row("r/<name>")` fixture shape.
    fn locked(name: &str, kind: ArtifactKind, byte: char) -> crate::lock::locked_artifact::LockedArtifact {
        let id = Identifier::new_registry(name, "r").clone_with_digest(crate::oci::Digest::Sha256(sha(byte)));
        crate::lock::locked_artifact::LockedArtifact::direct(
            name.to_string(),
            kind,
            crate::oci::PinnedIdentifier::try_from(id).unwrap(),
        )
    }

    fn lock_fixture(
        skills: Vec<crate::lock::locked_artifact::LockedArtifact>,
        rules: Vec<crate::lock::locked_artifact::LockedArtifact>,
    ) -> GrimoireLock {
        GrimoireLock {
            metadata: crate::lock::grimoire_lock::LockMetadata {
                lock_version: crate::lock::lock_version::LockVersion::V1,
                declaration_hash_version: 1,
                declaration_hash: format!("sha256:{}", sha('d')),
                generated_by: "grim test".to_string(),
                generated_at: "2026-06-11T00:00:00Z".to_string(),
            },
            skills,
            rules,
        }
    }

    #[test]
    fn post_batch_checks_selects_only_eligible_locked_rows() {
        // Regression: after an install of an old version the acted-on row
        // must be selected for an immediate registry re-check (that check is
        // what flips the badge to `outdated` without a manual refresh) —
        // while ineligible rows, unlocked rows, and out-of-range indices
        // contribute nothing.
        let lock = lock_fixture(vec![locked("a", ArtifactKind::Skill, '1')], Vec::new());
        let mut not_installed = installed_row("r/b");
        not_installed.state = ArtifactState::NotInstalled;
        let rows = vec![installed_row("r/a"), not_installed, installed_row("r/unlocked")];

        let checks = post_batch_checks(&lock, &rows, &[0, 1, 2, 99]);

        assert_eq!(checks.len(), 1, "only the installed + locked row is rechecked");
        assert_eq!(checks[0].repo, "r/a");
        assert_eq!(checks[0].locked_digest, crate::oci::Digest::Sha256(sha('1')));
    }

    #[test]
    fn single_entry_lock_projects_one_artifact_with_metadata() {
        let lock = lock_fixture(
            vec![
                locked("a", ArtifactKind::Skill, '1'),
                locked("b", ArtifactKind::Skill, '2'),
            ],
            vec![locked("c", ArtifactKind::Rule, '3')],
        );

        let single = single_entry_lock(&lock, ArtifactKind::Skill, "b").expect("entry exists");
        assert_eq!(single.skills.len(), 1);
        assert_eq!(single.skills[0].name, "b");
        assert!(single.rules.is_empty());
        assert_eq!(single.metadata, lock.metadata, "metadata carries over unchanged");

        let rule = single_entry_lock(&lock, ArtifactKind::Rule, "c").expect("rule entry exists");
        assert!(rule.skills.is_empty());
        assert_eq!(rule.rules.len(), 1);

        assert!(
            single_entry_lock(&lock, ArtifactKind::Skill, "missing").is_none(),
            "an absent entry projects to None"
        );
    }

    /// Publish a member skill (tar layer) and a bundle whose members-layer
    /// references it into a [`MemoryRegistry`], mirroring what
    /// `grim release` produces.
    async fn registry_with_bundle() -> Arc<dyn OciAccess> {
        use crate::oci::Algorithm;
        use crate::oci::access::memory_registry::MemoryRegistry;
        use crate::oci::bundle::{BUNDLE_LAYER_MEDIA_TYPE, BundleManifest, BundleMember};
        use crate::oci::manifest::{Descriptor, OciManifest};

        let reg = MemoryRegistry::new();

        // The member skill: a tar tree rooted at `demo/`.
        let body: &[u8] = b"---\nname: demo\ndescription: d\n---\n";
        let mut header = tar::Header::new_gnu();
        header.set_size(body.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        let mut builder = tar::Builder::new(Vec::new());
        builder.append_data(&mut header, "demo/SKILL.md", body).unwrap();
        let tar_blob = builder.into_inner().unwrap();

        let skill_repo = Identifier::new_registry("grimoire/skills/demo", "localhost:5050");
        let skill_layer = reg.push_blob(&skill_repo, &tar_blob).await.unwrap();
        let skill_manifest = OciManifest {
            media_type: Some("application/vnd.oci.image.manifest.v1+json".to_string()),
            artifact_type: Some(ArtifactKind::Skill.artifact_type().to_string()),
            config_media_type: Some(ArtifactKind::Skill.config_media_type().to_string()),
            layers: vec![Descriptor {
                digest: skill_layer,
                media_type: "application/vnd.grimoire.artifact.layer.v1.tar".to_string(),
                size: tar_blob.len() as u64,
            }],
            annotations: Default::default(),
        };
        let skill_digest = reg.push_manifest(&skill_repo, &skill_manifest).await.unwrap();
        reg.put_tag(&skill_repo, "1.0.0", &skill_digest).await.unwrap();

        // The bundle: a single members-layer naming the skill.
        let members = BundleManifest::new(vec![BundleMember {
            kind: ArtifactKind::Skill,
            name: "demo".to_string(),
            id: "localhost:5050/grimoire/skills/demo:1.0.0".to_string(),
        }]);
        let members_blob = members.to_layer_bytes().unwrap();
        let bundle_repo = Identifier::new_registry("grimoire/bundles/starter-pack", "localhost:5050");
        let members_layer = reg.push_blob(&bundle_repo, &members_blob).await.unwrap();
        assert_eq!(members_layer, Algorithm::Sha256.hash(&members_blob));
        let bundle_manifest = OciManifest {
            media_type: Some("application/vnd.oci.image.manifest.v1+json".to_string()),
            artifact_type: Some(ArtifactKind::Bundle.artifact_type().to_string()),
            config_media_type: Some(ArtifactKind::Bundle.config_media_type().to_string()),
            layers: vec![Descriptor {
                digest: members_layer,
                media_type: BUNDLE_LAYER_MEDIA_TYPE.to_string(),
                size: members_blob.len() as u64,
            }],
            annotations: Default::default(),
        };
        let bundle_digest = reg.push_manifest(&bundle_repo, &bundle_manifest).await.unwrap();
        reg.put_tag(&bundle_repo, "latest", &bundle_digest).await.unwrap();

        Arc::new(reg)
    }

    /// A project-scope [`TuiContext`] rooted at `workspace`, wired to
    /// `access` and targeting the claude client.
    fn test_ctx(workspace: &std::path::Path, access: Arc<dyn OciAccess>) -> TuiContext {
        TuiContext {
            registry: "localhost:5050".to_string(),
            catalog_path: workspace.join("catalog.json"),
            access,
            offline: false,
            force_refresh: false,
            scope: ConfigScope::Project,
            workspace: workspace.to_path_buf(),
            lock_path: workspace.join("grimoire.lock"),
            state_path: workspace.join("install-state.json"),
            config_path: workspace.join("grimoire.toml"),
            clients_default: vec!["claude".to_string()],
            clients_selected: Vec::new(),
            scope_label: "project".to_string(),
            alt: None,
        }
    }

    #[tokio::test]
    async fn perform_installs_bundle_members_not_the_bundle_blob() {
        // Regression: a catalog bundle row must install like `grim add`
        // (declared under `[bundles]`, expanded into provenance-stamped
        // members, members materialized) — NOT be coerced to a skill,
        // which declared the bundle under `[skills]` and fed the bundle's
        // JSON members-layer to the tar materializer ("cannot read tar
        // entry: failed to read entire block").
        let tmp = tempfile::tempdir().unwrap();
        let workspace = tmp.path();
        std::fs::write(workspace.join("grimoire.toml"), "[skills]\n\n[rules]\n").unwrap();
        let ctx = test_ctx(workspace, registry_with_bundle().await);

        let mut row = installed_row("localhost:5050/grimoire/bundles/starter-pack");
        row.kind = "bundle".to_string();
        row.state = ArtifactState::NotInstalled;

        let label = perform(&ctx, &row, false).await.expect("bundle install succeeds");
        assert_eq!(label, "installed");

        // Declared under [bundles], never [skills].
        let body = std::fs::read_to_string(&ctx.config_path).unwrap();
        let cfg = ProjectConfig::from_toml_str(&body).expect("config parses");
        assert!(
            cfg.set.bundles.contains_key("starter-pack"),
            "bundle declared in [bundles]: {body}"
        );
        assert!(cfg.set.skills.is_empty(), "bundle must not land in [skills]: {body}");

        // The lock carries the provenance-stamped member, not the bundle.
        let lock = lock_io::load(&ctx.lock_path).expect("lock saved");
        assert_eq!(lock.skills.len(), 1);
        assert_eq!(lock.skills[0].name, "demo");
        assert_eq!(
            lock.skills[0].bundle.as_deref(),
            Some("localhost:5050/grimoire/bundles/starter-pack")
        );
        assert_eq!(lock.skills[0].bundle_tag.as_deref(), Some("latest"));

        // The member skill materialized into the claude target.
        assert!(
            workspace.join(".claude/skills/demo/SKILL.md").is_file(),
            "member skill files must exist"
        );

        // The bundle row badge derives `installed` from its members.
        let (lock, install_state) = load_scope_for_badges(&ctx);
        assert_eq!(
            derive_row_state(
                "bundle",
                "localhost:5050",
                "grimoire/bundles/starter-pack",
                lock.as_ref(),
                &install_state
            ),
            ArtifactState::Installed
        );
    }

    #[tokio::test]
    async fn perform_uninstall_removes_bundle_members_and_declaration() {
        // The full inverse: deleting an installed bundle row removes the
        // member files + records, drops the `[bundles]` declaration, and
        // evicts the provenance-stamped members from the lock.
        let tmp = tempfile::tempdir().unwrap();
        let workspace = tmp.path();
        std::fs::write(workspace.join("grimoire.toml"), "[skills]\n\n[rules]\n").unwrap();
        let ctx = test_ctx(workspace, registry_with_bundle().await);

        let mut row = installed_row("localhost:5050/grimoire/bundles/starter-pack");
        row.kind = "bundle".to_string();
        row.state = ArtifactState::NotInstalled;
        perform(&ctx, &row, false).await.expect("bundle install succeeds");
        assert!(workspace.join(".claude/skills/demo/SKILL.md").is_file());

        perform_uninstall(&ctx, &row).expect("bundle uninstall succeeds");

        assert!(
            !workspace.join(".claude/skills/demo").exists(),
            "member files must be deleted"
        );
        let body = std::fs::read_to_string(&ctx.config_path).unwrap();
        let cfg = ProjectConfig::from_toml_str(&body).expect("config parses");
        assert!(cfg.set.bundles.is_empty(), "bundle must be undeclared: {body}");
        let lock = lock_io::load(&ctx.lock_path).expect("lock saved");
        assert!(lock.skills.is_empty(), "members must be evicted from the lock");

        let (lock, install_state) = load_scope_for_badges(&ctx);
        assert_eq!(
            derive_row_state(
                "bundle",
                "localhost:5050",
                "grimoire/bundles/starter-pack",
                lock.as_ref(),
                &install_state
            ),
            ArtifactState::NotInstalled
        );
    }

    #[test]
    fn row_kind_maps_every_catalog_kind() {
        assert_eq!(row_kind("skill"), ArtifactKind::Skill);
        assert_eq!(row_kind("rule"), ArtifactKind::Rule);
        assert_eq!(row_kind("bundle"), ArtifactKind::Bundle);
        // Unknown / absent kind defaults to skill; the materializer
        // validates the actual payload shape.
        assert_eq!(row_kind("-"), ArtifactKind::Skill);
    }

    #[test]
    fn bundle_members_lock_projects_by_provenance_repo_and_tag() {
        let mut member = locked("member", ArtifactKind::Skill, '4');
        member.bundle = Some("r/bundles/pack".to_string());
        member.bundle_tag = Some("latest".to_string());
        let mut other_tag = locked("other", ArtifactKind::Skill, '5');
        other_tag.bundle = Some("r/bundles/pack".to_string());
        other_tag.bundle_tag = Some("v2".to_string());
        let mut rule_member = locked("rmember", ArtifactKind::Rule, '6');
        rule_member.bundle = Some("r/bundles/pack".to_string());
        rule_member.bundle_tag = Some("latest".to_string());
        let direct = locked("direct", ArtifactKind::Skill, '7');

        let lock = lock_fixture(vec![member, other_tag, direct], vec![rule_member]);
        let projected = bundle_members_lock(&lock, "r/bundles/pack", "latest");

        assert_eq!(projected.skills.len(), 1, "only the latest-tag member projects");
        assert_eq!(projected.skills[0].name, "member");
        assert_eq!(projected.rules.len(), 1);
        assert_eq!(projected.rules[0].name, "rmember");
        assert_eq!(projected.metadata, lock.metadata, "metadata carries over unchanged");

        let empty = bundle_members_lock(&lock, "r/bundles/unknown", "latest");
        assert!(empty.skills.is_empty() && empty.rules.is_empty());
    }

    #[test]
    fn outcome_label_covers_every_variant() {
        assert_eq!(outcome_label(&InstallOutcome::Installed), "installed");
        assert_eq!(outcome_label(&InstallOutcome::Updated), "updated");
        assert_eq!(outcome_label(&InstallOutcome::AlreadyInstalled), "unchanged");
        assert_eq!(outcome_label(&InstallOutcome::Skipped("x".to_string())), "skipped");
        assert_eq!(
            outcome_label(&InstallOutcome::Refused {
                recorded: crate::oci::Digest::Sha256("a".repeat(64)),
                actual: crate::oci::Digest::Sha256("b".repeat(64)),
            }),
            "refused (locally modified)"
        );
    }
}
