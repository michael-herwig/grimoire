// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! `grim publish` — manifest-driven batch release.
//!
//! Reads a `publish.toml` manifest describing a set of skills, rules,
//! agents, and bundles (each with a version and optional path override),
//! validates the whole manifest before any push, then releases each entry
//! in fixed kind order (skills → rules → agents → bundles, alphabetical
//! within kind) by composing [`super::release::run`] per entry.
//!
//! `--dry-run` validates and plans without pushing. `--force` moves
//! existing exact-version tags. Default behavior skips entries whose
//! exact-version tag already exists (`skip_existing`).

use std::collections::BTreeMap;
use std::path::PathBuf;

use clap::Args;
use schemars::JsonSchema;
use serde::Deserialize;

use crate::api::publish_report::{PublishEntry, PublishReport, PublishStatus};
use crate::cli::exit_code::ExitCode;
use crate::context::Context;
use crate::error::classify_error;
use crate::oci::ArtifactKind;
use crate::oci::identifier::{MAX_REPOSITORY_LENGTH, RepositoryPathIssue, repository_path_issue};

/// `grim publish` arguments.
#[derive(Debug, Args)]
pub struct PublishArgs {
    /// Path to the publish manifest (default: `./publish.toml`).
    #[arg(long, value_name = "PATH", default_value = "./publish.toml")]
    pub manifest: PathBuf,

    /// Publish only the named entry (repeatable; any name not in the
    /// manifest is a data error).
    #[arg(long, value_name = "NAME")]
    pub only: Vec<String>,

    /// Override the published tag with a movable channel tag (e.g.
    /// `canary`). Must be non-semver — semver values are rejected so that
    /// semver releases always come from the manifest and the repo records
    /// exactly what was published. A channel tag always moves: re-publishing
    /// with --tag overwrites the existing tag (no skip-existing, no --force
    /// needed).
    #[arg(long, value_name = "TAG", conflicts_with = "force")]
    pub tag: Option<String>,

    /// Print the push plan without pushing anything.
    #[arg(long)]
    pub dry_run: bool,

    /// Move existing exact-version tags that point at a different digest
    /// (default: skip entries whose exact-version tag already exists).
    #[arg(long, conflicts_with = "tag")]
    pub force: bool,

    /// Embed git provenance (commit revision, commit date, and the `origin`
    /// remote) as OCI annotations on every published entry. Forwarded to each
    /// `release`; requires `git` and a repository (a non-git path fails, 65).
    #[arg(long)]
    pub git: bool,

    /// After a fully successful publish, announce the published packages to
    /// a package-index git repository: write metadata pointers on a topic
    /// branch and open a pull request (github.com + `gh`) or push the
    /// branch (any other git host). No-op under `--dry-run`.
    #[arg(long)]
    pub announce: bool,

    /// The index git repository to announce to. Overrides the manifest's
    /// `[announce] repository`; default: `https://github.com/grimoire-rs/index`.
    #[arg(long, value_name = "REPO_URL", requires = "announce")]
    pub announce_repo: Option<String>,
}

/// The optional `[announce]` table in `publish.toml`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AnnounceSpec {
    /// The index git repository announcements target (https clone URL).
    /// Default: `https://github.com/grimoire-rs/index`.
    pub repository: Option<String>,

    /// The `index/github.com/<namespace>/` the packages land under.
    /// Default: the authenticated `gh` user's login.
    pub namespace: Option<String>,

    /// The namespace's numeric GitHub account id. Default: resolved live
    /// from the GitHub API. Set it explicitly for hermetic/offline runs
    /// against a custom index repository.
    pub owner_id: Option<u64>,
}

/// A single entry in a kind table (`[skills.name]`, `[rules.name]`,
/// `[agents.name]`, `[bundles.name]`).
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PublishEntrySpec {
    /// Strict semantic version (`X.Y.Z`). Required.
    pub version: String,

    /// Source path override. When absent, the conventional path relative
    /// to the manifest directory is used:
    /// `skills/{name}/`, `rules/{name}.md`, `agents/{name}.md`,
    /// `bundles/{name}.toml`.
    pub path: Option<PathBuf>,

    /// Full OCI repository path override (registry-relative, no tag), e.g.
    /// `durzn-technology/hearth/skill/hearth`. When present the entry name is
    /// NOT appended — the path is used verbatim (mirrors `grim release`). Wins
    /// over the manifest `repository_prefix` and the conventional
    /// `{kind-subdir}/{name}` default, letting an entry target a registry's
    /// group/project nesting (e.g. GitLab).
    pub repository: Option<String>,

    /// For bundle entries only: freeze every floating member tag to a
    /// digest in the published bundle (reproducible, tunnel-safe).
    /// A `pin = true` on a non-bundle entry is a data error (exit 65).
    #[serde(default)]
    pub pin: bool,
}

/// The deserialized content of a `publish.toml` manifest.
///
/// Top-level `registry` is required. Each kind table holds
/// `name = { version, [path], [pin] }` sub-tables.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PublishManifest {
    /// The OCI registry host to publish to (e.g. `grim.ocx.sh`). May be
    /// overridden by the `--registry` flag.
    pub registry: String,

    /// Optional repository path prefix applied to every entry that does not
    /// set its own `repository`: the published repository becomes
    /// `{repository_prefix}/{name}` (the prefix replaces the conventional
    /// `{kind-subdir}` segment). Registry-relative, no tag. Lets a whole
    /// manifest publish under a registry's group/project nesting (e.g.
    /// `durzn-technology/hearth/skill` on GitLab).
    pub repository_prefix: Option<String>,

    /// Skill entries, keyed by name.
    #[serde(default)]
    pub skills: BTreeMap<String, PublishEntrySpec>,

    /// Rule entries, keyed by name.
    #[serde(default)]
    pub rules: BTreeMap<String, PublishEntrySpec>,

    /// Agent entries, keyed by name.
    #[serde(default)]
    pub agents: BTreeMap<String, PublishEntrySpec>,

    /// Bundle entries, keyed by name.
    #[serde(default)]
    pub bundles: BTreeMap<String, PublishEntrySpec>,

    /// Announcement defaults for `--announce` (target index repository,
    /// namespace, owner id).
    #[serde(default)]
    pub announce: Option<AnnounceSpec>,
}

/// One planned publish operation, ready to be handed to `release::run`.
#[derive(Debug)]
pub(crate) struct PlannedEntry {
    /// The artifact kind.
    pub kind: crate::oci::ArtifactKind,
    /// The entry name (key in the manifest table).
    pub name: String,
    /// The resolved source path.
    pub path: PathBuf,
    /// The full OCI reference (`registry/namespace/name:tag-or-version`).
    pub reference: String,
    /// Whether to pin bundle members.
    pub pin: bool,
    /// True when a per-entry `repository` override was used verbatim and its
    /// last path segment is **not** the entry name — i.e. the name was not
    /// appended. Drives a `--dry-run`-only preview hint so a user who expected
    /// `repository_prefix` append-semantics notices the difference.
    pub name_not_appended: bool,
}

/// Strict `X.Y.Z` semver check: no prerelease, no build metadata, no
/// v-prefix, no leading zeros in any component.
///
/// Uses `semver::Version::parse` (already a crate dependency via OCI
/// resolution) so that inputs like "01.0.0" are rejected — the hand-rolled
/// digit check accepted leading zeros, which would silently produce cascade
/// tags like `01.0` and `01` that could collide or mislead.  Requiring
/// `pre.is_empty() && build.is_empty()` additionally rejects "1.0.0-beta"
/// and "1.0.0+meta", matching the strict `^\d+\.\d+\.\d+$`
/// intent but tighter where it prevents registry breakage.
fn is_strict_semver(s: &str) -> bool {
    match semver::Version::parse(s) {
        Ok(v) => v.pre.is_empty() && v.build.is_empty(),
        Err(_) => false,
    }
}

/// Emit a DataError (65) attributed to `path` via the
/// `SkillError::ValidationFailed` variant so `classify_error` maps it to
/// [`ExitCode::DataError`] and the formatted message reads cleanly as
/// `{path}: {msg}` with no extraneous prefix text.
fn data_error_at(path: &std::path::Path, msg: impl Into<String>) -> anyhow::Error {
    anyhow::Error::from(crate::error::Error::from(crate::skill::SkillError::new(
        path,
        crate::skill::SkillErrorKind::ValidationFailed(msg.into()),
    )))
}

/// Charset gate for manifest entry names (CWE-20): names become both a
/// filesystem path segment (`skills/{name}`) and an OCI repository
/// segment (`registry/skills/{name}:tag`), so reject anything outside
/// the OCI repository-segment alphabet up front — at manifest
/// validation time, where the error is cleanly attributed — instead of
/// letting a crafted name (`../evil`, `sub/name`, uppercase) surface as
/// a confusing runtime error deep in the release path.
fn validate_entry_name(name: &str, manifest_path: &std::path::Path) -> anyhow::Result<()> {
    let mut chars = name.chars();
    let head_ok = chars
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
    let tail_ok = chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_' | '.'));
    if !(head_ok && tail_ok) {
        return Err(data_error_at(
            manifest_path,
            format!("entry '{name}': name must start with [a-z0-9] and contain only [a-z0-9._-]"),
        ));
    }
    Ok(())
}

/// Structural gate for the resolved registry value (manifest `registry`
/// or `--registry` override): an empty value or one carrying a path
/// separator would compose a malformed or surprising OCI reference that
/// only fails deep in the release path — reject it here with a clear
/// message instead.
fn validate_registry_value(registry: &str, manifest_path: &std::path::Path) -> anyhow::Result<()> {
    if registry.is_empty() || registry.contains('/') {
        return Err(data_error_at(
            manifest_path,
            format!("registry '{registry}': must be a plain registry host (e.g. 'grim.ocx.sh' or 'localhost:5000')"),
        ));
    }
    Ok(())
}

/// Charset/structural gate for an authored OCI repository path
/// (`repository_prefix` or a per-entry `repository`). The value becomes part
/// of the pushed `registry/repo:tag` reference, so reject anything that would
/// compose a malformed reference up front, where the error is cleanly
/// attributed to the manifest (mirrors [`validate_entry_name`] /
/// [`validate_registry_value`]).
///
/// Delegates the path alphabet to the canonical [`repository_path_issue`]
/// gate in the `oci` layer (single source of truth, shared with the
/// distribution-spec name grammar) and renders the manifest-attributed
/// message. Rejects an empty value, an embedded `:` (which would smuggle a tag
/// into the reference), a leading/trailing `/`, an empty `//` segment, a
/// segment violating the OCI path-component grammar (`.`/`..`, uppercase,
/// leading/trailing/doubled separators, foreign characters), and a path longer
/// than [`MAX_REPOSITORY_LENGTH`]. `field` is the human label for the message
/// (`"repository_prefix"` or `"entry '<name>': repository"`).
fn validate_repository_path(value: &str, field: &str, manifest_path: &std::path::Path) -> anyhow::Result<()> {
    let Some(issue) = repository_path_issue(value) else {
        return Ok(());
    };
    let detail = match issue {
        RepositoryPathIssue::Empty => format!("{field}: must not be empty"),
        RepositoryPathIssue::ContainsColon => {
            format!("{field} '{value}': must not contain ':' (the tag comes from the entry version)")
        }
        RepositoryPathIssue::LeadingOrTrailingSlash => {
            format!("{field} '{value}': must not start or end with '/'")
        }
        RepositoryPathIssue::EmptySegment => {
            format!("{field} '{value}': must not contain an empty '//' path segment")
        }
        RepositoryPathIssue::SegmentGrammar => format!(
            "{field} '{value}': each path segment must match the OCI name grammar — \
             [a-z0-9] runs joined by '.', '_', '__', or '-', with no leading, trailing, or doubled separator"
        ),
        RepositoryPathIssue::TooLong => {
            format!("{field} '{value}': repository path must be at most {MAX_REPOSITORY_LENGTH} characters")
        }
    };
    Err(data_error_at(manifest_path, detail))
}

