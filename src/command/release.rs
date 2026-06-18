// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! `grim release` — validate, pack, and push a skill/rule with cascade
//! tags.
//!
//! `<ref>` is `registry/repo:version`. The artifact is validated+packed
//! (reusing `build`), the cascade tag set is computed from the version,
//! and then via the [`OciAccess`] seam: push the layer blob, push the
//! manifest (with annotations), then move every cascade tag onto the
//! manifest digest. The exact version tag is written FIRST so a crash
//! never leaves a floating tag (`1.2`/`1`/`latest`) pointing at a
//! manifest that has no specific tag. Re-releasing identical content is
//! an idempotent no-op (same digest). `--dry-run` prints the plan;
//! `--force` allows moving an existing exact-version tag that points
//! elsewhere.

use std::sync::Arc;

use clap::Args;

use crate::api::release_report::ReleaseReport;
use crate::cli::exit_code::ExitCode;
use crate::context::Context;
use crate::oci::access::{OciAccess, Operation};
use crate::oci::annotations::annotations_for_bundle;
use crate::oci::bundle::{BUNDLE_LAYER_MEDIA_TYPE, BundleManifest};
use crate::oci::manifest::{Descriptor, OciManifest};
use crate::oci::reference::ArtifactRef;
use crate::oci::release::{ReleaseError, ReleaseErrorKind, publish_tags};
use crate::oci::{Algorithm, ArtifactKind, Identifier};
use crate::resolve::resolve_error::{ResolveError, ResolveErrorKind};

use super::build::{detect_kind, read_bundle_members, validate_and_pack};

/// `grim release` arguments.
#[derive(Debug, Args)]
pub struct ReleaseArgs {
    /// Path to a skill directory or a rule `.md` file.
    pub path: std::path::PathBuf,

    /// The release reference: `registry/repo:version`.
    pub reference: String,

    /// Force the artifact kind instead of auto-detecting it.
    #[arg(long, value_parser = ["skill", "rule", "agent", "bundle"])]
    pub kind: Option<String>,

    /// Print the push plan (tags + digest) without pushing.
    #[arg(long)]
    pub dry_run: bool,

    /// Move an existing exact-version tag that points at a different
    /// digest (default: refuse).
    #[arg(long)]
    pub force: bool,

    /// Skip the release entirely (success, nothing pushed) when the
    /// exact-version tag already exists — for manifest-driven publishers
    /// that re-run blanket releases and only want bumped versions pushed.
    #[arg(long, conflicts_with = "force")]
    pub skip_existing: bool,

    /// For a bundle: resolve every floating member tag to a digest and
    /// freeze it into the published bundle (reproducible, tunnel-safe).
    /// Ignored for skills and rules.
    #[arg(long)]
    pub pin: bool,
}

