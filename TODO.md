# TODO

All items from the 2026-06-11 sweep are addressed (see
`.claude/artifacts/plan_todo_overnight.md` for decisions and commits).

## Open

### Republish grim-essentials bundle

The registry copy of `grim.ocx.sh/bundles/grim-essentials:latest` still
references members at the floating `:1` tag (`skill … :1: tag not found`
on install); the repo's bundle TOML already references `:0`. Needs a
`task catalog:release` by a maintainer with registry credentials.

## Follow-ups (deferred from review, warn/suggest tier)

- Search/TUI: both now build the same unscoped browse window (equivalent
  results), so any query can miss repos past the 500-repo cap. Truncation
  is visible in CLI (stderr warning) and TUI (legend hint); a namespaced
  `--registry host/namespace` scopes the build deterministically, and a
  pagination/multi-fetch rework would close the gap fully.
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
- TUI: bundle rows get no floating-tag "outdated" re-check (the lock
  records member pins but no bundle digest, so there is no baseline to
  compare the registry's bundle tag against). Member rows still re-check
  individually; recording the bundle digest in lock provenance would
  close the gap.
