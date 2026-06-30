# Updating This Skill

You loaded this file because you maintain the grim-usage package and
need to refresh it against a newer grim release.

## Re-Verification Protocol

1. Run `grim --version` and `grim <cmd> --help` for every command this
   package narrates (init, add, lock, install, update, status, remove,
   uninstall, search, schema, tui, mcp, build, release, publish, login,
   logout). Diff the help output against what the reference files claim.
2. Re-read the docs pages each reference file distills (links below) and
   diff against the file's claims — especially lifecycle semantics
   (pruning, effective declarations, integrity gates, deprecation
   warnings) and precedence chains (registry, clients).
3. Re-check the exit-code table in
   [troubleshooting.md](troubleshooting.md) against the docs' command
   reference — codes are a stable contract but new codes can appear.
4. Bump the `compatibility` frontmatter and the "Verified against"
   footer in `SKILL.md` to the verified version line.

## What Drifts, and How Fast

Tier-1 invariants (the four kinds, reference syntax, exit-code classes,
cascade-tag semantics) are design commitments — they rarely move.
Tier-2 content (flag names, command lifecycles, precedence details)
drifts with **every minor release** — re-verify it on each new grim
minor. Anything resembling a flag list belongs in `--help`, not here; if
a reference file has accreted one, delete it and link instead.

## Durable Search Terms

- `grimoire grim oci package manager skills rules agents`
- `github michael-herwig grimoire releases changelog`
- `grim release cascade tags pin bundle`
- `grim exit codes sysexits`

## Canonical Pages

- [Command reference][commands] — consume.md, publish.md, registries.md
- [Concepts][concepts] — consume.md (lock, bundles), registries.md
  (scopes, clients, offline)
- [Configuration][config] — consume.md (the two files), registries.md
  (env vars, precedence)
- [Publishing][publishing] — publish.md
- [Authentication][auth] — publish.md, troubleshooting.md

[commands]: https://michael-herwig.github.io/grimoire/commands.html
[concepts]: https://michael-herwig.github.io/grimoire/concepts.html
[config]: https://michael-herwig.github.io/grimoire/configuration.html
[publishing]: https://michael-herwig.github.io/grimoire/publishing.html
[auth]: https://michael-herwig.github.io/grimoire/authentication.html
