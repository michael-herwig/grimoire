# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.3.0] - 2026-06-04

### Added

- Add login and logout commands *(auth)*
- Add OCI bundles with conflict policy and provenance *(bundles)*
- Prune lock-orphaned artifacts, preserving local edits *(update)*

### Changed

- Collapse access modes to online/offline *(access)* **BREAKING**

### Documentation

- Update project logo
- Document registry authentication, login and logout

## [0.2.0] - 2026-06-01

### Added

- Build musl archives and publish grim to ocx.sh *(release)*

### Documentation

- Refresh README for the v0.1.0 release
- Replace SVG logo with PNG

## [0.1.0] - 2026-06-01

### Added

- Domain core, errors, exit codes, output (phase 1)
- Config (global+project), lock, atomic store (phase 2)
- OCI access seam, cache, resolve (phase 3)
- Install, integrity, spine commands (phase 4)
- Skill standard, build, release/cascade, multi-editor transform (phase 5)
- Catalog + TUI (phase 6)
- Richer status states + color/icon polish *(tui)*
- Multi-select marks + batch install/update *(tui)*
- Uninstall seam + grim uninstall + TUI delete
- Runtime Global<->Project scope toggle *(tui)*
- Fixed-width columns, full colorization, ? help overlay *(tui)*
- Grouped tree view with version picker and UX polish *(tui)*

### Documentation

- Document multi-select/batch/scope/delete in manual rig *(tui)*
- Add mdBook documentation site
- Document registry resolution precedence

### Fixed

- Make release-update.sh executable; add rolling-release regression tests
- Contact loopback registries over plain HTTP on any port
[0.3.0]: https://github.com/michael-herwig/grimoire/compare/v0.2.0..v0.3.0
[0.2.0]: https://github.com/michael-herwig/grimoire/compare/v0.1.0..v0.2.0
[0.1.0]: https://github.com/michael-herwig/grimoire/tree/v0.1.0

