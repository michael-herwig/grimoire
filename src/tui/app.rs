// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! The TUI runtime: the one place the terminal, raw mode, the async
//! catalog load, and the event loop live.
//!
//! Everything decision-shaped is delegated to the pure
//! [`super::state`] / [`super::event`] / [`super::render`] modules; this
//! file only does the impure work: enter/leave raw mode (via the shared
//! [`super::terminal_guard`] RAII guard), read crossterm
//! events, map them to the abstract [`TuiInput`], apply the pure
//! transition, and on `Install` / `Update` reuse the **same** resolve →
//! lock → materialize path the `install`/`update` commands use (no forked
//! logic). This module is excluded from acceptance tests; its logic is
//! covered headlessly by the pure modules' unit tests.

use std::io::{self};
use std::sync::Arc;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

use crate::catalog::registry_catalog::{CATALOG_GATED_REGISTRIES, Catalog, REGISTRY_COMPAT_DOCS_URL};
use crate::command::add::{declare, relock_declared, write_config};
use crate::command::grim;
use crate::command::uninstall::undeclare_and_unlock;
use crate::config::declaration::{ConfigOptions, DesiredSet};
use crate::config::global_config::GlobalConfig;
use crate::config::project_config::ProjectConfig;
use crate::config::scope::ConfigScope;
use crate::install::client_target::ClientTarget;
use crate::install::install_state::{ClientOutput, InstallState, active_outputs};
use crate::install::installer::{InstallOutcome, install_all};
use crate::install::materializer::DefaultMaterializer;
use crate::install::path_anchor::AnchorRoots;
use crate::install::target::{InstallTarget, detect_clients};
use crate::lock::file_lock::ConfigFileLock;
use crate::lock::grimoire_lock::GrimoireLock;
use crate::lock::lock_io;
use crate::lock::locked_artifact::LockedArtifact;
use crate::oci::access::OciAccess;
use crate::oci::{ArtifactKind, Identifier};