/// Run `grim publish`.
///
/// Reads and validates the manifest, then releases each entry in kind
/// order (skills → rules → agents → bundles, alpha within kind).
/// Fail-fast: the first failing entry stops the batch. The report
/// contains all completed entries plus the failed one.
///
/// # Errors
///
/// Manifest parse failures (65), validation errors (65), and release
/// errors propagate via the typed error chain.
pub async fn run(ctx: &Context, args: &PublishArgs) -> anyhow::Result<(PublishReport, ExitCode)> {
    // Flow (ADR D1–D5): load_manifest → resolve_publish_registry →
    // validate_manifest → plan_entries → per entry compose release::run
    // (skip_existing = !force), fail-fast into the report.

    let manifest = load_manifest(&args.manifest)?;
    let registry = resolve_publish_registry(ctx, &manifest.registry);
    validate_registry_value(&registry, &args.manifest)?;

    let manifest_dir = args
        .manifest
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."))
        .to_path_buf();
    // Resolve to absolute path so relative parent paths work correctly.
    let manifest_dir = if manifest_dir.as_os_str().is_empty() {
        std::path::PathBuf::from(".")
    } else {
        manifest_dir
    };

    validate_manifest(
        &manifest,
        &manifest_dir,
        &args.manifest,
        &args.only,
        args.tag.as_deref(),
    )?;

    let entries = plan_entries(&manifest, &manifest_dir, &registry, &args.only, args.tag.as_deref());

    // Dry-run preview only: flag entries whose per-entry `repository` is used
    // verbatim and does not end in the entry name (the name was not appended).
    // Surfaced here — not as a publish-time warning — so a real publish of a
    // deliberately-renamed repository stays quiet while a `--dry-run` preview
    // still catches a user who expected `repository_prefix` append-semantics.
    if args.dry_run {
        for planned in &entries {
            if planned.name_not_appended {
                tracing::info!(
                    "entry '{}': repository '{}' is used verbatim — the entry name is not appended (use repository_prefix to append the name)",
                    planned.name,
                    planned.reference,
                );
            }
        }
    }

    let mut report_entries: Vec<PublishEntry> = Vec::new();

    // A channel tag (`--tag canary`) is movable by definition: skip-existing
    // would freeze it at its first digest forever, so a tag run always
    // moves the tag (release's exact-tag overwrite guard is force-waived
    // for the channel tag only — manifest semver tags are not in play).
    let (force, skip_existing) = resolve_force_skip(args.tag.as_deref(), args.force);

    for planned in &entries {
        let release_args = super::release::ReleaseArgs {
            path: planned.path.clone(),
            reference: planned.reference.clone(),
            kind: Some(kind_str(planned.kind).to_string()),
            dry_run: args.dry_run,
            force,
            skip_existing,
            pin: planned.pin,
            git: args.git,
        };

        match super::release::run(ctx, &release_args).await {
            Ok((report, _exit)) => {
                // Status from the report data, not from the flags we sent
                // (subsystem-cli-api: report actual results). Release's
                // skip-existing branch runs before its dry-run branch, so
                // an already-published entry under --dry-run reports as
                // Skipped (honest: a real run would skip it too). The two
                // unpushed shapes are distinguishable: the skip path
                // reports no tags, the dry-run path reports the planned
                // tag set (never empty).
                let status = if report.pushed {
                    PublishStatus::Pushed
                } else if report.tags.is_empty() {
                    PublishStatus::Skipped
                } else {
                    PublishStatus::DryRun
                };
                let entry = publish_entry_from_release(planned, &report, status);
                report_entries.push(entry);
            }
            Err(err) => {
                // Fail-fast: print the full error chain to stderr so the
                // caller can diagnose mid-batch failures even when consuming
                // the structured report (ADR D4). Same "{err:#}" chain-walk
                // format as main.rs, plus an "error:" prefix here because
                // this path returns Ok(partial report) — main.rs never sees
                // the error, so the prefix marks the line for log scanners.
                eprintln!("error: {err:#}");
                let failed_entry = PublishEntry {
                    reference: planned.reference.clone(),
                    kind: planned.kind,
                    digest: None,
                    tags: Vec::new(),
                    status: PublishStatus::Failed,
                };
                report_entries.push(failed_entry);
                let code = classify_error(&err);
                let report = PublishReport::new(report_entries);
                return Ok((report, code));
            }
        }
    }

    // Announce only after a fully successful, non-dry-run publish: every
    // planned entry is now live on the registry (freshly pushed or already
    // present via skip-existing).
    if args.announce {
        if args.dry_run {
            eprintln!("announce: skipped (dry run)");
        } else if let Err(err) = run_announce(ctx, args, &manifest, &entries).await {
            // The publish itself succeeded — keep the report, surface the
            // announce failure on stderr, and exit Unavailable (69): the
            // index repository is a remote resource the announce needs.
            eprintln!("error: announce failed: {err:#}");
            return Ok((PublishReport::new(report_entries), ExitCode::Unavailable));
        }
    }

    Ok((PublishReport::new(report_entries), ExitCode::Success))
}

/// Execute the `--announce` step: derive one metadata pointer per planned
/// entry (description read back from the just-published manifest via the
/// access seam), then hand the set to
/// [`crate::catalog::index_announce::announce`].
async fn run_announce(
    ctx: &Context,
    args: &PublishArgs,
    manifest: &PublishManifest,
    entries: &[PlannedEntry],
) -> anyhow::Result<()> {
    use crate::catalog::index_announce::{
        AnnounceOutcome, AnnouncePackage, AnnounceRequest, DEFAULT_INDEX_REPO, announce, gh_login,
    };

    let spec = manifest.announce.as_ref();
    let repo_url = args
        .announce_repo
        .clone()
        .or_else(|| spec.and_then(|s| s.repository.clone()))
        .unwrap_or_else(|| DEFAULT_INDEX_REPO.to_string());
    let namespace = match spec.and_then(|s| s.namespace.clone()) {
        Some(ns) => ns,
        None => gh_login().await.ok_or_else(|| {
            super::config_usage(
                "no announce namespace: set `[announce] namespace` in publish.toml \
                 or authenticate the `gh` CLI",
            )
        })?,
    };

    let access = super::access_seam(ctx)?;
    let mut packages = Vec::with_capacity(entries.len());
    for planned in entries {
        let id = crate::oci::Identifier::parse(&planned.reference)
            .map_err(|e| anyhow::Error::from(crate::error::Error::from(e)))?;
        let reference = format!("{}/{}", id.registry(), id.repository());
        let (description, repository_url) =
            crate::catalog::index_announce::pointer_metadata(access.as_ref(), &id).await;
        packages.push(AnnouncePackage {
            name: planned.name.clone(),
            kind: kind_str(planned.kind).to_string(),
            reference,
            // The index spec requires a description; grim-published
            // artifacts always carry the annotation, but degrade honestly
            // for foreign/unreachable manifests.
            description: description.unwrap_or_else(|| format!("grimoire {} {}", kind_str(planned.kind), planned.name)),
            repository_url,
        });
    }

    let request = AnnounceRequest {
        repo_url: repo_url.clone(),
        namespace,
        owner_id: spec.and_then(|s| s.owner_id),
        packages,
    };
    match super::grim(announce(&request).await)? {
        AnnounceOutcome::PullRequest { url } => eprintln!("announced: {url}"),
        AnnounceOutcome::BranchPushed { branch } => eprintln!(
            "announced: pushed branch '{branch}' to {repo_url} — open the merge request to publish the pointers"
        ),
        AnnounceOutcome::UpToDate => eprintln!("announce: index already up to date"),
    }
    Ok(())
}

/// Return the singular kind string for constructing `--kind` flag value.
fn kind_str(kind: ArtifactKind) -> &'static str {
    match kind {
        ArtifactKind::Skill => "skill",
        ArtifactKind::Rule => "rule",
        ArtifactKind::Agent => "agent",
        ArtifactKind::Bundle => "bundle",
    }
}

/// Derive the `(force, skip_existing)` pair for a publish run.
///
/// Three cases:
/// - `tag = Some(_)` — a channel tag always moves: force=true,
///   skip_existing=false (ADR D3 amendment).
/// - `tag = None, force = true` — explicit flag: force=true,
///   skip_existing=false (flags are mutually exclusive).
/// - `tag = None, force = false` — default: force=false,
///   skip_existing=true (idempotent CI default, ADR D3).
pub(crate) fn resolve_force_skip(tag: Option<&str>, force: bool) -> (bool, bool) {
    if tag.is_some() { (true, false) } else { (force, !force) }
}

/// Resolve the registry to publish to: the `--registry` global flag wins
/// over the manifest's required `registry` value (ADR D1). The env /
/// config tiers of the usual precedence chain do **not** apply here —
/// the manifest value is an explicit input, like a fully-qualified
/// reference passed to `grim release`.
fn resolve_publish_registry(ctx: &Context, manifest_registry: &str) -> String {
    // Only the flag tier (not env, not config) overrides the manifest's
    // explicit registry value. Mirror how release.rs reads just the flag tier.
    ctx.registry_flag().unwrap_or(manifest_registry).to_string()
}

/// Load and deserialize the publish manifest from `path`, enforcing the
/// 64 KiB cap via `config::read_capped`.
///
/// # Errors
///
/// Returns a data error (65) when the file cannot be read or the TOML
/// is invalid. A bundle-shaped file (kind tables holding reference
/// strings instead of sub-tables) must NOT surface the raw serde error:
/// the D7 guard detects the shape and errors with a hint toward
/// `grim release --kind bundle` (mirror of the `read_bundle_members`
/// guard in `build.rs`).
fn load_manifest(path: &std::path::Path) -> anyhow::Result<PublishManifest> {
    // Read with 64 KiB cap. ConfigError's Display already embeds the path
    // ("{path}: {kind}"), so we must NOT pass e.to_string() as the msg into
    // data_error_at — that would produce "{path}: {path}: …" double-path.
    // Instead inspect the ConfigError kind and produce a single-path message
    // via data_error_at(path, msg) where msg contains NO path. All three
    // branches route to DataError (65) so that acceptance tests get a
    // consistent exit code regardless of whether the manifest is missing,
    // oversized, or unreadable (documented normalisation: callers expect 65
    // for all manifest-load failures, not 74/78).
    let content = crate::config::read_capped(path).map_err(|e| {
        use crate::config::ConfigErrorKind;
        let msg = match &e.kind {
            ConfigErrorKind::Io(io_err) if io_err.kind() == std::io::ErrorKind::NotFound => {
                "manifest not found".to_string()
            }
            ConfigErrorKind::FileTooLarge { size: _, limit } => {
                format!("manifest exceeds the {limit}-byte (64 KiB) size limit")
            }
            ConfigErrorKind::Io(io_err) => {
                // Use the source cause, not the ConfigError Display, to avoid
                // the "{path}: I/O error: {cause}" → "{path}: {path}: …" double.
                format!("cannot read manifest: {io_err}")
            }
            // Format the kind, not the whole ConfigError — its Display
            // embeds the path, which data_error_at prepends again.
            _ => format!("cannot read manifest: {}", e.kind),
        };
        crate::error::Error::from(crate::skill::SkillError::new(
            path,
            crate::skill::SkillErrorKind::ValidationFailed(msg),
        ))
    })?;

    // D7 guard: if the file is bundle-shaped (kind tables with string values,
    // no `registry` key at the top level), `toml::from_str` into
    // `PublishManifest` will fail with a cryptic serde/TOML type mismatch.
    // Detect this BEFORE the parse to emit a friendly hint.
    if is_bundle_shaped(&content) {
        return Err(data_error_at(
            path,
            "this looks like a bundle source file, not a publish manifest; \
             use `grim release --kind bundle` to publish a bundle directly",
        ));
    }

    toml::from_str::<PublishManifest>(&content).map_err(|e| {
        // Check if this looks like a bundle-shaped file that slipped past the
        // cheap pre-parse guard (e.g. has `registry` key but also has string
        // kind values). serde/TOML signals this as a type mismatch; the exact
        // phrase varies by toml crate version:
        //   "expected a map"          — older TOML error text
        //   "expected table"          — some serde_toml variants
        //   "expected struct"         — toml 0.8 "invalid type: string, expected struct"
        // Match all known phrases so bundle-shape detection is robust.
        let msg = e.to_string();
        if msg.contains("expected a map") || msg.contains("expected table") || msg.contains("expected struct") {
            // Hint toward grim release --kind bundle (ADR D7).
            data_error_at(
                path,
                "this looks like a bundle source file, not a publish manifest; \
                 use `grim release --kind bundle` to publish a bundle directly",
            )
        } else {
            data_error_at(path, format!("invalid manifest: {e}"))
        }
    })
}

