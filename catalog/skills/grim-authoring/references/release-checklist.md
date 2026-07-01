# Release Checklist

You loaded this file because you are about to `grim release` an
artifact, or a build/release just failed and you need the triage table.

Contents: [Pre-Release](#pre-release) ·
[Release Mechanics](#release-mechanics) · [Exit-65 Triage](#exit-65-triage)

## Pre-Release

Work through these in order; each catches a class of failure before it
reaches a registry:

1. **Catalog metadata authored** — `summary` present (search shows it
   instead of the description), `keywords` is one comma-separated string
   (never a YAML/TOML list), `repository` is an `https://` URL. Check
   the per-kind location: skill/agent → `metadata` map; rule → top-level
   frontmatter; bundle → top-level TOML. If this release retires a
   package, set the `deprecated` notice in the same location (a re-release
   without it clears the flag).
2. **`grim build <path>` exits 0** — and read the *warnings* too:
   warn-and-drop vendor keys and migration nudges are silent data loss
   if shipped.
3. **Agents: `--kind agent` on both build and release** — a forgotten
   flag publishes a rule, with only a warning.
4. **Bundles: members published first** — a bundle referencing unpushed
   members breaks the consumer's `grim lock`, not your release.
5. **`grim release … --dry-run`** — prints the exact push plan: every
   tag and the digest each will point at, without touching the registry.

## Release Mechanics

- **Cascade tags.** Releasing `1.2.3` also moves the floating tags
  `1`, `1.2`, and `latest` to the new digest — that is how consumers
  tracking `:1` pick up your patch on `grim update`. Implication: a
  release is immediately visible to every floating consumer; do not
  release a breaking change under the same major.
- **Immutability gate.** An exact-version tag that already exists and
  points at different bytes refuses to move. `--force` overrides —
  use it only to deliberately rewrite history. Re-releasing *identical*
  bytes is idempotent and always fine — *unless* you pass `--git` (see
  below), where only a re-release from the **same** commit stays idempotent.
- **Git provenance is opt-in (`--git`).** `grim build`/`release`/`publish`
  accept `--git`, which stamps the source commit, commit date, and
  `origin` remote onto the manifest as OCI annotations. It is off by
  default to keep re-release byte-deterministic: with `--git`, a re-release
  from a *different* commit changes the digest and is refused unless
  `--force`. Confirm the exact behavior with `grim release --help`.
- **Bundles: `--pin`** resolves every floating member to a digest at
  release time for a self-contained, reproducible bundle
  ([pinning][pin]).

## Exit-65 Triage

`grim build`/`grim release` validation failures exit 65 (DataError).
Symptom → cause → fix:

| Symptom (error mentions…) | Cause | Fix |
|---|---|---|
| name "must contain only lowercase…" / hyphen rules | Charset, leading/trailing or consecutive hyphens, > 64 chars | Rename to `[a-z0-9-]`, fix hyphens |
| Name mismatch | Skill `name` ≠ directory name, or agent `name` ≠ file stem | Make them equal (rename file/dir or edit frontmatter) |
| Missing frontmatter | Skill without `---` fence, unclosed fence, or agent with no frontmatter | Add the fenced block with required fields |
| Frontmatter parse | Malformed YAML; missing `name`/`description`; empty or > 1024-char description | Fix the YAML; supply required fields |
| Missing `SKILL.md` | Directory built as a skill has no index | Add `SKILL.md` or point at the right path |
| Invalid value for metadata key `repository` | Non-`https://` URL (`git@…`, `http://`) | Use the `https://` forge URL |
| Bad vendor literal (bool/enum/int/float) | Known `<vendor>.<field>` key with an invalid string | Use the registry's accepted literals — see [vendor-metadata.md](vendor-metadata.md) |
| Invalid version / missing tag | Release ref has no tag or a malformed version | Release as `repo:X.Y.Z` |
| Tag exists | Exact-version tag points at different bytes | Bump the version; `--force` only for deliberate rewrites |
| `--git` on a non-git path | `--git` passed to build/release on an artifact path not inside a git repository, or no `git` on the host | Build/release an artifact that lives inside a git repo (with `git` installed), or drop `--git` (confirm with `grim release --help`) |

Bundle source errors (typo'd key, non-qualified member ref, > 512
members) surface as config/parse failures rather than 65 — see
[bundle-spec.md](bundle-spec.md). If the message fits no row, run
`grim build --format json` for the structured detail and check
[publish-time validation][publish-val].

## Further Reading

- [Validate before you push][validate] — the build-then-release flow.
- [Dry runs and overwrites][dry-run] — preview and immutability.
- [Cascade tags][cascade] — what a release actually moves.

[validate]: https://grimoire.rs/publishing.html#validate-before-you-push
[dry-run]: https://grimoire.rs/publishing.html#dry-runs-and-overwrites
[cascade]: https://grimoire.rs/publishing.html#cascade-tags
[pin]: https://grimoire.rs/publishing.html#pin
[publish-val]: https://grimoire.rs/vendor-metadata.html#publish-validation
