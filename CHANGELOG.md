# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.4.0] - 2026-06-11

### Added

- Allow non-version tags without cascade *(release)*
- Infer artifact kind from manifest annotation *(oci)*
- Replace editor option with clients array *(config)* **BREAKING**
- Infer kind, optional name, honor default registry *(add)*
- Discriminate artifact kind by OCI artifactType *(oci)* **BREAKING**
- Support multi-file rules with a sibling support dir *(install)*
- In-file metadata with summary column and width-aware search *(catalog)*
- Render skills and rules per client with vendor env overrides *(install)*
- Shared multi-term matcher for CLI and TUI, tui --refresh *(search)*
- Detect installed clients and centralize registry precedence *(clients)*
- Flat kind-sorted list, shared search matcher, clients line *(tui)*
- Live background update checks for catalog and floating tags *(tui)*
- Add agent artifact kind with canonical frontmatter and packing *(oci)*
- Declare, hash, lock, and resolve agents *(config,lock,resolve)*
- Per-vendor agent materialization *(install)*
- Agent command surface and TUI parity *(cli,tui)*
- Cache bundle expansion and compute effective sets offline *(lock,resolve)*
- Authored repository metadata wins the source annotation *(oci)*
- Read back repository URL from the source annotation *(catalog)*
- Semantic detail pane with scrolling and o-to-open *(tui)*
- Page keys scroll the detail pane without focusing it *(tui)*
- Clamp detail scrolling at the content's end *(tui)*

### Changed

- Drop dead effective_default_registry helper *(command)*

### Documentation

- Update for clients array, new add CLI, non-version release tags
- Describe artifactType-based kind discrimination
- Document the rule support directory
- Align rig README, detection rule, env table, auth doc comments
- Document TUI declare/relock semantics
- Agent artifact reference and ADR
- Lock [[bundle]] cache and effective-declaration removal semantics
- Add artifact reference page
- Repository metadata key, source annotation, and TUI detail pane

### Fixed

- Resolve catalog under namespaced default registry *(catalog)*
- Use authorized catalog endpoint of oci dep *(oci)*
- Harden background update checks and catalog merge *(tui)*
- Release global registry tier, dedup helper, clients display *(command)*
- Longest-term prefilter and visible catalog truncation *(search)*
- Surface catalog truncation on the legend line *(tui)*
- Generation-key in-flight dedup and stamp catalog refreshes *(tui)*
- Extract shared declare/undeclare seams *(command)*
- Declare installs in grimoire.toml, flip outdated badge fast *(tui)*
- Install bundles as bundles, not skills *(tui)*
- Keep shared bundle members on bundle removal *(lock,resolve)*
- Pack [agents] members of an authored bundle *(build)*
- Mutate the lock via before/after effective sets *(remove,uninstall,tui)*
- Recompute all row states after a batch operation *(tui)*
- Hold the config flock on a sidecar, not the file itself *(lock)*

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

### Release

- V0.3.0

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
[0.4.0]: https://github.com/michael-herwig/grimoire/compare/v0.3.0..v0.4.0
[0.3.0]: https://github.com/michael-herwig/grimoire/compare/v0.2.0..v0.3.0
[0.2.0]: https://github.com/michael-herwig/grimoire/compare/v0.1.0..v0.2.0
[0.1.0]: https://github.com/michael-herwig/grimoire/tree/v0.1.0

