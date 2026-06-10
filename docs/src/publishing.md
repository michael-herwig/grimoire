# Publishing Skills and Rules

Consuming artifacts is only half of Grimoire. The other half is producing them:
turning a local skill directory or rule file into a versioned OCI artifact that
others can [`grim add`](./commands.md#add).

## Author locally

A **skill** is a directory containing a `SKILL.md` and any supporting files; a
**rule** is a Markdown file, optionally with a
[sibling support directory](#rule-support-dir); a
[**bundle**](./concepts.md#bundles) is a `.toml` file listing members. Grimoire
detects which one you mean from the path — a directory packs as a skill, a `.md`
file as a rule, a `.toml` file as a bundle — and `--kind` overrides the guess
when you need to.

### Rules with a support directory {#rule-support-dir}

An index rule often references extra context — examples, a schema, a script —
that does not belong inside the rule body. Put those in a folder beside the
rule that shares its stem, and Grimoire packs both into the one artifact:

```
rules/
  my-rule.md        # the index you pass to build/release
  my-rule/          # optional support directory, same stem
    examples.md
    schema.json
```

You still point [`grim build`](./commands.md#build) and
[`grim release`](./commands.md#release) at the index `.md` file — the sibling
directory is discovered automatically when it exists:

```sh
grim release ./my-rule.md ghcr.io/acme/my-rule:1.0.0
```

Every file under `my-rule/` rides along in the same layer and installs beside
the index (`.claude/rules/my-rule.md` + `.claude/rules/my-rule/…`), so the
index's relative links resolve on the consumer. Support files are copied
verbatim for every [client](./concepts.md#clients) — only the index is ever
transformed. A rule with no support directory packs to exactly the single
`my-rule.md` it always did.

## Validate before you push

[`grim build`](./commands.md#build) validates and packs an artifact **without**
pushing it. Run it while iterating to catch a malformed skill before anyone
else sees it:

```sh
grim build ./code-review
grim build ./rust-style.md --kind rule
```

## Release

[`grim release`](./commands.md#release) validates, packs, and pushes to a
registry in one step. Give it the source path and the release reference:

```sh
grim release ./code-review ghcr.io/acme/code-review:1.2.3
```

### Cascade tags

A release does more than push one tag. From a `1.2.3` version it also moves the
**floating** tags that consumers track — `1`, `1.2`, and `latest` — to the new
digest. That is what lets a consumer who declared `:1` pick up `1.2.3` with a
plain [`grim update`](./commands.md#update).

### Dry runs and overwrites

Preview the exact push plan — every tag and the digest each will point at —
without touching the registry:

```sh
grim release ./code-review ghcr.io/acme/code-review:1.2.3 --dry-run
```

An exact-version tag is immutable by default: if `1.2.3` already exists and
points at different bytes, the release refuses rather than rewrite history.
Pass `--force` only when you deliberately mean to move it.

## Publishing bundles {#bundles}

A [bundle](./concepts.md#bundles) groups skills and rules so consumers declare
one reference instead of a dozen. You author it as a small TOML file whose
`[skills]`/`[rules]` tables list the members — the same shape as a
`grimoire.toml`:

```toml
# python-stack.toml
[skills]
code-review = "ghcr.io/acme/code-review:1"

[rules]
rust-style = "ghcr.io/acme/rust-style:2"
```

[`grim build`](./commands.md#build) validates it (a `.toml` path packs as a
bundle), and [`grim release`](./commands.md#release) pushes it with the same
cascade tags as any other artifact:

```sh
grim build ./python-stack.toml
grim release ./python-stack.toml ghcr.io/acme/python-stack:1.0.0
```

### Floating or pinned members {#pin}

By default the bundle stores its members exactly as written — floating tags stay
floating, and each consumer's [`grim lock`](./commands.md#lock) re-resolves them
fresh. Add `--pin` to resolve every floating member to a digest at release time
and freeze it into the published bundle:

```sh
grim release ./python-stack.toml ghcr.io/acme/python-stack:1.0.0 --pin
```

A pinned bundle is reproducible on its own: it always expands to the exact same
member digests, even on an air-gapped or tunneled network that cannot re-resolve
a tag. Re-run the release (a cron job tracking `:stable`, say) to roll the
pinned members forward.

## Authenticate

Grimoire pushes over standard OCI, so it reuses your existing registry
credentials — the same login your container tooling uses. Authenticate once
with your registry (for example, `docker login ghcr.io`) and `grim release`
inherits it.

<!-- external -->
[ghcr]: https://docs.github.com/en/packages/working-with-a-github-packages-registry/working-with-the-container-registry
