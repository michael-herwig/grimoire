# TODO

All items from the 2026-06-11 sweep are addressed (see
`.claude/artifacts/plan_todo_overnight.md` for decisions and commits).

## Open

- TUI: installing an outdated version first shows `installed`; flips to
  `outdated` only after a catalog refresh. Root cause: badge derivation
  (`status_badge.rs::derive_badge`, `app.rs::derive_artifact_state`,
  `status.rs::derive_state`) compares only install-record pin vs lock pin
  — both hold the just-installed digest. "Latest" knowledge arrives only
  via the background update check / catalog refresh (`update_check.rs`).
  Yet `app.rs::perform` has `row.latest_tag` in hand at install time.
  Fix direction: after a successful install where the pinned version ≠
  latest, flip the badge immediately (`state.rs::mark_outdated_if_installed`)
  or enqueue a per-row update check.
- TUI install never declares the artifact in `grimoire.toml` — entries
  land only in the lock (`app.rs::perform` → `merge_and_save_lock`),
  unlike `grim add`, which writes manifest + lock. Also overwrites
  `lock.metadata` with synthetic single-artifact declaration metadata,
  corrupting the manifest-drift hash; a later `grim lock` from the real
  manifest can drop TUI-installed entries. Fix direction: TUI install
  should declare like `grim add` (shared `write_config` seam).
  (CLI split itself is by design: `add` declares, `install` materializes.)

## Follow-ups (deferred from review, warn/suggest tier)

- Search: multi-term queries whose terms only match summary/description/
  keywords can still miss repos beyond the 500-repo browse window (the
  longest-term prefilter is name-scoped). Truncation is now visible in CLI
  (stderr warning) and TUI (legend hint); a pagination/multi-fetch rework
  would close the gap fully.
- Search JSON report: add a machine-readable `truncated` field (currently
  stderr-only) so scripts can detect incomplete results.
- TUI: background task panics are reaped but deliberately swallowed
  (raw-mode terminal, no stderr); consider a status-line error tally.
- TUI: string truncation in `fit()` counts chars, not terminal display
  width (pre-existing; matters for wide glyphs).
- TUI: selected-clients line degrades to detection when config has invalid
  client names while install errors hard — acceptable as best-effort
  display, revisit if confusing.
- TUI: synchronous lock/install-state reads run on the event loop each
  drain/schedule pass — fine at current sizes, move off-loop if it grows.