/// Cheap structural check: does this TOML document look like a bundle
/// source file (kind tables with flat string values, no `registry` key)?
///
/// Parses as `toml::Value` so the check is O(file size) but allocation-
/// cheap. Used in two places:
/// 1. `load_manifest`: detect bundle-shaped file BEFORE the full parse.
/// 2. `read_bundle_members` (build.rs): detect publish-manifest-shaped file.
///
/// Returns `true` when the document has at least one kind table (`[skills]`,
/// `[rules]`, `[agents]`, `[bundles]`) whose values are strings rather than
/// sub-tables — the structural hallmark of a bundle source file.
fn is_bundle_shaped(content: &str) -> bool {
    let Ok(val) = toml::from_str::<toml::Value>(content) else {
        return false;
    };
    // A publish manifest always has a top-level `registry` string key.
    // A bundle source file does not.
    let has_registry = val.get("registry").is_some_and(|v| v.as_str().is_some());

    if has_registry {
        // Could be a publish manifest; not bundle-shaped.
        return false;
    }

    // Check if any kind table holds string values (bundle shape).
    for kind_key in &["skills", "rules", "agents", "bundles"] {
        if let Some(toml::Value::Table(t)) = val.get(*kind_key)
            && t.values().any(|v| v.is_str())
        {
            return true;
        }
    }
    false
}

/// Validate the whole manifest and the CLI flags before any push.
///
/// Checks performed (all must pass; fail before side effects):
/// - Every `version` is strict semver (`X.Y.Z`).
/// - Every `path` override (or conventional path) exists on disk.
/// - `pin = true` only on bundle entries.
/// - Every `--only` name appears in the manifest.
/// - `--tag` is non-semver.
///
/// # Errors
///
/// Returns a data error (65) for the first violation found.
fn validate_manifest(
    manifest: &PublishManifest,
    manifest_dir: &std::path::Path,
    manifest_path: &std::path::Path,
    only: &[String],
    tag: Option<&str>,
) -> anyhow::Result<()> {
    // -- Guard: manifest must declare at least one entry (ADR D2) --
    // An entirely empty manifest (all kind tables absent or empty) is a
    // data error: the caller almost certainly provided the wrong file.
    // Note: --only filtering cannot produce this condition (unknown names
    // already error before this point).
    let total_entries = manifest.skills.len() + manifest.rules.len() + manifest.agents.len() + manifest.bundles.len();
    if total_entries == 0 {
        return Err(data_error_at(manifest_path, "no packages declared in manifest"));
    }

    // -- Validate --tag is non-semver (ADR D1) --
    if let Some(t) = tag
        && is_strict_semver(t)
    {
        return Err(data_error_at(
            manifest_path,
            format!(
                "--tag '{t}' is a semver version; semver releases must come from the manifest version field, \
                 not --tag (use a movable channel tag like 'canary' or 'edge')"
            ),
        ));
    }

    // -- Validate --only names exist in the manifest (ADR D1) --
    // Collect all known entry names across all kinds for O(n) lookup.
    let all_names: std::collections::HashSet<&str> = manifest
        .skills
        .keys()
        .chain(manifest.rules.keys())
        .chain(manifest.agents.keys())
        .chain(manifest.bundles.keys())
        .map(String::as_str)
        .collect();

    for name in only {
        if !all_names.contains(name.as_str()) {
            return Err(data_error_at(
                manifest_path,
                format!(
                    "--only '{name}': name not found in manifest; \
                     known entries: {}",
                    {
                        let mut names: Vec<&str> = all_names.iter().copied().collect();
                        names.sort_unstable();
                        names.join(", ")
                    }
                ),
            ));
        }
    }

    // -- Validate the manifest-level repository_prefix (axis B) --
    // Charset/shape gate before it composes any reference; per-entry
    // `repository` overrides are validated inside `validate_entry`.
    if let Some(prefix) = &manifest.repository_prefix {
        validate_repository_path(prefix, "repository_prefix", manifest_path)?;
    }

    // -- Validate per-kind entries --
    for (name, spec) in &manifest.skills {
        validate_entry(name, spec, ArtifactKind::Skill, manifest_dir, manifest_path)?;
    }
    for (name, spec) in &manifest.rules {
        validate_entry(name, spec, ArtifactKind::Rule, manifest_dir, manifest_path)?;
    }
    for (name, spec) in &manifest.agents {
        validate_entry(name, spec, ArtifactKind::Agent, manifest_dir, manifest_path)?;
    }
    // Bundles accept pin=true; validate_entry skips the pin check for Bundle kind.
    for (name, spec) in &manifest.bundles {
        validate_entry(name, spec, ArtifactKind::Bundle, manifest_dir, manifest_path)?;
    }

    Ok(())
}

/// Validate a single manifest entry.
///
/// Checks: name charset (CWE-20), strict semver version (ADR D2), source
/// path exists (ADR D2), and — for non-bundle kinds — that `pin = true`
/// is absent (pin is bundle-only, ADR D2).
///
/// `kind` determines which check applies: bundle entries may carry `pin`;
/// all other kinds reject it. Skill/rule/agent messages are byte-identical
/// to the former `validate_entry`; bundle entries now share the longer
/// semver message (adds the prerelease/v-prefix hint the old
/// `validate_bundle_entry` lacked — no test asserted the shorter text).
fn validate_entry(
    name: &str,
    spec: &PublishEntrySpec,
    kind: ArtifactKind,
    manifest_dir: &std::path::Path,
    manifest_path: &std::path::Path,
) -> anyhow::Result<()> {
    // Name charset gate (CWE-20) before the name reaches a path join or
    // a reference string.
    validate_entry_name(name, manifest_path)?;

    // Per-entry repository override charset/shape gate (axis B): a full
    // repository path used verbatim in the pushed reference.
    if let Some(repo) = &spec.repository {
        validate_repository_path(repo, &format!("entry '{name}': repository"), manifest_path)?;
    }

    // Strict semver version (ADR D2). The bundle variant previously used a
    // shorter message; unify to the full message for all kinds.
    if !is_strict_semver(&spec.version) {
        return Err(data_error_at(
            manifest_path,
            format!(
                "entry '{}': version '{}' is not strict semver (X.Y.Z required); \
                 prerelease markers and v-prefixes are not allowed in manifest versions",
                name, spec.version
            ),
        ));
    }

    // pin=true rejected for non-bundle entries only (ADR D2).
    if spec.pin && kind != ArtifactKind::Bundle {
        return Err(data_error_at(
            manifest_path,
            format!(
                "entry '{}': pin=true is only valid on bundle entries (not {})",
                name,
                kind_str(kind)
            ),
        ));
    }

    // Source path must exist (ADR D2).
    let src = resolve_source_path(name, kind, spec, manifest_dir);
    if !src.exists() {
        return Err(data_error_at(
            manifest_path,
            format!("entry '{}': source path '{}' does not exist", name, src.display()),
        ));
    }

    Ok(())
}

/// Resolve the source path for an entry: use the explicit path override if
/// present (relative to manifest dir), otherwise the convention:
/// `skills/{name}/`, `rules/{name}.md`, `agents/{name}.md`,
/// `bundles/{name}.toml` (ADR D2).
fn resolve_source_path(
    name: &str,
    kind: ArtifactKind,
    spec: &PublishEntrySpec,
    manifest_dir: &std::path::Path,
) -> PathBuf {
    if let Some(ref override_path) = spec.path {
        manifest_dir.join(override_path)
    } else {
        conventional_source_path(name, kind, manifest_dir)
    }
}

/// Resolve the OCI repository path (registry-relative, no tag) for an entry.
///
/// Precedence (highest first):
/// 1. `spec.repository` — a full repository path; the entry name is NOT
///    appended (mirrors `grim release`, which takes the repository verbatim).
/// 2. manifest `repository_prefix` → `{prefix}/{name}` (the prefix replaces
///    the conventional `{kind-subdir}` segment).
/// 3. default → `{kind.subdir()}/{name}` (today's behavior, unchanged when
///    neither override is set).
///
/// Both override values are charset-validated up front by
/// [`validate_repository_path`]; `name` is gated by [`validate_entry_name`].
fn entry_repository(name: &str, kind: ArtifactKind, spec: &PublishEntrySpec, prefix: Option<&str>) -> String {
    if let Some(repo) = spec.repository.as_deref() {
        repo.to_string()
    } else if let Some(prefix) = prefix {
        format!("{prefix}/{name}")
    } else {
        format!("{}/{name}", kind.subdir())
    }
}

/// Compute the conventional source path relative to the manifest directory.
fn conventional_source_path(name: &str, kind: ArtifactKind, manifest_dir: &std::path::Path) -> PathBuf {
    match kind {
        ArtifactKind::Skill => manifest_dir.join("skills").join(name),
        ArtifactKind::Rule => manifest_dir.join("rules").join(format!("{name}.md")),
        ArtifactKind::Agent => manifest_dir.join("agents").join(format!("{name}.md")),
        ArtifactKind::Bundle => manifest_dir.join("bundles").join(format!("{name}.toml")),
    }
}

/// Build the ordered list of entries to publish from a validated manifest.
///
/// Order: skills → rules → agents → bundles, alphabetical within each
/// kind. When `--only` is non-empty only matching entries are included.
/// When `--tag` is set it replaces the version tag for every entry.
/// The `registry` parameter (already resolved against `--registry` flag
/// precedence) is used to construct fully-qualified OCI references.
fn plan_entries(
    manifest: &PublishManifest,
    manifest_dir: &std::path::Path,
    registry: &str,
    only: &[String],
    tag: Option<&str>,
) -> Vec<PlannedEntry> {
    let mut entries = Vec::new();

    // BTreeMap iteration is already alphabetical, so each block gives
    // alpha within kind. The block order gives the fixed kind ordering.
    let only_set: std::collections::HashSet<&str> = only.iter().map(String::as_str).collect();

    macro_rules! add_kind {
        ($table:expr, $kind:expr) => {
            for (name, spec) in &$table {
                if !only_set.is_empty() && !only_set.contains(name.as_str()) {
                    continue;
                }
                let src = resolve_source_path(name, $kind, spec, manifest_dir);
                let publish_tag = tag.unwrap_or(&spec.version);
                let repo = entry_repository(name, $kind, spec, manifest.repository_prefix.as_deref());
                let reference = format!("{registry}/{repo}:{publish_tag}");
                // Only a verbatim per-entry `repository` can drop the name; the
                // prefix and default branches always append it.
                let name_not_appended = spec
                    .repository
                    .as_deref()
                    .is_some_and(|r| r.rsplit('/').next() != Some(name.as_str()));
                entries.push(PlannedEntry {
                    kind: $kind,
                    name: name.clone(),
                    path: src,
                    reference,
                    pin: spec.pin,
                    name_not_appended,
                });
            }
        };
    }

    // Order is a correctness assumption: bundle members (skills/rules/agents)
    // publish before bundles so a bundle manifest can reference already-pushed
    // members. A future bundle-of-bundles would require a topological sort
    // instead of this fixed order — see ADR D4.
    add_kind!(manifest.skills, ArtifactKind::Skill);
    add_kind!(manifest.rules, ArtifactKind::Rule);
    add_kind!(manifest.agents, ArtifactKind::Agent);
    add_kind!(manifest.bundles, ArtifactKind::Bundle);

    entries
}