/// Run `grim release`.
///
/// # Errors
///
/// A validation/pack failure (65/74), an invalid version (65), a refused
/// tag overwrite (65), or a registry/auth failure (69/80) propagate via
/// the typed error chain.
pub async fn run(ctx: &Context, args: &ReleaseArgs) -> anyhow::Result<(ReleaseReport, ExitCode)> {
    // Parse the release reference, expanding a short identifier against the
    // effective default registry (config `[options].default_registry` first,
    // then `--registry` / `GRIM_DEFAULT_REGISTRY`).
    let default_registry = release_default_registry(ctx);
    let id = super::grim(parse_reference(&args.reference, Some(&default_registry)))?;
    // The published tag is the reference tag; a reference with no tag is
    // rejected (a release must carry a tag). A non-version tag publishes
    // exactly itself (no cascade); full semver cascades.
    let version = id.tag().unwrap_or("").to_string();
    let tags = super::grim(publish_tags(&version))?;

    let kind = detect_kind(&args.path, args.kind.as_deref())?;
    let repo = id.without_tag();
    let source = repo.registry_repository();

    if kind == ArtifactKind::Bundle {
        return release_bundle(ctx, args, &id, &repo, &version, &tags, &source).await;
    }

    let packed = validate_and_pack(&args.path, kind, &version, Some(&source))?;

    let layer_digest = Algorithm::Sha256.hash(&packed.tar);
    let manifest = OciManifest {
        media_type: Some("application/vnd.oci.image.manifest.v1+json".to_string()),
        artifact_type: Some(kind.artifact_type().to_string()),
        // No config kind type on the wire (`adr_oci_empty_config_compat.md`):
        // the push path stamps the OCI empty config type, and the kind rides
        // on `artifactType` + the `com.grimoire.kind` annotation. Keep the
        // in-memory manifest faithful to what is pushed.
        config_media_type: None,
        layers: vec![Descriptor {
            digest: layer_digest.clone(),
            media_type: "application/vnd.grimoire.artifact.layer.v1.tar".to_string(),
            size: packed.tar.len() as u64,
        }],
        annotations: packed.annotations.clone(),
    };

    let access: Arc<dyn OciAccess> = super::access_seam(ctx)?;

    // --skip-existing: an exact-version tag that already exists (any
    // digest) turns the release into a success no-op before anything is
    // pushed. A lookup failure counts as "absent" — the push path surfaces
    // real transport errors.
    if args.skip_existing
        && let Some(existing) = resolve_existing_version(&access, &repo, &version).await
    {
        let report = ReleaseReport::new(id.to_string(), existing.to_string(), Vec::new(), false);
        return Ok((report, ExitCode::Success));
    }

    if args.dry_run {
        // No push: report the plan with a deterministic preview digest.
        let preview = preview_manifest_digest(&manifest);
        let report = ReleaseReport::new(id.to_string(), preview, tags, false);
        return Ok((report, ExitCode::Success));
    }

    // Push blob + manifest first. Both are content-addressed, so a
    // re-push of identical content is an idempotent no-op that yields the
    // same `manifest_digest` — nothing observable changes until a tag is
    // moved.
    super::grim(access.push_blob(&repo, &packed.tar).await)?;
    let manifest_digest = super::grim(access.push_manifest(&repo, &manifest).await)?;

    // Overwrite guard: if the exact-version tag already resolves to a
    // *different* manifest digest, refuse unless --force (a published
    // version is immutable by default; an identical re-release is a
    // no-op success).
    if !args.force {
        super::grim(guard_existing_version(&access, &repo, &version, &manifest_digest).await)?;
    }

    // Move the exact version tag FIRST, then the wider floating tags last
    // (crash safety: `1.2.3` exists before `1.2`/`1`/`latest` move to it).
    super::grim(move_tags(&access, &repo, &tags, &version, &manifest_digest).await)?;

    let report = ReleaseReport::new(id.to_string(), manifest_digest.to_string(), tags, true);
    Ok((report, ExitCode::Success))
}

/// Release a bundle: pack its members document, optionally freezing
/// floating member tags to digests (`--pin`), then push blob + manifest +
/// cascade tags exactly like a skill/rule release.
#[allow(clippy::too_many_arguments)]
async fn release_bundle(
    ctx: &Context,
    args: &ReleaseArgs,
    id: &Identifier,
    repo: &Identifier,
    version: &str,
    tags: &[String],
    source: &str,
) -> anyhow::Result<(ReleaseReport, ExitCode)> {
    let (name, mut members, metadata) = read_bundle_members(&args.path)?;
    let access: Arc<dyn OciAccess> = super::access_seam(ctx)?;

    // Same --skip-existing semantics as the skill/rule/agent path.
    if args.skip_existing
        && let Some(existing) = resolve_existing_version(&access, repo, version).await
    {
        let report = ReleaseReport::new(id.to_string(), existing.to_string(), Vec::new(), false);
        return Ok((report, ExitCode::Success));
    }

    // `--pin`: resolve every floating member to a digest and bake it in, so
    // the published bundle is fully reproducible regardless of later tag
    // movement (the strong guarantee air-gapped / tunneled consumers want).
    if args.pin {
        super::grim(pin_members(&access, &mut members).await)?;
    }

    let manifest = BundleManifest::new(members);
    let layer = manifest
        .to_layer_bytes()
        .map_err(|e| anyhow::anyhow!("failed to serialize bundle layer: {e}"))?;
    let layer_digest = Algorithm::Sha256.hash(&layer);
    // An authored `repository` URL wins over the release-ref fallback
    // inside `annotations_for_bundle`.
    let annotations = annotations_for_bundle(&name, version, manifest.members.len(), Some(source), &metadata);
    let oci_manifest = OciManifest {
        media_type: Some("application/vnd.oci.image.manifest.v1+json".to_string()),
        artifact_type: Some(ArtifactKind::Bundle.artifact_type().to_string()),
        // OCI empty config on the wire — see the skill/rule path in `run`.
        config_media_type: None,
        layers: vec![Descriptor {
            digest: layer_digest.clone(),
            media_type: BUNDLE_LAYER_MEDIA_TYPE.to_string(),
            size: layer.len() as u64,
        }],
        annotations,
    };

    if args.dry_run {
        let preview = preview_manifest_digest(&oci_manifest);
        let report = ReleaseReport::new(id.to_string(), preview, tags.to_vec(), false);
        return Ok((report, ExitCode::Success));
    }

    super::grim(access.push_blob(repo, &layer).await)?;
    let manifest_digest = super::grim(access.push_manifest(repo, &oci_manifest).await)?;

    if !args.force {
        super::grim(guard_existing_version(&access, repo, version, &manifest_digest).await)?;
    }
    super::grim(move_tags(&access, repo, tags, version, &manifest_digest).await)?;

    let report = ReleaseReport::new(id.to_string(), manifest_digest.to_string(), tags.to_vec(), true);
    Ok((report, ExitCode::Success))
}

