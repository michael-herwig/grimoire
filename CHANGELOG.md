# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.6.2] - 2026-07-01

### Added

- Add grim config command for settings and registries *(config)*
- Accept multiple --registry values *(cli)*

### Fixed

- Group namespaced-registry rows under their configured root *(tui)*

## [0.6.1] - 2026-06-30

### Added

- Browse all configured registries (#16) *(tui)*
- Mark, warn on, and highlight deprecated packages *(deprecation)*
- Embed git provenance via opt-in --git *(publish)*
- Join single-child group chains in the browse tree (#19) *(tui)*
- Show a progress bar during install *(install)*
- Show a progress dialog during install/update/uninstall *(tui)*

### Fixed

- Detect updates via fresh tag discovery, not the cached catalog tag *(tui)*

### Release

- V0.6.1

## [0.6.0] - 2026-06-21

### Added

- Add grouped tree view with scrollable help overlay *(tui)*
- Show bundle members as virtual tree children *(tui)*
- Make [[registries]] the single source of truth for the default registry *(config)*
- Collapsible bundle nodes with per-member install *(tui)*
- Add via-bundle badge; key member actions by member name *(tui)*

### Changed

- Extract fetch_bundle_members seam from expand_bundles *(resolve)*

### Fixed

- TOML-escape the registry url written by grim init *(config)*
- Keep log output off the alternate screen and clarify the bundle-supersede note *(tui)*
- Mark a bundle installed when its members are installed directly *(tui)*
- Keep files when a declared bundle still provides the artifact *(uninstall)*
- Derive bundle row state from the declaration, not member installs *(tui)*
- Delete orphaned bundle members and refresh stale member badges *(tui)*
- Protect bundle-provided members and derive via-bundle from the snapshot *(tui)*

## [0.5.0] - 2026-06-19

### Added

- Multi-registry support, shared catalog core, and grim mcp server
- Add shell and PowerShell installers hosted on the docs site *(release)*
- Support repository_prefix / per-entry repository in publish.toml *(publish)*

### Documentation

- Document grim mcp and multi-registry config
- Lead the install page with ocx, then the install script
- Supersede artifact-type ADR with empty-config compatibility *(adr)*
- Document registry compatibility for catalog discovery
- Record that GitLab rejects the custom artifactType *(adr)*

### Fixed

- Reconcile install state against the active client set *(install)*
- Tolerate destroyed or malformed state when removing and syncing *(install)*
- Re-materialize all active clients on a partial-client version bump *(install)*
- Warn instead of failing on vendor-config sync after install/uninstall *(tui)*
- Use the OCI empty config media type so GitLab accepts manifests *(oci)*
- Drop the custom artifactType too — GitLab rejects it *(oci)*
- Apply swarm-review remediations across gitlab-registry-compat

### Release

- V0.5.0

## [0.4.3] - 2026-06-14

### Added

- Add `grim schema` to emit JSON Schemas for the TOML formats *(cli)*
- Portable anchor-relativized install state *(install)*

### Release

- V0.4.3

## [0.4.2] - 2026-06-13

### Added

- Manifest-driven batch release command *(publish)*
- Popup init dialog persisting default registry *(tui)*

### Fixed

- Checkout LFS logo so ocx describe publishes real PNG *(ci)*
- Reap empty OpenCode rules dir on last uninstall *(install)*
- Align table columns by chars, not bytes *(cli)*

### Release

- V0.4.2

## [0.4.1] - 2026-06-12

### Added

- Add first-party skills and starter bundle *(catalog)*
- Add validation and release tooling *(catalog)*
- Cross-link companion skills bidirectionally *(catalog)*
- Add install-by-identifier fallback to companion links *(catalog)*
- Selective publish flags and release-triggered publishing *(catalog)*
- Add --skip-existing for manifest-driven publishing *(release)*
- Skip published versions and start the 0.x line *(catalog)*
- Document the scripted-publishing pattern in grim-usage *(catalog)*
- Default to grim.ocx.sh when nothing is configured *(registry)*
- Snapshot the default registry into the seed config *(init)*
- Offer config init when the scope has no grimoire.toml *(tui)*

### Fixed

- Correct registry resolution precedence *(docs)*
- Render error chains once *(error)*
- Degrade to anonymous when the credential store fails *(oci)*
- Remove the lock sidecar on drop *(lock)*
- Open repository URLs on all platforms *(tui)*
- Build the same catalog window the TUI loads *(search)*
- Fall back to all clients when none are detected *(install)*
- Set DOCKER_CONFIG via GITHUB_ENV in publish-catalog *(ci)*

### Release

- Catalog
- V0.4.1

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

### Release

- V0.4.0

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
[0.6.2]: https://github.com/michael-herwig/grimoire/compare/v0.6.1..v0.6.2
[0.6.1]: https://github.com/michael-herwig/grimoire/compare/v0.6.0..v0.6.1
[0.6.0]: https://github.com/michael-herwig/grimoire/compare/v0.5.0..v0.6.0
[0.5.0]: https://github.com/michael-herwig/grimoire/compare/v0.4.3..v0.5.0
[0.4.3]: https://github.com/michael-herwig/grimoire/compare/v0.4.2..v0.4.3
[0.4.2]: https://github.com/michael-herwig/grimoire/compare/v0.4.1..v0.4.2
[0.4.1]: https://github.com/michael-herwig/grimoire/compare/v0.4.0..v0.4.1
[0.4.0]: https://github.com/michael-herwig/grimoire/compare/v0.3.0..v0.4.0
[0.3.0]: https://github.com/michael-herwig/grimoire/compare/v0.2.0..v0.3.0
[0.2.0]: https://github.com/michael-herwig/grimoire/compare/v0.1.0..v0.2.0
[0.1.0]: https://github.com/michael-herwig/grimoire/tree/v0.1.0