/// Convert a [`crate::api::release_report::ReleaseReport`] and the
/// [`PlannedEntry`] it came from into a [`PublishEntry`] for the batch
/// report.
///
/// `digest` is always populated from the release report's manifest
/// digest (pushed, skipped, and dry-run outcomes all carry one).
/// `digest: None` is reserved for `Failed` entries, which have no
/// release report and are constructed directly in the batch loop.
fn publish_entry_from_release(
    planned: &PlannedEntry,
    report: &crate::api::release_report::ReleaseReport,
    status: PublishStatus,
) -> PublishEntry {
    PublishEntry {
        reference: planned.reference.clone(),
        kind: planned.kind,
        digest: Some(report.manifest_digest.clone()),
        tags: report.tags.clone(),
        status,
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::cli::options::{GlobalOptions, OutputFormat};
    use crate::context::Context;

    // ── serde / declarative tests (7 pre-existing + new) ──────────────────

    #[test]
    fn manifest_deserializes_all_kinds() {
        let toml = r#"
            registry = "grim.ocx.sh"

            [skills.grim-usage]
            version = "0.1.1"

            [rules.custom-rule]
            version = "0.2.0"
            path = "shared/custom-rule.md"

            [agents.helper]
            version = "0.1.0"

            [bundles.grim-essentials]
            version = "0.1.0"
            pin = true
        "#;
        let manifest: PublishManifest = toml::from_str(toml).unwrap();
        assert_eq!(manifest.registry, "grim.ocx.sh");
        assert_eq!(manifest.skills.len(), 1);
        assert_eq!(manifest.rules.len(), 1);
        assert_eq!(manifest.agents.len(), 1);
        assert_eq!(manifest.bundles.len(), 1);
        assert_eq!(manifest.skills["grim-usage"].version, "0.1.1");
        assert!(manifest.rules["custom-rule"].path.is_some());
        assert!(manifest.bundles["grim-essentials"].pin);
        assert!(!manifest.skills["grim-usage"].pin);
    }

    #[test]
    fn manifest_rejects_unknown_fields() {
        let toml = r#"
            registry = "grim.ocx.sh"
            unknown_field = "oops"
        "#;
        assert!(toml::from_str::<PublishManifest>(toml).is_err());
    }

    #[test]
    fn entry_spec_rejects_unknown_fields() {
        let toml = r#"
            registry = "grim.ocx.sh"

            [skills.foo]
            version = "0.1.0"
            unsupported_key = "value"
        "#;
        assert!(toml::from_str::<PublishManifest>(toml).is_err());
    }

    #[test]
    fn entry_spec_pin_defaults_false() {
        let toml = r#"
            registry = "grim.ocx.sh"

            [bundles.foo]
            version = "0.1.0"
        "#;
        let manifest: PublishManifest = toml::from_str(toml).unwrap();
        assert!(!manifest.bundles["foo"].pin);
    }

    // ── helpers ──────────────────────────────────────────────────────────

    fn opts(registry: Option<&str>) -> GlobalOptions {
        GlobalOptions {
            format: OutputFormat::Plain,
            offline: false,
            log_level: None,
            config: None,
            global: false,
            registry: registry.into_iter().map(str::to_string).collect(),
        }
    }

    /// Write a file at `p`, creating parent dirs as needed.
    fn write(p: &Path, body: &str) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }

    /// Build a minimal manifest with 1 skill, 1 rule, 1 agent, 1 bundle
    /// in a temp directory, returning (manifest, manifest_dir, tmp).
    fn make_manifest_dir() -> (PublishManifest, std::path::PathBuf, tempfile::TempDir) {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();

        // skill source: a skills/<name>/ directory with SKILL.md
        write(
            &dir.join("skills/my-skill/SKILL.md"),
            "---\nname: my-skill\ndescription: A test skill.\n---\n# My Skill\n",
        );
        // rule source: rules/<name>.md
        write(
            &dir.join("rules/my-rule.md"),
            "---\npaths: ['**/*.rs']\n---\n# My Rule\n",
        );
        // agent source: agents/<name>.md
        write(
            &dir.join("agents/my-agent.md"),
            "---\nname: my-agent\ndescription: A test agent.\n---\nYou are an agent.\n",
        );
        // bundle source: bundles/<name>.toml
        write(
            &dir.join("bundles/my-bundle.toml"),
            "[skills]\nmy-skill = \"localhost:5000/acme/skills/my-skill:0.1.0\"\n",
        );

        let manifest: PublishManifest = toml::from_str(
            r#"
            registry = "localhost:5000"

            [skills.my-skill]
            version = "0.1.0"

            [rules.my-rule]
            version = "0.2.0"

            [agents.my-agent]
            version = "0.3.0"

            [bundles.my-bundle]
            version = "0.4.0"
            "#,
        )
        .unwrap();

        let manifest_dir = dir.to_path_buf();
        (manifest, manifest_dir, tmp)
    }

    // ── validate_manifest: bad semver rejected ────────────────────────────

    #[test]
    fn validate_manifest_rejects_partial_semver_no_patch() {
        // "1.0" has no patch component — strict X.Y.Z only (ADR D2)
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        write(
            &dir.join("skills/my-skill/SKILL.md"),
            "---\nname: my-skill\ndescription: d.\n---\n",
        );
        let manifest: PublishManifest =
            toml::from_str("registry = \"r.example\"\n\n[skills.my-skill]\nversion = \"1.0\"\n").unwrap();
        let err = validate_manifest(&manifest, dir, Path::new("test.toml"), &[], None);
        assert!(err.is_err(), "partial semver '1.0' must be rejected");
        let msg = format!("{:#}", err.unwrap_err());
        assert!(
            msg.contains("1.0") || msg.contains("semver") || msg.contains("version"),
            "error should reference the invalid version, got: {msg}"
        );
    }

    #[test]
    fn validate_manifest_rejects_v_prefixed_semver() {
        // "v1.0.0" with a 'v' prefix is not strict X.Y.Z (ADR D2)
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        write(&dir.join("rules/my-rule.md"), "---\npaths: ['*.rs']\n---\nbody\n");
        let manifest: PublishManifest =
            toml::from_str("registry = \"r.example\"\n\n[rules.my-rule]\nversion = \"v1.0.0\"\n").unwrap();
        let err = validate_manifest(&manifest, dir, Path::new("test.toml"), &[], None);
        assert!(err.is_err(), "'v1.0.0' must be rejected (v-prefix)");
    }

    #[test]
    fn validate_manifest_rejects_prerelease_semver() {
        // "1.0.0-beta" is prerelease — ADR D2 requires strict X.Y.Z
        // (prerelease marker is forbidden in the manifest; use `--tag` for
        // channel tags)
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        write(&dir.join("skills/s/SKILL.md"), "---\nname: s\ndescription: d.\n---\n");
        let manifest: PublishManifest =
            toml::from_str("registry = \"r.example\"\n\n[skills.s]\nversion = \"1.0.0-beta\"\n").unwrap();
        let err = validate_manifest(&manifest, dir, Path::new("test.toml"), &[], None);
        assert!(
            err.is_err(),
            "'1.0.0-beta' prerelease must be rejected (ADR D2 strict X.Y.Z)"
        );
    }

    #[test]
    fn validate_manifest_accepts_strict_xyz_semver() {
        // Happy path: "1.2.3" is valid strict X.Y.Z
        let (manifest, dir, _tmp) = make_manifest_dir();
        validate_manifest(&manifest, &dir, Path::new("test.toml"), &[], None)
            .expect("strictly-formed X.Y.Z versions must pass validation");
    }

    // ── validate_manifest: missing source path rejected ───────────────────

    #[test]
    fn validate_manifest_rejects_missing_source_path() {
        // A skill whose conventional path does not exist is a validation error
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        // Do NOT create the skill directory — path is absent
        let manifest: PublishManifest =
            toml::from_str("registry = \"r.example\"\n\n[skills.missing-skill]\nversion = \"1.0.0\"\n").unwrap();
        let err = validate_manifest(&manifest, dir, Path::new("test.toml"), &[], None);
        assert!(
            err.is_err(),
            "absent source path must be rejected (ADR D2 whole-manifest validation)"
        );
        let msg = format!("{:#}", err.unwrap_err());
        assert!(
            msg.contains("missing-skill") || msg.contains("path") || msg.contains("exist"),
            "error should mention the missing path, got: {msg}"
        );
    }

    #[test]
    fn validate_manifest_respects_explicit_path_override() {
        // A rule with an explicit path override that exists must pass;
        // the conventional path (rules/<name>.md) is irrelevant.
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        write(&dir.join("shared/custom-rule.md"), "---\npaths: ['*.rs']\n---\nbody\n");
        let manifest: PublishManifest = toml::from_str(
            "registry = \"r.example\"\n\n[rules.custom-rule]\nversion = \"0.2.0\"\npath = \"shared/custom-rule.md\"\n",
        )
        .unwrap();
        validate_manifest(&manifest, dir, Path::new("test.toml"), &[], None)
            .expect("explicit path override that exists must pass (ADR D2)");
    }

    #[test]
    fn validate_manifest_rejects_missing_explicit_path_override() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        // override path points at a file that does not exist
        let manifest: PublishManifest = toml::from_str(
            "registry = \"r.example\"\n\n[rules.custom-rule]\nversion = \"0.2.0\"\npath = \"shared/nonexistent.md\"\n",
        )
        .unwrap();
        let err = validate_manifest(&manifest, dir, Path::new("test.toml"), &[], None);
        assert!(err.is_err(), "missing explicit path override must be rejected");
    }

    // ── validate_manifest: pin=true on non-bundle rejected ────────────────

    #[test]
    fn validate_manifest_rejects_pin_on_skill() {
        // `pin = true` is bundle-only (ADR D2)
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        write(
            &dir.join("skills/my-skill/SKILL.md"),
            "---\nname: my-skill\ndescription: d.\n---\n",
        );
        // `pin` on PublishEntrySpec defaults false and is accepted by serde;
        // validate_manifest must catch it on non-bundle entries
        let mut manifest: PublishManifest =
            toml::from_str("registry = \"r.example\"\n\n[skills.my-skill]\nversion = \"1.0.0\"\n").unwrap();
        // Manually force pin=true on the skill entry (bypasses serde
        // deny_unknown_fields since pin is a real field)
        manifest.skills.get_mut("my-skill").unwrap().pin = true;
        let err = validate_manifest(&manifest, dir, Path::new("test.toml"), &[], None);
        assert!(
            err.is_err(),
            "pin=true on a skill must be rejected (ADR D2 bundle-only)"
        );
        let msg = format!("{:#}", err.unwrap_err());
        assert!(
            msg.contains("pin") || msg.contains("bundle"),
            "error must mention pin/bundle, got: {msg}"
        );
    }

    #[test]
    fn validate_manifest_rejects_pin_on_rule() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        write(&dir.join("rules/my-rule.md"), "---\npaths: ['*.rs']\n---\nbody\n");
        let mut manifest: PublishManifest =
            toml::from_str("registry = \"r.example\"\n\n[rules.my-rule]\nversion = \"1.0.0\"\n").unwrap();
        manifest.rules.get_mut("my-rule").unwrap().pin = true;
        let err = validate_manifest(&manifest, dir, Path::new("test.toml"), &[], None);
        assert!(err.is_err(), "pin=true on a rule must be rejected (ADR D2 bundle-only)");
    }

    #[test]
    fn validate_manifest_rejects_pin_on_agent() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        write(
            &dir.join("agents/my-agent.md"),
            "---\nname: my-agent\ndescription: d.\n---\nbody\n",
        );
        let mut manifest: PublishManifest =
            toml::from_str("registry = \"r.example\"\n\n[agents.my-agent]\nversion = \"1.0.0\"\n").unwrap();
        manifest.agents.get_mut("my-agent").unwrap().pin = true;
        let err = validate_manifest(&manifest, dir, Path::new("test.toml"), &[], None);
        assert!(
            err.is_err(),
            "pin=true on an agent must be rejected (ADR D2 bundle-only)"
        );
    }

    #[test]
    fn validate_manifest_accepts_pin_on_bundle() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        write(
            &dir.join("bundles/my-bundle.toml"),
            "[skills]\nms = \"localhost:5000/acme/skills/ms:1.0.0\"\n",
        );
        let manifest: PublishManifest =
            toml::from_str("registry = \"r.example\"\n\n[bundles.my-bundle]\nversion = \"1.0.0\"\npin = true\n")
                .unwrap();
        validate_manifest(&manifest, dir, Path::new("test.toml"), &[], None)
            .expect("pin=true on a bundle is valid (ADR D2)");
    }

    // ── validate_entry_name: charset gate (CWE-20) ────────────────────────

    #[test]
    fn validate_manifest_rejects_name_with_slash() {
        // A name with a path separator would smuggle extra path/reference
        // segments into `skills/{name}` joins and OCI references.
        let tmp = tempfile::tempdir().unwrap();
        let manifest: PublishManifest =
            toml::from_str("registry = \"r.example\"\n\n[skills.\"sub/name\"]\nversion = \"1.0.0\"\n").unwrap();
        let err = validate_manifest(&manifest, tmp.path(), Path::new("test.toml"), &[], None).unwrap_err();
        assert!(format!("{err:#}").contains("name must start with"), "got: {err:#}");
        assert_eq!(crate::error::classify_error(&err), ExitCode::DataError);
    }

    #[test]
    fn validate_manifest_rejects_name_with_dotdot() {
        // `../evil` must die at manifest validation, not deep in release.
        let tmp = tempfile::tempdir().unwrap();
        let manifest: PublishManifest =
            toml::from_str("registry = \"r.example\"\n\n[skills.\"../evil\"]\nversion = \"1.0.0\"\n").unwrap();
        let err = validate_manifest(&manifest, tmp.path(), Path::new("test.toml"), &[], None).unwrap_err();
        assert!(format!("{err:#}").contains("name must start with"), "got: {err:#}");
        assert_eq!(crate::error::classify_error(&err), ExitCode::DataError);
    }

    #[test]
    fn validate_manifest_rejects_uppercase_name() {
        // OCI repository segments are lowercase; reject early with a
        // clearly-attributed manifest error.
        let tmp = tempfile::tempdir().unwrap();
        let manifest: PublishManifest =
            toml::from_str("registry = \"r.example\"\n\n[skills.MySkill]\nversion = \"1.0.0\"\n").unwrap();
        let err = validate_manifest(&manifest, tmp.path(), Path::new("test.toml"), &[], None).unwrap_err();
        assert_eq!(crate::error::classify_error(&err), ExitCode::DataError);
    }

    // ── validate_registry_value: structural gate ──────────────────────────

    #[test]
    fn registry_value_rejects_empty_and_slash() {
        let err = validate_registry_value("", Path::new("test.toml")).unwrap_err();
        assert_eq!(crate::error::classify_error(&err), ExitCode::DataError);
        let err = validate_registry_value("evil.com/extra", Path::new("test.toml")).unwrap_err();
        assert!(format!("{err:#}").contains("plain registry host"), "got: {err:#}");
        assert_eq!(crate::error::classify_error(&err), ExitCode::DataError);
    }

    #[test]
    fn registry_value_accepts_host_and_host_with_port() {
        validate_registry_value("grim.ocx.sh", Path::new("t.toml")).expect("plain host valid");
        validate_registry_value("localhost:5000", Path::new("t.toml")).expect("host:port valid");
    }

    // ── axis B: repository_prefix / per-entry repository (issue #11) ───────

    /// Build a bare entry spec for `entry_repository` unit tests.
    fn entry_spec(version: &str, repository: Option<&str>) -> PublishEntrySpec {
        PublishEntrySpec {
            version: version.to_string(),
            path: None,
            repository: repository.map(str::to_string),
            pin: false,
        }
    }

    #[test]
    fn entry_repository_default_uses_kind_subdir() {
        // No override → today's behavior: `{kind-subdir}/{name}` (backward compat).
        let s = entry_spec("1.0.0", None);
        assert_eq!(
            entry_repository("hearth", ArtifactKind::Skill, &s, None),
            "skills/hearth"
        );
        assert_eq!(entry_repository("r", ArtifactKind::Rule, &s, None), "rules/r");
        assert_eq!(entry_repository("a", ArtifactKind::Agent, &s, None), "agents/a");
        assert_eq!(entry_repository("b", ArtifactKind::Bundle, &s, None), "bundles/b");
    }

    #[test]
    fn entry_repository_prefix_replaces_kind_subdir() {
        // The manifest prefix replaces `{kind-subdir}`, appending the name.
        let s = entry_spec("1.0.0", None);
        assert_eq!(
            entry_repository("hearth", ArtifactKind::Skill, &s, Some("durzn-technology/hearth/skill")),
            "durzn-technology/hearth/skill/hearth"
        );
    }

    #[test]
    fn entry_repository_per_entry_override_wins_and_omits_name() {
        // A per-entry `repository` wins over the prefix and is used verbatim
        // (the name is NOT appended — mirrors `grim release`).
        let s = entry_spec("1.0.0", Some("durzn-technology/hearth/skill/hearth"));
        assert_eq!(
            entry_repository("hearth", ArtifactKind::Skill, &s, Some("ignored/prefix")),
            "durzn-technology/hearth/skill/hearth"
        );
    }

    #[test]
    fn validate_repository_path_accepts_nested_lowercase() {
        validate_repository_path(
            "durzn-technology/hearth/skill",
            "repository_prefix",
            Path::new("t.toml"),
        )
        .expect("nested lowercase path valid");
        validate_repository_path("a/b_c/d.e/f-g", "repository_prefix", Path::new("t.toml"))
            .expect("OCI path alphabet valid");
        // The OCI grammar also blesses a double underscore and runs of dashes
        // between alnum runs.
        validate_repository_path("a__b/c--d", "repository_prefix", Path::new("t.toml"))
            .expect("__ and -- separators valid");
    }

    #[test]
    fn validate_repository_path_rejects_bad_values() {
        let long = format!("a/{}", "b".repeat(crate::oci::identifier::MAX_REPOSITORY_LENGTH));
        for bad in [
            "",             // empty
            "/leading",     // leading slash
            "trailing/",    // trailing slash
            "a//b",         // empty segment
            "../evil",      // parent-dir traversal
            "a/./b",        // cur-dir segment
            "UPPER/case",   // uppercase
            "has:tag",      // embedded tag separator
            "group-/proj",  // trailing separator in a segment (Codex bypass class)
            "group./proj",  // trailing dot in a segment
            "grp/-leading", // leading separator in a segment
            "a..b/c",       // doubled dot separator
            "a._b/c",       // mixed doubled separator
            long.as_str(),  // exceeds MAX_REPOSITORY_LENGTH
        ] {
            let err = validate_repository_path(bad, "repository_prefix", Path::new("t.toml")).unwrap_err();
            assert_eq!(
                crate::error::classify_error(&err),
                ExitCode::DataError,
                "{bad:?} must be rejected as DataError (65)"
            );
        }
    }

    #[test]
    fn validate_manifest_rejects_bad_repository_prefix() {
        let (mut manifest, dir, _tmp) = make_manifest_dir();
        manifest.repository_prefix = Some("Bad/Prefix".to_string());
        let err = validate_manifest(&manifest, &dir, Path::new("test.toml"), &[], None).unwrap_err();
        assert_eq!(crate::error::classify_error(&err), ExitCode::DataError);
    }

    #[test]
    fn validate_manifest_rejects_bad_entry_repository() {
        let (mut manifest, dir, _tmp) = make_manifest_dir();
        // A segment-grammar violation (trailing separator), not the `:` early
        // guard — proves the per-entry `repository` actually flows into the
        // shared OCI segment validation, not just the colon check.
        manifest.skills.get_mut("my-skill").unwrap().repository = Some("group-/my-skill".to_string());
        let err = validate_manifest(&manifest, &dir, Path::new("test.toml"), &[], None).unwrap_err();
        assert_eq!(crate::error::classify_error(&err), ExitCode::DataError);
    }

    #[test]
    fn validate_manifest_accepts_valid_repository_overrides() {
        let (mut manifest, dir, _tmp) = make_manifest_dir();
        manifest.repository_prefix = Some("durzn-technology/hearth/skill".to_string());
        manifest.rules.get_mut("my-rule").unwrap().repository =
            Some("durzn-technology/hearth/rule/my-rule".to_string());
        validate_manifest(&manifest, &dir, Path::new("test.toml"), &[], None)
            .expect("valid repository overrides must pass");
    }

    #[test]
    fn manifest_deserializes_repository_fields() {
        let toml = r#"
            registry = "registry.gitlab.com"
            repository_prefix = "group/project/skill"

            [skills.hearth]
            version = "0.1.0"
            repository = "group/project/skill/hearth"
        "#;
        let m: PublishManifest = toml::from_str(toml).unwrap();
        assert_eq!(m.repository_prefix.as_deref(), Some("group/project/skill"));
        assert_eq!(
            m.skills["hearth"].repository.as_deref(),
            Some("group/project/skill/hearth")
        );
    }

    #[test]
    fn manifest_repository_fields_default_to_none() {
        // Backward compat: a manifest with neither field parses, and
        // `entry_repository` falls back to the kind-subdir default.
        let m: PublishManifest =
            toml::from_str("registry = \"grim.ocx.sh\"\n\n[skills.s]\nversion = \"1.0.0\"\n").unwrap();
        assert!(m.repository_prefix.is_none());
        assert!(m.skills["s"].repository.is_none());
    }

    #[test]
    fn plan_entries_nested_repository_prefix_builds_reporter_path() {
        // Headline regression for issue #11 axis B: the reporter's exact path.
        // A `repository_prefix` nests the push under the registry's
        // group/project path instead of the hardcoded `{kind-subdir}/{name}`.
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        write(
            &dir.join("skills/hearth/SKILL.md"),
            "---\nname: hearth\ndescription: d.\n---\n",
        );
        let mut manifest: PublishManifest =
            toml::from_str("registry = \"registry.gitlab.com\"\n\n[skills.hearth]\nversion = \"0.1.0\"\n").unwrap();
        manifest.repository_prefix = Some("durzn-technology/hearth/skill".to_string());
        let entries = plan_entries(&manifest, dir, "registry.gitlab.com", &[], None);
        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries[0].reference,
            "registry.gitlab.com/durzn-technology/hearth/skill/hearth:0.1.0"
        );
    }

    #[test]
    fn plan_entries_per_entry_repository_overrides_prefix() {
        // A per-entry `repository` wins over the manifest prefix and is used
        // verbatim (no name appended).
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        write(
            &dir.join("skills/hearth/SKILL.md"),
            "---\nname: hearth\ndescription: d.\n---\n",
        );
        let manifest: PublishManifest = toml::from_str(
            "registry = \"registry.gitlab.com\"\nrepository_prefix = \"group/ignored\"\n\n\
             [skills.hearth]\nversion = \"0.1.0\"\nrepository = \"durzn-technology/hearth/skill/hearth\"\n",
        )
        .unwrap();
        let entries = plan_entries(&manifest, dir, "registry.gitlab.com", &[], None);
        assert_eq!(
            entries[0].reference,
            "registry.gitlab.com/durzn-technology/hearth/skill/hearth:0.1.0"
        );
        // Last repo segment == entry name → the name is effectively present, no
        // dry-run hint.
        assert!(!entries[0].name_not_appended);
    }

    #[test]
    fn plan_entries_flags_name_not_appended_for_renamed_repository() {
        // A per-entry `repository` whose last segment differs from the entry
        // name is used verbatim (name dropped) → `name_not_appended` is set so
        // a `--dry-run` preview can hint about it. The prefix case never sets it.
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        write(
            &dir.join("skills/hearth/SKILL.md"),
            "---\nname: hearth\ndescription: d.\n---\n",
        );
        let manifest: PublishManifest = toml::from_str(
            "registry = \"registry.gitlab.com\"\n\n\
             [skills.hearth]\nversion = \"0.1.0\"\nrepository = \"durzn-technology/hearth/skill\"\n",
        )
        .unwrap();
        let entries = plan_entries(&manifest, dir, "registry.gitlab.com", &[], None);
        assert_eq!(
            entries[0].reference,
            "registry.gitlab.com/durzn-technology/hearth/skill:0.1.0"
        );
        assert!(entries[0].name_not_appended);
    }

    // ── validate_manifest: unknown --only name rejected ───────────────────

    #[test]
    fn validate_manifest_rejects_unknown_only_name() {
        // --only with a name not in the manifest is a DataError (65) (ADR D1)
        let (manifest, dir, _tmp) = make_manifest_dir();
        let err = validate_manifest(
            &manifest,
            &dir,
            Path::new("test.toml"),
            &["nonexistent-entry".to_string()],
            None,
        );
        assert!(err.is_err(), "unknown --only name must be rejected");
        let msg = format!("{:#}", err.unwrap_err());
        assert!(
            msg.contains("nonexistent-entry"),
            "error must name the unknown entry, got: {msg}"
        );
    }

    #[test]
    fn validate_manifest_accepts_known_only_name() {
        let (manifest, dir, _tmp) = make_manifest_dir();
        validate_manifest(&manifest, &dir, Path::new("test.toml"), &["my-skill".to_string()], None)
            .expect("known --only name must pass validation");
    }

    // ── validate_manifest: --tag semver rejected ──────────────────────────

    #[test]
    fn validate_manifest_rejects_semver_tag() {
        // --tag with a semver value is a DataError (65) (ADR D1)
        let (manifest, dir, _tmp) = make_manifest_dir();
        let err = validate_manifest(&manifest, &dir, Path::new("test.toml"), &[], Some("1.2.3"));
        assert!(err.is_err(), "--tag with semver value must be rejected (ADR D1)");
        let msg = format!("{:#}", err.unwrap_err());
        assert!(
            msg.contains("1.2.3") || msg.contains("semver") || msg.contains("tag"),
            "error must mention the rejected tag, got: {msg}"
        );
    }

    #[test]
    fn validate_manifest_rejects_semver_tag_major_minor_patch() {
        // "2.0.0" is semver and must be rejected as a --tag value
        let (manifest, dir, _tmp) = make_manifest_dir();
        let err = validate_manifest(&manifest, &dir, Path::new("test.toml"), &[], Some("2.0.0"));
        assert!(err.is_err(), "--tag 2.0.0 (semver) must be rejected");
    }

    #[test]
    fn validate_manifest_accepts_nonversion_tag() {
        // "canary" is not semver and is a valid --tag (ADR D1)
        let (manifest, dir, _tmp) = make_manifest_dir();
        validate_manifest(&manifest, &dir, Path::new("test.toml"), &[], Some("canary"))
            .expect("--tag canary (non-semver) must be accepted");
    }

    #[test]
    fn validate_manifest_accepts_edge_nonversion_tag() {
        // "edge" is not semver — valid movable channel tag
        let (manifest, dir, _tmp) = make_manifest_dir();
        validate_manifest(&manifest, &dir, Path::new("test.toml"), &[], Some("edge"))
            .expect("--tag edge (non-semver) must be accepted");
    }

    // ── validate_manifest: empty manifest rejected (Fix #1) ──────────────

    #[test]
    fn validate_manifest_rejects_empty_manifest_exits_65() {
        // A manifest with no declared packages (all kind tables empty) must
        // error with exit 65 (DataError) and message "no packages declared in manifest".
        let tmp = tempfile::tempdir().unwrap();
        let manifest_path = tmp.path().join("publish.toml");
        // No skills/rules/agents/bundles declared
        let manifest: PublishManifest = toml::from_str("registry = \"r.example\"\n").unwrap();
        let err = validate_manifest(&manifest, tmp.path(), &manifest_path, &[], None);
        assert!(err.is_err(), "empty manifest must be rejected");
        let msg = format!("{:#}", err.unwrap_err());
        assert!(
            msg.contains("no packages declared"),
            "error must say 'no packages declared in manifest', got: {msg}"
        );
        // Verify the exit code classifies to DataError (65)
        let err2 = validate_manifest(&manifest, tmp.path(), &manifest_path, &[], None).unwrap_err();
        assert_eq!(
            crate::error::classify_error(&err2),
            crate::cli::exit_code::ExitCode::DataError,
            "empty manifest must classify to DataError (65)"
        );
    }

    // ── semver tightening (Fix #4) ────────────────────────────────────────

    #[test]
    fn is_strict_semver_rejects_leading_zero_major() {
        // "01.0.0" has a leading zero — semver::Version::parse rejects it.
        // The hand-rolled check accepted leading zeros; the new check does not.
        assert!(
            !is_strict_semver("01.0.0"),
            "'01.0.0' must be rejected (leading zero in major)"
        );
    }

    #[test]
    fn is_strict_semver_rejects_build_metadata() {
        // "1.0.0+meta" has build metadata — rejected even though semver-valid.
        assert!(
            !is_strict_semver("1.0.0+meta"),
            "'1.0.0+meta' must be rejected (build metadata not allowed in manifest)"
        );
    }

    #[test]
    fn is_strict_semver_rejects_prerelease() {
        // "1.0.0-beta" is a prerelease — already rejected by old code too.
        assert!(
            !is_strict_semver("1.0.0-beta"),
            "'1.0.0-beta' must be rejected (prerelease not allowed in manifest)"
        );
    }

    #[test]
    fn is_strict_semver_accepts_plain_version() {
        // "1.0.0" is valid strict semver — the common case.
        assert!(is_strict_semver("1.0.0"), "'1.0.0' must be accepted as strict semver");
    }

    // ── data_error / error message format (Fix #5) ────────────────────────

    #[test]
    fn data_error_at_formats_without_metadata_invalid_prefix() {
        // The old data_error used MetadataInvalid → produced ": invalid tool metadata: <msg>"
        // The new data_error_at uses ValidationFailed → produces "{path}: {msg}" cleanly.
        let path = Path::new("publish.toml");
        let err = data_error_at(path, "no packages declared in manifest");
        let msg = format!("{:#}", err);
        assert!(
            !msg.contains("invalid tool metadata"),
            "error must not contain 'invalid tool metadata' prefix, got: {msg}"
        );
        assert!(
            msg.contains("no packages declared in manifest"),
            "error must contain the actual message, got: {msg}"
        );
        // Must classify to DataError (65)
        assert_eq!(
            crate::error::classify_error(&err),
            crate::cli::exit_code::ExitCode::DataError,
            "data_error_at must classify to DataError (65)"
        );
    }

    #[test]
    fn data_error_semver_tag_formats_cleanly() {
        // The user-visible message for --tag semver rejection must read cleanly.
        let (manifest, dir, _tmp) = make_manifest_dir();
        let manifest_path = dir.join("publish.toml");
        let err = validate_manifest(&manifest, &dir, &manifest_path, &[], Some("1.2.3")).unwrap_err();
        let msg = format!("{:#}", err);
        // Must NOT have "invalid tool metadata" prefix
        assert!(
            !msg.contains("invalid tool metadata"),
            "semver tag error must not contain 'invalid tool metadata', got: {msg}"
        );
        // Must contain the meaningful rejection message
        assert!(
            msg.contains("1.2.3") || msg.contains("semver"),
            "semver tag error must mention the rejected tag, got: {msg}"
        );
        // Must classify to DataError
        assert_eq!(
            crate::error::classify_error(&err),
            crate::cli::exit_code::ExitCode::DataError,
            "semver tag error must classify to DataError (65)"
        );
    }

    // ── classify_error assertions on existing validation tests (Fix #6) ───

    #[test]
    fn validate_manifest_bad_semver_classifies_to_data_error() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        write(&dir.join("skills/s/SKILL.md"), "---\nname: s\ndescription: d.\n---\n");
        let manifest: PublishManifest =
            toml::from_str("registry = \"r.example\"\n\n[skills.s]\nversion = \"1.0\"\n").unwrap();
        let err = validate_manifest(&manifest, dir, Path::new("test.toml"), &[], None).unwrap_err();
        assert_eq!(
            crate::error::classify_error(&err),
            crate::cli::exit_code::ExitCode::DataError
        );
    }

    #[test]
    fn validate_manifest_pin_on_skill_classifies_to_data_error() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        write(
            &dir.join("skills/my-skill/SKILL.md"),
            "---\nname: my-skill\ndescription: d.\n---\n",
        );
        let mut manifest: PublishManifest =
            toml::from_str("registry = \"r.example\"\n\n[skills.my-skill]\nversion = \"1.0.0\"\n").unwrap();
        manifest.skills.get_mut("my-skill").unwrap().pin = true;
        let err = validate_manifest(&manifest, dir, Path::new("test.toml"), &[], None).unwrap_err();
        assert_eq!(
            crate::error::classify_error(&err),
            crate::cli::exit_code::ExitCode::DataError
        );
    }

    #[test]
    fn validate_manifest_unknown_only_name_classifies_to_data_error() {
        let (manifest, dir, _tmp) = make_manifest_dir();
        let err = validate_manifest(
            &manifest,
            &dir,
            Path::new("test.toml"),
            &["nonexistent".to_string()],
            None,
        )
        .unwrap_err();
        assert_eq!(
            crate::error::classify_error(&err),
            crate::cli::exit_code::ExitCode::DataError
        );
    }

    #[test]
    fn validate_manifest_semver_tag_classifies_to_data_error() {
        let (manifest, dir, _tmp) = make_manifest_dir();
        let err = validate_manifest(&manifest, &dir, Path::new("test.toml"), &[], Some("1.2.3")).unwrap_err();
        assert_eq!(
            crate::error::classify_error(&err),
            crate::cli::exit_code::ExitCode::DataError
        );
    }

    #[test]
    fn load_manifest_bundle_shaped_classifies_to_data_error() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("publish.toml");
        std::fs::write(&path, "[skills]\ncr = \"ghcr.io/acme/cr:1\"\n").unwrap();
        let err = load_manifest(&path).unwrap_err();
        assert_eq!(
            crate::error::classify_error(&err),
            crate::cli::exit_code::ExitCode::DataError
        );
    }

    // ── load_manifest: bundle-shaped file → guard error ──────────────────

    #[test]
    fn load_manifest_bundle_shaped_file_hints_grim_release() {
        // A bundle TOML (flat name=ref string values) must not surface a raw
        // serde error — instead emit a D7 guard hinting at `grim release --kind bundle`
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("publish.toml");
        std::fs::write(
            &path,
            // Bundle shape: [skills] table with string values, NOT sub-tables
            "[skills]\ncr = \"ghcr.io/acme/cr:1\"\n",
        )
        .unwrap();
        let err = load_manifest(&path).expect_err("bundle-shaped file must be rejected by load_manifest (ADR D7)");
        let msg = format!("{:#}", err);
        assert!(
            msg.contains("bundle") || msg.contains("grim release"),
            "error must hint at `grim release --kind bundle` (ADR D7), got: {msg}"
        );
    }

    #[test]
    fn load_manifest_rejects_oversized_file() {
        // Files exceeding the 64 KiB cap must be rejected (ADR D2 / config::read_capped)
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("publish.toml");
        // Write a file exceeding 64 KiB
        let big = "x".repeat(65 * 1024 + 1);
        std::fs::write(&path, big).unwrap();
        let err = load_manifest(&path).expect_err("file larger than 64 KiB must be rejected");
        let msg = format!("{:#}", err);
        assert!(
            msg.contains("large") || msg.contains("64") || msg.contains("limit") || msg.contains("size"),
            "error must mention size/limit, got: {msg}"
        );
    }

    // ── plan_entries: ordering ────────────────────────────────────────────

    #[test]
    fn plan_entries_order_is_skills_rules_agents_bundles_alpha_within_kind() {
        // ADR D4: fixed kind order + alphabetical within kind
        // Build a richer manifest with multiple entries per kind to test alpha
        let toml = r#"
            registry = "localhost:5000"

            [skills.zebra-skill]
            version = "0.1.0"

            [skills.alpha-skill]
            version = "0.1.0"

            [rules.z-rule]
            version = "0.1.0"

            [rules.a-rule]
            version = "0.1.0"

            [agents.z-agent]
            version = "0.1.0"

            [agents.a-agent]
            version = "0.1.0"

            [bundles.z-bundle]
            version = "0.1.0"

            [bundles.a-bundle]
            version = "0.1.0"
            "#;

        let tmp2 = tempfile::tempdir().unwrap();
        let dir2 = tmp2.path();
        // Create all source paths
        for name in &["alpha-skill", "zebra-skill"] {
            write(
                &dir2.join(format!("skills/{name}/SKILL.md")),
                &format!("---\nname: {name}\ndescription: d.\n---\n"),
            );
        }
        for name in &["a-rule", "z-rule"] {
            write(
                &dir2.join(format!("rules/{name}.md")),
                "---\npaths: ['*.rs']\n---\nbody\n",
            );
        }
        for name in &["a-agent", "z-agent"] {
            write(
                &dir2.join(format!("agents/{name}.md")),
                &format!("---\nname: {name}\ndescription: d.\n---\nbody\n"),
            );
        }
        for name in &["a-bundle", "z-bundle"] {
            write(
                &dir2.join(format!("bundles/{name}.toml")),
                "[skills]\ns = \"localhost:5000/acme/s:1.0.0\"\n",
            );
        }

        let manifest2: PublishManifest = toml::from_str(toml).unwrap();
        let entries = plan_entries(&manifest2, dir2, "localhost:5000", &[], None);

        // Verify ordering: skills first (alpha within), then rules, agents, bundles
        let kinds: Vec<crate::oci::ArtifactKind> = entries.iter().map(|e| e.kind).collect();
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();

        // Kind order: skills → rules → agents → bundles
        assert_eq!(kinds[0], crate::oci::ArtifactKind::Skill, "first entry must be a skill");
        assert_eq!(
            kinds[1],
            crate::oci::ArtifactKind::Skill,
            "second entry must be a skill"
        );
        assert_eq!(kinds[2], crate::oci::ArtifactKind::Rule, "third entry must be a rule");
        assert_eq!(kinds[3], crate::oci::ArtifactKind::Rule, "fourth entry must be a rule");
        assert_eq!(
            kinds[4],
            crate::oci::ArtifactKind::Agent,
            "fifth entry must be an agent"
        );
        assert_eq!(
            kinds[5],
            crate::oci::ArtifactKind::Agent,
            "sixth entry must be an agent"
        );
        assert_eq!(
            kinds[6],
            crate::oci::ArtifactKind::Bundle,
            "seventh entry must be a bundle"
        );
        assert_eq!(
            kinds[7],
            crate::oci::ArtifactKind::Bundle,
            "eighth entry must be a bundle"
        );

        // Alpha within kind
        assert_eq!(names[0], "alpha-skill", "skills must be alphabetical");
        assert_eq!(names[1], "zebra-skill");
        assert_eq!(names[2], "a-rule", "rules must be alphabetical");
        assert_eq!(names[3], "z-rule");
        assert_eq!(names[4], "a-agent", "agents must be alphabetical");
        assert_eq!(names[5], "z-agent");
        assert_eq!(names[6], "a-bundle", "bundles must be alphabetical");
        assert_eq!(names[7], "z-bundle");
    }

    // ── plan_entries: conventional path construction ───────────────────────

    #[test]
    fn plan_entries_builds_conventional_paths_relative_to_manifest_dir() {
        // ADR D2: skills/{name}/, rules/{name}.md, agents/{name}.md, bundles/{name}.toml
        let (manifest, dir, _tmp) = make_manifest_dir();
        let entries = plan_entries(&manifest, &dir, "localhost:5000", &[], None);

        let by_name: std::collections::HashMap<&str, &PlannedEntry> =
            entries.iter().map(|e| (e.name.as_str(), e)).collect();

        assert_eq!(
            by_name["my-skill"].path,
            dir.join("skills/my-skill"),
            "skill conventional path: skills/<name>/"
        );
        assert_eq!(
            by_name["my-rule"].path,
            dir.join("rules/my-rule.md"),
            "rule conventional path: rules/<name>.md"
        );
        assert_eq!(
            by_name["my-agent"].path,
            dir.join("agents/my-agent.md"),
            "agent conventional path: agents/<name>.md"
        );
        assert_eq!(
            by_name["my-bundle"].path,
            dir.join("bundles/my-bundle.toml"),
            "bundle conventional path: bundles/<name>.toml"
        );
    }

    #[test]
    fn plan_entries_respects_explicit_path_override() {
        // When `path` is set in the manifest entry, it overrides the convention
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        write(&dir.join("shared/custom-rule.md"), "---\npaths: ['*.rs']\n---\nbody\n");
        let manifest: PublishManifest = toml::from_str(
            "registry = \"r.example\"\n\n[rules.custom-rule]\nversion = \"0.2.0\"\npath = \"shared/custom-rule.md\"\n",
        )
        .unwrap();
        let entries = plan_entries(&manifest, dir, "r.example", &[], None);
        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries[0].path,
            dir.join("shared/custom-rule.md"),
            "explicit path override must take precedence over convention"
        );
    }

    // ── plan_entries: reference format ───────────────────────────────────

    #[test]
    fn plan_entries_builds_correct_oci_reference_format() {
        // Reference format: {registry}/{skills|rules|agents|bundles}/{name}:{version}
        let (manifest, dir, _tmp) = make_manifest_dir();
        let entries = plan_entries(&manifest, &dir, "localhost:5000", &[], None);

        let by_name: std::collections::HashMap<&str, &PlannedEntry> =
            entries.iter().map(|e| (e.name.as_str(), e)).collect();

        assert_eq!(
            by_name["my-skill"].reference, "localhost:5000/skills/my-skill:0.1.0",
            "skill reference: registry/skills/<name>:<version>"
        );
        assert_eq!(
            by_name["my-rule"].reference, "localhost:5000/rules/my-rule:0.2.0",
            "rule reference: registry/rules/<name>:<version>"
        );
        assert_eq!(
            by_name["my-agent"].reference, "localhost:5000/agents/my-agent:0.3.0",
            "agent reference: registry/agents/<name>:<version>"
        );
        assert_eq!(
            by_name["my-bundle"].reference, "localhost:5000/bundles/my-bundle:0.4.0",
            "bundle reference: registry/bundles/<name>:<version>"
        );
    }

    #[test]
    fn plan_entries_tag_override_replaces_version_in_reference() {
        // --tag canary replaces the version tag in the OCI reference (ADR D1)
        let (manifest, dir, _tmp) = make_manifest_dir();
        let entries = plan_entries(&manifest, &dir, "localhost:5000", &[], Some("canary"));

        for entry in &entries {
            assert!(
                entry.reference.ends_with(":canary"),
                "reference must end with :canary when --tag canary, got: {}",
                entry.reference
            );
        }
    }

    // ── plan_entries: --only filter ───────────────────────────────────────

    #[test]
    fn plan_entries_only_filter_limits_entries() {
        // --only filters the entry list to just the named entries (ADR D1)
        let (manifest, dir, _tmp) = make_manifest_dir();
        let entries = plan_entries(&manifest, &dir, "localhost:5000", &["my-skill".to_string()], None);

        assert_eq!(entries.len(), 1, "--only my-skill must yield exactly 1 entry");
        assert_eq!(entries[0].name, "my-skill");
        assert_eq!(entries[0].kind, crate::oci::ArtifactKind::Skill);
    }

    #[test]
    fn plan_entries_only_multiple_filters_preserves_kind_order() {
        // Multiple --only names still come out in kind order (skills before rules)
        let (manifest, dir, _tmp) = make_manifest_dir();
        let entries = plan_entries(
            &manifest,
            &dir,
            "localhost:5000",
            &["my-rule".to_string(), "my-skill".to_string()],
            None,
        );

        assert_eq!(entries.len(), 2, "--only with 2 names must yield 2 entries");
        assert_eq!(
            entries[0].kind,
            crate::oci::ArtifactKind::Skill,
            "skill must come before rule even when --only names are reversed"
        );
        assert_eq!(entries[1].kind, crate::oci::ArtifactKind::Rule);
    }

    // ── resolve_publish_registry ─────────────────────────────────────────

    #[test]
    fn resolve_publish_registry_uses_manifest_registry_by_default() {
        // When no --registry flag, the manifest registry is used (ADR D1)
        let tmp = tempfile::tempdir().unwrap();
        let ctx = Context::hermetic(tmp.path().to_path_buf());
        assert_eq!(
            resolve_publish_registry(&ctx, "manifest.example"),
            "manifest.example",
            "manifest registry must be used when --registry is absent"
        );
    }

    #[test]
    fn resolve_publish_registry_registry_flag_wins_over_manifest() {
        // --registry flag overrides the manifest registry (ADR D1, top-tier)
        let ctx = Context::new(&opts(Some("flag.example")));
        assert_eq!(
            resolve_publish_registry(&ctx, "manifest.example"),
            "flag.example",
            "--registry flag must win over manifest.registry"
        );
    }

    // ── plan_entries contract tests (truthful names, formerly "batch_*") ──

    #[cfg(test)]
    fn make_test_manifest_sources(dir: &Path) {
        // Skill source
        write(
            &dir.join("skills/test-skill/SKILL.md"),
            "---\nname: test-skill\ndescription: A test skill.\n---\n# Test Skill\n",
        );
        write(&dir.join("skills/test-skill/scripts/run.sh"), "echo hi\n");
        // Rule source
        write(
            &dir.join("rules/test-rule.md"),
            "---\npaths: ['**/*.rs']\n---\n# Test Rule\n",
        );
    }

    #[tokio::test]
    async fn plan_entries_order_is_skills_then_rules_for_two_skills_one_rule() {
        // Renamed from batch_pushes_entries_to_memory_registry_and_reports_pushed_status.
        // This test verifies plan_entries ordering, not the push path.
        use crate::oci::ArtifactKind;

        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        make_test_manifest_sources(dir);

        // Second skill
        write(
            &dir.join("skills/another-skill/SKILL.md"),
            "---\nname: another-skill\ndescription: Another skill.\n---\n# Another\n",
        );

        let manifest: PublishManifest = toml::from_str(
            r#"
            registry = "localhost:5000"

            [skills.test-skill]
            version = "0.1.0"

            [skills.another-skill]
            version = "0.2.0"

            [rules.test-rule]
            version = "0.3.0"
            "#,
        )
        .unwrap();

        validate_manifest(&manifest, dir, Path::new("test.toml"), &[], None)
            .expect("manifest must be valid before batch");

        let registry = "localhost:5000";
        let entries = plan_entries(&manifest, dir, registry, &[], None);
        assert_eq!(entries.len(), 3, "2 skills + 1 rule = 3 entries");

        // Kind order: skills before rules (ADR D4)
        assert_eq!(entries[0].kind, ArtifactKind::Skill);
        assert_eq!(entries[1].kind, ArtifactKind::Skill);
        assert_eq!(entries[2].kind, ArtifactKind::Rule);

        // Alpha within skills
        assert_eq!(entries[0].name, "another-skill");
        assert_eq!(entries[1].name, "test-skill");
    }

    #[tokio::test]
    async fn plan_entries_skip_existing_flag_set_and_reference_correct() {
        // Renamed from batch_second_run_all_entries_would_be_skipped.
        // This test verifies plan_entries output (reference format, count);
        // the skip-existing behavior is exercised by run_pushes_then_skips_on_second_call.
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        make_test_manifest_sources(dir);

        let manifest: PublishManifest = toml::from_str(
            "registry = \"localhost:5000\"\n\n[skills.test-skill]\nversion = \"0.1.0\"\n\n[rules.test-rule]\nversion = \"0.1.0\"\n",
        )
        .unwrap();

        validate_manifest(&manifest, dir, Path::new("test.toml"), &[], None).expect("valid manifest");
        let entries = plan_entries(&manifest, dir, "localhost:5000", &[], None);

        assert_eq!(entries.len(), 2);

        let skill = entries
            .iter()
            .find(|e| e.kind == crate::oci::ArtifactKind::Skill)
            .unwrap();
        assert!(skill.reference.contains("localhost:5000/skills/test-skill:0.1.0"));
    }

    #[tokio::test]
    async fn plan_entries_dry_run_reference_and_pin_correct() {
        // Renamed from dry_run_flag_propagated_to_release_args.
        // This test verifies plan_entries output only; the dry_run=true
        // behavior is exercised by run_dry_run_pushes_nothing.
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        make_test_manifest_sources(dir);

        let manifest: PublishManifest =
            toml::from_str("registry = \"localhost:5000\"\n\n[skills.test-skill]\nversion = \"0.1.0\"\n").unwrap();

        validate_manifest(&manifest, dir, Path::new("test.toml"), &[], None).expect("valid manifest");
        let entries = plan_entries(&manifest, dir, "localhost:5000", &[], None);
        assert_eq!(entries.len(), 1);

        assert_eq!(entries[0].reference, "localhost:5000/skills/test-skill:0.1.0");
        // The version is encoded in the reference tag; PlannedEntry.version was removed.
        assert!(!entries[0].pin, "skill entries must not be pinned");
    }

    // ── True MemoryRegistry e2e tests for run() (Fix #3) ─────────────────

    /// Build a `PublishArgs` pointing at a manifest written to `manifest_path`.
    fn make_publish_args(
        manifest_path: std::path::PathBuf,
        only: Vec<String>,
        tag: Option<String>,
        dry_run: bool,
        force: bool,
    ) -> PublishArgs {
        PublishArgs {
            manifest: manifest_path,
            only,
            tag,
            dry_run,
            force,
            git: false,
            announce: false,
            announce_repo: None,
        }
    }

    #[tokio::test]
    async fn run_pushes_then_skips_on_second_call() {
        // Mirror release.rs memory_registry_release_pushes_cascade_idempotent_and_guards:
        // first run() → all pushed, second run() → all skipped (skip-existing default).
        use crate::api::publish_report::PublishStatus;
        use crate::oci::access::memory_registry::MemoryRegistry;

        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        make_test_manifest_sources(dir);

        let manifest_path = dir.join("publish.toml");
        std::fs::write(
            &manifest_path,
            "registry = \"localhost:5000\"\n\n[skills.test-skill]\nversion = \"0.1.0\"\n",
        )
        .unwrap();

        let registry = MemoryRegistry::new();
        let ctx = Context::with_access(tmp.path().to_path_buf(), registry.clone());

        let args = make_publish_args(manifest_path.clone(), vec![], None, false, false);

        // First run: skill must be pushed
        let (report1, exit1) = run(&ctx, &args).await.expect("first run must succeed");
        assert_eq!(exit1, crate::cli::exit_code::ExitCode::Success, "first run must exit 0");
        assert_eq!(report1.entries().len(), 1, "first run must produce 1 entry");
        assert_eq!(
            report1.entries()[0].status,
            PublishStatus::Pushed,
            "first run must push the skill"
        );
        let first_digest = report1.entries()[0]
            .digest
            .clone()
            .expect("pushed entry must have digest");

        // Second run: skip-existing default → skill already at that digest, skip
        let ctx2 = Context::with_access(tmp.path().to_path_buf(), registry);
        let args2 = make_publish_args(manifest_path, vec![], None, false, false);
        let (report2, exit2) = run(&ctx2, &args2).await.expect("second run must succeed");
        assert_eq!(
            exit2,
            crate::cli::exit_code::ExitCode::Success,
            "second run must exit 0"
        );
        assert_eq!(report2.entries().len(), 1, "second run must produce 1 entry");
        assert_eq!(
            report2.entries()[0].status,
            PublishStatus::Skipped,
            "second run must skip (existing version, skip_existing=true by default)"
        );
        // Skipped entry digest is the existing tag digest (populated from registry)
        let _ = first_digest; // verified via status above
    }

    #[tokio::test]
    async fn run_channel_tag_moves_on_republish() {
        // --tag is a movable channel tag: a second publish with changed
        // content must MOVE the tag (no silent skip-existing freeze, no
        // --force required) — the Codex cross-model review caught that
        // skip-existing would otherwise pin `canary` to its first digest
        // forever.
        use crate::api::publish_report::PublishStatus;
        use crate::oci::access::memory_registry::MemoryRegistry;

        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        make_test_manifest_sources(dir);

        let manifest_path = dir.join("publish.toml");
        std::fs::write(
            &manifest_path,
            "registry = \"localhost:5000\"\n\n[skills.test-skill]\nversion = \"0.1.0\"\n",
        )
        .unwrap();

        let registry = MemoryRegistry::new();
        let ctx = Context::with_access(tmp.path().to_path_buf(), registry.clone());
        let args = make_publish_args(manifest_path.clone(), vec![], Some("canary".to_string()), false, false);
        let (report1, exit1) = run(&ctx, &args).await.expect("first canary run must succeed");
        assert_eq!(exit1, crate::cli::exit_code::ExitCode::Success);
        assert_eq!(report1.entries()[0].status, PublishStatus::Pushed);
        let first_digest = report1.entries()[0]
            .digest
            .clone()
            .expect("pushed entry must have digest");

        // Change the skill content so the manifest digest differs.
        std::fs::write(
            dir.join("skills/test-skill/SKILL.md"),
            "---\nname: test-skill\ndescription: changed body for canary move\n---\n\n# test-skill v2\n",
        )
        .unwrap();

        let ctx2 = Context::with_access(tmp.path().to_path_buf(), registry);
        let args2 = make_publish_args(manifest_path, vec![], Some("canary".to_string()), false, false);
        let (report2, exit2) = run(&ctx2, &args2).await.expect("second canary run must succeed");
        assert_eq!(exit2, crate::cli::exit_code::ExitCode::Success);
        assert_eq!(
            report2.entries()[0].status,
            PublishStatus::Pushed,
            "channel tag re-publish must push (move), not skip"
        );
        let second_digest = report2.entries()[0]
            .digest
            .clone()
            .expect("pushed entry must have digest");
        assert_ne!(first_digest, second_digest, "canary must move to the new digest");
    }

    #[tokio::test]
    async fn run_dry_run_pushes_nothing() {
        // --dry-run: run() returns dry-run statuses; nothing written to registry.
        use crate::api::publish_report::PublishStatus;
        use crate::oci::access::memory_registry::MemoryRegistry;

        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        make_test_manifest_sources(dir);

        let manifest_path = dir.join("publish.toml");
        std::fs::write(
            &manifest_path,
            "registry = \"localhost:5000\"\n\n[skills.test-skill]\nversion = \"0.1.0\"\n",
        )
        .unwrap();

        let registry = MemoryRegistry::new();
        let ctx = Context::with_access(tmp.path().to_path_buf(), registry.clone());
        let args = make_publish_args(manifest_path, vec![], None, true /* dry_run */, false);

        let (report, exit) = run(&ctx, &args).await.expect("dry-run must not error");
        assert_eq!(exit, crate::cli::exit_code::ExitCode::Success, "dry-run must exit 0");
        assert_eq!(report.entries().len(), 1);
        assert_eq!(
            report.entries()[0].status,
            PublishStatus::DryRun,
            "dry-run must produce DryRun status"
        );

        // Verify the registry has no blobs (nothing was actually pushed)
        let access: std::sync::Arc<dyn crate::oci::access::OciAccess> = std::sync::Arc::new(registry);
        let repo = crate::oci::Identifier::parse("localhost:5000/skills/test-skill").unwrap();
        let id = repo.clone_with_tag("0.1.0");
        let resolved = access
            .resolve_digest(&id, crate::oci::access::Operation::Query)
            .await
            .unwrap();
        assert!(
            resolved.is_none(),
            "dry-run must not push anything to registry; tag 0.1.0 found: {resolved:?}"
        );
    }

    // ── F1: skip-existing after a first real push reports Skipped not DryRun ─

    #[tokio::test]
    async fn run_skip_existing_after_push_reports_skipped_not_dryrun() {
        // ADR D3 amendment: an already-published entry under --dry-run must
        // report Skipped (honest — a real run would skip it too), not DryRun.
        // This locks the status-mapping claim in the ADR amendment.
        use crate::api::publish_report::PublishStatus;
        use crate::oci::access::memory_registry::MemoryRegistry;

        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        make_test_manifest_sources(dir);

        let manifest_path = dir.join("publish.toml");
        std::fs::write(
            &manifest_path,
            "registry = \"localhost:5000\"\n\n[skills.test-skill]\nversion = \"0.1.0\"\n",
        )
        .unwrap();

        // Step 1: real push (no dry-run)
        let registry = MemoryRegistry::new();
        let ctx = Context::with_access(tmp.path().to_path_buf(), registry.clone());
        let args = make_publish_args(manifest_path.clone(), vec![], None, false, false);
        let (report1, exit1) = run(&ctx, &args).await.expect("first run must succeed");
        assert_eq!(exit1, crate::cli::exit_code::ExitCode::Success);
        assert_eq!(
            report1.entries()[0].status,
            PublishStatus::Pushed,
            "first run must push"
        );

        // Step 2: dry-run on the same (already-pushed) entry.
        // skip-existing runs before the dry-run branch in release::run, so the
        // already-existing entry skips (Skipped), not DryRun.
        let ctx2 = Context::with_access(tmp.path().to_path_buf(), registry);
        let args2 = make_publish_args(manifest_path, vec![], None, true /* dry_run */, false);
        let (report2, exit2) = run(&ctx2, &args2).await.expect("dry-run after push must not error");
        assert_eq!(exit2, crate::cli::exit_code::ExitCode::Success);
        assert_eq!(
            report2.entries()[0].status,
            PublishStatus::Skipped,
            "dry-run on already-pushed entry must report Skipped, not DryRun (ADR D3 amendment)"
        );
    }

    // ── F2: resolve_force_skip covers all 3 branches ──────────────────────

    #[test]
    fn resolve_force_skip_no_tag_no_force_gives_default() {
        // (None, false) → (force=false, skip_existing=true): idempotent CI default
        assert_eq!(resolve_force_skip(None, false), (false, true));
    }

    #[test]
    fn resolve_force_skip_no_tag_force_true_gives_force() {
        // (None, true) → (force=true, skip_existing=false)
        assert_eq!(resolve_force_skip(None, true), (true, false));
    }

    #[test]
    fn resolve_force_skip_tag_present_forces_move() {
        // (Some("canary"), false) → (force=true, skip_existing=false)
        // A channel tag always moves regardless of the force flag.
        assert_eq!(resolve_force_skip(Some("canary"), false), (true, false));
    }

    // ── F3: string-valued kind table hints grim release --kind bundle ──────

    #[test]
    fn load_manifest_registry_with_string_kind_values_hints_grim_release() {
        // A TOML with `registry = "..."` AND string kind values looks like a
        // bundle file with a stray registry key. The post-parse fallback in
        // load_manifest (D7 guard after the full parse) must hint at
        // `grim release --kind bundle`.
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("publish.toml");
        // Crucially, this document has `registry` (so is_bundle_shaped() returns
        // false for it) but the kind table holds string values that serde will
        // reject as "expected table".
        std::fs::write(&path, "registry = \"r.example\"\n\n[skills]\nfoo = \"ghcr.io/x:1\"\n").unwrap();
        let err = load_manifest(&path).expect_err("string-valued skills table must be rejected");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("bundle") || msg.contains("grim release"),
            "error must hint at `grim release --kind bundle`, got: {msg}"
        );
        assert_eq!(crate::error::classify_error(&err), ExitCode::DataError);
    }

    // ── F4: --registry flag wins over manifest registry ────────────────────

    #[tokio::test]
    async fn run_registry_flag_wins_over_manifest_registry() {
        // ADR D1: --registry flag overrides the manifest's registry value.
        // References in the produced report must start with the flag registry.
        use crate::oci::access::memory_registry::MemoryRegistry;

        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        make_test_manifest_sources(dir);

        let manifest_path = dir.join("publish.toml");
        // Manifest says "manifest.example", flag says "flag.example".
        std::fs::write(
            &manifest_path,
            "registry = \"manifest.example\"\n\n[skills.test-skill]\nversion = \"0.1.0\"\n",
        )
        .unwrap();

        let registry = MemoryRegistry::new();
        // Inject both access AND registry flag.
        let ctx = Context::with_access_and_registry(tmp.path().to_path_buf(), registry, "localhost:5000".to_string());
        let args = make_publish_args(manifest_path, vec![], None, false, false);
        let (report, exit) = run(&ctx, &args).await.expect("run with flag registry must succeed");
        assert_eq!(exit, crate::cli::exit_code::ExitCode::Success);
        assert_eq!(report.entries().len(), 1);
        // The reference must start with the flag registry, not manifest.example.
        assert!(
            report.entries()[0].reference.starts_with("localhost:5000/"),
            "--registry flag must override manifest registry; got: {}",
            report.entries()[0].reference
        );
        assert!(
            !report.entries()[0].reference.contains("manifest.example"),
            "manifest registry must not appear when --registry flag is set; got: {}",
            report.entries()[0].reference
        );
    }

    // ── F6: --only foo with same name in [skills] and [rules] → 2 entries ─

    #[test]
    fn plan_entries_only_same_name_in_skills_and_rules_gives_two_entries_in_kind_order() {
        // F6: plan_entries with same name `foo` under [skills] and [rules]
        // + --only foo → 2 entries in kind order (skills before rules).
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        write(
            &dir.join("skills/foo/SKILL.md"),
            "---\nname: foo\ndescription: d.\n---\n",
        );
        write(&dir.join("rules/foo.md"), "---\npaths: ['*.rs']\n---\nbody\n");

        let manifest: PublishManifest = toml::from_str(
            "registry = \"localhost:5000\"\n\n[skills.foo]\nversion = \"0.1.0\"\n\n[rules.foo]\nversion = \"0.2.0\"\n",
        )
        .unwrap();

        let entries = plan_entries(&manifest, dir, "localhost:5000", &["foo".to_string()], None);
        assert_eq!(
            entries.len(),
            2,
            "--only foo with same name in skills+rules must yield 2 entries"
        );
        // Kind order: skills before rules (ADR D4)
        assert_eq!(
            entries[0].kind,
            crate::oci::ArtifactKind::Skill,
            "skill entry must come first"
        );
        assert_eq!(
            entries[1].kind,
            crate::oci::ArtifactKind::Rule,
            "rule entry must come second"
        );
        assert_eq!(entries[0].name, "foo");
        assert_eq!(entries[1].name, "foo");
    }

    // ── F7: is_strict_semver edge cases ──────────────────────────────────

    #[test]
    fn is_strict_semver_zero_patch_version_is_valid() {
        // "0.0.0" is a valid strict semver (all-zero is not a leading zero
        // violation — a leading zero in e.g. "01.0.0" is the violation).
        assert!(is_strict_semver("0.0.0"), "'0.0.0' must be accepted as strict semver");
    }

    #[test]
    fn is_strict_semver_empty_string_is_invalid() {
        assert!(!is_strict_semver(""), "empty string must not be accepted as semver");
    }

    // ── W1: load_manifest error messages (exact substring contract) ───────

    #[test]
    fn load_manifest_missing_file_says_manifest_not_found() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("nonexistent.toml");
        let err = load_manifest(&path).expect_err("missing file must error");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("manifest not found"),
            "missing manifest error must contain 'manifest not found', got: {msg}"
        );
        assert_eq!(crate::error::classify_error(&err), ExitCode::DataError);
    }

    #[test]
    fn load_manifest_oversized_file_mentions_64_kib_limit() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("big.toml");
        // Write a file exceeding 64 KiB
        let big = "x".repeat(65 * 1024 + 1);
        std::fs::write(&path, big).unwrap();
        let err = load_manifest(&path).expect_err("oversized file must error");
        let msg = format!("{err:#}");
        // Must mention the limit
        assert!(
            msg.contains("64") || msg.contains("KiB") || msg.contains("limit") || msg.contains("large"),
            "oversized manifest error must mention the 64 KiB limit, got: {msg}"
        );
        // Must NOT double-embed the path
        let path_str = path.to_string_lossy();
        let path_count = msg.matches(path_str.as_ref()).count();
        assert_eq!(path_count, 1, "path must appear exactly once in error, got: {msg}");
        assert_eq!(crate::error::classify_error(&err), ExitCode::DataError);
    }

    #[test]
    fn load_manifest_missing_file_has_single_path_in_message() {
        // W1 contract: data_error_at(path, msg) where msg contains NO path.
        // ConfigError Display embeds path already; using e.to_string() as msg
        // would yield "{path}: {path}: …" — this test guards against regression.
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("nonexistent.toml");
        let err = load_manifest(&path).expect_err("missing file must error");
        let msg = format!("{err:#}");
        let path_str = path.to_string_lossy();
        let path_count = msg.matches(path_str.as_ref()).count();
        assert_eq!(
            path_count, 1,
            "path must appear exactly once in error (no double-path), got: {msg}"
        );
    }
}