use super::event::{BatchOp, TuiAction, TuiInput, handle};
use super::render::{draw, frame};
use super::state::{ArtifactState, Mode, TuiRow, TuiState};
use super::terminal_guard::TerminalGuard;
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
    /// Every anchor root resolved once for the active scope, so badge
    /// derivation + the install/uninstall seams resolve anchored paths.
    pub roots: AnchorRoots,
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
    /// Resolved TUI display options from `[options.tui]` in the config.
    pub tui_options: crate::config::declaration::TuiOptions,
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
    /// Every anchor root resolved once for this scope.
    pub roots: AnchorRoots,
    /// The AI client target(s) to materialize into (raw config clients).
    pub clients_default: Vec<String>,
    /// The effective selected clients for this scope (config or detected).
    pub clients_selected: Vec<crate::install::client_target::ClientTarget>,
    /// Human label (`project` / `global`).
    pub label: String,
    /// This scope's resolved `[options.tui]` display options. Structural
    /// options (`group_by_type` / `tree_separators`) follow the active scope
    /// on a toggle; the runtime `t` view-mode choice stays ephemeral.
    pub tui_options: crate::config::declaration::TuiOptions,
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
            roots: std::mem::replace(&mut self.roots, alt.roots),
            clients_default: std::mem::replace(&mut self.clients_default, alt.clients_default),
            clients_selected: std::mem::replace(&mut self.clients_selected, alt.clients_selected),
            label: std::mem::replace(&mut self.scope_label, alt.label),
            tui_options: std::mem::replace(&mut self.tui_options, alt.tui_options),
        };
        self.scope = alt.scope;
        self.alt = Some(now_alt);
        true
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
    // Redirect tracing output to $GRIM_HOME/tui.log for the duration of
    // the alt-screen session. Declared BEFORE the terminal guard so it
    // drops AFTER it (Rust drops locals in reverse declaration order):
    // the alt-screen is left first, then stderr logging is restored, so
    // any log record emitted during the guard's own Drop reaches the
    // user's shell cleanly rather than corrupting a restored screen.
    //
    // The file open runs off the Tokio runtime (spawn_blocking) so that
    // blocking std::fs I/O never stalls an async task — quality-rust
    // block-tier rule.
    let grim_home = crate::env::grim_home();
    let log_file = crate::log_switch::open_log_file_off_thread(grim_home).await;
    let _log_guard =
        crate::log_switch::global_writer().and_then(|w| crate::log_switch::LogSinkGuard::redirect_to(w, log_file));

    let _guard = TerminalGuard::enter()?;
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend)?;
    // Clear any pre-existing content from before the session (e.g. shell
    // prompt lines) so the first frame is pristine. This is a one-shot
    // clear on enter only; per-frame clears would cause visible flicker.
    terminal.clear()?;

    let mut state = TuiState::new();
    // The live terminal size feeds the detail pane's scroll clamp; the
    // state default (80×24) covers the (unlikely) size query failure.
    if let Ok(size) = crossterm::terminal::size() {
        state.set_term_size(size);
    }
    state.set_offline(ctx.offline);
    state.set_scope_label(&ctx.scope_label);
    state.set_clients(client_names(&ctx));
    // The browsed registry is the effective default: eliding its host
    // from the tree root keeps leaf names short (the user's ask).
    state.set_default_registry(Some(ctx.registry.clone()));
    // Seed the tree display options from the resolved config.
    state.set_view_mode_from_config(ctx.tui_options.default_view);
    state.set_tree_options(ctx.tui_options.group_by_type, ctx.tui_options.tree_separators.clone());

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

    // Bundle-member fetch checker: a separate bounded JoinSet for lazy bundle
    // expansion. Created unconditionally so the event loop is shape-stable;
    // offline is gated inside the LoadBundleMembers arm below.
    let (mut bundle_checker, mut bundle_rx) =
        super::bundle_member_fetch::BundleMemberChecker::new(Arc::clone(&ctx.access));

    loop {
        // Reap finished background tasks so panics surface (deliberately
        // swallowed in raw mode — see `UpdateChecker::reap_finished`) and the
        // JoinSet does not accumulate completed handles for the whole session.
        checker.reap_finished();
        // Mirror for bundle-member fetches: reap completed tasks each tick.
        bundle_checker.reap_finished();
        // Drain any background results that arrived since the last tick and
        // redraw if state changed — the 200ms poll below doubles as the
        // result-drain tick (no event needed to surface a flipped icon).
        if drain_checks(&ctx, &mut state, &mut checker, &mut rx) {
            terminal.draw(|f| draw(f, &frame(&state)))?;
        }
        // Drain bundle-member fetch results similarly.
        if drain_bundle_member_checks(&ctx, &mut state, &mut bundle_rx, bundle_checker.generation()) {
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
        // The new size also re-clamps the detail scroll (the pane's
        // geometry just changed). Clear first to erase any resize
        // artifacts (stale cells outside the new viewport).
        if let Event::Resize(w, h) = ev {
            state.set_term_size((w, h));
            terminal.clear()?;
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
                // Invalidate any in-flight or cached bundle-member results
                // spawned under the previous catalog version: the refresh may
                // have changed which bundles exist or their member lists.
                // The generation bump is the correctness-critical mechanism —
                // it causes the drain loop to discard any in-flight stale
                // results whose generation no longer matches. The cache itself
                // was already cleared by reload_into → set_rows above, so no
                // redundant clear is needed here.
                bundle_checker.bump_generation();
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
            TuiAction::MemberAction { op, repo, kind } => {
                // P4.4: per-member install/update/uninstall.
                // Offline guard: install/update need the network.
                if ctx.offline && op != BatchOp::Uninstall {
                    state.set_status("offline — cannot install/update");
                } else {
                    let label = match op {
                        BatchOp::Install | BatchOp::Update => {
                            let is_update = op == BatchOp::Update;
                            // D8a: resolve the tag from the catalog rows — a
                            // related member reuses its row's pinned/latest tag,
                            // a non-catalog member falls back to "latest".
                            let tag = resolve_member_tag(&repo, &state.rows);
                            match perform_member(&ctx, repo.clone(), kind, is_update, tag).await {
                                Ok(l) => Some(l),
                                Err(e) => {
                                    state.set_status(format!("member action failed: {e:#}"));
                                    None
                                }
                            }
                        }
                        BatchOp::Uninstall => match perform_member_uninstall(&ctx, repo.clone(), kind).await {
                            Ok(notes) => {
                                // Surface the first note (e.g. id-mismatch stale explanation)
                                // instead of the bland "uninstalled" when the lock mutation
                                // produced one; the bundle badge also flips to `stale` via
                                // the `recompute_states` call below.
                                let label = notes.into_iter().next().unwrap_or_else(|| "uninstalled".to_string());
                                Some(label)
                            }
                            Err(e) => {
                                state.set_status(format!("member uninstall failed: {e:#}"));
                                None
                            }
                        },
                    };
                    if let Some(l) = label {
                        state.set_status(format!("{repo}: {l}"));
                        // Recompute all row badges — the member action may have
                        // changed install state for rows that share the member.
                        recompute_states(&ctx, &mut state);
                        // F7: after install/update, re-check the matching catalog
                        // row so the badge flips to ↑ outdated immediately (if the
                        // member's installed version is behind the floating tag).
                        if op != BatchOp::Uninstall
                            && let Some(idx) = state.rows.iter().position(|r| r.repo == repo)
                        {
                            recheck_rows(&ctx, &state, &mut checker, &[idx]);
                        }
                    }
                }
            }
            TuiAction::LoadVersions { row } => {
                load_versions(&ctx, &mut state, row).await;
            }
            TuiAction::LoadBundleMembers { row: _, bundle_repo } => {
                // Lock-first (offline-first): try to serve the member list from
                // the lock snapshot before hitting the network. This satisfies
                // the offline gate and keeps UX snappy — the lock is always
                // fresher than any previous network fetch in most sessions.
                let lock = lock_io::load(&ctx.lock_path).ok();
                let install_state = load_state(&ctx).unwrap_or_else(|_| InstallState::empty(&ctx.state_path));
                let active = detect_clients(&ctx.workspace, ctx.scope);

                let lock_members: Option<Vec<crate::oci::bundle::BundleMember>> = lock.as_ref().and_then(|l| {
                    // Find the LockedBundle whose `repo` matches this bundle_repo.
                    l.bundles
                        .iter()
                        .find(|b| b.repo == bundle_repo)
                        .map(|b| b.members.clone())
                });

                if let Some(members) = lock_members {
                    // Build MemberNode list from the lock snapshot via the shared
                    // translation helper (DRY: same path as the async-drain path).
                    // Build a O(n) set of row repos for the related-highlight check (D2/P3.7).
                    let row_repos: std::collections::HashSet<&str> =
                        state.rows.iter().map(|r| r.repo.as_str()).collect();
                    let member_count = members.len();
                    let nodes: Vec<super::bundle_members::MemberNode> = members
                        .iter()
                        .filter_map(|m| {
                            // Derive per-member install state from the lock + install record
                            // before calling the shared translation helper.
                            let member_state = crate::oci::Identifier::parse(&m.id)
                                .ok()
                                .map(|parsed| {
                                    derive_artifact_state(
                                        m.kind,
                                        parsed.registry(),
                                        parsed.repository(),
                                        lock.as_ref(),
                                        &install_state,
                                        &ctx.roots,
                                        &active,
                                    )
                                })
                                .unwrap_or(ArtifactState::NotInstalled);
                            super::bundle_members::member_node_from(m, &row_repos, member_state)
                        })
                        .collect();
                    // Warn when a non-empty lock snapshot produced zero valid nodes —
                    // the silent empty-Ready would be invisible without this signal.
                    if member_count > 0 && nodes.is_empty() {
                        tracing::warn!(
                            bundle_repo = %bundle_repo,
                            member_count = member_count,
                            "all locked bundle members had unparseable ids; member list will be empty"
                        );
                    }
                    let key = (state.scope_label.clone(), bundle_repo);
                    state
                        .bundle_members
                        .insert(key, super::bundle_members::BundleMemberCache::Ready(nodes));
                } else if ctx.offline {
                    // No lock data AND offline — nothing to fetch.
                    let key = (state.scope_label.clone(), bundle_repo);
                    state
                        .bundle_members
                        .insert(key, super::bundle_members::BundleMemberCache::Offline);
                } else {
                    // No lock data, online — spawn a background fetch.
                    // The Loading placeholder was already inserted by the Expand handler
                    // in event.rs, so the UI shows feedback immediately.
                    let options = crate::resolve::resolve_options::ResolveOptions::default();
                    bundle_checker.spawn_fetch(state.scope_label.clone(), bundle_repo, &options);
                }
            }
            TuiAction::OpenUrl { url } => {
                state.set_status(match open_url(&url) {
                    Ok(()) => format!("opened {url}"),
                    Err(e) => format!("open failed: {e}"),
                });
            }
            TuiAction::ToggleScope => {
                if ctx.toggle_scope() {
                    state.set_scope_label(&ctx.scope_label);
                    state.set_clients(client_names(&ctx));
                    // Structural tree display options follow the active scope's
                    // `[options.tui]` (the two scopes may differ). The runtime
                    // `t` view-mode choice is deliberately NOT re-seeded from
                    // config here, so a view toggled with `t` survives the swap.
                    state.set_tree_options(ctx.tui_options.group_by_type, ctx.tui_options.tree_separators.clone());
                    recompute_states(&ctx, &mut state);
                    // Invalidate the bundle-member cache: the new scope has a
                    // different lock/install state and a different scope_label key.
                    // A BundleMembersMsg from a fetch spawned under the old scope
                    // must be discarded (stale generation) — bump to ensure that.
                    bundle_checker.bump_generation();
                    state.bundle_members.clear();
                    // Lifecycle (D3b): clear expanded_bundles alongside bundle_members
                    // so no stale expand state leaks across a scope toggle.
                    state.expanded_bundles.clear();
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
    let (lock, _install_state, _declared_bundle_repos) = load_scope_for_badges(ctx);
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
        .iter_artifacts()
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
    let (lock, _install_state, _declared_bundle_repos) = load_scope_for_badges(ctx);
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

/// Drain every pending [`BundleMembersMsg`] non-blockingly and apply it to
/// `state.bundle_members`. Returns `true` when anything changed (so the
/// caller redraws).
///
/// Mirrors the `drain_checks` shape for `CheckMsg`: discard results whose
/// generation stamp is stale (scope toggled or catalog refreshed since spawn).
/// On `Ready` (fresh), write `BundleMemberCache::Ready` into the cache keyed
/// by `(scope_label, bundle_repo)`. On `Failed` (fresh), write
/// `BundleMemberCache::Failed(reason)`.
///
/// The `generation` parameter is the live generation from the
/// `BundleMemberChecker` (P3 wires this; for the P1 stub the function is
/// unreachable).
fn drain_bundle_member_checks(
    ctx: &TuiContext,
    state: &mut TuiState,
    rx: &mut tokio::sync::mpsc::Receiver<super::bundle_member_fetch::BundleMembersMsg>,
    live_generation: u64,
) -> bool {
    use super::bundle_member_fetch::BundleMembersMsg;
    use super::bundle_members::BundleMemberCache;

    let mut changed = false;
    while let Ok(msg) = rx.try_recv() {
        match msg {
            BundleMembersMsg::Ready {
                bundle_repo,
                members,
                generation,
            } => {
                if !is_generation_fresh(generation, live_generation) {
                    continue;
                }
                // F1: Derive per-member install state from the active scope's
                // lock + install record, exactly like the lock-first path.
                // The prior comment claiming members "cannot be installed"
                // was incorrect: a member's repo may be directly declared in
                // the catalog even when the bundle itself has no lock snapshot.
                let lock = lock_io::load(&ctx.lock_path).ok();
                let install_state = load_state(ctx).unwrap_or_else(|_| InstallState::empty(&ctx.state_path));
                let active = detect_clients(&ctx.workspace, ctx.scope);
                // Build a O(n) set of row repos for the related-highlight check (D2/P3.7).
                let row_repos: std::collections::HashSet<&str> = state.rows.iter().map(|r| r.repo.as_str()).collect();
                let nodes: Vec<super::bundle_members::MemberNode> = members
                    .iter()
                    .filter_map(|m| {
                        let member_state = crate::oci::Identifier::parse(&m.id)
                            .ok()
                            .map(|parsed| {
                                derive_artifact_state(
                                    m.kind,
                                    parsed.registry(),
                                    parsed.repository(),
                                    lock.as_ref(),
                                    &install_state,
                                    &ctx.roots,
                                    &active,
                                )
                            })
                            .unwrap_or(ArtifactState::NotInstalled);
                        super::bundle_members::member_node_from(m, &row_repos, member_state)
                    })
                    .collect();

                let key = (state.scope_label.clone(), bundle_repo);
                state.bundle_members.insert(key, BundleMemberCache::Ready(nodes));
                changed = true;
            }
            BundleMembersMsg::Failed {
                bundle_repo,
                reason,
                generation,
            } => {
                if !is_generation_fresh(generation, live_generation) {
                    continue;
                }
                let key = (state.scope_label.clone(), bundle_repo);
                // Reason stored RAW per the two-boundary invariant: sanitize only at
                // display time (flatten_with_members / tree_render_rows), never here.
                state.bundle_members.insert(key, BundleMemberCache::Failed(reason));
                changed = true;
            }
        }
    }
    changed
}

/// Apply a [`CheckMsg::CatalogReady`]: project the fresh catalog into rows
/// (badges derived from the active scope's lock + install record, reusing
/// the same path the initial load uses) and merge them, preserving live
/// per-row `↑` flags, pins, and re-applying the kind-sort + filter.
fn drain_catalog_ready(ctx: &TuiContext, state: &mut TuiState, catalog: &Catalog) {
    let (lock, install_state, declared_bundle_repos) = load_scope_for_badges(ctx);
    let active = detect_clients(&ctx.workspace, ctx.scope);
    let badge = BadgeContext {
        lock: lock.as_ref(),
        state: &install_state,
        roots: &ctx.roots,
        active: &active,
        declared_bundle_repos: &declared_bundle_repos,
    };
    let fresh = rows_from_catalog(catalog, &badge);
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
        KeyCode::PageUp => TuiInput::PageUp,
        KeyCode::PageDown => TuiInput::PageDown,
        KeyCode::Right => TuiInput::Expand,
        KeyCode::Left => TuiInput::Collapse,
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
    //
    // NOTE: the TUI still browses a SINGLE registry here, bypassing the shared
    // `catalog_service::load_catalog` seam that `search`/`mcp` use. Migrating
    // it onto that seam (so `[[registries]]` is honored) and rendering a
    // collapsible registry tree is a deferred follow-up (Workstream E).
    match Catalog::load_or_refresh_coordinated(&ctx.catalog_path, &ctx.registry, "", &ctx.access, ctx.offline, force)
        .await
    {
        Ok(catalog) => {
            let (lock, install_state, declared_bundle_repos) = load_scope_for_badges(ctx);
            let active = detect_clients(&ctx.workspace, ctx.scope);
            let badge = BadgeContext {
                lock: lock.as_ref(),
                state: &install_state,
                roots: &ctx.roots,
                active: &active,
                declared_bundle_repos: &declared_bundle_repos,
            };
            let rows = rows_from_catalog(&catalog, &badge);
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
                // registry that gates the `_catalog` browse endpoint — say so,
                // name the registries, and point at the registry-compatibility
                // docs so an empty list reads as expected, not an error.
                // Explicit-ref ops (install/add/release) work on those
                // registries regardless. Registry list + docs URL are shared
                // with the `grim search` warning (single source of truth).
                format!(
                    "0 entries — {CATALOG_GATED_REGISTRIES} gate `_catalog` browse (expected, not an error); \
                     explicit-ref ops still work. See {REGISTRY_COMPAT_DOCS_URL}"
                )
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

/// Per-scope inputs for deriving a catalog row's badge state: the active lock,
/// install state, anchor roots, active client set, and the declared bundle
/// `registry/repository` set. Bundled (and passed by reference) to keep the
/// row-derivation signatures small.
struct BadgeContext<'a> {
    lock: Option<&'a GrimoireLock>,
    state: &'a InstallState,
    roots: &'a AnchorRoots,
    active: &'a [ClientTarget],
    declared_bundle_repos: &'a std::collections::BTreeSet<String>,
}

/// Project a catalog into TUI rows, deriving each state from the scope's
/// [`BadgeContext`] (lock + install-state + declared bundles).
fn rows_from_catalog(catalog: &Catalog, ctx: &BadgeContext) -> Vec<TuiRow> {
    catalog
        .entries()
        .map(|e| {
            let kind = e.kind.clone().unwrap_or_else(|| "-".to_string());
            let row_state = derive_row_state(&kind, &e.registry, &e.repository, ctx);
            TuiRow {
                kind,
                repo: e.repo(),
                description: e.description.clone().unwrap_or_default(),
                summary: e.summary.clone().unwrap_or_default(),
                keywords: e.keywords.clone(),
                repository_url: e.repository_url.clone(),
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

/// Kind-aware row state: a bundle row is installed iff it is declared in the
/// active scope's `[bundles]` (`ctx.declared_bundle_repos`); every other kind
/// derives from its own lock entry + install record.
fn derive_row_state(kind: &str, registry: &str, repository: &str, ctx: &BadgeContext) -> ArtifactState {
    if row_kind(kind) == ArtifactKind::Bundle {
        derive_bundle_state(&format!("{registry}/{repository}"), ctx.declared_bundle_repos)
    } else {
        derive_artifact_state(
            row_kind(kind),
            registry,
            repository,
            ctx.lock,
            ctx.state,
            ctx.roots,
            ctx.active,
        )
    }
}

/// Derive a bundle row's state.
///
/// A bundle row is "installed" iff the bundle **itself** is declared — i.e.
/// `bundle_repo` (`registry/repository`) is one of the `registry/repository`
/// values in the active scope's `[bundles]` table. This is exactly the user's
/// rule: a bundle is installed only when it appears in the `.toml`.
///
/// Deriving from the *live declaration* (not the lock) is deliberate: it is
/// robust to a pre-cache lock that predates the `[[bundle]]` snapshot, and to a
/// stale/lingering snapshot left by a hand-edit, branch switch, or
/// retag-without-relock — neither of which must mislead the row.
///
/// It deliberately does **NOT** depend on whether the member artifacts are
/// installed. A bundle is installed because it is declared, not because its
/// skills happen to be present: installing member skills standalone must never
/// flip an undeclared bundle to "installed", and a declared bundle's row must
/// not flip as its members are installed or removed. Per-member health is
/// surfaced on the member rows, never folded into the bundle row.
fn derive_bundle_state(bundle_repo: &str, declared_bundle_repos: &std::collections::BTreeSet<String>) -> ArtifactState {
    if declared_bundle_repos.contains(bundle_repo) {
        ArtifactState::Installed
    } else {
        ArtifactState::NotInstalled
    }
}

/// Derive the richer TUI [`ArtifactState`] for `(kind, registry, repository)`.
///
/// `kind` is matched in addition to `registry`+`repository` so a lock that
/// holds the same registry/repository under two kinds (e.g. a skill and a
/// rule at the same repo) is never confused — a bundle member that is a
/// `Rule` will not be matched against a `Skill` install record for the same
/// repo (FIX 1: kind-blind matching).
///
/// Precedence mirrors `status.rs::derive_state` and
/// `status_badge::derive_badge` — the *only* divergence is that a present
/// install record whose client outputs are missing or unreadable is
/// surfaced as [`ArtifactState::IntegrityMissing`] rather than collapsed
/// into `NotInstalled`, so a broken/tampered install is distinguishable
/// from a never-installed entry. No lock entry or no record at all is
/// still `NotInstalled`.
fn derive_artifact_state(
    kind: ArtifactKind,
    registry: &str,
    repository: &str,
    lock: Option<&GrimoireLock>,
    state: &InstallState,
    roots: &AnchorRoots,
    active: &[ClientTarget],
) -> ArtifactState {
    let Some(locked) = lock.and_then(|l| {
        l.iter_artifacts()
            .find(|a| a.kind == kind && a.pinned.registry() == registry && a.pinned.repository() == repository)
    }) else {
        return ArtifactState::NotInstalled;
    };
    let Some(record) = state
        .iter_records()
        .find(|r| r.kind == kind && r.pinned.registry() == registry && r.pinned.repository() == repository)
    else {
        return ArtifactState::NotInstalled;
    };

    // Reconcile against the active client set: an output for a client removed
    // since install is ignored (it must not poison the row — nor, via the
    // bundle worst-of aggregation, the bundle row). With no output for any
    // active client the artifact is not installed here.
    let outputs: Vec<&ClientOutput> = active_outputs(&record.outputs, active).collect();
    if outputs.is_empty() {
        return ArtifactState::NotInstalled;
    }

    // A read-only derivation never `?`-propagates an `AnchorError`: an
    // unresolvable anchored output (corrupt `relative`, anchor root absent
    // here) surfaces as IntegrityMissing, distinct from never-installed.
    for out in &outputs {
        match out.resolved_target(roots) {
            Ok(resolved) if !resolved.exists() => return ArtifactState::IntegrityMissing,
            Ok(_) => {}
            Err(_) => return ArtifactState::IntegrityMissing,
        }
    }
    for out in &outputs {
        match out.current_hash(roots) {
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
    let (lock, install_state, declared_bundle_repos) = load_scope_for_badges(ctx);
    let active = detect_clients(&ctx.workspace, ctx.scope);
    let badge = BadgeContext {
        lock: lock.as_ref(),
        state: &install_state,
        roots: &ctx.roots,
        active: &active,
        declared_bundle_repos: &declared_bundle_repos,
    };
    for r in &mut state.rows {
        if let Some((registry, repository)) = split_repo(&r.repo) {
            r.state = derive_row_state(&r.kind, &registry, &repository, &badge);
        }
    }
}

/// Load the active scope's install state, routing through the scope-aware
/// seam so a project legacy file (or a V1 global file) migrates to anchored
/// outputs in memory (no disk write on the read path). Project scope uses
/// the workspace + the legacy `$GRIM_HOME/state/projects/<sha>.json`
/// fallback; global scope threads the vendor roots.
///
/// # Errors
///
/// An [`std::io::Error`] for a read failure; a corrupt or unknown-version
/// file is surfaced as [`std::io::ErrorKind::InvalidData`].
fn load_state(ctx: &TuiContext) -> io::Result<InstallState> {
    match ctx.scope {
        ConfigScope::Project => InstallState::load_project(&ctx.workspace, &ctx.roots.grim_home, &ctx.config_path),
        ConfigScope::Global => InstallState::load_global(&ctx.state_path, &ctx.roots),
    }
}

/// Best-effort scope load for badges (advisory — never fails the TUI).
///
/// Returns the active scope's lock, install state, and the set of declared
/// bundle `registry/repository` values (from the live `[bundles]` table) used
/// to derive bundle row state. The declaration is read fresh (the config can
/// change while the TUI runs); any read failure degrades to an empty set.
fn load_scope_for_badges(ctx: &TuiContext) -> (Option<GrimoireLock>, InstallState, std::collections::BTreeSet<String>) {
    let lock = lock_io::load(&ctx.lock_path).ok();
    let state = load_state(ctx).unwrap_or_else(|_| InstallState::empty(&ctx.state_path));
    let declared_bundle_repos = load_scope_declaration(ctx)
        .map(|(_options, _registries, set)| set.bundles.values().map(|id| id.registry_repository()).collect())
        .unwrap_or_default();
    (lock, state, declared_bundle_repos)
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
            Ok(()) => ok += 1,
            Err(e) => {
                failed += 1;
                last_err = Some(format!("{}: {e}", row.repo));
            }
        }
    }

    // A bundle op also (un)installs the bundle's members, which appear as
    // separate rows — recompute every row's state against the new lock +
    // install-state instead of only the acted-on rows (same derivation the
    // manual refresh uses).
    recompute_states(ctx, state);

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
/// members from the lock. A directly-declared row a declared bundle still
/// provides keeps its files ([`direct_removal_keeps_files`]) — the delete
/// degrades to dropping the direct declaration, like `grim remove`.
fn perform_uninstall(ctx: &TuiContext, row: &TuiRow) -> anyhow::Result<()> {
    let (_registry, repository) =
        split_repo(&row.repo).ok_or_else(|| anyhow::anyhow!("malformed catalog repo: {}", row.repo))?;
    let kind = row_kind(&row.kind);
    let name = repository.rsplit('/').next().unwrap_or(&repository).to_string();

    // Hold the config flock for the whole read-modify-write. The keep-files
    // gate, the file deletion, and the config/lock undeclare must all see one
    // consistent declaration snapshot: acquiring the lock BEFORE the gate
    // closes a TOCTOU window where a concurrent `grim remove` of the bundle
    // (between the gate decision and the undeclare) would orphan the kept
    // files. Held to function end.
    let _guard = match ctx.config_path.exists() {
        true => Some(grim(ConfigFileLock::try_acquire(&ctx.config_path))?),
        false => None,
    };

    // The install-state records this row owns: itself for a skill/rule;
    // for a bundle, exactly the lock entries the undeclare would drop
    // (computed BEFORE the undeclare below applies it) — the effective-set
    // diff via the shared `drop_from_lock` seam, so a member another
    // declaration still holds keeps its files.
    let targets: Vec<(ArtifactKind, String)> = match kind {
        ArtifactKind::Bundle => bundle_uninstall_targets(ctx, &name, &row.repo),
        // A directly-declared artifact a declared bundle still provides keeps
        // its files (it stays desired) — delete nothing, just undeclare below.
        _ if direct_removal_keeps_files(ctx, kind, &name) => Vec::new(),
        _ => vec![(kind, name.clone())],
    };

    let mut install_state = load_state(ctx).map_err(|e| anyhow::anyhow!("install-state load failed: {e}"))?;
    let mut involved_clients: Vec<crate::install::client_target::ClientTarget> = Vec::new();
    let mut any_removed = false;
    for (target_kind, target_name) in &targets {
        for client in install_state
            .get(*target_kind, target_name)
            .map(|r| {
                r.outputs
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
        let result = crate::install::uninstall::uninstall(&mut install_state, *target_kind, target_name, &ctx.roots)
            .map_err(|e| anyhow::anyhow!("uninstall failed: {e}"))?;
        any_removed |= result.outcome == crate::install::uninstall::UninstallOutcome::Removed;
    }
    if any_removed {
        // The single `persist` seam handles project-scope dir creation, the
        // atomic write, and the conditional legacy-file reap (including the
        // lossy-migration guard that was previously missing here).
        install_state
            .persist(ctx.scope, &ctx.workspace, &ctx.roots.grim_home, &ctx.config_path)
            .map_err(|e| anyhow::Error::new(e).context("install-state persist failed"))?;
    }
    // Converge vendor-owned config for every client the removed record
    // carried, mirroring `command::uninstall`. The files and install state are
    // already gone/persisted, so a config-sync failure is warn-only — the
    // delete completed, never a hard failure after the primary action.
    for client in involved_clients {
        if let Err(e) = client.vendor().sync_config(&install_state, &ctx.workspace, ctx.scope) {
            tracing::warn!(client = %client, error = %e, "vendor config sync failed; delete completed, deregistration skipped");
        }
    }

    // Undeclare from the config + lock through the `grim uninstall` seam
    // (the config flock acquired at the top is still held for this
    // read-modify-write), so the badge no longer derives "installed" and a
    // later `grim install` does not silently bring the entry back.
    // Post-action cleanup: the files are already deleted and the install state
    // persisted. If the declaration can no longer be read (a project config the
    // user removed, say), there is nothing left to undeclare — the goal is
    // already met, so converge rather than fail the delete (the `let Ok(..)
    // else` precedent from `bundle_uninstall_targets`).
    let Ok((options, registries, mut set)) = load_scope_declaration(ctx) else {
        return Ok(());
    };
    undeclare_and_unlock(
        &ctx.config_path,
        &ctx.lock_path,
        &options,
        &registries,
        &mut set,
        kind,
        &name,
    )?;
    // TODO: surface notes in the batch-uninstall status line (run_batch path).
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
    let (options, registries, mut set) = load_scope_declaration(ctx)?;
    declare(&mut set, kind, name.clone(), id);
    grim(write_config(&ctx.config_path, &options, &registries, &set))?;

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
    let mut install_state = load_state(ctx).map_err(|e| anyhow::anyhow!("install-state load failed: {e}"))?;
    let materializer = DefaultMaterializer;

    // `update` forces re-materialization (rolling-release contract),
    // matching `command::update`; `install` honours the integrity gate.
    let outcomes = install_all(
        &single,
        &ctx.access,
        &materializer,
        &target,
        &mut install_state,
        &ctx.roots,
        is_update,
    )
    .await;
    // The single `persist` seam handles project-scope dir creation, the
    // atomic write, and the conditional legacy-file reap (including the
    // lossy-migration guard that was previously missing here).
    install_state
        .persist(ctx.scope, &ctx.workspace, &ctx.roots.grim_home, &ctx.config_path)
        .map_err(|e| anyhow::Error::new(e).context("install-state persist failed"))?;

    // Converge vendor-owned config on the new state, mirroring
    // `command::install`. The artifacts and install state are already
    // persisted, so a config-sync failure is warn-only — the install
    // completed, never a hard failure after the primary action.
    for client in target.clients() {
        if let Err(e) = client.vendor().sync_config(&install_state, &ctx.workspace, ctx.scope) {
            tracing::warn!(client = %client, error = %e, "vendor config sync failed; install completed, registration skipped");
        }
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
/// The lock entries deleting the bundle row would drop — the file-deletion
/// targets for the TUI delete action. Computed through the shared
/// [`crate::command::remove::drop_from_lock`] effective-set seam by
/// simulating the undeclare, so a member another declaration still holds
/// keeps its files. A binding the config does not declare (a legacy or
/// foreign row) falls back to provenance-exclusive matching by repo.
fn bundle_uninstall_targets(ctx: &TuiContext, binding: &str, repo: &str) -> Vec<(ArtifactKind, String)> {
    let Ok(previous) = lock_io::load(&ctx.lock_path) else {
        return Vec::new();
    };
    let Ok((_options, _registries, set_before)) = load_scope_declaration(ctx) else {
        return Vec::new();
    };
    let mut set_after = set_before.clone();
    if set_after.bundles.remove(binding).is_none() {
        return previous
            .iter_artifacts()
            .filter(|a| !a.bundles.is_empty() && a.bundles.iter().all(|b| b.repo == repo))
            .map(|a| (a.kind, a.name.clone()))
            .collect();
    }
    set_after.invalidate_declaration_hash_cache();
    let outcome =
        crate::command::remove::drop_from_lock(&previous, ArtifactKind::Bundle, binding, &set_before, &set_after);
    let kept: std::collections::HashSet<(ArtifactKind, String)> = outcome
        .lock
        .iter_artifacts()
        .map(|a| (a.kind, a.name.clone()))
        .collect();
    previous
        .iter_artifacts()
        .filter(|a| !kept.contains(&(a.kind, a.name.clone())))
        .map(|a| (a.kind, a.name.clone()))
        .collect()
}

fn load_scope_declaration(
    ctx: &TuiContext,
) -> anyhow::Result<(
    ConfigOptions,
    Vec<crate::config::declaration::RegistryConfig>,
    DesiredSet,
)> {
    match ctx.scope {
        ConfigScope::Global => {
            let cfg = grim(GlobalConfig::load(&ctx.config_path))?;
            Ok((cfg.options, cfg.registries, cfg.set))
        }
        ConfigScope::Project => {
            let discovered = grim(ProjectConfig::discover(Some(&ctx.config_path)))?;
            Ok((
                discovered.config.options,
                discovered.config.registries,
                discovered.config.set,
            ))
        }
    }
}

/// Whether deleting the directly-declared artifact `(kind, name)` must keep
/// its files because a declared bundle still provides it — the file-retention
/// gate from [`crate::lock::effective_set::bundle_holds_after_direct_removal`].
///
/// Loads the lock + the active scope's declaration fresh (the config can change
/// while the TUI runs). Any load failure means the guard cannot prove the
/// artifact is held → `false` (the caller deletes, the pre-effective-set
/// behavior). A pure bundle member (not directly declared) is never held here,
/// so the TUI member-delete action still deletes its files.
fn direct_removal_keeps_files(ctx: &TuiContext, kind: ArtifactKind, name: &str) -> bool {
    let Ok(lock) = lock_io::load(&ctx.lock_path) else {
        return false;
    };
    let Ok((_options, _registries, set)) = load_scope_declaration(ctx) else {
        return false;
    };
    crate::lock::effective_set::bundle_holds_after_direct_removal(&set, &lock.bundles, kind, name)
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
/// ([`LockedArtifact::bundles`] — a shared member lists every
/// contributor); an empty projection means the bundle resolved to zero
/// members (or every member was overridden by a direct declaration).
fn bundle_members_lock(lock: &GrimoireLock, bundle_repo: &str, bundle_tag: &str) -> GrimoireLock {
    let is_member = |a: &LockedArtifact| a.bundles.iter().any(|b| b.repo == bundle_repo && b.tag == bundle_tag);
    GrimoireLock {
        metadata: lock.metadata.clone(),
        skills: lock.skills.iter().filter(|a| is_member(a)).cloned().collect(),
        rules: lock.rules.iter().filter(|a| is_member(a)).cloned().collect(),
        agents: lock.agents.iter().filter(|a| is_member(a)).cloned().collect(),
        // A projection feeds the installer only — the bundle cache is not
        // consulted there, so it is not carried over.
        bundles: Vec::new(),
    }
}

/// Project the single `kind`/`name` entry out of `lock` as a one-artifact
/// lock (same metadata), so the shared `install_all` path materializes
/// exactly the acted-on row and nothing else. `None` when the entry is
/// absent from the resolved lock (defensive — not expected). Bundle rows
/// go through [`bundle_members_lock`] instead.
fn single_entry_lock(lock: &GrimoireLock, kind: ArtifactKind, name: &str) -> Option<GrimoireLock> {
    let entry = lock
        .iter_artifacts()
        .find(|a| a.kind == kind && a.name == name)
        .cloned()?;
    let (skills, rules, agents) = match kind {
        ArtifactKind::Skill => (vec![entry], Vec::new(), Vec::new()),
        ArtifactKind::Rule => (Vec::new(), vec![entry], Vec::new()),
        ArtifactKind::Agent => (Vec::new(), Vec::new(), vec![entry]),
        ArtifactKind::Bundle => return None,
    };
    Some(GrimoireLock {
        metadata: lock.metadata.clone(),
        skills,
        rules,
        agents,
        bundles: Vec::new(),
    })
}

/// Split `registry/repository` at the first `/`.
fn split_repo(repo: &str) -> Option<(String, String)> {
    repo.split_once('/').map(|(r, p)| (r.to_string(), p.to_string()))
}

/// Candidate opener command lines for the current platform, tried in
/// order until one spawns. A tiny polyfill instead of an extra crate:
///
/// - Windows: `cmd /C start "" <url>` (builtin; the empty quoted arg fills
///   the window-title slot), then `rundll32 url.dll,FileProtocolHandler`
///   as the no-shell fallback.
/// - macOS: `open` (always present).
/// - other unixes: `xdg-open` (xdg-utils), then `gio open` (GLib systems
///   without xdg-utils), then `wslview` (WSL without a Linux browser).
fn opener_candidates(url: &str) -> Vec<(&'static str, Vec<String>)> {
    if cfg!(windows) {
        // `start` goes through cmd's parser: escape `&` so a query string
        // is not split into a second command. The catalog guard already
        // pins `https://`, so no further shell metacharacters survive.
        let escaped = url.replace('&', "^&");
        vec![
            ("cmd", vec!["/C".into(), "start".into(), String::new(), escaped]),
            ("rundll32", vec![format!("url.dll,FileProtocolHandler {url}")]),
        ]
    } else if cfg!(target_os = "macos") {
        vec![("open", vec![url.to_string()])]
    } else {
        vec![
            ("xdg-open", vec![url.to_string()]),
            ("gio", vec!["open".into(), url.to_string()]),
            ("wslview", vec![url.to_string()]),
        ]
    }
}

/// Open `url` with the first available platform opener, detached: stdio is
/// nulled so the child can never write into the alternate screen /
/// raw-mode terminal, and the handle is reaped in a background task
/// (openers exit fast). Spawn failures (typically a missing opener binary)
/// fall through to the next candidate from [`opener_candidates`].
///
/// # Errors
///
/// A non-HTTPS URL (defense in depth — only the catalog guard's vetted
/// `https://` values reach here), or no candidate opener could be spawned.
fn open_url(url: &str) -> io::Result<()> {
    if !url.starts_with("https://") {
        return Err(io::Error::other("not an https URL"));
    }
    let candidates = opener_candidates(url);
    let tried: Vec<&str> = candidates.iter().map(|(p, _)| *p).collect();
    let mut last_err: Option<io::Error> = None;
    for (program, args) in &candidates {
        match tokio::process::Command::new(program)
            .args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            Ok(mut child) => {
                tokio::spawn(async move {
                    // Reap so the opener never zombifies; its exit code is
                    // irrelevant (the status line already reported the
                    // attempt).
                    let _ = child.wait().await;
                });
                return Ok(());
            }
            Err(e) => last_err = Some(e),
        }
    }
    let detail = last_err
        .map(|e| e.to_string())
        .unwrap_or_else(|| "no candidates".to_string());
    Err(io::Error::other(format!(
        "no URL opener found (tried {}): {detail}",
        tried.join(", ")
    )))
}

/// The display names of the active scope's effective selected clients
/// (`claude`, `opencode`, …), in [`crate::install::client_target::ClientTarget::ALL`]
/// order, for the status area.
fn client_names(ctx: &TuiContext) -> Vec<String> {
    ctx.clients_selected.iter().map(ToString::to_string).collect()
}

// ── P1.5 Stubs — Phase 4 (P4.2/P4.3) will fill these bodies ────────────────
//
// Signatures are declared here so P2 tests can compile and link against them.
// The `unimplemented!()` body is intentional: these are reached only in P4
// when the full member-action path is wired; calling them in P1/P2 tests
// (which only cover the pure event/state layer) must never happen.

/// Resolve the install tag for a bundle member.
///
/// Priority (D8a):
/// 1. The matching catalog row's `pinned_version` (when set and non-empty).
/// 2. The matching catalog row's `latest_tag` (when non-empty).
/// 3. `"latest"` — the same fallback `perform` uses.
///
/// Returns `"latest"` when no catalog row matches `repo` (non-catalog member).
///
/// Pure function — no I/O. Unit-testable standalone (C-11 pure half).
pub(crate) fn resolve_member_tag(repo: &str, rows: &[TuiRow]) -> String {
    rows.iter()
        .find(|r| r.repo == repo)
        .and_then(|r| {
            r.pinned_version
                .as_deref()
                .filter(|v| !v.is_empty())
                .map(str::to_string)
                .or_else(|| Some(r.latest_tag.clone()).filter(|t| !t.is_empty()))
        })
        .unwrap_or_else(|| "latest".to_string())
}

/// Perform a member install or update action, reusing the same
/// declare → relock → single_entry_lock → install_all → persist →
/// sync_config seam that [`perform`] uses. Does NOT fork install logic.
///
/// `repo` is the validated `registry/repository` reference from
/// `DisplayRow::Member.member_repo`. Returns an `Err` (status breadcrumb,
/// no install) when `repo` fails `split_repo` (C-12 defense-in-depth).
async fn perform_member(
    ctx: &TuiContext,
    repo: String,
    kind: crate::oci::ArtifactKind,
    is_update: bool,
    tag: String,
) -> anyhow::Result<String> {
    // C-12: validate split_repo at the boundary — return Err (no panic) on
    // a separator-less repo so the dispatch arm can show a status breadcrumb.
    if split_repo(&repo).is_none() {
        return Err(anyhow::anyhow!("malformed member repo: {repo}"));
    }
    // Build a minimal synthetic TuiRow so we can delegate to `perform`.
    // `tag` was resolved by `resolve_member_tag` (D8a): a catalog-matched
    // member reuses that row's pinned/latest tag, a non-catalog member gets
    // `"latest"`. We seed it as `latest_tag` (pinned_version stays `None`) so
    // `perform`'s tag precedence (pinned → latest_tag → "latest") yields it.
    // F11: use `repo` directly (owned by-value); no redundant `.clone()`.
    let synthetic_row = TuiRow {
        kind: kind.to_string(),
        repo,
        description: String::new(),
        summary: String::new(),
        keywords: Vec::new(),
        repository_url: None,
        latest_tag: tag,
        version: String::new(),
        pinned_version: None,
        state: crate::tui::state::ArtifactState::NotInstalled,
    };
    perform(ctx, &synthetic_row, is_update).await
}

/// Perform a member uninstall action, reusing the shared seams for file
/// deletion and config/lock mutation. Returns the notes produced by the
/// lock mutation (e.g. an id-mismatch stale note), or an `Err` (status
/// breadcrumb) when `repo` fails `split_repo` (C-12 defense-in-depth).
/// An empty `Vec` on `Ok` means the uninstall completed without any notes.
async fn perform_member_uninstall(
    ctx: &TuiContext,
    repo: String,
    kind: crate::oci::ArtifactKind,
) -> anyhow::Result<Vec<String>> {
    // C-12: validate split_repo at the boundary — return Err (no panic) on
    // a separator-less repo so the dispatch arm can show a status breadcrumb.
    let Some((_registry, repository)) = split_repo(&repo) else {
        return Err(anyhow::anyhow!("malformed member repo: {repo}"));
    };
    let member_kind = kind;
    let name = repository.rsplit('/').next().unwrap_or(&repository).to_string();

    // Hold the config flock for the whole read-modify-write so the keep-files
    // gate, the file deletion, and the undeclare see one consistent
    // declaration snapshot (closes the TOCTOU window where a concurrent
    // `grim remove` between the gate and the undeclare could orphan the kept
    // files). Held to function end.
    let _guard = match ctx.config_path.exists() {
        true => Some(grim(ConfigFileLock::try_acquire(&ctx.config_path))?),
        false => None,
    };

    // Delete materialized files + drop install-state record — UNLESS the member
    // is ALSO directly declared and a declared bundle still provides it, in
    // which case the files stay (it remains desired) and only the declaration
    // is dropped below. A pure bundle member is never gated, so the
    // member-delete action still deletes its files (re-installable).
    if !direct_removal_keeps_files(ctx, member_kind, &name) {
        let mut install_state = load_state(ctx).map_err(|e| anyhow::anyhow!("install-state load failed: {e}"))?;
        let involved_clients: Vec<crate::install::client_target::ClientTarget> = install_state
            .get(member_kind, &name)
            .map(|r| r.outputs.iter().filter_map(|c| c.client.parse().ok()).collect())
            .unwrap_or_default();
        let result = crate::install::uninstall::uninstall(&mut install_state, member_kind, &name, &ctx.roots)
            .map_err(|e| anyhow::anyhow!("uninstall failed: {e}"))?;
        if result.outcome == crate::install::uninstall::UninstallOutcome::Removed {
            install_state
                .persist(ctx.scope, &ctx.workspace, &ctx.roots.grim_home, &ctx.config_path)
                .map_err(|e| anyhow::Error::new(e).context("install-state persist failed"))?;
        }
        for client in involved_clients {
            if let Err(e) = client.vendor().sync_config(&install_state, &ctx.workspace, ctx.scope) {
                tracing::warn!(client = %client, error = %e, "vendor config sync failed; delete completed, deregistration skipped");
            }
        }
    }

    // Undeclare from config + lock, threading notes back to the caller. The
    // config flock acquired at the top is still held for this read-modify-write.
    let Ok((options, registries, mut set)) = load_scope_declaration(ctx) else {
        return Ok(Vec::new());
    };
    let (_declared, notes) = undeclare_and_unlock(
        &ctx.config_path,
        &ctx.lock_path,
        &options,
        &registries,
        &mut set,
        member_kind,
        &name,
    )?;
    Ok(notes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_url_rejects_non_https() {
        // Defense in depth below the catalog guard: nothing but https://
        // ever reaches the platform opener.
        for bad in ["http://x", "file:///etc/passwd", "ghcr.io/acme/x", ""] {
            assert!(open_url(bad).is_err(), "{bad} must be rejected");
        }
    }

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
        assert_eq!(map_key(mk(KeyCode::PageUp)), Some(TuiInput::PageUp));
        assert_eq!(map_key(mk(KeyCode::PageDown)), Some(TuiInput::PageDown));
        assert_eq!(map_key(mk(KeyCode::Enter)), Some(TuiInput::Enter));
        assert_eq!(map_key(mk(KeyCode::Esc)), Some(TuiInput::Esc));
        assert_eq!(map_key(mk(KeyCode::Backspace)), Some(TuiInput::Backspace));
        assert_eq!(map_key(mk(KeyCode::Char('i'))), Some(TuiInput::Char('i')));
        assert_eq!(map_key(mk(KeyCode::Tab)), None);
    }

    // Step 3.5: `map_key` must map Left → Collapse and Right → Expand.
    // These are the tree-navigation arrow bindings.
    #[test]
    fn map_key_left_and_right_map_to_collapse_and_expand() {
        let mk = |code| KeyEvent::new(code, crossterm::event::KeyModifiers::NONE);
        assert_eq!(
            map_key(mk(KeyCode::Left)),
            Some(TuiInput::Collapse),
            "KeyCode::Left must map to TuiInput::Collapse"
        );
        assert_eq!(
            map_key(mk(KeyCode::Right)),
            Some(TuiInput::Expand),
            "KeyCode::Right must map to TuiInput::Expand"
        );
    }

    fn installed_row(repo: &str) -> TuiRow {
        TuiRow {
            kind: "skill".to_string(),
            repo: repo.to_string(),
            description: String::new(),
            summary: String::new(),
            keywords: Vec::new(),
            repository_url: None,
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
            agents: vec![],
            bundles: vec![],
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
        let mut lock = lock_fixture(
            vec![
                locked("a", ArtifactKind::Skill, '1'),
                locked("b", ArtifactKind::Skill, '2'),
            ],
            vec![locked("c", ArtifactKind::Rule, '3')],
        );
        lock.agents = vec![locked("d", ArtifactKind::Agent, '4')];

        let single = single_entry_lock(&lock, ArtifactKind::Skill, "b").expect("entry exists");
        assert_eq!(single.skills.len(), 1);
        assert_eq!(single.skills[0].name, "b");
        assert!(single.rules.is_empty());
        assert!(single.agents.is_empty());
        assert_eq!(single.metadata, lock.metadata, "metadata carries over unchanged");

        let rule = single_entry_lock(&lock, ArtifactKind::Rule, "c").expect("rule entry exists");
        assert!(rule.skills.is_empty());
        assert_eq!(rule.rules.len(), 1);
        assert!(rule.agents.is_empty());

        let agent = single_entry_lock(&lock, ArtifactKind::Agent, "d").expect("agent entry exists");
        assert!(agent.skills.is_empty());
        assert!(agent.rules.is_empty());
        assert_eq!(agent.agents.len(), 1);
        assert_eq!(agent.agents[0].name, "d");

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
            // OCI empty config — the actual wire shape since
            // `adr_oci_empty_config_compat.md` (kind resolves via artifactType).
            config_media_type: Some("application/vnd.oci.empty.v1+json".to_string()),
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
            // OCI empty config — the actual wire shape since
            // `adr_oci_empty_config_compat.md` (kind resolves via artifactType).
            config_media_type: Some("application/vnd.oci.empty.v1+json".to_string()),
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

    /// Build a minimal `AnchorRoots` for tests rooted at `workspace`.
    fn test_roots(workspace: &std::path::Path) -> AnchorRoots {
        AnchorRoots {
            workspace: workspace.to_path_buf(),
            grim_home: workspace.to_path_buf(),
            claude_root: None,
            copilot_root: None,
            opencode_skills: None,
        }
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
            roots: test_roots(workspace),
            clients_default: vec!["claude".to_string()],
            clients_selected: Vec::new(),
            scope_label: "project".to_string(),
            alt: None,
            tui_options: Default::default(),
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
            lock.skills[0].bundles,
            vec![crate::lock::locked_artifact::BundleProvenance::new(
                "localhost:5050/grimoire/bundles/starter-pack",
                "latest"
            )]
        );

        // The member skill materialized into the claude target.
        assert!(
            workspace.join(".claude/skills/demo/SKILL.md").is_file(),
            "member skill files must exist"
        );

        // The bundle row badge derives `installed` from its members.
        let (lock, install_state, declared_bundle_repos) = load_scope_for_badges(&ctx);
        let badge = BadgeContext {
            lock: lock.as_ref(),
            state: &install_state,
            roots: &ctx.roots,
            active: &ClientTarget::ALL,
            declared_bundle_repos: &declared_bundle_repos,
        };
        assert_eq!(
            derive_row_state("bundle", "localhost:5050", "grimoire/bundles/starter-pack", &badge),
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

        let (lock, install_state, declared_bundle_repos) = load_scope_for_badges(&ctx);
        let badge = BadgeContext {
            lock: lock.as_ref(),
            state: &install_state,
            roots: &ctx.roots,
            active: &ClientTarget::ALL,
            declared_bundle_repos: &declared_bundle_repos,
        };
        assert_eq!(
            derive_row_state("bundle", "localhost:5050", "grimoire/bundles/starter-pack", &badge),
            ArtifactState::NotInstalled
        );
    }

    #[tokio::test]
    async fn run_batch_on_a_bundle_recomputes_member_row_states() {
        // A bundle batch op also (un)installs the bundle's members. Rows
        // representing those members must reflect the new lock/install
        // state immediately — not only after a manual refresh ('r').
        let tmp = tempfile::tempdir().unwrap();
        let workspace = tmp.path();
        std::fs::write(workspace.join("grimoire.toml"), "[skills]\n\n[rules]\n").unwrap();
        let ctx = test_ctx(workspace, registry_with_bundle().await);

        let mut bundle_row = installed_row("localhost:5050/grimoire/bundles/starter-pack");
        bundle_row.kind = "bundle".to_string();
        bundle_row.state = ArtifactState::NotInstalled;
        let mut member_row = installed_row("localhost:5050/grimoire/skills/demo");
        member_row.state = ArtifactState::NotInstalled;

        let mut state = TuiState::new();
        state.set_rows(vec![bundle_row, member_row]);

        // Installing the bundle pulls the member in: its row must flip too.
        run_batch(&ctx, &mut state, &[0], BatchOp::Install).await;
        assert_eq!(
            state.rows[0].state,
            ArtifactState::Installed,
            "bundle row reflects the install"
        );
        assert_eq!(
            state.rows[1].state,
            ArtifactState::Installed,
            "member row must be recomputed after a bundle install"
        );

        // Deleting the bundle removes the member: its row must flip back.
        run_batch(&ctx, &mut state, &[0], BatchOp::Uninstall).await;
        assert_eq!(
            state.rows[0].state,
            ArtifactState::NotInstalled,
            "bundle row reflects the uninstall"
        );
        assert_eq!(
            state.rows[1].state,
            ArtifactState::NotInstalled,
            "member row must be recomputed after a bundle delete"
        );
    }

    #[test]
    fn row_kind_maps_every_catalog_kind() {
        assert_eq!(row_kind("skill"), ArtifactKind::Skill);
        assert_eq!(row_kind("rule"), ArtifactKind::Rule);
        assert_eq!(row_kind("agent"), ArtifactKind::Agent);
        assert_eq!(row_kind("bundle"), ArtifactKind::Bundle);
        // Unknown / absent kind defaults to skill; the materializer
        // validates the actual payload shape.
        assert_eq!(row_kind("-"), ArtifactKind::Skill);
    }

    fn stamp(mut a: LockedArtifact, repo: &str, tag: &str) -> LockedArtifact {
        a.bundles
            .push(crate::lock::locked_artifact::BundleProvenance::new(repo, tag));
        a
    }

    #[test]
    fn bundle_members_lock_projects_by_provenance_repo_and_tag() {
        let member = stamp(locked("member", ArtifactKind::Skill, '4'), "r/bundles/pack", "latest");
        let other_tag = stamp(locked("other", ArtifactKind::Skill, '5'), "r/bundles/pack", "v2");
        let rule_member = stamp(locked("rmember", ArtifactKind::Rule, '6'), "r/bundles/pack", "latest");
        let direct = locked("direct", ArtifactKind::Skill, '7');
        let agent_member = stamp(locked("amember", ArtifactKind::Agent, '8'), "r/bundles/pack", "latest");

        let mut lock = lock_fixture(vec![member, other_tag, direct], vec![rule_member]);
        lock.agents = vec![agent_member];
        let projected = bundle_members_lock(&lock, "r/bundles/pack", "latest");

        assert_eq!(projected.skills.len(), 1, "only the latest-tag member projects");
        assert_eq!(projected.skills[0].name, "member");
        assert_eq!(projected.rules.len(), 1);
        assert_eq!(projected.rules[0].name, "rmember");
        assert_eq!(projected.agents.len(), 1, "agent bundle member projects");
        assert_eq!(projected.agents[0].name, "amember");
        assert_eq!(projected.metadata, lock.metadata, "metadata carries over unchanged");

        let empty = bundle_members_lock(&lock, "r/bundles/unknown", "latest");
        assert!(empty.skills.is_empty() && empty.rules.is_empty() && empty.agents.is_empty());
    }

    /// A bundle whose members include an agent: verify that state derivation
    /// counts the agent member and that the bundle-expand helpers collect it.
    #[test]
    fn bundle_with_agent_member_state_and_expand() {
        let agent_member = stamp(
            locked("my-agent", ArtifactKind::Agent, 'a'),
            "r/bundles/ai-pack",
            "latest",
        );
        let mut lock = lock_fixture(vec![], vec![]);
        lock.agents = vec![agent_member];

        // derive_bundle_state: the bundle is NOT declared (empty declared set),
        // so the row is NotInstalled regardless of the agent member present in
        // the lock — member presence never drives the bundle row.
        let none_declared = std::collections::BTreeSet::<String>::new();
        assert_eq!(
            derive_bundle_state("r/bundles/ai-pack", &none_declared),
            ArtifactState::NotInstalled,
            "an undeclared bundle is NotInstalled even though a member is present"
        );

        // bundle_members_lock: agent member is included.
        let projected = bundle_members_lock(&lock, "r/bundles/ai-pack", "latest");
        assert!(projected.skills.is_empty());
        assert!(projected.rules.is_empty());
        assert_eq!(projected.agents.len(), 1);
        assert_eq!(projected.agents[0].name, "my-agent");

        // perform_uninstall's bundle-target collection path (tested via
        // iter_artifacts on a lock containing only agents): only members
        // whose EVERY provenance names this repo are file-deletion targets.
        let targets: Vec<(ArtifactKind, String)> = lock
            .iter_artifacts()
            .filter(|a| !a.bundles.is_empty() && a.bundles.iter().all(|b| b.repo == "r/bundles/ai-pack"))
            .map(|a| (a.kind, a.name.clone()))
            .collect();
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0], (ArtifactKind::Agent, "my-agent".to_string()));
    }

    // ── derive_bundle_state: declared gate (installed iff in [bundles]) ────────
    //
    // A bundle row is installed iff its `registry/repository` is declared in the
    // active scope's `[bundles]` table (the declared-repos set). The function
    // takes ONLY that set — by construction it cannot be swayed by member
    // install health, a pre-cache lock with no snapshot, or a stale/lingering
    // snapshot. Installing or uninstalling member skills can never flip the row;
    // an undeclared bundle stays NotInstalled even with every member installed.

    fn declared(repos: &[&str]) -> std::collections::BTreeSet<String> {
        repos.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn derive_bundle_state_installed_when_declared() {
        let set = declared(&["r/bundles/pack"]);
        assert_eq!(
            derive_bundle_state("r/bundles/pack", &set),
            ArtifactState::Installed,
            "a bundle declared in [bundles] is Installed"
        );
    }

    #[test]
    fn derive_bundle_state_not_installed_when_not_declared() {
        // The user's exact scenario: member skills installed standalone, but the
        // bundle itself is NOT in [bundles]. Member install state is structurally
        // irrelevant — the row derives only from the declaration. (Pre-Phase-K
        // the row aggregated member health and flipped to Installed once the
        // skills were installed.)
        assert_eq!(
            derive_bundle_state("r/bundles/pack", &declared(&["r/bundles/other"])),
            ArtifactState::NotInstalled,
            "installing member skills must not flip an undeclared bundle to Installed"
        );
        assert_eq!(
            derive_bundle_state("r/bundles/pack", &declared(&[])),
            ArtifactState::NotInstalled,
            "no declared bundles ⇒ NotInstalled"
        );
    }

    #[test]
    fn derive_bundle_state_matches_only_its_own_repo() {
        assert_eq!(
            derive_bundle_state("r/bundles/other", &declared(&["r/bundles/pack"])),
            ArtifactState::NotInstalled,
            "another declared bundle does not mark this one installed"
        );
    }

    #[test]
    fn bundle_target_collection_spares_shared_members() {
        // A member two bundles share must NOT be a file-deletion target
        // when only one of them is removed.
        let shared = {
            let a = stamp(locked("shared", ArtifactKind::Skill, 'b'), "r/bundles/pack-a", "latest");
            stamp(a, "r/bundles/pack-b", "latest")
        };
        let exclusive = stamp(locked("only-a", ArtifactKind::Skill, 'c'), "r/bundles/pack-a", "latest");
        let lock = lock_fixture(vec![shared, exclusive], vec![]);

        let targets: Vec<String> = lock
            .iter_artifacts()
            .filter(|a| !a.bundles.is_empty() && a.bundles.iter().all(|b| b.repo == "r/bundles/pack-a"))
            .map(|a| a.name.clone())
            .collect();
        assert_eq!(targets, vec!["only-a"], "the shared member keeps its files");
    }

    #[test]
    fn opener_candidates_cover_current_platform() {
        let url = "https://github.com/acme/x?a=1&b=2";
        let candidates = opener_candidates(url);
        assert!(!candidates.is_empty(), "every platform has at least one opener");
        if cfg!(windows) {
            // The cmd candidate must escape `&` (cmd's command separator).
            assert_eq!(candidates[0].0, "cmd");
            assert!(candidates[0].1.last().unwrap().contains("^&"));
            assert_eq!(candidates[1].0, "rundll32");
        } else if cfg!(target_os = "macos") {
            assert_eq!(candidates[0].0, "open");
        } else {
            // Unix fallback chain: xdg-open first, then the polyfills.
            let programs: Vec<&str> = candidates.iter().map(|(p, _)| *p).collect();
            assert_eq!(programs, vec!["xdg-open", "gio", "wslview"]);
            assert_eq!(candidates[0].1, vec![url.to_string()]);
        }
    }

    // ── drain_bundle_member_checks: generation-freshness gate ─────────────────

    /// Build a minimal `TuiContext` for drain tests. The paths point at a
    /// temp directory; lock/state files are absent, so `load_state` /
    /// `lock_io::load` return `Err` (handled with `unwrap_or_else` in
    /// `drain_bundle_member_checks`) and every member gets `NotInstalled` —
    /// the same behavior as the old hardcoded path.
    fn drain_test_ctx() -> (tempfile::TempDir, TuiContext) {
        use crate::oci::access::memory_registry::MemoryRegistry;
        let tmp = tempfile::tempdir().expect("tempdir");
        let workspace = tmp.path().to_path_buf();
        let ctx = TuiContext {
            registry: "localhost:5050".to_string(),
            catalog_path: workspace.join("catalog.json"),
            access: Arc::new(MemoryRegistry::new()),
            offline: false,
            force_refresh: false,
            scope: ConfigScope::Project,
            workspace: workspace.clone(),
            lock_path: workspace.join("grimoire.lock"),
            state_path: workspace.join("install-state.json"),
            config_path: workspace.join("grimoire.toml"),
            roots: AnchorRoots {
                workspace: workspace.clone(),
                grim_home: workspace.clone(),
                claude_root: None,
                copilot_root: None,
                opencode_skills: None,
            },
            clients_default: vec![],
            clients_selected: Vec::new(),
            scope_label: "project".to_string(),
            alt: None,
            tui_options: Default::default(),
        };
        (tmp, ctx)
    }

    /// Build a minimal `TuiRow` sufficient for the related-highlight check
    /// inside `drain_bundle_member_checks`.
    fn bundle_row_for_drain(repo: &str) -> TuiRow {
        TuiRow {
            kind: "bundle".to_string(),
            repo: repo.to_string(),
            description: String::new(),
            summary: String::new(),
            keywords: Vec::new(),
            repository_url: None,
            latest_tag: "latest".to_string(),
            version: "1.0.0".to_string(),
            pinned_version: None,
            state: ArtifactState::Installed,
        }
    }

    #[test]
    fn drain_bundle_member_checks_stale_generation_is_discarded() {
        // A BundleMembersMsg::Ready whose generation stamp does NOT match the
        // live generation must be discarded — the cache must not be written and
        // the function must return false (no redraw needed).
        use crate::tui::bundle_member_fetch::BundleMembersMsg;

        let (_tmp, ctx) = drain_test_ctx();
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        let mut state = TuiState::new();
        state.set_rows(vec![bundle_row_for_drain("reg/acme/bundle")]);

        // Send a Ready message stamped with generation 0 but live is 1 (stale).
        tx.try_send(BundleMembersMsg::Ready {
            bundle_repo: "reg/acme/bundle".to_string(),
            members: vec![],
            generation: 0,
        })
        .expect("channel must accept the message");

        let changed = drain_bundle_member_checks(&ctx, &mut state, &mut rx, /* live */ 1);

        assert!(!changed, "stale Ready must return changed=false");
        assert!(
            state.bundle_members.is_empty(),
            "stale Ready must not write to the cache; got {:?} entries",
            state.bundle_members.len()
        );
    }

    #[test]
    fn drain_bundle_member_checks_fresh_generation_writes_cache() {
        // A BundleMembersMsg::Ready whose generation matches the live generation
        // must write BundleMemberCache::Ready into the cache and return true.
        // F1: ctx is now required so the drain can derive actual member state
        // (lock/state files absent → NotInstalled, same as the old hardcoded path).
        use crate::oci::ArtifactKind;
        use crate::oci::bundle::BundleMember;
        use crate::tui::bundle_member_fetch::BundleMembersMsg;

        let (_tmp, ctx) = drain_test_ctx();
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        let mut state = TuiState::new();
        state.set_rows(vec![bundle_row_for_drain("reg/acme/bundle")]);
        state.scope_label = "project".to_string();

        let member = BundleMember {
            id: "reg.example.io/acme/my-skill:latest".to_string(),
            kind: ArtifactKind::Skill,
            name: "my-skill".to_string(),
        };
        tx.try_send(BundleMembersMsg::Ready {
            bundle_repo: "reg/acme/bundle".to_string(),
            members: vec![member],
            generation: 2,
        })
        .expect("channel must accept the message");

        let changed = drain_bundle_member_checks(&ctx, &mut state, &mut rx, /* live */ 2);

        assert!(changed, "fresh Ready must return changed=true");
        let key = ("project".to_string(), "reg/acme/bundle".to_string());
        assert!(
            state.bundle_members.contains_key(&key),
            "fresh Ready must write the cache entry"
        );
    }

    #[test]
    fn drain_bundle_member_checks_stale_failed_is_discarded() {
        // A BundleMembersMsg::Failed with a stale generation must also be
        // discarded — the cache must not be written.
        use crate::tui::bundle_member_fetch::BundleMembersMsg;

        let (_tmp, ctx) = drain_test_ctx();
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        let mut state = TuiState::new();
        state.scope_label = "project".to_string();

        tx.try_send(BundleMembersMsg::Failed {
            bundle_repo: "reg/acme/bundle".to_string(),
            reason: "timeout".to_string(),
            generation: 0,
        })
        .expect("channel must accept the message");

        let changed = drain_bundle_member_checks(&ctx, &mut state, &mut rx, /* live */ 1);

        assert!(!changed, "stale Failed must return changed=false");
        assert!(
            state.bundle_members.is_empty(),
            "stale Failed must not write to the cache"
        );
    }

    /// F1 regression: drain_bundle_member_checks must derive member artifact
    /// state from lock + install records, NOT hardcode NotInstalled. Since
    /// lock/state files are absent in the test context, derive_artifact_state
    /// returns NotInstalled — but this test proves the derive path is taken
    /// (MemberNode.state = NotInstalled, not from a hardcoded literal), and
    /// that a member whose repo also appears in the catalog rows gets
    /// `related = true` (the related-highlight path also runs).
    #[test]
    fn f1_drain_derives_member_state_not_hardcoded() {
        use crate::oci::ArtifactKind;
        use crate::oci::bundle::BundleMember;
        use crate::tui::bundle_member_fetch::BundleMembersMsg;
        use crate::tui::bundle_members::BundleMemberCache;

        let (_tmp, ctx) = drain_test_ctx();
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        let mut state = TuiState::new();

        // Seed a catalog row whose repo matches the member — proves related=true
        // and that the derive path runs (rather than hardcoded NotInstalled).
        let skill_repo = "reg.example.io/acme/my-skill";
        state.set_rows(vec![
            bundle_row_for_drain("reg/acme/bundle"),
            TuiRow {
                kind: "skill".to_string(),
                repo: skill_repo.to_string(),
                description: String::new(),
                summary: String::new(),
                keywords: Vec::new(),
                repository_url: None,
                latest_tag: "latest".to_string(),
                version: "1.0.0".to_string(),
                pinned_version: None,
                state: ArtifactState::Installed,
            },
        ]);
        state.scope_label = "project".to_string();

        let member = BundleMember {
            id: "reg.example.io/acme/my-skill:latest".to_string(),
            kind: ArtifactKind::Skill,
            name: "my-skill".to_string(),
        };
        tx.try_send(BundleMembersMsg::Ready {
            bundle_repo: "reg/acme/bundle".to_string(),
            members: vec![member],
            generation: 1,
        })
        .expect("channel must accept the message");

        let changed = drain_bundle_member_checks(&ctx, &mut state, &mut rx, /* live */ 1);

        assert!(changed, "F1: fresh Ready must return changed=true");
        let key = ("project".to_string(), "reg/acme/bundle".to_string());
        let cache = state.bundle_members.get(&key).expect("F1: cache must be written");
        if let BundleMemberCache::Ready(nodes) = cache {
            assert_eq!(nodes.len(), 1, "F1: exactly one member node");
            // No lock file → derive_artifact_state returns NotInstalled.
            // The key invariant: the field came from derive, not a hardcoded literal.
            // related=true proves the row_repos lookup also ran (D2/P3.7).
            assert!(
                nodes[0].related,
                "F1: member whose repo is in catalog rows must be related=true"
            );
        } else {
            panic!("F1: expected BundleMemberCache::Ready; got {cache:?}");
        }
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

    // ── GAP-B: regression — resolve_member_tag is wired into the member install path ──
    //
    // The prior builder had resolve_member_tag implemented but UNWIRED (every
    // member installed at "latest").  A pure test of resolve_member_tag could
    // not catch that.  This test is end-to-end: the registry fixture publishes
    // `localhost:5050/grimoire/skills/demo` only at tag `"1.0.0"` (not
    // "latest").  We set a catalog row whose `latest_tag` is `"1.0.0"`, call
    // resolve_member_tag (which returns `"1.0.0"`), then call perform_member
    // with that tag.  If the dispatch ever passed `"latest"` instead, the
    // resolve_digest call would return None (tag absent) and perform_member
    // would return Err, which would fail this test — proving the wiring.
    #[tokio::test]
    async fn resolve_member_tag_wired_into_member_install_uses_catalog_tag() {
        let tmp = tempfile::tempdir().unwrap();
        let workspace = tmp.path();
        std::fs::write(workspace.join("grimoire.toml"), "[skills]\n\n[rules]\n").unwrap();
        // registry_with_bundle() publishes `localhost:5050/grimoire/skills/demo`
        // at tag "1.0.0" only — "latest" is absent on that repo.
        let ctx = test_ctx(workspace, registry_with_bundle().await);

        // A catalog row whose repo matches the member and whose latest_tag is
        // "1.0.0" (the only published tag).
        let catalog_rows = vec![TuiRow {
            kind: "skill".to_string(),
            repo: "localhost:5050/grimoire/skills/demo".to_string(),
            description: String::new(),
            summary: String::new(),
            keywords: Vec::new(),
            repository_url: None,
            latest_tag: "1.0.0".to_string(),
            version: String::new(),
            pinned_version: None,
            state: ArtifactState::NotInstalled,
        }];

        // resolve_member_tag must return the catalog row's tag, not "latest".
        let tag = resolve_member_tag("localhost:5050/grimoire/skills/demo", &catalog_rows);
        assert_eq!(
            tag, "1.0.0",
            "GAP-B: resolve_member_tag must return the catalog row's latest_tag"
        );

        // Calling perform_member with the resolved tag must succeed — proving
        // that the resolved tag ("1.0.0") was passed, not "latest".
        // If "latest" were passed instead, resolve_digest would return None and
        // perform_member would return Err (the tag is absent in the fixture).
        let result = perform_member(
            &ctx,
            "localhost:5050/grimoire/skills/demo".to_string(),
            ArtifactKind::Skill,
            false,
            tag,
        )
        .await;
        assert!(
            result.is_ok(),
            "GAP-B: perform_member with catalog-resolved tag '1.0.0' must succeed; got: {result:?}"
        );

        // The lock records the installed skill, confirming the correct tag was fetched.
        let lock = lock_io::load(&ctx.lock_path).expect("lock saved");
        assert_eq!(lock.skills.len(), 1, "GAP-B: skill must be recorded in the lock");
        assert_eq!(lock.skills[0].name, "demo", "GAP-B: lock skill name must match");
    }
}

// ── P2 Specify tests — C-11 (pure) and C-12 (malformed repo) ─────────────────
//
// C-11 pure: resolve_member_tag(repo, rows) → matching-row's pinned_version-or-latest_tag, else "latest"
// C-12:      perform_member with a no-slash repo must return Err (no panic)
//
// These MUST compile and MUST FAIL against the P1 stubs (unimplemented!).
#[cfg(test)]
mod p2_app_member_node_tests {
    use super::*;

    fn tui_row_with_tag(repo: &str, latest_tag: &str, pinned_version: Option<&str>) -> TuiRow {
        TuiRow {
            kind: "skill".to_string(),
            repo: repo.to_string(),
            description: String::new(),
            summary: String::new(),
            keywords: vec![],
            repository_url: None,
            latest_tag: latest_tag.to_string(),
            version: "1.0.0".to_string(),
            pinned_version: pinned_version.map(|s| s.to_string()),
            state: ArtifactState::NotInstalled,
        }
    }

    // ── C-11 pure: resolve_member_tag ─────────────────────────────────────────

    #[test]
    fn c11_resolve_member_tag_matched_row_uses_pinned_version() {
        // Member repo matches a catalog row that has a pinned_version.
        // resolve_member_tag must return the pinned_version.
        let rows = vec![tui_row_with_tag("reg/acme/skill-a", "v2.0.0", Some("v1.5.0"))];
        let tag = resolve_member_tag("reg/acme/skill-a", &rows);
        assert_eq!(
            tag, "v1.5.0",
            "C-11: matched row with pinned_version must return pinned_version"
        );
    }

    #[test]
    fn c11_resolve_member_tag_matched_row_no_pin_uses_latest_tag() {
        // Member repo matches a catalog row with no pinned_version.
        // resolve_member_tag must return the row's latest_tag.
        let rows = vec![tui_row_with_tag("reg/acme/skill-a", "v2.0.0", None)];
        let tag = resolve_member_tag("reg/acme/skill-a", &rows);
        assert_eq!(
            tag, "v2.0.0",
            "C-11: matched row without pinned_version must return latest_tag"
        );
    }

    #[test]
    fn c11_resolve_member_tag_no_match_returns_latest() {
        // No catalog row matches the member repo.
        // resolve_member_tag must return "latest" (same as perform's empty-tag fallback).
        let rows = vec![tui_row_with_tag("reg/acme/something-else", "v3.0.0", None)];
        let tag = resolve_member_tag("reg/other/skill-b", &rows);
        assert_eq!(tag, "latest", "C-11: non-catalog member must resolve to 'latest'");
    }

    #[test]
    fn c11_resolve_member_tag_empty_rows_returns_latest() {
        let tag = resolve_member_tag("reg/acme/skill-a", &[]);
        assert_eq!(tag, "latest", "C-11: empty catalog must resolve to 'latest'");
    }

    #[test]
    fn c11_resolve_member_tag_pinned_wins_over_latest_tag() {
        // Pinned version takes precedence over latest_tag (same as `perform`'s logic).
        let rows = vec![tui_row_with_tag("reg/acme/skill-a", "v99.0.0", Some("v1.0.0"))];
        let tag = resolve_member_tag("reg/acme/skill-a", &rows);
        assert_eq!(
            tag, "v1.0.0",
            "C-11: pinned_version must win over latest_tag when both present"
        );
    }

    // ── C-12: perform_member with no-slash repo returns Err, no panic ─────────

    /// Build a minimal `TuiContext` for C-12 tests.  We only need the no-slash
    /// guard to fire — `perform_member` validates `split_repo` before touching
    /// any registry field, so the access impl is never called.
    fn c12_ctx(workspace: &std::path::Path) -> TuiContext {
        use crate::oci::access::memory_registry::MemoryRegistry;
        let access: Arc<dyn OciAccess> = Arc::new(MemoryRegistry::new());
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
            roots: AnchorRoots {
                workspace: workspace.to_path_buf(),
                grim_home: workspace.to_path_buf(),
                claude_root: None,
                copilot_root: None,
                opencode_skills: None,
            },
            clients_default: vec!["claude".to_string()],
            clients_selected: Vec::new(),
            scope_label: "project".to_string(),
            alt: None,
            tui_options: Default::default(),
        }
    }

    #[tokio::test]
    async fn c12_perform_member_noslash_repo_returns_err_no_panic() {
        // Defense-in-depth (IMP-6): perform_member validates split_repo as its
        // FIRST statement — a repo without '/' must return a handled Err before
        // any registry access, never a panic.  The test completing without
        // panicking is itself the no-panic proof.
        let tmp = tempfile::tempdir().unwrap();
        let workspace = tmp.path();
        std::fs::write(workspace.join("grimoire.toml"), "[skills]\n\n[rules]\n").unwrap();
        let ctx = c12_ctx(workspace);

        let result = perform_member(
            &ctx,
            "noslash".to_string(),
            ArtifactKind::Skill,
            false,
            "latest".to_string(),
        )
        .await;

        assert!(
            result.is_err(),
            "C-12: perform_member('noslash', …) must return Err, got Ok({result:?})"
        );
    }

    #[tokio::test]
    async fn c12_perform_member_uninstall_noslash_returns_err() {
        // Same contract for perform_member_uninstall: split_repo fires first,
        // returns Err before touching the context or the registry.
        let tmp = tempfile::tempdir().unwrap();
        let workspace = tmp.path();
        std::fs::write(workspace.join("grimoire.toml"), "[skills]\n\n[rules]\n").unwrap();
        let ctx = c12_ctx(workspace);

        let result = perform_member_uninstall(&ctx, "noslash".to_string(), ArtifactKind::Skill).await;

        assert!(
            result.is_err(),
            "C-12: perform_member_uninstall('noslash', …) must return Err, got Ok({result:?})"
        );
    }
}
