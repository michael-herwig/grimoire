<div align="center">

<img src="./assets/logo.svg" width="192" />

# grimoire

**An OCI-backed package manager for AI skills and rules**

[![CI][ci-badge]][ci]
[![License][license-badge]][license]

</div>

`grim` installs, maintains, and publishes AI agent skills and rules using OCI
registries (GHCR, Docker Hub, private registries) as storage — the same way
container images are distributed. It is a backend tool: a building block for
keeping agent configuration versioned, shareable, and reproducible.

> **Status:** early scaffold. The CLI surface (`add` / `install` / `publish` /
> `pull`) is not implemented yet — this repository currently provides the
> project skeleton, automation, and AI-assisted development setup.

## Quick Start

```sh
git clone https://github.com/michael-herwig/grimoire.git
cd grimoire
task              # check: fmt + clippy + cargo check
task verify       # full verification suite
cargo run -- --help
```

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
[ci]: https://github.com/michael-herwig/grimoire/actions/workflows/verify-basic.yml
[ci-badge]: https://github.com/michael-herwig/grimoire/actions/workflows/verify-basic.yml/badge.svg
[license]: LICENSE
[license-badge]: https://img.shields.io/badge/license-Apache--2.0-blue.svg