/// Resolve every floating member to a digest in place. A member already
/// pinned is left untouched. Failures carry the member as context.
async fn pin_members(
    access: &Arc<dyn OciAccess>,
    members: &mut [crate::oci::bundle::BundleMember],
) -> Result<(), ResolveError> {
    for member in members.iter_mut() {
        let mid = Identifier::parse(&member.id).map_err(|_| {
            member_error(
                member,
                ResolveErrorKind::BundleInvalid(format!("invalid member identifier '{}'", member.id)),
            )
        })?;
        if mid.digest().is_some() {
            continue;
        }
        let digest = access
            .resolve_digest(&mid, Operation::Resolve)
            .await
            .map_err(|e| member_error(member, ResolveErrorKind::RegistryUnreachable(e)))?
            .ok_or_else(|| member_error(member, ResolveErrorKind::TagNotFound))?;
        member.id = mid.clone_with_digest(digest).to_string();
    }
    Ok(())
}

/// Build a [`ResolveError`] carrying a bundle member as its reference.
fn member_error(member: &crate::oci::bundle::BundleMember, kind: ResolveErrorKind) -> ResolveError {
    let id = Identifier::parse(&member.id)
        .unwrap_or_else(|_| Identifier::new_registry(member.name.clone(), "invalid.localhost"));
    ResolveError::new(
        ArtifactRef {
            kind: member.kind,
            name: member.name.clone(),
            id,
        },
        kind,
    )
}

/// Parse `<ref>`, expanding a short identifier against `default_registry`
/// when one is configured.
fn parse_reference(
    reference: &str,
    default_registry: Option<&str>,
) -> Result<Identifier, crate::oci::identifier::error::IdentifierError> {
    match default_registry {
        Some(def) => Identifier::parse_with_default_registry(reference, def),
        None => Identifier::parse(reference),
    }
}

/// The effective default registry for the publish reference, via the
/// centralized precedence helper. Precedence (highest first): the
/// `--registry` flag, then `GRIM_DEFAULT_REGISTRY`, then the discovered
/// project config `[options].default_registry` (best-effort — a publish run
/// from outside a project tree simply has none), then the global config
/// `[options].default_registry`, then the built-in
/// [`crate::command::FALLBACK_REGISTRY`]. A release is never a global-scope
/// operation, so the global config is always consulted as a fallback
/// (project scope). Owned so the transient discovered config can drop.
fn release_default_registry(ctx: &Context) -> String {
    let project_default = crate::config::project_config::ProjectConfig::discover(None)
        .ok()
        .and_then(|d| d.config.options.default_registry);
    let global_default = super::global_config_default(ctx, crate::config::scope::ConfigScope::Project);
    super::resolve_default_registry(ctx, project_default.as_deref(), global_default.as_deref())
}

