# TODO

All items from the 2026-06-11 sweep are addressed (see
`.claude/artifacts/plan_todo_overnight.md` for decisions and commits).

## Open

### Artifacts Reference

The documentation should have a bit more expamples of valid artifacts.
Further, there should be a reference of all supported artifact types and their attributes, including vendor-specific extensions.

### Support for Agents

A new kind of artifacts 'agent'.
See:

Claude:
 - https://code.claude.com/docs/en/sub-agents
OpenCode:
 - https://opencode.ai/docs/en/agents/
Copilot:
 - https://docs.github.com/en/copilot/how-tos/copilot-cli/customize-copilot/create-custom-agents-for-cli
 - https://docs.github.com/en/copilot/concepts/agents/copilot-cli/about-custom-agents

Similar to others. We should support metadata extensions for vendor specific providers, but also have common attributes in the top-tier configuration.
The configuration should be fully documented, incl. reference.

### Manual tests and documentation regression

Examples in the documentation and the manual test environemnt break very often.
All standard workflows should be covered by automated tests, to ensure they are always up to date and working.

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
- TUI: bundle rows get no floating-tag "outdated" re-check (the lock
  records member pins but no bundle digest, so there is no baseline to
  compare the registry's bundle tag against). Member rows still re-check
  individually; recording the bundle digest in lock provenance would
  close the gap.
