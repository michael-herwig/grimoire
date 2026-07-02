 - bundle expansion "wrong registry": ROOT-CAUSED, not a grim defect. The four
   ghcr.io/grimoire-rs packages were NEVER PUBLISHED — the last successful
   publish-catalog run (June 12) still targeted grim.ocx.sh; after the GHCR
   port the workflow was never re-run, and the index refs were repointed via a
   direct push (no ref_reachable gate). Re-dispatch (run 28614540347) FAILED:
   the workflow publishes with the latest *released* grim (0.6.x), which
   rejects the `[announce]` key now in main's catalog/publish.toml
   ("unknown field `announce`"). Blocked on releasing 0.7.0 — its post-release
   workflow_call re-runs publish-catalog with the new binary. AFTER that lands:
   packages are created PRIVATE — flip each to public (container packages with
   '/' in the name may not list in the Packages tab; use direct settings URLs,
   e.g. github.com/orgs/grimoire-rs/packages/container/skills%2Fgrim-usage/settings,
   …/skills%2Fai-config-authoring/settings, …/skills%2Fgrim-authoring/settings,
   …/bundles%2Fgrim-essentials/settings). Then re-test TUI bundle expansion.
 - [x] registry longest-prefix / "ghcr.io/grimoire-rs splitted into ghcr.io and
   grimoire-rs": fixed in two commits — 7f7e609 (index-only sets corrupted short-id
   adds with a registry-less ref; now falls back through the documented default
   chain) and 8b12470 (TUI tree roots index-sourced rows at their source locator;
   host/namespace chains fold into one node).
