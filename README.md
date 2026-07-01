<div align="center">

<img src="./assets/logo.png" width="192" />

# grimoire

**An OCI-backed package manager for AI skills and rules**

[![CI][ci-badge]][ci]
[![Release][release-badge]][releases]
[![Docs][docs-badge]][docs]
[![License][license-badge]][license]

</div>

`grim` installs, maintains, and publishes AI agent skills and rules using OCI
registries (GHCR, Docker Hub, private registries) as storage — the same way
container images are distributed. It is a backend tool: a building block for
keeping agent configuration versioned, shareable, and reproducible.

> **Status:** provisional, pre-1.0. The core CLI — `init`, `add`, `lock`,
> `install`, `update`, `status`, `search`, `tui`, `build`, `release`,
> `remove`, `uninstall` — is implemented and shipping in
> [released binaries][releases]. The surface is still moving toward 1.0, so
> pin a version when you depend on it.

## Install

Grab a pre-built binary for macOS, Linux, or Windows (aarch64 or x86_64) from
the [latest release][releases], or build from source:

```sh
cargo install --git https://github.com/grimoire-rs/grimoire grimoire
```

## Quick Start

```sh
grim init --registry ghcr.io/acme                      # create grimoire.toml
grim add skill code-review ghcr.io/acme/code-review:1  # declare + lock
grim install                                           # materialize into your editor
grim tui                                               # browse the catalog
```

Full documentation: **[grimoire docs][docs]**.

## Development

See [CONTRIBUTING.md](CONTRIBUTING.md) for the full guide.

**Prerequisites:** [Rust](https://rustup.rs), [task](https://taskfile.dev),
[uv](https://docs.astral.sh/uv/) (for the Python acceptance suite).

## Community

- [Code of Conduct](CODE_OF_CONDUCT.md)
- [Security Policy](SECURITY.md)

## License

Grimoire is licensed under the [Apache License, Version 2.0][license].

<!-- badges -->
[ci]: https://github.com/grimoire-rs/grimoire/actions/workflows/verify-basic.yml
[ci-badge]: https://github.com/grimoire-rs/grimoire/actions/workflows/verify-basic.yml/badge.svg
[releases]: https://github.com/grimoire-rs/grimoire/releases
[release-badge]: https://img.shields.io/github/v/release/grimoire-rs/grimoire
[docs]: https://grimoire.rs/
[docs-badge]: https://img.shields.io/badge/docs-grimoire-blue
[license]: LICENSE
[license-badge]: https://img.shields.io/badge/license-Apache--2.0-blue.svg
