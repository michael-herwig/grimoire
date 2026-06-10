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

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind};
use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

use crate::catalog::registry_catalog::Catalog;
use crate::config::declaration::DesiredSet;
use crate::config::scope::ConfigScope;
use crate::install::install_state::InstallState;
use crate::install::installer::{InstallOutcome, install_all};
use crate::install::materializer::DefaultMaterializer;
use crate::install::target::InstallTarget;
use crate::lock::grimoire_lock::GrimoireLock;
use crate::lock::lock_io;
use crate::oci::access::OciAccess;
use crate::oci::{ArtifactKind, Identifier};
use crate::resolve::resolve_options::ResolveOptions;
use crate::resolve::resolver::resolve_lock;

use super::event::{BatchOp, TuiAction, TuiInput, handle};
use super::render::{draw, frame};
use super::state::{ArtifactState, TuiRow, TuiState};

use std::collections::BTreeMap;

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
    /// The scope install/update materialize into.
    pub scope: ConfigScope,
    /// The workspace root targets are rooted at.
    pub workspace: std::path::PathBuf,
    /// The scope's lock path (for badge derivation only — the TUI
    /// resolves a fresh single-artifact lock per action).
    pub lock_path: std::path::PathBuf,
    /// The scope's install-state path.
    pub state_path: std::path::PathBuf,
    /// The AI client target(s) to materialize into (the config `clients`
    /// option; empty defaults to `claude`).
    pub clients_default: Vec<String>,
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
    /// The AI client target(s) to materialize into.
    pub clients_default: Vec<String>,
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
            clients_default: std::mem::replace(&mut self.clients_default, alt.clients_default),
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
    // The browsed registry is the effective default: eliding its host
    // from the tree root keeps leaf names short (the user's ask).
    state.set_default_registry(Some(ctx.registry.clone()));

    // Initial async catalog load: show `loading`, then populate.
    terminal.draw(|f| draw(f, &frame(&state)))?;
    load_into(&ctx, &mut state).await;
    terminal.draw(|f| draw(f, &frame(&state)))?;

    loop {
        // Poll so a slow terminal does not spin; redraw on any event.
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

        match handle(&mut state, input) {
            TuiAction::Quit => break,
            TuiAction::None => {}
            TuiAction::Refresh => {
                state.set_loading(true);
                state.set_status("refreshing catalog…");
                terminal.draw(|f| draw(f, &frame(&state)))?;
                reload_into(&ctx, &mut state, true).await;
            }
            TuiAction::Batch { op, rows } => {
                run_batch(&ctx, &mut state, &rows, op).await;
            }
            TuiAction::LoadVersions { row } => {
                load_versions(&ctx, &mut state, row).await;
            }
            TuiAction::ToggleScope => {
                if ctx.toggle_scope() {
                    state.set_scope_label(&ctx.scope_label);
                    recompute_states(&ctx, &mut state);
                    // The colored MODE box already shows the active scope
                    // — no redundant title-bar status.
                    state.set_status("");
                } else {
                    state.set_status("no alternate scope to switch to");
                }
            }
        }
        terminal.draw(|f| draw(f, &frame(&state)))?;
    }
    Ok(())
}

/// Map a crossterm key to the abstract [`TuiInput`]. The *only*
/// crossterm-aware decision in the codebase; the alphabet it targets is
/// pure and fully unit-tested in `event.rs`.
fn map_key(key: KeyEvent) -> Option<TuiInput> {
    Some(match key.code {
        KeyCode::Up => TuiInput::Up,
        KeyCode::Down => TuiInput::Down,
        KeyCode::Right => TuiInput::Expand,
        KeyCode::Left => TuiInput::Collapse,
        KeyCode::Enter => TuiInput::Enter,
        KeyCode::Esc => TuiInput::Esc,
        KeyCode::Backspace => TuiInput::Backspace,
        KeyCode::Char(c) => TuiInput::Char(c),
        _ => return None,
    })
}

