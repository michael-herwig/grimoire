# Quick Start

This walkthrough declares a skill from a registry, installs it into a project,
and then upgrades it. It assumes `grim` is on your `PATH` (see
[Installation][install]) and that you can reach an [OCI registry][oci] that
hosts Grimoire artifacts.

## 1. Create a project config

`grim init` writes a fresh `grimoire.toml` in the current directory. Seed it
with the registry you pull from so short references resolve without repeating
the host:

```sh
grim init --registry ghcr.io/acme
```

## 2. Declare an artifact

`grim add` records a skill or rule in `grimoire.toml` and immediately pins it
in `grimoire.lock`. The only required argument is the reference to fetch; the
kind is inferred from the artifact's OCI `artifactType` and the binding name
defaults to the reference's last path segment:

```sh
grim add ghcr.io/acme/code-review:1
```

The reference is `registry/repo:tag` (or `registry/repo@sha256:…` to pin an
exact digest). A floating tag like `:1` tracks the newest `1.x` release, which
is what makes [`grim update`](#5-upgrade) meaningful later.

## 3. Install into your AI client(s)

`grim install` materializes every locked artifact into your AI client's
configuration directory. By default it targets [Claude Code][claude]; pass
`--client` to select [opencode][opencode], [GitHub Copilot][copilot], or
[OpenAI Codex][codex], or supply a comma-separated list to install into
several AI clients at once. Note that [Codex][codex] supports skills and
agents only — rules are not supported and are skipped with a warning.

```sh
grim install
grim install --client claude,copilot
```

## 4. Check the state

`grim status` reports each declared artifact as installed, outdated, locally
modified, or missing — the same model the [TUI][tui] paints in colour.

```sh
grim status
```

## 5. Upgrade {#5-upgrade}

When the publisher ships a newer version behind the same floating tag,
`grim update` re-resolves the tag, rolls the lock forward, and re-materializes
only what changed:

```sh
grim update            # everything
grim update code-review # one binding by name
```

## Undo

To take an artifact back out completely — files, install record, and config
entry — use [`grim uninstall`][uninstall]. To browse what a registry offers
before declaring anything, launch the interactive browser with
[`grim tui`][tui].

<!-- external -->
[oci]: https://github.com/opencontainers/distribution-spec
[claude]: https://docs.anthropic.com/en/docs/claude-code/overview
[opencode]: https://opencode.ai
[copilot]: https://github.com/features/copilot
[codex]: https://openai.com/index/openai-codex/

<!-- internal -->
[install]: ./installation.md
[uninstall]: ./commands.md#uninstall
[tui]: ./commands.md#tui
