# Bundle Spec

You loaded this file because you are authoring or fixing a grim bundle —
a `.toml` file listing member artifacts — for `grim build` or
`grim release`.

Contents: [File Shape](#file-shape) · [Top-Level Keys](#top-level-keys) ·
[Member Tables](#member-tables) · [Floating vs. Pinned](#floating-vs-pinned) ·
[Limits](#limits) · [Publish Order](#publish-order) · [Example](#example) ·
[Validation Pitfalls](#validation-pitfalls)

## File Shape

A bundle is a curated set of references to other artifacts: one `.toml`
file (`.toml` path → bundle, no flag needed), named by its file stem
under the standard name rules. A bundle never materializes files of its
own — installing it expands to installing its members.

## Top-Level Keys

Catalog metadata sits at the **top level** of the TOML, above the member
tables — not in any nested map:

| Key | Notes |
|---|---|
| `summary` | One-line catalog blurb |
| `keywords` | One comma-separated string — a TOML array is rejected |
| `description` | Overrides the automatic `grimoire bundle of N members` |
| `repository` | `https://` source URL; anything else fails release (exit 65) |
| `deprecated` | Deprecation notice; non-empty marks the bundle deprecated (flagged in search/TUI, warned on `add`) |

The bundle source parser is **strict** (`deny_unknown_fields`): any key
outside this set and the three member tables is a hard parse error.
Unlike skill/rule frontmatter, a typo'd bundle key cannot slip through.

## Member Tables

Three optional tables, each mapping a **config binding name** (the name
the member installs under when a consumer adds the bundle) to a
fully-qualified reference:

```toml
[skills]
code-reviewer = "registry.example.com/grimoire/skills/code-reviewer:1"
[rules]
rust-style = "registry.example.com/grimoire/rules/rust-style:1"
[agents]
reviewer = "registry.example.com/grimoire/agents/reviewer@sha256:8f4b..."
```

References must be fully qualified — `registry/repo:tag` or
`registry/repo@sha256:…`; a registry-less ref fails validation. There is
no `[bundles]` table — nested bundles are invalid.

## Floating vs. Pinned

By default the bundle stores members exactly as written: floating tags
stay floating and each consumer's `grim lock` re-resolves them fresh;
digest members (`@sha256:…`) never move. Add `--pin` at release to
freeze every floating member to a digest in the published bundle — it
then always expands to the same digests, even offline; re-run the
release to roll it forward ([pinning][pin]).

## Limits

- At most **512 members** per bundle (parse-time error beyond that).
- The members document is capped at **512 KiB**; no nesting.

## Publish Order

Publish **members first, bundle last**. A bundle stores references —
nothing checks at bundle-release time that members exist, so a bundle
pushed first resolves to 404s on the consumer's `grim lock`. `--pin`
enforces the order naturally: pinning must resolve every member.

## Example

```toml
# starter-pack.toml
summary = "Curated starter pack"
keywords = "starter,review,style"
repository = "https://github.com/acme/starter-pack"

[skills]
code-reviewer = "ghcr.io/acme/code-reviewer:1"
[rules]
rust-style = "ghcr.io/acme/rust-style:2"
```

## Validation Pitfalls

| Pitfall | Outcome |
|---|---|
| Typo'd top-level key (`sumary = …`) | Hard parse error — strict schema, build fails |
| `keywords` as a TOML array | Hard parse error — must be one string |
| Member ref not fully qualified | Hard error — invalid reference |
| Nested bundle member | Invalid — no `[bundles]` table exists |
| More than 512 members, or members document > 512 KiB | Rejected |
| `repository` not `https://` | Hard error, exit 65 |
| Two declared bundles disagreeing on a member | Consumer-side `BundleConflict` at lock time (exit 78, fail-closed) — keep curated sets disjoint |
| Bundle pushed before its members | No publish error — consumers hit 404 at lock |

## Further Reading

- [Bundle schema and example][bundles-ref] — the authoritative key table.
- [Publishing bundles][pub-bundles] — build/release walk-through.
- [Floating or pinned members][pin] — `--pin` semantics.

[bundles-ref]: https://grimoire.rs/artifacts.html#bundles
[pub-bundles]: https://grimoire.rs/publishing.html#bundles
[pin]: https://grimoire.rs/publishing.html#pin