/// Load the catalog (cold) into `state`, degrading on any failure.
async fn load_into(ctx: &TuiContext, state: &mut TuiState) {
    reload_into(ctx, state, false).await;
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
        .map(|e| TuiRow {
            kind: e.kind.clone().unwrap_or_else(|| "-".to_string()),
            repo: e.repo(),
            description: e.description.clone().unwrap_or_default(),
            summary: e.summary.clone().unwrap_or_default(),
            keywords: e.keywords.clone(),
            latest_tag: e.latest_tag.clone().unwrap_or_default(),
            // Show the explicit highest version; fall back to the
            // representative tag when no semver tag exists.
            version: e.version.clone().or_else(|| e.latest_tag.clone()).unwrap_or_default(),
            pinned_version: None,
            state: derive_artifact_state(&e.registry, &e.repository, lock, state),
        })
        .collect()
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
            r.state = derive_artifact_state(&registry, &repository, lock.as_ref(), &install_state);
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
                    r.state = derive_artifact_state(&registry, &repository, lock.as_ref(), &install_state);
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

/// Uninstall one catalog row through the shared
/// [`crate::install::uninstall`] seam: delete the materialized files,
/// drop the install-state record, and drop the lock pin (the TUI does
/// not edit `grimoire.toml` — it never wrote a config binding). Mirrors
/// the file/state half of `command::uninstall` without forking it.
fn perform_uninstall(ctx: &TuiContext, row: &TuiRow) -> anyhow::Result<()> {
    let (_registry, repository) =
        split_repo(&row.repo).ok_or_else(|| anyhow::anyhow!("malformed catalog repo: {}", row.repo))?;
    let kind = match row.kind.as_str() {
        "rule" => ArtifactKind::Rule,
        _ => ArtifactKind::Skill,
    };
    let name = repository.rsplit('/').next().unwrap_or(&repository).to_string();

    let mut install_state =
        InstallState::load(&ctx.state_path).map_err(|e| anyhow::anyhow!("install-state load failed: {e}"))?;
    let result = crate::install::uninstall::uninstall(&mut install_state, kind, &name)
        .map_err(|e| anyhow::anyhow!("uninstall failed: {e}"))?;
    if result.outcome == crate::install::uninstall::UninstallOutcome::Removed {
        install_state
            .save()
            .map_err(|e| anyhow::anyhow!("install-state save failed: {e}"))?;
    }

    // Drop the lock pin so the badge no longer derives "installed".
    if let Ok(previous) = lock_io::load(&ctx.lock_path) {
        let mut lock = previous.clone();
        match kind {
            ArtifactKind::Skill => lock.skills.retain(|a| a.name != name),
            ArtifactKind::Rule => lock.rules.retain(|a| a.name != name),
            // The TUI lists individual skills/rules; bundles are not
            // browsable or installable through it.
            ArtifactKind::Bundle => unreachable!("the TUI never operates on bundles"),
        }
        lock_io::save(&ctx.lock_path, &lock, Some(&previous))
            .map_err(|e| anyhow::Error::from(crate::error::Error::from(e)))?;
    }
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
async fn perform(ctx: &TuiContext, row: &TuiRow, is_update: bool) -> anyhow::Result<String> {
    let (registry, repository) =
        split_repo(&row.repo).ok_or_else(|| anyhow::anyhow!("malformed catalog repo: {}", row.repo))?;

    let kind = match row.kind.as_str() {
        "rule" => ArtifactKind::Rule,
        // Default unknown/"-" to skill (a directory artifact); the
        // materializer validates the actual payload shape.
        _ => ArtifactKind::Skill,
    };
    let name = repository.rsplit('/').next().unwrap_or(&repository).to_string();
    // A user-pinned version (chosen in the picker) wins; otherwise the
    // representative tag, otherwise the conventional `latest`.
    let tag = row
        .pinned_version
        .clone()
        .filter(|t| !t.is_empty())
        .or_else(|| Some(row.latest_tag.clone()).filter(|t| !t.is_empty()))
        .unwrap_or_else(|| "latest".to_string());
    let id = Identifier::new_registry(repository.clone(), registry).clone_with_tag(tag);

    // A single-artifact desired set — exactly the shape the commands feed
    // the resolver, so resolution/locking/materializing are unforked.
    let mut skills = BTreeMap::new();
    let mut rules = BTreeMap::new();
    match kind {
        ArtifactKind::Skill => {
            skills.insert(name.clone(), id);
        }
        ArtifactKind::Rule => {
            rules.insert(name.clone(), id);
        }
        ArtifactKind::Bundle => unreachable!("the TUI never operates on bundles"),
    }
    let set = DesiredSet::from_parts(skills, rules);

    let new_lock = resolve_lock(&set, &ctx.access, ctx.scope, &ResolveOptions::default())
        .await
        .map_err(|e| anyhow::Error::from(crate::error::Error::from(e)))?;

    let target = InstallTarget::parse(&ctx.workspace, &[], &ctx.clients_default)
        .map_err(|e| anyhow::Error::from(crate::error::Error::from(e)))?;
    let mut install_state =
        InstallState::load(&ctx.state_path).map_err(|e| anyhow::anyhow!("install-state load failed: {e}"))?;
    let materializer = DefaultMaterializer;

    // `update` forces re-materialization (rolling-release contract),
    // matching `command::update`; `install` honours the integrity gate.
    let outcomes = install_all(
        &new_lock,
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

    // Persist the resolved single-artifact lock alongside the scope so the
    // badge derivation (and a later command run) observes the new pin.
    merge_and_save_lock(ctx, &new_lock, kind, &name)?;

    let mut label = "unchanged".to_string();
    for o in outcomes {
        match o.result {
            Ok(outcome) => label = outcome_label(&outcome).to_string(),
            Err(e) => return Err(anyhow::Error::from(e)),
        }
    }
    Ok(label)
}

/// Splice the single resolved artifact into the scope's existing lock (or
/// create one) and persist it, so the row badge reflects the new pin
/// without clobbering other locked artifacts.
fn merge_and_save_lock(
    ctx: &TuiContext,
    resolved: &GrimoireLock,
    kind: ArtifactKind,
    name: &str,
) -> anyhow::Result<()> {
    let mut lock = lock_io::load(&ctx.lock_path).unwrap_or_else(|_| resolved.clone());
    let Some(entry) = resolved
        .skills
        .iter()
        .chain(resolved.rules.iter())
        .find(|a| a.kind == kind && a.name == name)
        .cloned()
    else {
        return Ok(());
    };
    let bucket = match kind {
        ArtifactKind::Skill => &mut lock.skills,
        ArtifactKind::Rule => &mut lock.rules,
        ArtifactKind::Bundle => unreachable!("the TUI never operates on bundles"),
    };
    match bucket.iter_mut().find(|a| a.name == name) {
        Some(slot) => *slot = entry,
        None => bucket.push(entry),
    }
    // Carry the freshly-resolved declaration metadata so the lock stays
    // self-consistent for a subsequent command-line run.
    lock.metadata = resolved.metadata.clone();
    lock_io::save(&ctx.lock_path, &lock, None).map_err(|e| anyhow::Error::from(crate::error::Error::from(e)))?;
    Ok(())
}

/// Split `registry/repository` at the first `/`.
fn split_repo(repo: &str) -> Option<(String, String)> {
    repo.split_once('/').map(|(r, p)| (r.to_string(), p.to_string()))
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