/// Move every cascade tag onto `digest`. The exact version (`version`) is
/// moved before the wider floating tags for crash safety.
async fn move_tags(
    access: &Arc<dyn OciAccess>,
    repo: &Identifier,
    tags: &[String],
    version: &str,
    digest: &crate::oci::Digest,
) -> Result<(), crate::oci::access::error::AccessError> {
    access.put_tag(repo, version, digest).await?;
    for tag in tags {
        if tag == version {
            continue;
        }
        access.put_tag(repo, tag, digest).await?;
    }
    Ok(())
}

/// Resolve the digest an exact-version tag currently points at, if any.
/// A lookup failure is treated as "no existing tag" — the push path
/// surfaces any real transport failure.
async fn resolve_existing_version(
    access: &Arc<dyn OciAccess>,
    repo: &Identifier,
    version: &str,
) -> Option<crate::oci::Digest> {
    let tagged = repo.clone_with_tag(version);
    access
        .resolve_digest(&tagged, crate::oci::access::Operation::Query)
        .await
        .ok()
        .flatten()
}

/// Refuse to move an existing exact-version tag onto a different digest.
/// An absent tag, or a tag already pointing at `new_digest` (idempotent
/// re-release), is allowed.
async fn guard_existing_version(
    access: &Arc<dyn OciAccess>,
    repo: &Identifier,
    version: &str,
    new_digest: &crate::oci::Digest,
) -> Result<(), ReleaseError> {
    let Some(existing_digest) = resolve_existing_version(access, repo, version).await else {
        return Ok(());
    };
    if &existing_digest == new_digest {
        return Ok(());
    }
    Err(ReleaseError::with_reference(
        repo.clone(),
        ReleaseErrorKind::TagExists {
            tag: version.to_string(),
            existing: existing_digest.to_string(),
            new: new_digest.to_string(),
        },
    ))
}

