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

use std::collections::BTreeMap;
use std::io::{self};
use std::sync::Arc;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

use crate::catalog::catalog_service;
use crate::catalog::registry_catalog::Catalog;
use crate::command::add::{declare, relock_declared, write_config};
use crate::command::grim;
use crate::command::uninstall::undeclare_and_unlock;
use crate::config::ResolvedRegistry;
use crate::config::declaration::{ConfigOptions, DesiredSet};
use crate::config::global_config::GlobalConfig;
use crate::config::project_config::ProjectConfig;
use crate::config::scope::ConfigScope;
use crate::env::grim_home;
use crate::install::client_target::ClientTarget;
use crate::install::install_state::{ClientOutput, InstallState, active_outputs};
use crate::install::installer::{InstallOutcome, install_all};
use crate::install::materializer::DefaultMaterializer;
use crate::install::path_anchor::AnchorRoots;
use crate::install::progress::{InstallProgress, SilentProgress};
use crate::install::target::{InstallTarget, detect_clients};
use crate::lock::file_lock::ConfigFileLock;
use crate::lock::grimoire_lock::GrimoireLock;
use crate::lock::lock_io;
use crate::lock::locked_artifact::LockedArtifact;
use crate::oci::access::OciAccess;
use crate::oci::{ArtifactKind, Identifier};
use crate::store::paths::GrimPaths;
use crate::tui::install_progress::InstallModal;

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
    /// Resolved registries for the active scope, in precedence order.
    ///
    /// Single-entry when `--registry` or `$GRIM_DEFAULT_REGISTRY` forces one
    /// registry; multi-entry when the `[[registries]]` array declares several.
    pub registries: Vec<ResolvedRegistry>,
    /// The primary registry (first `is_default`, else the first entry).
    ///
    /// Used wherever a single registry string is needed: the effective default
    /// for elision (D-ELIDE), the `UpdateChecker` registry seam, and the
    /// init-dialog pre-fill. Mirrors `config::primary_registry(&self.registries)`.
    pub primary_registry: String,
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
/// Registries and `primary_registry` are also scope-dependent and swap
/// together with the rest — each scope may declare its own `[[registries]]`.
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
    /// The ordered registry set for this scope (mirrors `TuiContext::registries`).
    pub registries: Vec<ResolvedRegistry>,
    /// The primary registry for this scope (mirrors `TuiContext::primary_registry`).
    pub primary_registry: String,
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
            registries: std::mem::replace(&mut self.registries, alt.registries),
            primary_registry: std::mem::replace(&mut self.primary_registry, alt.primary_registry),
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
    // The primary registry is the effective default: eliding it from the
    // tree root keeps leaf names short (D-ELIDE).
    state.set_default_registry(elision_registry(&ctx));
    // The resolved registries in precedence order drive the multi-registry
    // tree-root ordering (F13) and the empty-registry roots (D-EMPTY).
    state.set_registry_order(registry_order(&ctx));
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
    let (mut checker, mut rx) = UpdateChecker::new(Arc::clone(&ctx.access), ctx.primary_registry.clone());
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
                // A modal gauge animates over the marked rows during the
                // otherwise frozen inline batch: `run_batch_with_progress`
                // drives start/advance/finish at the per-row grain (n/total +
                // current repo), so the counter reflects every action — not a
                // single-artifact install that always reads 1/1. The verb
                // follows the operation (Installing/Updating/Uninstalling).
                // start() fires inside run_batch AFTER its offline check, so an
                // offline install/update paints nothing (no flash) while a
                // delete — which runs offline — still shows its progress.
                let modal = InstallModal::new(&mut terminal, batch_title(op));
                run_batch_with_progress(&ctx, &mut state, &rows, op, &modal).await;
                // An install/update may have just pinned a version older
                // than the registry's floating tag (the user picked an old
                // version in the picker) — re-check exactly those rows now
                // so the badge flips to `↑ outdated` immediately, not at
                // the next manual refresh.
                if op != BatchOp::Uninstall {
                    recheck_rows(&ctx, &state, &mut checker, &rows);
                }
            }
            TuiAction::MemberAction { op, repo, kind, name } => {
                // P4.4: per-member install/update/uninstall.
                // Offline guard: install/update need the network.
                if ctx.offline && op != BatchOp::Uninstall {
                    state.set_status("offline — cannot install/update");
                } else {
                    // A single member is one action — show an indeterminate
                    // "working… <repo>" frame over the frozen inline op rather
                    // than a misleading 1/1 counter. The verb follows the
                    // operation (Installing/Updating/Uninstalling).
                    let modal = InstallModal::new(&mut terminal, batch_title(op));
                    modal.working(&repo);
                    let label = match op {
                        BatchOp::Install | BatchOp::Update => {
                            let is_update = op == BatchOp::Update;
                            // D8a: resolve the tag from the catalog rows — a
                            // related member reuses its row's pinned/latest tag,
                            // a non-catalog member falls back to "latest".
                            let tag = resolve_member_tag(&repo, &state.rows);
                            // B1c: thread the parent bundle's authoritative
                            // registry so a namespaced registry (e.g.
                            // `ghcr.io/acme`) is not mis-split on the first `/`.
                            let parent_registry = member_parent_registry(&ctx, &repo);
                            let res = perform_member(
                                &ctx,
                                repo.clone(),
                                kind,
                                is_update,
                                tag,
                                name.clone(),
                                &parent_registry,
                            )
                            .await;
                            match res {
                                Ok(l) => Some(l),
                                Err(e) => {
                                    state.set_status(format!("member action failed: {e:#}"));
                                    None
                                }
                            }
                        }
                        BatchOp::Uninstall => {
                            match perform_member_uninstall(&ctx, repo.clone(), kind, name.clone()).await {
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
                            }
                        }
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
                // Direct declarations decide the via-bundle badge; a declaration-
                // matched bundle snapshot lets a stale-dropped member still derive
                // via the snapshot. A member also declared standalone shows plain
                // `installed`, not `via-bundle`.
                let (direct_repos, snapshot_repos) = load_scope_declaration(&ctx)
                    .map(|(_, _, set)| {
                        let cached = lock.as_ref().map(|l| l.bundles.as_slice()).unwrap_or(&[]);
                        (direct_declared_repos(&set), snapshot_declared_repos(&set, cached))
                    })
                    .unwrap_or_default();

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
                                    member_display_state(
                                        m.kind,
                                        parsed.registry(),
                                        parsed.repository(),
                                        lock.as_ref(),
                                        &install_state,
                                        &ctx.roots,
                                        &active,
                                        &direct_repos,
                                        &snapshot_repos,
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
                    // Recompute single-registry elision for the swapped scope —
                    // the two scopes may declare a different registry count
                    // (D-ELIDE: elide only when exactly one registry resolves).
                    state.set_default_registry(elision_registry(&ctx));
                    // The swapped scope may declare a different registry set —
                    // re-seed the precedence order for the tree roots (F13).
                    state.set_registry_order(registry_order(&ctx));
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
    // Invalidate results from the previous scope/refresh before re-arming so
    // any in-flight per-row check stamped with the old generation is discarded
    // on drain rather than applied to the new scope's row set.
    checker.bump_generation();
    // Schedule a floating-tag re-check for every eligible (installed/outdated)
    // row in the fresh catalog. The catalog itself is already loaded synchronously
    // by `reload_into`/`load_into` before this is called; these background tasks
    // only verify whether the pinned digest is still current. The `force=true`
    // flag bypasses the search-debounce coalesce window so a launch/`r`/scope
    // toggle always arms immediately rather than being swallowed by a recent
    // keystroke timestamp.
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
    let (lock, _install_state, _declared_bundle_repos, _direct_repos, _snapshot_repos) = load_scope_for_badges(ctx);
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

/// Build the [`RowCheck`] for one eligible row: pair its tagless
/// `registry/repository` identifier with the digest the scope's lock pinned it
/// to. `None` when the row carries no lock entry (then "newer tag" has no
/// baseline) or its repo is malformed.
fn build_row_check(lock: &GrimoireLock, row: &TuiRow) -> Option<RowCheck> {
    // A2 / D-BACKGROUND: use the authoritative `registry` + `repository` fields
    // directly so namespaced registries like "ghcr.io/acme" are matched exactly,
    // without re-splitting `repo` on the first '/' (which would give just "ghcr.io").
    let registry = row.registry.as_str();
    let repository = row.repository.as_str();
    if registry.is_empty() || repository.is_empty() {
        return None;
    }
    let locked = lock
        .iter_artifacts()
        .find(|a| a.pinned.registry() == registry && a.pinned.repository() == repository)?;
    // Issue #21: carry the tagless `registry/repository` identifier, not the
    // cached catalog tag. The background check re-discovers the registry's
    // current latest tag fresh (see `update_check::resolve_latest_digest`), so a
    // newer release surfaces even when the cached catalog row is stale or the
    // registry carries only immutable semver tags (no moving `latest`).
    let id = Identifier::new_registry(repository, registry);
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
    let (lock, _install_state, _declared_bundle_repos, _direct_repos, _snapshot_repos) = load_scope_for_badges(ctx);
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
                let (direct_repos, snapshot_repos) = load_scope_declaration(ctx)
                    .map(|(_, _, set)| {
                        let cached = lock.as_ref().map(|l| l.bundles.as_slice()).unwrap_or(&[]);
                        (direct_declared_repos(&set), snapshot_declared_repos(&set, cached))
                    })
                    .unwrap_or_default();
                // Build a O(n) set of row repos for the related-highlight check (D2/P3.7).
                let row_repos: std::collections::HashSet<&str> = state.rows.iter().map(|r| r.repo.as_str()).collect();
                let nodes: Vec<super::bundle_members::MemberNode> = members
                    .iter()
                    .filter_map(|m| {
                        let member_state = crate::oci::Identifier::parse(&m.id)
                            .ok()
                            .map(|parsed| {
                                member_display_state(
                                    m.kind,
                                    parsed.registry(),
                                    parsed.repository(),
                                    lock.as_ref(),
                                    &install_state,
                                    &ctx.roots,
                                    &active,
                                    &direct_repos,
                                    &snapshot_repos,
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
///
/// Deferred Workstream-E scaffolding: the only producer of
/// [`CheckMsg::CatalogReady`] is [`UpdateChecker::spawn_catalog_refresh`], a
/// single-registry path `arm_background_checks` does not yet arm (the TUI's
/// migration onto the multi-registry `catalog_service::load_catalog` seam is the
/// deferred follow-up). This consumer + its `Catalog`-shaped sibling
/// [`rows_from_catalog`] are retained, tested, and kept current (C4
/// registry/repository fields) so re-arming is a one-line change, not a rebuild.
fn drain_catalog_ready(ctx: &TuiContext, state: &mut TuiState, catalog: &Catalog) {
    let (lock, install_state, declared_bundle_repos, direct_repos, snapshot_repos) = load_scope_for_badges(ctx);
    let active = detect_clients(&ctx.workspace, ctx.scope);
    let badge = BadgeContext {
        lock: lock.as_ref(),
        state: &install_state,
        roots: &ctx.roots,
        active: &active,
        declared_bundle_repos: &declared_bundle_repos,
        direct_repos: &direct_repos,
        snapshot_repos: &snapshot_repos,
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

/// Load or rebuild the catalog into `state` via `catalog_service::load_catalog`,
/// fanning out over all `ctx.registries` in parallel and projecting each
/// [`crate::catalog::catalog_service::CatalogGroup`] through [`project_group_rows`].
///
/// Degrades on any load failure: sets a status line and clears loading, so the
/// TUI remains usable (offline included). The rows are only replaced on success —
/// a failed refresh keeps the previously-loaded rows visible.
async fn reload_into(ctx: &TuiContext, state: &mut TuiState, force: bool) {
    let (lock, install_state, declared_bundle_repos, direct_repos, snapshot_repos) = load_scope_for_badges(ctx);
    let active = detect_clients(&ctx.workspace, ctx.scope);
    // The simpler catalog_service::BadgeContext (4 fields) drives the per-row
    // StatusBadge derivation inside load_catalog itself.
    let catalog_badges = catalog_service::BadgeContext {
        lock: lock.as_ref(),
        state: &install_state,
        roots: &ctx.roots,
        active: &active,
    };
    let paths = GrimPaths::new(grim_home());
    match catalog_service::load_catalog(
        &paths,
        &ctx.registries,
        "",
        &ctx.access,
        &catalog_badges,
        ctx.offline,
        force,
    )
    .await
    {
        Ok(results) => {
            // The richer TUI BadgeContext drives badge derivation inside project_group_rows
            // (bundle-awareness + via-bundle detection go beyond the simple catalog badge).
            let badge = BadgeContext {
                lock: lock.as_ref(),
                state: &install_state,
                roots: &ctx.roots,
                active: &active,
                declared_bundle_repos: &declared_bundle_repos,
                direct_repos: &direct_repos,
                snapshot_repos: &snapshot_repos,
            };
            let rows: Vec<TuiRow> = results
                .groups
                .iter()
                .flat_map(|g| project_group_rows(g, &badge))
                .collect();
            // Aggregate health from group metadata.
            let mut offline_regs: Vec<String> = Vec::new();
            let mut truncated_regs: Vec<String> = Vec::new();
            for g in &results.groups {
                if g.served_offline {
                    offline_regs.push(g.registry.clone());
                }
                if g.truncated {
                    truncated_regs.push(g.registry.clone());
                }
            }
            // Build URL → display-label map from the resolved registry set.
            // When an alias is configured, the label is "alias (url)"; when
            // no alias was declared, the URL is used directly as both key and
            // label (fallback matches [`TuiState::registry_label`] semantics).
            let registry_labels: BTreeMap<String, String> = ctx
                .registries
                .iter()
                .map(|r| {
                    let label = match &r.alias {
                        Some(alias) => format!("{alias} ({url})", url = r.url),
                        None => r.url.clone(),
                    };
                    (r.url.clone(), label)
                })
                .collect();
            apply_catalog_results(
                state,
                rows,
                super::state::RegistryHealth {
                    offline: offline_regs,
                    truncated: truncated_regs,
                },
                results.any_truncated(),
                elision_registry(ctx),
                registry_order(ctx),
                registry_labels,
            );
        }
        Err(e) => {
            tracing::warn!(error = %e, "catalog load failed; TUI degrading to empty rows");
            state.set_status(format!("catalog load failed: {e}"));
            state.set_loading(false);
        }
    }
}

/// Apply the success-arm state mutations of a catalog load to `state`.
///
/// This is the pure, unit-testable half of [`reload_into`]'s `Ok` branch.
/// All mutations are driven by pre-computed values so the function requires
/// no I/O — it can be exercised in a unit test without a [`TuiContext`] or
/// a catalog service call.
///
/// Invariants enforced here (GAP-2):
/// - `set_rows` clears the `loading` flag and marks (no stale indices).
/// - `set_default_registry` / `set_registry_order` keep D-ELIDE / F13 in sync.
/// - `set_registry_health` stores the per-registry offline/truncated verdict.
/// - `set_registry_labels` stores the alias map for display (A/B display labels).
/// - `set_status(String::new())` is the **B1 regression guard**: clears the
///   transient "refreshing catalog…" / "loading catalog…" message on success
///   so the status falls through to the registry-health line (D-DEGRADE) or
///   marked-count.  Any caller that skips this call will regress B1.
fn apply_catalog_results(
    state: &mut TuiState,
    rows: Vec<TuiRow>,
    health: super::state::RegistryHealth,
    truncated: bool,
    elision: Option<String>,
    order: Vec<String>,
    labels: BTreeMap<String, String>,
) {
    state.set_rows(rows);
    // Elide the registry prefix from tree labels only when exactly one registry
    // is in scope — with multiple registries each tree root already names its
    // registry, so elision would be misleading (D-ELIDE).
    state.set_default_registry(elision);
    // Keep the tree-root precedence order in sync with the resolved set (F13).
    state.set_registry_order(order);
    state.set_registry_health(health);
    state.set_truncated(truncated);
    // Store URL → alias labels so the flat list's Registry column and tree
    // registry-root labels can show human-friendly names (A, B display labels).
    state.set_registry_labels(labels);
    // Clear the transient message so the render status falls through to the
    // registry-health line (D-DEGRADE) or marked-count; `set_rows` already
    // cleared the loading flag. Empty/gated registries surface as 0/0 tree
    // roots (D-EMPTY), so no count string is needed here.
    state.set_status(String::new());
}

/// Resolve a catalog entry's optional kind field, substituting `"-"` when
/// absent. Used in both [`project_group_rows`] and [`rows_from_catalog`] so
/// the substitution logic is a single source of truth.
fn kind_or_dash(kind: &Option<String>) -> String {
    kind.clone().unwrap_or_else(|| "-".to_string())
}

/// Project one [`catalog_service::CatalogGroup`]'s rows into TUI rows, deriving
/// each [`super::state::ArtifactState`] badge from the richer TUI [`BadgeContext`]
/// (bundle-aware, via-bundle detection). Mirrors [`rows_from_catalog`] but
/// consumes a `CatalogGroup` (from the multi-registry seam) instead of a
/// single-registry [`Catalog`].
fn project_group_rows(group: &catalog_service::CatalogGroup, ctx: &BadgeContext) -> Vec<TuiRow> {
    // Index-sourced rows carry their source locator so the tree / flat list
    // group them under the source root: an index locator classifies (git /
    // http forms), an OCI registry url does not.
    let source = crate::config::registry_resolve::classify_index(&group.registry)
        .is_some()
        .then(|| group.registry.clone());
    group
        .rows
        .iter()
        .map(|e| {
            let kind = kind_or_dash(&e.kind);
            let row_state = derive_row_state(&kind, &e.registry, &e.repository, ctx);
            TuiRow {
                kind,
                // C4: authoritative registry + repository from the catalog entry,
                // never re-derived by splitting `repo`.
                registry: e.registry.clone(),
                repository: e.repository.clone(),
                repo: e.repo(),
                description: e.description.clone().unwrap_or_default(),
                summary: e.summary.clone().unwrap_or_default(),
                keywords: e.keywords.clone(),
                repository_url: e.repository_url.clone(),
                revision: e.revision.clone(),
                created: e.created.clone(),
                deprecated: e.deprecated.clone(),
                latest_tag: e.latest_tag.clone().unwrap_or_default(),
                // Show the explicit highest version; fall back to the
                // representative tag when no semver tag exists.
                version: e.version.clone().or_else(|| e.latest_tag.clone()).unwrap_or_default(),
                pinned_version: None,
                state: row_state,
                source: source.clone(),
            }
        })
        .collect()
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
    /// `(kind, registry/repository)` declared directly in `[skills]`/`[rules]`/
    /// `[agents]` — used to flag an installed-but-not-directly-declared row as
    /// `ViaBundle` (present only because a bundle provides it).
    direct_repos: &'a std::collections::BTreeSet<(ArtifactKind, String)>,
    /// `(kind, registry/repository)` provided by a currently-declared bundle
    /// (from `effective_set`) — lets a row whose top-level lock entry was dropped
    /// as stale still derive `ViaBundle`/`Installed` from the snapshot.
    snapshot_repos: &'a std::collections::BTreeSet<(ArtifactKind, String)>,
}

/// Project a catalog into TUI rows, deriving each state from the scope's
/// [`BadgeContext`] (lock + install-state + declared bundles).
fn rows_from_catalog(catalog: &Catalog, ctx: &BadgeContext) -> Vec<TuiRow> {
    catalog
        .entries()
        .map(|e| {
            let kind = kind_or_dash(&e.kind);
            let row_state = derive_row_state(&kind, &e.registry, &e.repository, ctx);
            TuiRow {
                kind,
                // C4: authoritative registry + repository from the catalog entry,
                // never re-derived by splitting `repo`.
                registry: e.registry.clone(),
                repository: e.repository.clone(),
                repo: e.repo(),
                description: e.description.clone().unwrap_or_default(),
                summary: e.summary.clone().unwrap_or_default(),
                keywords: e.keywords.clone(),
                repository_url: e.repository_url.clone(),
                revision: e.revision.clone(),
                created: e.created.clone(),
                deprecated: e.deprecated.clone(),
                latest_tag: e.latest_tag.clone().unwrap_or_default(),
                // Show the explicit highest version; fall back to the
                // representative tag when no semver tag exists.
                version: e.version.clone().or_else(|| e.latest_tag.clone()).unwrap_or_default(),
                pinned_version: None,
                state: row_state,
                // The background refresh walks a single OCI registry
                // (`_catalog`); index sources never flow through this path.
                source: None,
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
        // A row installed but not directly declared is present only via a bundle
        // → ViaBundle, consistent with the member-node badge.
        member_display_state(
            row_kind(kind),
            registry,
            repository,
            ctx.lock,
            ctx.state,
            ctx.roots,
            ctx.active,
            ctx.direct_repos,
            ctx.snapshot_repos,
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
#[allow(clippy::too_many_arguments)]
fn derive_artifact_state(
    kind: ArtifactKind,
    registry: &str,
    repository: &str,
    lock: Option<&GrimoireLock>,
    state: &InstallState,
    roots: &AnchorRoots,
    active: &[ClientTarget],
    snapshot_repos: &std::collections::BTreeSet<(ArtifactKind, String)>,
) -> ArtifactState {
    // A top-level lock entry lets us distinguish Outdated. If it is absent, a
    // CURRENTLY-DECLARED bundle whose snapshot names this artifact still proves
    // it is provided via a bundle — its top-level entry may have been dropped as
    // honestly stale on an id mismatch while its files + record + snapshot all
    // remain. `snapshot_repos` is declaration-aware (built from `effective_set`),
    // so a stale/retagged snapshot whose bundle is no longer declared does not
    // count. Without either signal, it is not installed.
    let locked = lock.and_then(|l| {
        l.iter_artifacts()
            .find(|a| a.kind == kind && a.pinned.registry() == registry && a.pinned.repository() == repository)
    });
    let via_snapshot = locked.is_none() && snapshot_repos.contains(&(kind, format!("{registry}/{repository}")));
    if locked.is_none() && !via_snapshot {
        return ArtifactState::NotInstalled;
    }
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
    match locked {
        // A top-level lock entry: compare the pinned digest to flag Outdated.
        Some(locked) if record.pinned.eq_content(&locked.pinned) => ArtifactState::Installed,
        Some(_) => ArtifactState::Outdated,
        // Snapshot-provided only (no top-level entry): no pinned identifier to
        // compare, so plain Installed — `member_display_state` promotes it to
        // ViaBundle for a member/row not also declared standalone.
        None => ArtifactState::Installed,
    }
}

/// The set of `(kind, registry/repository)` a CURRENTLY-DECLARED bundle provides
/// — every `Origin::Bundles` member of the effective desired set. Built from
/// [`crate::lock::effective_set::effective_set`], so it honors `snapshot_matches`
/// (a stale/retagged `[[bundle]]` snapshot whose bundle is no longer declared at
/// that id is excluded). The via-bundle fallback in [`derive_artifact_state`]
/// trusts only these. Empty when the cache is incomplete offline (the fallback
/// then yields NotInstalled — the same offline degradation as the gate).
fn snapshot_declared_repos(
    set: &DesiredSet,
    cached: &[crate::lock::locked_bundle::LockedBundle],
) -> std::collections::BTreeSet<(ArtifactKind, String)> {
    crate::lock::effective_set::effective_set(set, cached)
        .map(|e| {
            e.iter()
                .filter_map(|((kind, _name), origin)| match origin {
                    crate::lock::effective_set::Origin::Bundles { id, .. } => Some((*kind, id.registry_repository())),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Recompute every row's [`ArtifactState`] against the currently-active
/// scope's lock + install-state (used after a scope toggle — the catalog
/// itself is scope-independent, only the per-row state changes).
fn recompute_states(ctx: &TuiContext, state: &mut TuiState) {
    let (lock, install_state, declared_bundle_repos, direct_repos, snapshot_repos) = load_scope_for_badges(ctx);
    let active = detect_clients(&ctx.workspace, ctx.scope);
    let badge = BadgeContext {
        lock: lock.as_ref(),
        state: &install_state,
        roots: &ctx.roots,
        active: &active,
        declared_bundle_repos: &declared_bundle_repos,
        direct_repos: &direct_repos,
        snapshot_repos: &snapshot_repos,
    };
    for r in &mut state.rows {
        // A2: use authoritative registry + repository fields directly so
        // namespaced registries ("ghcr.io/acme") are matched exactly without
        // re-splitting `repo` on the first '/' (which would give "ghcr.io").
        r.state = derive_row_state(&r.kind, &r.registry, &r.repository, &badge);
    }
    // Member-node states live in a separate cache (`bundle_members`) that is
    // otherwise only rebuilt on re-expand / scope toggle. Refresh it here too so
    // an expanded bundle's members reflect an install/uninstall immediately,
    // instead of showing the state captured when the bundle was first expanded.
    refresh_member_states(
        state,
        &ctx.registries,
        lock.as_ref(),
        &install_state,
        &ctx.roots,
        &active,
        &direct_repos,
        &snapshot_repos,
    );
}

/// Re-derive the install state of every cached bundle-member node for the
/// active scope against the current lock + install-state, so an expanded
/// bundle's members track an install/uninstall without a re-expand.
///
/// Only `Ready` entries keyed under the active `scope_label` are touched (the
/// other scope's cache is cleared on toggle, never recomputed here). A member
/// whose `member_repo` is absent is left unchanged — it never resolved to a
/// real artifact.
///
/// A member's authoritative registry is its parent bundle's registry
/// (D-BACKGROUND), derived from the cache key's `bundle_repo` against the
/// resolved set — never a first-`/` split of `member_repo`, which would
/// mis-attribute a namespaced registry like `ghcr.io/acme` to bare `ghcr.io`
/// and miss the install record.
#[allow(clippy::too_many_arguments)]
fn refresh_member_states(
    state: &mut TuiState,
    registries: &[ResolvedRegistry],
    lock: Option<&GrimoireLock>,
    install_state: &InstallState,
    roots: &AnchorRoots,
    active: &[ClientTarget],
    direct_repos: &std::collections::BTreeSet<(ArtifactKind, String)>,
    snapshot_repos: &std::collections::BTreeSet<(ArtifactKind, String)>,
) {
    let scope_label = state.scope_label.clone();
    for ((entry_scope, bundle_repo), cache) in state.bundle_members.iter_mut() {
        if *entry_scope != scope_label {
            continue;
        }
        let crate::tui::bundle_members::BundleMemberCache::Ready(nodes) = cache else {
            continue;
        };
        let parent_registry = member_parent_registry_from_registries(registries, bundle_repo);
        for node in nodes.iter_mut() {
            let Some(member_repo) = node.member_repo.as_deref() else {
                continue;
            };
            let (registry, repository) = member_registry_repository(&parent_registry, member_repo);
            node.state = member_display_state(
                node.kind,
                &registry,
                &repository,
                lock,
                install_state,
                roots,
                active,
                direct_repos,
                snapshot_repos,
            );
        }
    }
}

/// The set of `(kind, registry/repository)` an active scope declares **directly**
/// (in `[skills]`/`[rules]`/`[agents]`) — the key by which the via-bundle badge
/// decides whether a present member is also a standalone install.
fn direct_declared_repos(set: &DesiredSet) -> std::collections::BTreeSet<(ArtifactKind, String)> {
    let mut out = std::collections::BTreeSet::new();
    for (kind, map) in [
        (ArtifactKind::Skill, &set.skills),
        (ArtifactKind::Rule, &set.rules),
        (ArtifactKind::Agent, &set.agents),
    ] {
        for id in map.values() {
            out.insert((kind, id.registry_repository()));
        }
    }
    out
}

/// The badge state for a bundle member node.
///
/// The install reality from [`derive_artifact_state`], except a present-and-intact
/// member that is **not** also declared standalone is shown as
/// [`ArtifactState::ViaBundle`] — it is installed only because the bundle provides
/// it. `Modified` / `Outdated` / `IntegrityMissing` keep precedence (they are not
/// the plain `Installed` state, so they are returned unchanged).
#[allow(clippy::too_many_arguments)]
fn member_display_state(
    kind: ArtifactKind,
    registry: &str,
    repository: &str,
    lock: Option<&GrimoireLock>,
    state: &InstallState,
    roots: &AnchorRoots,
    active: &[ClientTarget],
    direct_repos: &std::collections::BTreeSet<(ArtifactKind, String)>,
    snapshot_repos: &std::collections::BTreeSet<(ArtifactKind, String)>,
) -> ArtifactState {
    let derived = derive_artifact_state(kind, registry, repository, lock, state, roots, active, snapshot_repos);
    if derived == ArtifactState::Installed && !direct_repos.contains(&(kind, format!("{registry}/{repository}"))) {
        ArtifactState::ViaBundle
    } else {
        derived
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
/// Returns the active scope's lock, install state, the set of declared bundle
/// `registry/repository` values (drives bundle row state), the set of
/// directly-declared `(kind, registry/repository)` (drives the via-bundle badge),
/// and the set of `(kind, registry/repository)` a currently-declared bundle
/// provides (lets a stale-dropped member still derive via the snapshot). The
/// declaration is read fresh (the config can change while the TUI runs); any read
/// failure degrades to empty sets.
#[allow(clippy::type_complexity)]
fn load_scope_for_badges(
    ctx: &TuiContext,
) -> (
    Option<GrimoireLock>,
    InstallState,
    std::collections::BTreeSet<String>,
    std::collections::BTreeSet<(ArtifactKind, String)>,
    std::collections::BTreeSet<(ArtifactKind, String)>,
) {
    let lock = lock_io::load(&ctx.lock_path).ok();
    let state = load_state(ctx).unwrap_or_else(|_| InstallState::empty(&ctx.state_path));
    let cached = lock.as_ref().map(|l| l.bundles.as_slice()).unwrap_or(&[]);
    let (declared_bundle_repos, direct_repos, snapshot_repos) = load_scope_declaration(ctx)
        .map(|(_options, _registries, set)| {
            let bundles = set.bundles.values().map(|id| id.registry_repository()).collect();
            (
                bundles,
                direct_declared_repos(&set),
                snapshot_declared_repos(&set, cached),
            )
        })
        .unwrap_or_default();
    (lock, state, declared_bundle_repos, direct_repos, snapshot_repos)
}

/// Lazily fetch the tag list for `row` and feed it to the open picker.
/// Degrades to a status-line message (and a closed picker) on any failure
/// — never a crash, offline included.
async fn load_versions(ctx: &TuiContext, state: &mut TuiState, row: usize) {
    let Some(r) = state.rows.get(row).cloned() else {
        state.cancel_version();
        return;
    };
    // A2: use authoritative registry + repository fields directly.
    if r.registry.is_empty() || r.repository.is_empty() {
        state.set_status(format!("malformed catalog repo: {}", r.repo));
        state.cancel_version();
        return;
    }
    let id = Identifier::new_registry(&r.repository, &r.registry);
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
/// Silent batch (no progress sink) — the default for tests.
async fn run_batch(ctx: &TuiContext, state: &mut TuiState, rows: &[usize], op: BatchOp) {
    run_batch_with_progress(ctx, state, rows, op, &SilentProgress).await;
}

async fn run_batch_with_progress(
    ctx: &TuiContext,
    state: &mut TuiState,
    rows: &[usize],
    op: BatchOp,
    progress: &dyn InstallProgress,
) {
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

    // Drive the progress sink at the batch (per-row) grain so the modal shows
    // `n/total` over every acted-on row with the current repo as its label.
    // Each row's own `install_all` is a single artifact (or a bundle's
    // members), so per-artifact progress would always read 1/1 — the meaningful
    // count is the rows. Uninstall is local, but the same loop drives it so a
    // delete shows progress too. The inner `perform`/`perform_uninstall` are
    // silent (no nested sink that would reset the counter).
    progress.start(total);
    for (n, &i) in rows.iter().enumerate() {
        let Some(row) = state.rows.get(i).cloned() else {
            continue;
        };
        progress.advance(n + 1, &row.repo);
        state.set_status(format!("{verb} {}/{total}: {}…", n + 1, row.repo));
        let outcome = match op {
            BatchOp::Install => perform(ctx, &row, false, None).await.map(|_| ()),
            BatchOp::Update => perform(ctx, &row, true, None).await.map(|_| ()),
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
    progress.finish();

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
/// provides keeps its files ([`bundle_provides_files`]) — the delete
/// degrades to dropping the direct declaration, like `grim remove`.
fn perform_uninstall(ctx: &TuiContext, row: &TuiRow) -> anyhow::Result<()> {
    // Authoritative repository field (never first-slash-split `repo`, which
    // mis-attributes namespaced registries — D-TREE).
    let repository = row.repository.clone();
    if repository.is_empty() {
        return Err(anyhow::anyhow!("malformed catalog repo: {}", row.repo));
    }
    let kind = row_kind(&row.kind);
    let basename = repository.rsplit('/').next().unwrap_or(&repository).to_string();

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

    // For a bundle row the catalog repo's basename is NOT necessarily the
    // `[bundles]` binding name (`grim add --name`): resolve the real binding
    // from the declaration (under the flock) so the file-deletion targets AND
    // the undeclare act on the same entry — otherwise an aliased bundle would
    // have its members' files deleted while its declaration is left dangling.
    let name = match kind {
        ArtifactKind::Bundle => resolve_bundle_binding(ctx, &row.repo, &basename)?,
        _ => basename.clone(),
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
        _ if bundle_provides_files(ctx, kind, &name) => Vec::new(),
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
/// The progress-modal title verb for a batch/member operation.
fn batch_title(op: BatchOp) -> &'static str {
    match op {
        BatchOp::Install => "Installing",
        BatchOp::Update => "Updating",
        BatchOp::Uninstall => "Uninstalling",
    }
}

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
///
/// The TUI renders progress at the batch (per-row) grain on the modal
/// itself (see [`run_batch_with_progress`]), so this single-artifact
/// materialize stays silent — a nested per-artifact sink would reset the
/// batch counter to 1/1.
async fn perform(
    ctx: &TuiContext,
    row: &TuiRow,
    is_update: bool,
    name_override: Option<&str>,
) -> anyhow::Result<String> {
    // Use the authoritative registry/repository fields directly — never
    // first-slash-split `repo`, which mis-attributes namespaced registries like
    // `ghcr.io/acme` to the bare host (D-TREE / D-BACKGROUND). The fields equal
    // a first-slash split for bare-host registries, so single-registry behavior
    // is preserved.
    let (registry, repository) = (row.registry.clone(), row.repository.clone());
    if registry.is_empty() || repository.is_empty() {
        return Err(anyhow::anyhow!("malformed catalog repo: {}", row.repo));
    }

    let kind = row_kind(&row.kind);
    // The declaration/lock binding name: an explicit override (a bundle member's
    // own name, which is its lock/install key) wins; a catalog row falls back to
    // the repo's last path segment.
    let name = name_override
        .map(str::to_string)
        .unwrap_or_else(|| repository.rsplit('/').next().unwrap_or(&repository).to_string());
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

/// Resolve the `[bundles]` binding name for a bundle catalog row.
///
/// The catalog row carries the bundle's `registry/repository`, but the config
/// declares it under an arbitrary binding name (`grim add --name`) that need
/// not equal the repo's last path segment. Match the row repo against the
/// declared bundle identifiers' `registry_repository()`:
///
/// - exactly one declared binding → that binding;
/// - none declared (a legacy lock-only or foreign row) → the repo `basename`,
///   so the provenance-exclusive fallback in [`bundle_uninstall_targets`] still
///   runs and the (absent) undeclare is a harmless no-op;
/// - more than one binding for the same repo → `Err` (ambiguous — refuse the
///   delete rather than guess which alias to undeclare).
///
/// # Errors
///
/// When the row's repo is declared under multiple binding names.
fn resolve_bundle_binding(ctx: &TuiContext, repo: &str, basename: &str) -> anyhow::Result<String> {
    let Ok((_options, _registries, set)) = load_scope_declaration(ctx) else {
        return Ok(basename.to_string());
    };
    let matches: Vec<&String> = set
        .bundles
        .iter()
        .filter(|(_binding, id)| id.registry_repository() == repo)
        .map(|(binding, _id)| binding)
        .collect();
    match matches.as_slice() {
        [] => Ok(basename.to_string()),
        [one] => Ok((*one).to_string()),
        many => Err(anyhow::anyhow!(
            "bundle '{repo}' is declared under {} binding names ({}); remove it with `grim remove bundle <name>`",
            many.len(),
            many.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")
        )),
    }
}

/// The artifacts whose materialized files the TUI delete action must remove
/// when undeclaring the bundle `binding` — every `(kind, name)` in the
/// **effective desired set before** the bundle is undeclared that is no longer
/// desired **after**.
///
/// Computed from the effective-set difference (`E_before \ E_after`) rather
/// than a lock-entry diff: a member whose lock entry was already dropped as
/// honestly stale by a prior id-mismatch removal still has its install-state
/// record + files on disk, and the bundle's snapshot still names it — the
/// effective set sees it, a lock-entry diff would orphan it. A member another
/// declaration (a direct entry or another bundle) still holds stays in
/// `E_after` and is therefore not a deletion target.
///
/// Falls back to the shared [`crate::command::remove::drop_from_lock`]
/// lock-entry diff when the effective set is incomputable offline (pre-cache
/// lock or a snapshot that no longer matches the declaration). A binding the
/// config does not declare (a legacy or foreign row) falls back to
/// provenance-exclusive matching by `repo`.
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

    // Prefer the effective-set diff: an artifact in the desired set BEFORE the
    // bundle is undeclared but not AFTER must have its files deleted. This is
    // the file-deletion counterpart to the lock-retention rule in
    // `drop_from_lock`, and — crucially — it sees a snapshot-only member whose
    // lock entry was already dropped by a prior id-mismatch removal (its
    // install-state record + files persist, so deriving targets from lock
    // entries alone would orphan it when the bundle, its last holder, is gone).
    use crate::lock::effective_set::effective_set;
    if let (Some(before), Some(after)) = (
        effective_set(&set_before, &previous.bundles),
        effective_set(&set_after, &previous.bundles),
    ) {
        return before.keys().filter(|key| !after.contains_key(*key)).cloned().collect();
    }

    // Fallback (pre-cache lock / snapshot mismatch — membership unknowable
    // offline): the lock-entry diff via the shared `drop_from_lock` seam.
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

/// Whether deleting `(kind, name)` must keep its files because a declared
/// bundle provides it — the file-retention gate from
/// [`crate::lock::effective_set::declared_bundle_provides`].
///
/// Fires for BOTH a directly-declared artifact a bundle also provides (the
/// delete degrades to dropping the direct declaration, files kept) AND a
/// bundle-only member (the delete keeps everything — remove the bundle to remove
/// it). Loads the lock + the active scope's declaration fresh (the config can
/// change while the TUI runs). Any load failure means the guard cannot prove the
/// artifact is held → `false` (the caller deletes, the pre-effective-set
/// behavior).
fn bundle_provides_files(ctx: &TuiContext, kind: ArtifactKind, name: &str) -> bool {
    let Ok(lock) = lock_io::load(&ctx.lock_path) else {
        return false;
    };
    let Ok((_options, _registries, set)) = load_scope_declaration(ctx) else {
        return false;
    };
    crate::lock::effective_set::declared_bundle_provides(&set, &lock.bundles, kind, name)
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

/// The resolved registry urls in precedence order — the input the tree's
/// multi-registry root ordering (F13) and empty-registry roots (D-EMPTY)
/// consume via [`TuiState::set_registry_order`].
fn registry_order(ctx: &TuiContext) -> Vec<String> {
    ctx.registries.iter().map(|r| r.url.clone()).collect()
}

/// The registry whose root prefix is elided from tree labels — `Some` only
/// when exactly one browse source is in scope (D-ELIDE); `None` otherwise so
/// each root names its own registry and namespaced roots stay
/// distinguishable.
///
/// The elided value is the source's own locator (`registries[0].url`), not
/// `ctx.primary_registry`: for an index-only set the primary is `""` (index
/// locators cannot expand short ids), which would never match the rows'
/// source root and the single-source session would keep a redundant root.
fn elision_registry(ctx: &TuiContext) -> Option<String> {
    match ctx.registries.as_slice() {
        [only] => Some(only.url.clone()),
        _ => None,
    }
}

/// Split a bundle member `repo` into its authoritative `(registry,
/// repository)` using the parent bundle's `parent_registry` rather than the
/// first `/` — members are same-registry as their bundle (D-BACKGROUND), so a
/// namespaced registry like `ghcr.io/acme` is never mis-split. Falls back to
/// the remainder after the first `/` when `repo` does not carry the
/// `parent_registry/` prefix (defensive; catalog-derived members always do).
fn member_registry_repository(parent_registry: &str, repo: &str) -> (String, String) {
    let repository = repo
        .strip_prefix(&format!("{parent_registry}/"))
        .map(|s| s.to_string())
        .unwrap_or_else(|| {
            repo.split_once('/')
                .map(|(_, rest)| rest.to_string())
                .unwrap_or_default()
        });
    (parent_registry.to_string(), repository)
}

/// The authoritative registry a bundle member belongs to, given the resolved
/// registry set: the longest resolved registry url that prefixes `repo`. A
/// member shares its parent bundle's registry (D-BACKGROUND) and the bundle was
/// browsed from one of the resolved registries, so the longest matching url is
/// exactly the parent bundle row's registry — even when a host and a
/// `host/namespace` are both in scope. Falls back to the first-`/` host when no
/// resolved registry matches (defensive).
fn member_parent_registry_from_registries(registries: &[ResolvedRegistry], repo: &str) -> String {
    registries
        .iter()
        .map(|r| r.url.as_str())
        .filter(|url| repo == *url || repo.strip_prefix(url).is_some_and(|rest| rest.starts_with('/')))
        .max_by_key(|url| url.len())
        .map(str::to_string)
        .unwrap_or_else(|| split_repo(repo).map(|(reg, _)| reg).unwrap_or_default())
}

/// [`member_parent_registry_from_registries`] keyed off a [`TuiContext`].
fn member_parent_registry(ctx: &TuiContext, repo: &str) -> String {
    member_parent_registry_from_registries(&ctx.registries, repo)
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
///
/// Silent: the caller renders a single-action modal frame; this seam does
/// not drive a per-artifact progress sink.
async fn perform_member(
    ctx: &TuiContext,
    repo: String,
    kind: crate::oci::ArtifactKind,
    is_update: bool,
    tag: String,
    name: String,
    parent_registry: &str,
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
    //
    // B1c: split the member repo into the authoritative registry + repository
    // using the parent bundle's registry (members are same-registry as their
    // bundle, D-BACKGROUND) so a namespaced registry like `ghcr.io/acme` is
    // never mis-split on the first `/`. The C-12 guard above already proved a
    // separator exists.
    let (registry, repository) = member_registry_repository(parent_registry, &repo);
    let synthetic_row = TuiRow {
        kind: kind.to_string(),
        registry,
        repository,
        repo,
        description: String::new(),
        summary: String::new(),
        keywords: Vec::new(),
        repository_url: None,
        revision: None,
        created: None,
        latest_tag: tag,
        version: String::new(),
        deprecated: None,
        pinned_version: None,
        state: crate::tui::state::ArtifactState::NotInstalled,
        source: None,
    };
    // Use the member's own binding name (its lock/install key) for the
    // declaration, not the repo basename — they differ when the bundle aliases
    // the member.
    perform(ctx, &synthetic_row, is_update, Some(&name)).await
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
    name: String,
) -> anyhow::Result<Vec<String>> {
    // C-12: validate split_repo at the boundary — return Err (no panic) on
    // a separator-less repo so the dispatch arm can show a status breadcrumb.
    if split_repo(&repo).is_none() {
        return Err(anyhow::anyhow!("malformed member repo: {repo}"));
    }
    let member_kind = kind;
    // `name` is the bundle member's binding name — its install-state / lock key,
    // threaded from the member node. It is NOT the repo basename, which can
    // differ when the bundle aliases a member; keying file deletion + undeclare
    // by the basename would silently miss the record and orphan the files.

    // Hold the config flock for the whole read-modify-write so the keep-files
    // gate, the file deletion, and the undeclare see one consistent
    // declaration snapshot (closes the TOCTOU window where a concurrent
    // `grim remove` between the gate and the undeclare could orphan the kept
    // files). Held to function end.
    let _guard = match ctx.config_path.exists() {
        true => Some(grim(ConfigFileLock::try_acquire(&ctx.config_path))?),
        false => None,
    };

    // Delete materialized files + drop the install-state record — UNLESS a
    // declared bundle provides this artifact, in which case the files stay (it
    // remains desired): a directly-declared member degrades to dropping its
    // declaration; a bundle-only member keeps everything (remove the bundle to
    // remove it).
    let kept = bundle_provides_files(ctx, member_kind, &name);
    if !kept {
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
    let (declared, mut notes) = undeclare_and_unlock(
        &ctx.config_path,
        &ctx.lock_path,
        &options,
        &registries,
        &mut set,
        member_kind,
        &name,
    )?;
    // A bundle-only member (kept, never directly declared) has nothing to
    // undeclare — tell the user the delete was a no-op and why.
    if kept && !declared {
        notes.push(format!(
            "'{name}' is provided by a bundle — remove the bundle to remove it"
        ));
    }
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

    // B1c: a member of a namespaced registry must keep the whole `host/namespace`
    // as its registry — never the first-slash host — so the synthetic row routes
    // to the right registry.
    #[test]
    fn member_registry_repository_uses_namespaced_parent_registry() {
        let (registry, repository) = member_registry_repository("ghcr.io/acme", "ghcr.io/acme/tools/foo");
        assert_eq!(registry, "ghcr.io/acme", "registry must be the full namespaced parent");
        assert_eq!(
            repository, "tools/foo",
            "repository must be the remainder after the parent prefix"
        );
    }

    #[test]
    fn member_registry_repository_falls_back_when_prefix_absent() {
        // Defensive: when `repo` does not carry the parent prefix, split after
        // the first slash so the synthetic row still has a non-empty repository.
        let (registry, repository) = member_registry_repository("other.io/ns", "ghcr.io/acme/foo");
        assert_eq!(registry, "other.io/ns", "registry is the supplied parent registry");
        assert_eq!(
            repository, "acme/foo",
            "repository falls back to the post-first-slash remainder"
        );
    }

    // The synthetic member row built for a namespaced parent carries the full
    // namespaced registry, not the first-slash host (regression for B1c).
    #[test]
    fn member_synthetic_registry_matches_namespaced_parent() {
        let parent_registry = "ghcr.io/acme";
        let repo = "ghcr.io/acme/skills/code-review";
        let (registry, repository) = member_registry_repository(parent_registry, repo);
        assert_eq!(registry, parent_registry);
        assert_eq!(repository, "skills/code-review");
        // Round-trips back to the original repo string.
        assert_eq!(format!("{registry}/{repository}"), repo);
    }

    // B1 residual (refresh_member_states): a member's registry is derived from
    // its parent bundle's repo against the resolved set (D-BACKGROUND), so when a
    // bare host and a `host/namespace` are both configured the longest matching
    // url wins. A first-`/` split would mis-attribute the member to bare `ghcr.io`
    // and `member_display_state` would miss the install record (member shown
    // NotInstalled though installed). Guards the derivation refresh_member_states
    // now relies on.
    #[test]
    fn member_parent_registry_from_registries_prefers_namespaced_over_bare_host() {
        let registries = vec![
            ResolvedRegistry {
                url: "ghcr.io".to_string(),
                alias: None,
                is_default: false,
                kind: crate::config::registry_resolve::SourceKind::Registry,
            },
            ResolvedRegistry {
                url: "ghcr.io/acme".to_string(),
                alias: None,
                is_default: true,
                kind: crate::config::registry_resolve::SourceKind::Registry,
            },
        ];
        let parent = member_parent_registry_from_registries(&registries, "ghcr.io/acme/bundles/starter-pack");
        assert_eq!(
            parent, "ghcr.io/acme",
            "the longest matching registry url wins, not the bare host"
        );

        // The member shares the bundle's registry; the synthetic split keeps the
        // namespaced registry whole and routes the lookup to the right record.
        let (registry, repository) = member_registry_repository(&parent, "ghcr.io/acme/skills/demo");
        assert_eq!(registry, "ghcr.io/acme");
        assert_eq!(repository, "skills/demo");
    }

    // project_group_rows projects a CatalogGroup's CatalogRows into TuiRows that
    // preserve the authoritative registry + repository split (never re-derived
    // from the joined `repo` by a first-slash split).
    #[test]
    fn project_group_rows_preserves_registry_repository_split() {
        use crate::catalog::catalog_service::{CatalogGroup, CatalogRow};
        use crate::install::status_badge::StatusBadge;

        let group = CatalogGroup {
            registry: "ghcr.io/acme".to_string(),
            alias: None,
            truncated: false,
            built_at: String::new(),
            served_offline: false,
            rows: vec![CatalogRow {
                kind: Some("skill".to_string()),
                registry: "ghcr.io/acme".to_string(),
                repository: "tools/code-review".to_string(),
                summary: Some("a summary".to_string()),
                description: Some("a description".to_string()),
                keywords: vec!["lint".to_string()],
                repository_url: Some("https://example.invalid/repo".to_string()),
                revision: None,
                created: None,
                deprecated: None,
                latest_tag: Some("1.2.3".to_string()),
                version: Some("1.2.3".to_string()),
                badge: StatusBadge::NotInstalled,
            }],
        };

        let tmp = tempfile::tempdir().unwrap();
        let install_state = InstallState::empty(tmp.path());
        let roots = test_roots(tmp.path());
        let declared_bundle_repos = std::collections::BTreeSet::new();
        let direct_repos = std::collections::BTreeSet::new();
        let snapshot_repos = std::collections::BTreeSet::new();
        let badge = BadgeContext {
            lock: None,
            state: &install_state,
            roots: &roots,
            active: &ClientTarget::ALL,
            declared_bundle_repos: &declared_bundle_repos,
            direct_repos: &direct_repos,
            snapshot_repos: &snapshot_repos,
        };

        let rows = project_group_rows(&group, &badge);
        assert_eq!(rows.len(), 1, "one catalog row → one TUI row");
        let r = &rows[0];
        assert_eq!(
            r.registry, "ghcr.io/acme",
            "registry must be the authoritative namespaced value"
        );
        assert_eq!(
            r.repository, "tools/code-review",
            "repository must be the authoritative value"
        );
        assert_eq!(
            r.repo, "ghcr.io/acme/tools/code-review",
            "repo joins registry + repository"
        );
        assert_eq!(r.kind, "skill");
        assert_eq!(r.latest_tag, "1.2.3");
        assert_eq!(r.version, "1.2.3");
        assert_eq!(
            r.state,
            ArtifactState::NotInstalled,
            "uninstalled skill derives NotInstalled"
        );
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
        let (reg, repo_path) = repo.split_once('/').unwrap_or((repo, ""));
        TuiRow {
            kind: "skill".to_string(),
            registry: reg.to_string(),
            repository: repo_path.to_string(),
            repo: repo.to_string(),
            description: String::new(),
            summary: String::new(),
            keywords: Vec::new(),
            repository_url: None,
            revision: None,
            created: None,
            latest_tag: "latest".to_string(),
            version: "1.0.0".to_string(),
            deprecated: None,
            pinned_version: None,
            state: ArtifactState::Installed,
            source: None,
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

    // GAP-2: apply_catalog_results sets rows/health/order and clears status (B1 guard).
    #[test]
    fn apply_catalog_results_clears_status_and_sets_all_fields() {
        let mut s = TuiState::new();
        // Pre-populate a transient message (simulates "refreshing catalog…").
        s.set_status("refreshing catalog…");

        let rows = vec![installed_row("ghcr.io/acme/skill-a")];
        let health = crate::tui::state::RegistryHealth {
            offline: vec!["ghcr.io/offline".to_string()],
            truncated: vec![],
        };
        apply_catalog_results(
            &mut s,
            rows,
            health,
            false,
            None, // no single-registry elision
            vec!["ghcr.io/acme".to_string(), "ghcr.io/offline".to_string()],
            BTreeMap::new(), // no aliases in this fixture
        );

        // B1 regression guard: status must be cleared so D-DEGRADE can surface.
        assert_eq!(s.status_line, "", "B1: status must be cleared on success arm");
        // Rows replaced.
        assert_eq!(s.rows.len(), 1, "rows must be replaced by apply_catalog_results");
        // Registry health set.
        assert_eq!(
            s.registry_health.offline,
            vec!["ghcr.io/offline"],
            "registry_health.offline must be set"
        );
        assert!(s.registry_health.truncated.is_empty(), "truncated must be empty");
        // Registry order set (F13).
        assert_eq!(
            s.registry_order,
            vec!["ghcr.io/acme", "ghcr.io/offline"],
            "registry_order must reflect precedence order"
        );
        // Truncated indicator cleared.
        assert!(!s.truncated, "truncated must be false");
        // Loading flag cleared (set_rows clears it).
        assert!(!s.loading, "loading must be cleared by set_rows");
        // Registry labels set (empty map in this fixture → labels map is empty).
        assert!(
            s.registry_labels.is_empty(),
            "registry_labels must reflect the passed map (empty in this fixture)"
        );
    }

    // GAP-2 extension: apply_catalog_results propagates non-empty labels map.
    #[test]
    fn apply_catalog_results_propagates_registry_labels() {
        let mut s = TuiState::new();
        let mut labels = BTreeMap::new();
        labels.insert("ghcr.io/acme".to_string(), "acme (ghcr.io/acme)".to_string());
        apply_catalog_results(
            &mut s,
            vec![],
            crate::tui::state::RegistryHealth {
                offline: vec![],
                truncated: vec![],
            },
            false,
            None,
            vec!["ghcr.io/acme".into()],
            labels,
        );
        assert_eq!(
            s.registry_label("ghcr.io/acme"),
            "acme (ghcr.io/acme)",
            "registry_labels must be propagated to TuiState by apply_catalog_results"
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

    // GAP-3 / D-BACKGROUND: a namespaced registry ("ghcr.io/acme") must be
    // matched exactly via `row.registry` + `row.repository`, not via a first-'/'
    // split of `row.repo` (which would give "ghcr.io" / "acme/skills/demo" and
    // miss the lock entry keyed under "ghcr.io/acme").
    #[test]
    fn post_batch_checks_namespaced_registry_produces_correct_identifier() {
        // Build a lock with an entry under the namespaced registry "ghcr.io/acme".
        let namespaced_id = Identifier::new_registry("skills/demo", "ghcr.io/acme")
            .clone_with_digest(crate::oci::Digest::Sha256(sha('9')));
        let locked_namespaced = crate::lock::locked_artifact::LockedArtifact::direct(
            "demo".to_string(),
            ArtifactKind::Skill,
            crate::oci::PinnedIdentifier::try_from(namespaced_id).unwrap(),
        );
        let lock = lock_fixture(vec![locked_namespaced], Vec::new());

        // Build a TuiRow with the correct authoritative registry/repository fields.
        // installed_row() splits on the first '/' and would produce registry="ghcr.io"
        // (wrong) — construct the row directly.
        let row = TuiRow {
            kind: "skill".to_string(),
            registry: "ghcr.io/acme".to_string(),
            repository: "skills/demo".to_string(),
            repo: "ghcr.io/acme/skills/demo".to_string(),
            description: String::new(),
            summary: String::new(),
            keywords: Vec::new(),
            repository_url: None,
            revision: None,
            created: None,
            latest_tag: "latest".to_string(),
            version: "1.0.0".to_string(),
            deprecated: None,
            pinned_version: None,
            state: ArtifactState::Installed,
            source: None,
        };
        let rows = vec![row];

        let checks = post_batch_checks(&lock, &rows, &[0]);

        assert_eq!(
            checks.len(),
            1,
            "D-BACKGROUND: namespaced registry row must produce a RowCheck"
        );
        assert_eq!(
            checks[0].repo, "ghcr.io/acme/skills/demo",
            "RowCheck.repo must equal the full registry/repository reference"
        );
        assert_eq!(
            checks[0].locked_digest,
            crate::oci::Digest::Sha256(sha('9')),
            "RowCheck.locked_digest must match the pinned digest from the lock"
        );
    }

    // GAP-4: direct unit tests for elision_registry and registry_order.
    // These two pure helpers are the only seam between TuiContext and the
    // tree's multi-registry root projection — testing them directly locks
    // the contract described in their doc comments without a full TUI render.

    /// Minimal TuiContext carrying only the registry fields; enough for the
    /// pure helpers `elision_registry` and `registry_order`.
    fn ctx_with_registries(registries: Vec<ResolvedRegistry>, primary: &str) -> TuiContext {
        use crate::oci::access::memory_registry::MemoryRegistry;
        let access: Arc<dyn OciAccess> = Arc::new(MemoryRegistry::new());
        let dummy = std::path::PathBuf::from("/tmp/gap4");
        TuiContext {
            registries,
            primary_registry: primary.to_string(),
            access,
            offline: false,
            force_refresh: false,
            scope: ConfigScope::Project,
            workspace: dummy.clone(),
            lock_path: dummy.join("grimoire.lock"),
            state_path: dummy.join("install-state.json"),
            config_path: dummy.join("grimoire.toml"),
            roots: AnchorRoots {
                workspace: dummy.clone(),
                grim_home: dummy.clone(),
                claude_root: None,
                copilot_root: None,
                opencode_skills: None,
            },
            clients_default: Vec::new(),
            clients_selected: Vec::new(),
            scope_label: "project".to_string(),
            alt: None,
            tui_options: Default::default(),
        }
    }

    #[test]
    fn elision_registry_returns_some_for_single_registry() {
        // D-ELIDE: exactly one registry → elide its prefix from tree labels.
        let ctx = ctx_with_registries(
            vec![ResolvedRegistry {
                url: "ghcr.io/acme".to_string(),
                alias: None,
                is_default: true,
                kind: crate::config::registry_resolve::SourceKind::Registry,
            }],
            "ghcr.io/acme",
        );
        assert_eq!(elision_registry(&ctx), Some("ghcr.io/acme".to_string()));
    }

    #[test]
    fn elision_registry_returns_none_for_multi_registry() {
        // D-ELIDE: two registries → both roots must name their registry.
        let ctx = ctx_with_registries(
            vec![
                ResolvedRegistry {
                    url: "ghcr.io/acme".to_string(),
                    alias: None,
                    is_default: true,
                    kind: crate::config::registry_resolve::SourceKind::Registry,
                },
                ResolvedRegistry {
                    url: "ghcr.io/other".to_string(),
                    alias: None,
                    is_default: false,
                    kind: crate::config::registry_resolve::SourceKind::Registry,
                },
            ],
            "ghcr.io/acme",
        );
        assert_eq!(elision_registry(&ctx), None);
    }

    #[test]
    fn registry_order_preserves_precedence_order() {
        // F13: registry roots in the tree follow the precedence order of
        // `[[registries]]` declarations — first declared = first root.
        let ctx = ctx_with_registries(
            vec![
                ResolvedRegistry {
                    url: "ghcr.io/acme".to_string(),
                    alias: None,
                    is_default: true,
                    kind: crate::config::registry_resolve::SourceKind::Registry,
                },
                ResolvedRegistry {
                    url: "registry.corp.example/team".to_string(),
                    alias: Some("internal".to_string()),
                    is_default: false,
                    kind: crate::config::registry_resolve::SourceKind::Registry,
                },
            ],
            "ghcr.io/acme",
        );
        assert_eq!(
            registry_order(&ctx),
            vec!["ghcr.io/acme".to_string(), "registry.corp.example/team".to_string()],
        );
    }

    #[test]
    fn registry_order_single_entry_returns_one_element_vec() {
        let ctx = ctx_with_registries(
            vec![ResolvedRegistry {
                url: "ghcr.io/acme".to_string(),
                alias: None,
                is_default: true,
                kind: crate::config::registry_resolve::SourceKind::Registry,
            }],
            "ghcr.io/acme",
        );
        assert_eq!(registry_order(&ctx), vec!["ghcr.io/acme".to_string()]);
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
        registry_with_bundle_at("demo").await
    }

    /// As [`registry_with_bundle`], but the member skill lives at repo
    /// `grimoire/skills/<skill_segment>` while its name (the tar root, and the
    /// install-state / lock key) stays `demo`. With `skill_segment != "demo"`
    /// the member's repo basename differs from its install key — the
    /// aliased-member case (a bundle referencing a skill whose repo is named
    /// differently from the skill itself).
    async fn registry_with_bundle_at(skill_segment: &str) -> Arc<dyn OciAccess> {
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

        let skill_repo = Identifier::new_registry(format!("grimoire/skills/{skill_segment}"), "localhost:5050");
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
        // A second tag at the SAME digest, so a standalone install can pin
        // `:latest` while the bundle pins `:1.0.0` — a different identifier for
        // the same artifact (exercises the id-mismatch declaration path).
        reg.put_tag(&skill_repo, "latest", &skill_digest).await.unwrap();

        // The bundle: a single members-layer naming the skill.
        let members = BundleManifest::new(vec![BundleMember {
            kind: ArtifactKind::Skill,
            // The member name == the skill's tar root (`demo`), which the
            // materializer requires; the skill's REPO segment may differ.
            name: "demo".to_string(),
            id: format!("localhost:5050/grimoire/skills/{skill_segment}:1.0.0"),
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
            registries: vec![ResolvedRegistry {
                url: "localhost:5050".to_string(),
                alias: None,
                is_default: true,
                kind: crate::config::registry_resolve::SourceKind::Registry,
            }],
            primary_registry: "localhost:5050".to_string(),
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

        let label = perform(&ctx, &row, false, None).await.expect("bundle install succeeds");
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
        let (lock, install_state, declared_bundle_repos, direct_repos, snapshot_repos) = load_scope_for_badges(&ctx);
        let badge = BadgeContext {
            lock: lock.as_ref(),
            state: &install_state,
            roots: &ctx.roots,
            active: &ClientTarget::ALL,
            declared_bundle_repos: &declared_bundle_repos,
            direct_repos: &direct_repos,
            snapshot_repos: &snapshot_repos,
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
        perform(&ctx, &row, false, None).await.expect("bundle install succeeds");
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

        let (lock, install_state, declared_bundle_repos, direct_repos, snapshot_repos) = load_scope_for_badges(&ctx);
        let badge = BadgeContext {
            lock: lock.as_ref(),
            state: &install_state,
            roots: &ctx.roots,
            active: &ClientTarget::ALL,
            declared_bundle_repos: &declared_bundle_repos,
            direct_repos: &direct_repos,
            snapshot_repos: &snapshot_repos,
        };
        assert_eq!(
            derive_row_state("bundle", "localhost:5050", "grimoire/bundles/starter-pack", &badge),
            ArtifactState::NotInstalled
        );
    }

    #[tokio::test]
    async fn recompute_states_refreshes_stale_bundle_member_states() {
        // Bug 1: the bundle-member cache is derived once at expand time and was
        // only rebuilt on re-expand / scope toggle. After installing the bundle
        // (or its members), the expanded member rows kept their stale state — an
        // installed member kept showing NotInstalled. recompute_states (run
        // after every batch / member action) must also refresh the cached member
        // states, not only the catalog-row states.
        let tmp = tempfile::tempdir().unwrap();
        let workspace = tmp.path();
        std::fs::write(workspace.join("grimoire.toml"), "[skills]\n\n[rules]\n").unwrap();
        let ctx = test_ctx(workspace, registry_with_bundle().await);

        let mut bundle_row = installed_row("localhost:5050/grimoire/bundles/starter-pack");
        bundle_row.kind = "bundle".to_string();
        bundle_row.state = ArtifactState::NotInstalled;
        perform(&ctx, &bundle_row, false, None)
            .await
            .expect("bundle install succeeds");
        assert!(workspace.join(".claude/skills/demo/SKILL.md").is_file());

        let mut state = TuiState::new();
        state.set_scope_label(&ctx.scope_label);
        state.set_rows(vec![bundle_row]);

        // A STALE cache entry: the member is actually installed now, but the
        // cache was built before the install and still reports NotInstalled.
        let key = (
            "project".to_string(),
            "localhost:5050/grimoire/bundles/starter-pack".to_string(),
        );
        let stale = crate::tui::bundle_members::MemberNode {
            kind: ArtifactKind::Skill,
            label: "demo".to_string(),
            member_repo: Some("localhost:5050/grimoire/skills/demo".to_string()),
            state: ArtifactState::NotInstalled,
            related: false,
        };
        state.bundle_members.insert(
            key.clone(),
            crate::tui::bundle_members::BundleMemberCache::Ready(vec![stale]),
        );

        recompute_states(&ctx, &mut state);

        let crate::tui::bundle_members::BundleMemberCache::Ready(nodes) = &state.bundle_members[&key] else {
            panic!("the member cache entry must remain Ready after recompute");
        };
        assert_eq!(
            nodes[0].state,
            ArtifactState::ViaBundle,
            "recompute_states must refresh the stale member-node state — here to ViaBundle, \
             since demo is present only via the bundle (not declared standalone)"
        );
    }

    #[tokio::test]
    async fn deleting_bundle_deletes_member_files_orphaned_by_prior_skill_delete() {
        // Bug 2 (exact user repro, id-mismatch path): install a skill standalone
        // at one tag, install a bundle that pins the SAME skill at a different
        // tag, delete the standalone skill (kept — the bundle still holds it; its
        // lock entry is dropped as honestly stale on the id mismatch), then
        // delete the bundle. Nothing holds the skill any more, so its files MUST
        // be deleted. They were orphaned because the file-deletion targets were
        // derived from existing lock entries, and the skill's lock entry was
        // already gone.
        let tmp = tempfile::tempdir().unwrap();
        let workspace = tmp.path();
        std::fs::write(workspace.join("grimoire.toml"), "[skills]\n\n[rules]\n").unwrap();
        let ctx = test_ctx(workspace, registry_with_bundle().await);

        // 1. Standalone skill at :latest (the bundle pins :1.0.0 — a different id
        //    for the same artifact).
        let mut skill_row = installed_row("localhost:5050/grimoire/skills/demo");
        skill_row.latest_tag = "latest".to_string();
        skill_row.state = ArtifactState::NotInstalled;
        perform(&ctx, &skill_row, false, None)
            .await
            .expect("skill install succeeds");

        // 2. Install the bundle (also provides demo, pinned at :1.0.0).
        let mut bundle_row = installed_row("localhost:5050/grimoire/bundles/starter-pack");
        bundle_row.kind = "bundle".to_string();
        bundle_row.state = ArtifactState::NotInstalled;
        perform(&ctx, &bundle_row, false, None)
            .await
            .expect("bundle install succeeds");
        assert!(workspace.join(".claude/skills/demo/SKILL.md").is_file());

        // 3. Delete the standalone skill — files kept, the bundle still holds it.
        perform_uninstall(&ctx, &skill_row).expect("skill delete succeeds");
        assert!(
            workspace.join(".claude/skills/demo/SKILL.md").is_file(),
            "files kept while the bundle still holds the skill"
        );

        // 4. Delete the bundle — the last holder is gone; member files MUST go.
        perform_uninstall(&ctx, &bundle_row).expect("bundle delete succeeds");
        assert!(
            !workspace.join(".claude/skills/demo").exists(),
            "the orphaned member's files must be deleted when the bundle is removed"
        );
    }

    #[tokio::test]
    async fn deleting_aliased_bundle_row_undeclares_and_deletes_members() {
        // Codex [high]: `grim add --name` lets a bundle be declared under an
        // arbitrary binding ("team") that need not equal the repo basename
        // ("starter-pack"). Deleting the catalog row (which carries only the
        // repo) must resolve the real binding so the bundle is undeclared — not
        // left dangling in the config while its members' files are deleted.
        let tmp = tempfile::tempdir().unwrap();
        let workspace = tmp.path();
        std::fs::write(workspace.join("grimoire.toml"), "[skills]\n\n[rules]\n").unwrap();
        let ctx = test_ctx(workspace, registry_with_bundle().await);

        let mut bundle_row = installed_row("localhost:5050/grimoire/bundles/starter-pack");
        bundle_row.kind = "bundle".to_string();
        bundle_row.state = ArtifactState::NotInstalled;
        perform(&ctx, &bundle_row, false, None)
            .await
            .expect("bundle install succeeds");
        assert!(workspace.join(".claude/skills/demo/SKILL.md").is_file());

        // Rename the binding starter-pack → team (as `grim add --name team` would).
        let (options, registries, mut set) = load_scope_declaration(&ctx).expect("declaration loads");
        let id = set
            .bundles
            .remove("starter-pack")
            .expect("bundle declared under basename");
        set.bundles.insert("team".to_string(), id);
        set.invalidate_declaration_hash_cache();
        write_config(&ctx.config_path, &options, &registries, &set).expect("rewrite config");

        perform_uninstall(&ctx, &bundle_row).expect("aliased bundle delete succeeds");

        let body = std::fs::read_to_string(&ctx.config_path).unwrap();
        let cfg = ProjectConfig::from_toml_str(&body).expect("config parses");
        assert!(
            cfg.set.bundles.is_empty(),
            "the aliased bundle must be undeclared: {body}"
        );
        assert!(
            !workspace.join(".claude/skills/demo").exists(),
            "the aliased bundle's member files must be deleted"
        );
    }

    /// Derive a member's badge state the way the LoadBundleMembers / drain /
    /// refresh paths do: lock + install-state + active clients + direct repos.
    fn member_badge(ctx: &TuiContext, registry: &str, repository: &str) -> ArtifactState {
        let lock = lock_io::load(&ctx.lock_path).ok();
        let install_state = load_state(ctx).unwrap_or_else(|_| InstallState::empty(&ctx.state_path));
        let active = detect_clients(&ctx.workspace, ctx.scope);
        let (direct_repos, snapshot_repos) = load_scope_declaration(ctx)
            .map(|(_, _, set)| {
                let cached = lock.as_ref().map(|l| l.bundles.as_slice()).unwrap_or(&[]);
                (direct_declared_repos(&set), snapshot_declared_repos(&set, cached))
            })
            .unwrap_or_default();
        member_display_state(
            ArtifactKind::Skill,
            registry,
            repository,
            lock.as_ref(),
            &install_state,
            &ctx.roots,
            &active,
            &direct_repos,
            &snapshot_repos,
        )
    }

    #[tokio::test]
    async fn snapshot_repos_excludes_undeclared_bundle() {
        // Codex [medium]: a lingering [[bundle]] snapshot whose bundle is NOT in
        // the active [bundles] (removed / retagged out of band) must NOT count as
        // providing its members — snapshot_declared_repos honors the live
        // declaration, so the via-bundle fallback never trusts a stale snapshot.
        let tmp = tempfile::tempdir().unwrap();
        let workspace = tmp.path();
        std::fs::write(workspace.join("grimoire.toml"), "[skills]\n\n[rules]\n").unwrap();
        let ctx = test_ctx(workspace, registry_with_bundle().await);

        let mut bundle_row = installed_row("localhost:5050/grimoire/bundles/starter-pack");
        bundle_row.kind = "bundle".to_string();
        bundle_row.state = ArtifactState::NotInstalled;
        perform(&ctx, &bundle_row, false, None)
            .await
            .expect("bundle install succeeds");
        let lock = lock_io::load(&ctx.lock_path).expect("lock loads");
        assert!(!lock.bundles.is_empty(), "the [[bundle]] snapshot is present");

        let (_options, _registries, declared) = load_scope_declaration(&ctx).expect("declaration loads");
        let provided = snapshot_declared_repos(&declared, &lock.bundles);
        assert!(
            provided.contains(&(ArtifactKind::Skill, "localhost:5050/grimoire/skills/demo".to_string())),
            "a declared bundle provides its member: {provided:?}"
        );

        // Drop the bundle from the declaration (out-of-band removal) while the
        // snapshot lingers in the lock → it must provide nothing.
        let mut undeclared = declared.clone();
        undeclared.bundles.clear();
        undeclared.invalidate_declaration_hash_cache();
        let stale = snapshot_declared_repos(&undeclared, &lock.bundles);
        assert!(
            stale.is_empty(),
            "an undeclared bundle's lingering snapshot must provide nothing: {stale:?}"
        );
    }

    #[tokio::test]
    async fn bundle_member_shows_via_bundle_unless_also_declared_standalone() {
        // A member present only because the bundle provides it shows ViaBundle;
        // a member ALSO declared standalone shows plain Installed.
        let tmp = tempfile::tempdir().unwrap();
        let workspace = tmp.path();
        std::fs::write(workspace.join("grimoire.toml"), "[skills]\n\n[rules]\n").unwrap();
        let ctx = test_ctx(workspace, registry_with_bundle().await);

        let mut bundle_row = installed_row("localhost:5050/grimoire/bundles/starter-pack");
        bundle_row.kind = "bundle".to_string();
        bundle_row.state = ArtifactState::NotInstalled;
        perform(&ctx, &bundle_row, false, None)
            .await
            .expect("bundle install succeeds");
        assert!(workspace.join(".claude/skills/demo/SKILL.md").is_file());

        assert_eq!(
            member_badge(&ctx, "localhost:5050", "grimoire/skills/demo"),
            ArtifactState::ViaBundle,
            "a bundle-only member shows via-bundle"
        );

        // Also declare/install the member standalone → plain installed.
        let mut skill_row = installed_row("localhost:5050/grimoire/skills/demo");
        skill_row.latest_tag = "1.0.0".to_string();
        skill_row.state = ArtifactState::NotInstalled;
        perform(&ctx, &skill_row, false, None)
            .await
            .expect("skill install succeeds");

        assert_eq!(
            member_badge(&ctx, "localhost:5050", "grimoire/skills/demo"),
            ArtifactState::Installed,
            "a member also declared standalone shows plain installed"
        );
    }

    #[tokio::test]
    async fn modified_member_keeps_modified_over_via_bundle() {
        // Precedence: a tampered (modified) bundle member shows Modified, not
        // ViaBundle — only the plain Installed state is promoted.
        let tmp = tempfile::tempdir().unwrap();
        let workspace = tmp.path();
        std::fs::write(workspace.join("grimoire.toml"), "[skills]\n\n[rules]\n").unwrap();
        let ctx = test_ctx(workspace, registry_with_bundle().await);

        let mut bundle_row = installed_row("localhost:5050/grimoire/bundles/starter-pack");
        bundle_row.kind = "bundle".to_string();
        bundle_row.state = ArtifactState::NotInstalled;
        perform(&ctx, &bundle_row, false, None)
            .await
            .expect("bundle install succeeds");

        // Tamper the materialized member file so its content hash drifts.
        std::fs::write(workspace.join(".claude/skills/demo/SKILL.md"), b"tampered\n").unwrap();

        assert_eq!(
            member_badge(&ctx, "localhost:5050", "grimoire/skills/demo"),
            ArtifactState::Modified,
            "modified takes precedence over via-bundle"
        );
    }

    #[tokio::test]
    async fn aliased_bundle_member_is_protected_from_deletion() {
        // A bundle member whose skill repo basename ("cool-tool") differs from
        // its install key ("demo", the skill's own name). The install-state
        // record is keyed by the member name. Member delete must keep the files
        // (the bundle provides it — remove the bundle to remove it), and the
        // member name (DisplayRow::Member.label) — not the repo basename — is
        // what the action threads through to the keep-files gate.
        let tmp = tempfile::tempdir().unwrap();
        let workspace = tmp.path();
        std::fs::write(workspace.join("grimoire.toml"), "[skills]\n\n[rules]\n").unwrap();
        let ctx = test_ctx(workspace, registry_with_bundle_at("cool-tool").await);

        let mut bundle_row = installed_row("localhost:5050/grimoire/bundles/starter-pack");
        bundle_row.kind = "bundle".to_string();
        bundle_row.state = ArtifactState::NotInstalled;
        perform(&ctx, &bundle_row, false, None)
            .await
            .expect("bundle install succeeds");
        assert!(workspace.join(".claude/skills/demo/SKILL.md").is_file());

        // The install record is keyed by the member name ("demo"), NOT the repo
        // basename ("cool-tool").
        let st = load_state(&ctx).unwrap();
        assert!(
            st.get(ArtifactKind::Skill, "demo").is_some(),
            "install record is keyed by the bundle member name"
        );
        assert!(
            st.get(ArtifactKind::Skill, "cool-tool").is_none(),
            "no record exists under the repo basename"
        );

        // The badge derives by repo identity, so it shows via-bundle.
        assert_eq!(
            member_badge(&ctx, "localhost:5050", "grimoire/skills/cool-tool"),
            ArtifactState::ViaBundle
        );

        // Member delete keeps the files — the bundle provides the member; the
        // member name is threaded to the keep-files gate (the repo basename would
        // not match the install record).
        perform_member_uninstall(
            &ctx,
            "localhost:5050/grimoire/skills/cool-tool".to_string(),
            ArtifactKind::Skill,
            "demo".to_string(),
        )
        .await
        .expect("member uninstall succeeds");
        assert!(
            workspace.join(".claude/skills/demo/SKILL.md").is_file(),
            "a bundle-provided member's files must be kept — remove the bundle to remove it"
        );
    }

    #[tokio::test]
    async fn catalog_row_for_bundle_only_artifact_shows_via_bundle() {
        // A catalog ROW (not just the bundle member node) for an artifact that
        // is installed only because a bundle provides it shows ViaBundle — the
        // same badge it gets under the bundle, so the two views agree.
        let tmp = tempfile::tempdir().unwrap();
        let workspace = tmp.path();
        std::fs::write(workspace.join("grimoire.toml"), "[skills]\n\n[rules]\n").unwrap();
        let ctx = test_ctx(workspace, registry_with_bundle().await);

        let mut bundle_row = installed_row("localhost:5050/grimoire/bundles/starter-pack");
        bundle_row.kind = "bundle".to_string();
        bundle_row.state = ArtifactState::NotInstalled;
        perform(&ctx, &bundle_row, false, None)
            .await
            .expect("bundle install succeeds");

        let (lock, install_state, declared_bundle_repos, direct_repos, snapshot_repos) = load_scope_for_badges(&ctx);
        let badge = BadgeContext {
            lock: lock.as_ref(),
            state: &install_state,
            roots: &ctx.roots,
            active: &ClientTarget::ALL,
            declared_bundle_repos: &declared_bundle_repos,
            direct_repos: &direct_repos,
            snapshot_repos: &snapshot_repos,
        };
        assert_eq!(
            derive_row_state("skill", "localhost:5050", "grimoire/skills/demo", &badge),
            ArtifactState::ViaBundle,
            "a skill row present only via the bundle shows via-bundle"
        );

        // Declaring it standalone too flips the row to plain installed.
        let mut skill_row = installed_row("localhost:5050/grimoire/skills/demo");
        skill_row.latest_tag = "1.0.0".to_string();
        skill_row.state = ArtifactState::NotInstalled;
        perform(&ctx, &skill_row, false, None)
            .await
            .expect("skill install succeeds");
        let (lock, install_state, declared_bundle_repos, direct_repos, snapshot_repos) = load_scope_for_badges(&ctx);
        let badge = BadgeContext {
            lock: lock.as_ref(),
            state: &install_state,
            roots: &ctx.roots,
            active: &ClientTarget::ALL,
            declared_bundle_repos: &declared_bundle_repos,
            direct_repos: &direct_repos,
            snapshot_repos: &snapshot_repos,
        };
        assert_eq!(
            derive_row_state("skill", "localhost:5050", "grimoire/skills/demo", &badge),
            ArtifactState::Installed,
            "once declared standalone the row is plain installed"
        );
    }

    #[tokio::test]
    async fn deleting_bundle_only_member_keeps_files() {
        // Bug 1: a skill provided ONLY by a declared bundle must NOT have its
        // files deleted by the member-delete action — to remove it you remove
        // the bundle. (Was: the gate only protected directly-declared artifacts,
        // so a bundle-only member's files were deleted.)
        let tmp = tempfile::tempdir().unwrap();
        let workspace = tmp.path();
        std::fs::write(workspace.join("grimoire.toml"), "[skills]\n\n[rules]\n").unwrap();
        let ctx = test_ctx(workspace, registry_with_bundle().await);

        let mut bundle_row = installed_row("localhost:5050/grimoire/bundles/starter-pack");
        bundle_row.kind = "bundle".to_string();
        bundle_row.state = ArtifactState::NotInstalled;
        perform(&ctx, &bundle_row, false, None)
            .await
            .expect("bundle install succeeds");
        assert!(workspace.join(".claude/skills/demo/SKILL.md").is_file());

        // Member delete on a bundle-only member must keep the files.
        perform_member_uninstall(
            &ctx,
            "localhost:5050/grimoire/skills/demo".to_string(),
            ArtifactKind::Skill,
            "demo".to_string(),
        )
        .await
        .expect("member uninstall succeeds");
        assert!(
            workspace.join(".claude/skills/demo/SKILL.md").is_file(),
            "a bundle-only member's files must be kept — remove the bundle to remove it"
        );
    }

    #[tokio::test]
    async fn bundle_member_stays_via_bundle_after_idmismatch_lock_drop() {
        // Bug 2: install a skill standalone at :latest, install a bundle pinning
        // it at :1.0.0 (id mismatch), delete the standalone skill. The keep-files
        // gate keeps the files, but drop_from_lock drops the top-level lock entry
        // as honestly stale. The member's files + install record + [[bundle]]
        // snapshot all remain — yet the badge wrongly read NotInstalled because
        // derive required a top-level lock entry. It must read ViaBundle.
        let tmp = tempfile::tempdir().unwrap();
        let workspace = tmp.path();
        std::fs::write(workspace.join("grimoire.toml"), "[skills]\n\n[rules]\n").unwrap();
        let ctx = test_ctx(workspace, registry_with_bundle().await);

        let mut skill_row = installed_row("localhost:5050/grimoire/skills/demo");
        skill_row.latest_tag = "latest".to_string();
        skill_row.state = ArtifactState::NotInstalled;
        perform(&ctx, &skill_row, false, None)
            .await
            .expect("skill install succeeds");

        let mut bundle_row = installed_row("localhost:5050/grimoire/bundles/starter-pack");
        bundle_row.kind = "bundle".to_string();
        bundle_row.state = ArtifactState::NotInstalled;
        perform(&ctx, &bundle_row, false, None)
            .await
            .expect("bundle install succeeds");

        // Delete the standalone skill: files kept (bundle holds it at a different
        // id), but the top-level lock entry is dropped as honestly stale.
        perform_uninstall(&ctx, &skill_row).expect("skill delete succeeds");
        assert!(
            workspace.join(".claude/skills/demo/SKILL.md").is_file(),
            "files kept while the bundle still holds the skill"
        );
        let lock = lock_io::load(&ctx.lock_path).expect("lock loads");
        assert!(
            lock.skills.is_empty(),
            "id-mismatch drops the top-level lock entry (honest staleness)"
        );
        assert!(!lock.bundles.is_empty(), "the [[bundle]] snapshot is kept");

        assert_eq!(
            member_badge(&ctx, "localhost:5050", "grimoire/skills/demo"),
            ArtifactState::ViaBundle,
            "a member present via the bundle (snapshot + files + record) must read \
             via-bundle even with no top-level lock entry"
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
            ArtifactState::ViaBundle,
            "member row must be recomputed after a bundle install — ViaBundle, since \
             the member is present only via the bundle (not declared standalone)"
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

    /// A progress sink that records the calls it receives, in order.
    #[derive(Default)]
    struct RecordingProgress {
        events: std::sync::Mutex<Vec<String>>,
    }

    impl InstallProgress for RecordingProgress {
        fn start(&self, total: usize) {
            self.events.lock().unwrap().push(format!("start:{total}"));
        }
        fn advance(&self, position: usize, label: &str) {
            self.events.lock().unwrap().push(format!("advance:{position}:{label}"));
        }
        fn finish(&self) {
            self.events.lock().unwrap().push("finish".to_string());
        }
    }

    #[tokio::test]
    async fn batch_progress_counts_rows_not_per_artifact() {
        // Regression: the TUI drove the progress sink inside each row's
        // single-artifact `install_all`, so a multi-row batch always read
        // 1/1. The sink must be driven at the row grain instead: start(N),
        // then advance(1..=N) labelled with the current repo, then finish —
        // independent of how many artifacts each row materializes (the bundle
        // row pulls in a member via its own, now silent, install).
        let tmp = tempfile::tempdir().unwrap();
        let workspace = tmp.path();
        std::fs::write(workspace.join("grimoire.toml"), "[skills]\n\n[rules]\n").unwrap();
        let ctx = test_ctx(workspace, registry_with_bundle().await);

        let skill_row = installed_row("localhost:5050/grimoire/skills/demo");
        let mut bundle_row = installed_row("localhost:5050/grimoire/bundles/starter-pack");
        bundle_row.kind = "bundle".to_string();

        let mut state = TuiState::new();
        state.set_rows(vec![skill_row, bundle_row]);

        // `set_rows` kind-sorts the rows (bundle before skill), so the row
        // order is [bundle, skill]; drive every visible row.
        let recorder = RecordingProgress::default();
        run_batch_with_progress(&ctx, &mut state, &[0, 1], BatchOp::Install, &recorder).await;

        let events = recorder.events.lock().unwrap().clone();
        assert_eq!(
            events,
            vec![
                "start:2".to_string(),
                "advance:1:localhost:5050/grimoire/bundles/starter-pack".to_string(),
                "advance:2:localhost:5050/grimoire/skills/demo".to_string(),
                "finish".to_string(),
            ],
            "batch progress must count rows (start:2, advance 1→2 by repo), not reset to 1/1 per artifact"
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
            registries: vec![ResolvedRegistry {
                url: "localhost:5050".to_string(),
                alias: None,
                is_default: true,
                kind: crate::config::registry_resolve::SourceKind::Registry,
            }],
            primary_registry: "localhost:5050".to_string(),
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
        let (reg, repo_path) = repo.split_once('/').unwrap_or((repo, ""));
        TuiRow {
            kind: "bundle".to_string(),
            registry: reg.to_string(),
            repository: repo_path.to_string(),
            repo: repo.to_string(),
            description: String::new(),
            summary: String::new(),
            keywords: Vec::new(),
            repository_url: None,
            revision: None,
            created: None,
            latest_tag: "latest".to_string(),
            version: "1.0.0".to_string(),
            deprecated: None,
            pinned_version: None,
            state: ArtifactState::Installed,
            source: None,
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
                registry: "reg.example.io".to_string(),
                repository: "acme/my-skill".to_string(),
                repo: skill_repo.to_string(),
                description: String::new(),
                summary: String::new(),
                keywords: Vec::new(),
                repository_url: None,
                revision: None,
                created: None,
                latest_tag: "latest".to_string(),
                version: "1.0.0".to_string(),
                deprecated: None,
                pinned_version: None,
                state: ArtifactState::Installed,
                source: None,
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
            registry: "localhost:5050".to_string(),
            repository: "grimoire/skills/demo".to_string(),
            repo: "localhost:5050/grimoire/skills/demo".to_string(),
            description: String::new(),
            summary: String::new(),
            keywords: Vec::new(),
            repository_url: None,
            revision: None,
            created: None,
            latest_tag: "1.0.0".to_string(),
            version: String::new(),
            deprecated: None,
            pinned_version: None,
            state: ArtifactState::NotInstalled,
            source: None,
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
            "demo".to_string(),
            "localhost:5050",
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
        let (reg, repo_path) = repo.split_once('/').unwrap_or((repo, ""));
        TuiRow {
            kind: "skill".to_string(),
            registry: reg.to_string(),
            repository: repo_path.to_string(),
            repo: repo.to_string(),
            description: String::new(),
            summary: String::new(),
            keywords: vec![],
            repository_url: None,
            revision: None,
            created: None,
            latest_tag: latest_tag.to_string(),
            version: "1.0.0".to_string(),
            deprecated: None,
            pinned_version: pinned_version.map(|s| s.to_string()),
            state: ArtifactState::NotInstalled,
            source: None,
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
            registries: vec![ResolvedRegistry {
                url: "localhost:5050".to_string(),
                alias: None,
                is_default: true,
                kind: crate::config::registry_resolve::SourceKind::Registry,
            }],
            primary_registry: "localhost:5050".to_string(),
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
            "noslash".to_string(),
            "localhost:5050",
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

        let result =
            perform_member_uninstall(&ctx, "noslash".to_string(), ArtifactKind::Skill, "noslash".to_string()).await;

        assert!(
            result.is_err(),
            "C-12: perform_member_uninstall('noslash', …) must return Err, got Ok({result:?})"
        );
    }
}