/// A deterministic, non-authoritative preview of the manifest digest for
/// `--dry-run` output. The real digest is whatever the registry returns
/// on the actual push (the overwrite guard uses that, not this); the
/// preview only has to be stable for identical content so the printed
/// plan does not flap.
fn preview_manifest_digest(manifest: &OciManifest) -> String {
    let mut key = String::new();
    if let Some(at) = &manifest.artifact_type {
        key.push_str(&format!("artifactType={at}\n"));
    }
    if let Some(cmt) = &manifest.config_media_type {
        key.push_str(&format!("configMediaType={cmt}\n"));
    }
    for d in &manifest.layers {
        key.push_str(&format!("{}|{}|{}\n", d.digest, d.media_type, d.size));
    }
    for (k, v) in &manifest.annotations {
        // `created` is the only non-deterministic annotation; exclude it
        // so a dry-run preview is stable for identical content.
        if k == "org.opencontainers.image.created" {
            continue;
        }
        key.push_str(&format!("{k}={v}\n"));
    }
    Algorithm::Sha256.hash(key.as_bytes()).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::options::{GlobalOptions, OutputFormat};

    fn opts(registry: Option<&str>) -> GlobalOptions {
        GlobalOptions {
            format: OutputFormat::Plain,
            offline: false,
            log_level: None,
            config: None,
            global: false,
            registry: registry.map(str::to_string),
        }
    }

    #[test]
    fn release_default_registry_honors_flag_tier() {
        // The `--registry` flag is the top tier and must win through the
        // composed `release_default_registry` chain — the refactor that
        // wired the global-config fallback in must not disturb it.
        let ctx = Context::new(&opts(Some("flag.example")));
        assert_eq!(release_default_registry(&ctx), "flag.example");
    }

    #[test]
    fn release_default_registry_consults_global_tier_then_builtin() {
        // Regression for the skipped global-config tier: the publish path now
        // routes through the centralized `global_config_default` (project
        // scope, so the global config is a live fallback) instead of passing
        // a hard-coded `None`. With no flag / env / project-or-global config
        // present in the test environment the built-in fallback applies, but
        // the call chain — not a literal — produced it. The flag tier above
        // proves the chain still orders correctly; the global-tier disk read
        // is exercised end-to-end by `test_default_registry.py`.
        //
        // Hermetic context: the developer's $GRIM_DEFAULT_REGISTRY /
        // $GRIM_HOME must not leak in. The project tier still walks the
        // CWD (`ProjectConfig::discover(None)` is not injectable here);
        // it stays `None` because the repo's own `grimoire.toml` carries
        // no `default_registry` — keep it that way.
        let tmp = tempfile::tempdir().unwrap();
        let ctx = Context::hermetic(tmp.path().to_path_buf());
        assert_eq!(release_default_registry(&ctx), crate::command::FALLBACK_REGISTRY);
    }

    #[test]
    fn preview_digest_is_stable() {
        let m = OciManifest {
            media_type: None,
            artifact_type: Some(ArtifactKind::Skill.artifact_type().to_string()),
            // Mirrors the wire: no config kind type since
            // `adr_oci_empty_config_compat.md`.
            config_media_type: None,
            layers: vec![Descriptor {
                digest: Algorithm::Sha256.hash(b"x"),
                media_type: "t".to_string(),
                size: 1,
            }],
            annotations: std::collections::BTreeMap::new(),
        };
        assert_eq!(preview_manifest_digest(&m), preview_manifest_digest(&m));
    }

    fn manifest_of(tar: &[u8]) -> OciManifest {
        OciManifest {
            media_type: Some("application/vnd.oci.image.manifest.v1+json".to_string()),
            artifact_type: Some(ArtifactKind::Skill.artifact_type().to_string()),
            // OCI empty config on the wire — see `adr_oci_empty_config_compat.md`.
            config_media_type: None,
            layers: vec![Descriptor {
                digest: Algorithm::Sha256.hash(tar),
                media_type: "application/vnd.grimoire.artifact.layer.v1.tar".to_string(),
                size: tar.len() as u64,
            }],
            annotations: std::collections::BTreeMap::new(),
        }
    }

    /// End-to-end push against the in-memory registry double: blob +
    /// manifest + every cascade tag, then idempotent re-release, then a
    /// refused overwrite without `--force`.
    #[tokio::test]
    async fn memory_registry_release_pushes_cascade_idempotent_and_guards() {
        use crate::oci::access::memory_registry::MemoryRegistry;

        let registry = MemoryRegistry::new();
        let access: Arc<dyn OciAccess> = Arc::new(registry.clone());
        let repo = Identifier::parse("localhost:5000/acme/code-review").unwrap();
        let tar = b"skill tarball v1".to_vec();
        let manifest = manifest_of(&tar);
        let tags = publish_tags("1.2.3").unwrap();

        // First release: blob + manifest + all four cascade tags.
        access.push_blob(&repo, &tar).await.unwrap();
        let digest = access.push_manifest(&repo, &manifest).await.unwrap();
        guard_existing_version(&access, &repo, "1.2.3", &digest)
            .await
            .expect("no prior tag ⇒ no guard");
        move_tags(&access, &repo, &tags, "1.2.3", &digest).await.unwrap();

        for tag in ["1.2.3", "1.2", "1", "latest"] {
            let id = repo.clone_with_tag(tag);
            let resolved = access
                .resolve_digest(&id, crate::oci::access::Operation::Query)
                .await
                .unwrap()
                .expect("cascade tag resolves");
            assert_eq!(resolved, digest, "{tag} must point at the manifest digest");
        }

        // Idempotent re-release of identical content ⇒ same digest, guard
        // allows it (the tag already points at the same digest).
        let digest2 = access.push_manifest(&repo, &manifest).await.unwrap();
        assert_eq!(digest, digest2, "re-release of identical content is idempotent");
        guard_existing_version(&access, &repo, "1.2.3", &digest2)
            .await
            .expect("identical re-release is a no-op success");

        // Different content at the same version ⇒ refuse without --force.
        let other = manifest_of(b"skill tarball v2 DIFFERENT");
        let other_digest = access.push_manifest(&repo, &other).await.unwrap();
        assert_ne!(digest, other_digest);
        let err = guard_existing_version(&access, &repo, "1.2.3", &other_digest)
            .await
            .expect_err("overwriting a version with different content must refuse");
        assert!(matches!(err.kind, ReleaseErrorKind::TagExists { .. }));
    }
}
