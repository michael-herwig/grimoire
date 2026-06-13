// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! Per-artifact install with the local-modification integrity gate.
//!
//! This is the grimoire divergence from a plain OCI pull: before
//! overwriting anything, an already-installed artifact whose on-disk
//! content no longer matches the recorded content hash is treated as
//! user-modified and the install is refused unless `force` is set. The
//! happy path fetches the pinned blob, materializes it into a sibling temp
//! directory, atomically replaces the target, recomputes the content hash,
//! and records the new install state.
//!
//! Order-preserving: outcomes are returned in the lock's
//! skills-then-rules iteration order so the caller can build a stable
//! report.

use std::sync::Arc;

use crate::lock::grimoire_lock::GrimoireLock;
use crate::lock::locked_artifact::LockedArtifact;
use crate::oci::access::OciAccess;
use crate::oci::reference::ArtifactRef;
use crate::oci::{ArtifactKind, Digest, Identifier};

use super::content_hash::footprint_hash;
use super::install_error::{InstallError, InstallErrorKind};
use super::install_state::{InstallRecord, InstallState};
use super::materializer::ArtifactMaterializer;
use super::path_anchor::AnchorRoots;
use super::target::InstallTarget;

/// What happened to one artifact during an install pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallOutcome {
    /// Freshly installed (no prior state).
    Installed,
    /// Reinstalled over a different prior pin / content.
    Updated,
    /// Already installed at the locked pin with intact content — no-op.
    AlreadyInstalled,
    /// Skipped for a benign reason (carried for forward use).
    Skipped(String),
    /// Refused: locally modified and `force` was not set. Carries the
    /// recorded vs. on-disk content hash so the caller can build a precise
    /// integrity error.
    Refused { recorded: Digest, actual: Digest },
}

/// One artifact's install result, paired with its reference for reporting.
///
/// The error is the top-level [`crate::error::Error`] (not just
/// [`InstallError`]) so a fetch failure carries its real subsystem
/// taxonomy — an offline miss must classify as `OfflineBlocked` (81), an
/// auth failure as `AuthError` (80), etc., not be flattened into a
/// generic install error.
#[derive(Debug)]
pub struct ArtifactInstall {
    /// The artifact this result is about.
    pub reference: ArtifactRef,
    /// The on-disk path the artifact installs to.
    pub target: std::path::PathBuf,
    /// The outcome (or the error if the install failed).
    pub result: Result<InstallOutcome, crate::error::Error>,
}

/// Install every locked artifact, in skills-then-rules-then-agents order.
///
/// `force` overrides the integrity gate (a locally modified artifact is
/// overwritten instead of refused). The first hard error for an artifact
/// is recorded against that artifact; siblings still process so the report
/// reflects the whole set. Each artifact is materialized into every
/// client target the [`InstallTarget`] selects.
pub async fn install_all<M: ArtifactMaterializer>(
    lock: &GrimoireLock,
    access: &Arc<dyn OciAccess>,
    materializer: &M,
    target: &InstallTarget,
    state: &mut InstallState,
    roots: &AnchorRoots,
    force: bool,
) -> Vec<ArtifactInstall> {
    let work: Vec<(&LockedArtifact, ArtifactKind)> = lock.iter_artifacts().map(|a| (a, a.kind)).collect();

    let mut results = Vec::with_capacity(work.len());
    for (artifact, kind) in work {
        let reference = ArtifactRef {
            kind,
            name: artifact.name.clone(),
            id: artifact.pinned.as_identifier().clone(),
        };
        // The primary client's path is the report target (back-compat).
        let primary = target
            .clients()
            .first()
            .copied()
            .unwrap_or(crate::install::client_target::ClientTarget::Claude);
        let report_target = target.path_for(primary, kind, &artifact.name);
        let result = install_one(artifact, kind, access, materializer, target, state, roots, force).await;
        results.push(ArtifactInstall {
            reference,
            target: report_target,
            result,
        });
    }
    results
}

/// Install one artifact into every selected client through the integrity
/// gate.
#[allow(clippy::too_many_arguments)]
async fn install_one<M: ArtifactMaterializer>(
    artifact: &LockedArtifact,
    kind: ArtifactKind,
    access: &Arc<dyn OciAccess>,
    materializer: &M,
    target: &InstallTarget,
    state: &mut InstallState,
    roots: &AnchorRoots,
    force: bool,
) -> Result<InstallOutcome, crate::error::Error> {
    use crate::install::install_state::ClientOutput;

    let recorded = state.get(kind, &artifact.name).cloned();
    let pinned_str = artifact.pinned.strip_advisory().to_string();

    // Integrity gate: for every client output a prior record described,
    // an on-disk content hash that drifted from what was recorded is a
    // local modification. Refuse unless forced; if every output is intact
    // and the pin is unchanged the install is a no-op.
    if let Some(rec) = &recorded {
        let mut all_intact = true;
        for out in &rec.outputs {
            let resolved = out.resolved_target(roots)?;
            if resolved.exists() {
                let actual = out.current_hash(roots)?;
                if actual != out.content_hash {
                    if !force {
                        return Ok(InstallOutcome::Refused {
                            recorded: out.content_hash.clone(),
                            actual,
                        });
                    }
                    all_intact = false;
                }
            } else {
                all_intact = false;
            }
        }
        if all_intact && rec.pinned.eq_content(&artifact.pinned) {
            return Ok(InstallOutcome::AlreadyInstalled);
        }
    }

    // `artifact.pinned` is the *manifest* digest. Resolve the manifest to
    // its single layer descriptor, then fetch that layer blob (the
    // artifact tar). An access failure (offline miss, auth, registry)
    // propagates with its own taxonomy so the exit code is correct
    // (81/80/69/...).
    let repo: Identifier = artifact.pinned.as_identifier().without_tag();
    let aref = || ArtifactRef {
        kind,
        name: artifact.name.clone(),
        id: artifact.pinned.as_identifier().clone(),
    };

    let manifest = access.fetch_manifest(&artifact.pinned).await?;
    let Some(manifest) = manifest else {
        return Err(InstallError::with_reference(aref(), InstallErrorKind::BlobMissing).into());
    };
    let Some(layer) = manifest.single_layer() else {
        return Err(InstallError::with_reference(
            aref(),
            InstallErrorKind::MaterializeFailed(format!(
                "expected a single-layer artifact, manifest has {} layers",
                manifest.layers.len()
            )),
        )
        .into());
    };
    let layer_digest = layer.digest.clone();

    let blob = access.fetch_blob(&repo, &layer_digest).await?;
    let Some(blob) = blob else {
        return Err(InstallError::with_reference(aref(), InstallErrorKind::BlobMissing).into());
    };

    // Defence in depth: verify blob bytes hash to the layer digest before
    // materializing. `CachedAccess`/`RegistryClient` already verify, but
    // the seam contract allows a mock that does not.
    let actual_blob_digest = layer_digest.algorithm().hash(&blob);
    if actual_blob_digest != layer_digest {
        return Err(InstallError::without_reference(InstallErrorKind::BlobDigestMismatch {
            expected: layer_digest.clone(),
            actual: actual_blob_digest,
        })
        .into());
    }

    // Materialize the canonical tree once into a temp dir; every client
    // target then transforms/copies from that single extracted tree.
    let staging = tempfile::Builder::new()
        .prefix(".grim-staging-")
        .tempdir_in(std::env::temp_dir())
        .map_err(|e| target_io(std::env::temp_dir().as_path(), e))?;
    let materialized_root = staging.path().join("content");
    materializer.materialize(kind, &artifact.name, &blob, &materialized_root)?;

    let canonical = match kind {
        ArtifactKind::Skill => materialized_root.join(&artifact.name),
        ArtifactKind::Rule | ArtifactKind::Agent => materialized_root.join(format!("{}.md", artifact.name)),
        // Bundles expand into members at resolve time and never enter the
        // lock, so the installer never sees one.
        ArtifactKind::Bundle => unreachable!("bundles are never materialized; they expand into members"),
    };
    if !canonical.exists() {
        return Err(
            InstallError::without_reference(InstallErrorKind::MaterializeFailed(format!(
                "artifact '{}' ({kind}) did not produce the expected '{}' entry",
                artifact.name,
                canonical.display()
            )))
            .into(),
        );
    }

    // A rule may carry a sibling support directory staged beside the index
    // file (`<root>/<name>/…`); a plain single-file rule has none. Skills
    // are a single directory tree, never a support dir; agents are a
    // single file with no support-directory contract.
    let staged_support: Option<std::path::PathBuf> = match kind {
        ArtifactKind::Rule => {
            let dir = materialized_root.join(&artifact.name);
            dir.is_dir().then_some(dir)
        }
        _ => None,
    };

    // Materialize into every selected client's final path, replacing any
    // prior output, and hash each client output for the integrity record.
    let mut client_records: Vec<ClientOutput> = Vec::with_capacity(target.clients().len());
    for client in target.clients() {
        let dest = target.path_for(*client, kind, &artifact.name);
        // Copilot documents no user-level instructions location: a
        // global-scope rule lands in the workspace layout, which Copilot
        // never scans. Install proceeds (consistent footprint) but warn.
        if kind == ArtifactKind::Rule
            && *client == crate::install::client_target::ClientTarget::Copilot
            && target.scope() == crate::config::scope::ConfigScope::Global
        {
            tracing::warn!(
                "Copilot has no user-level instructions path; global rule '{}' will not be discovered by Copilot",
                artifact.name
            );
        }
        // A rule's support dir always lives at `<parent>/<name>/`, whether
        // or not *this* version ships one. `cleanup` is that location (so a
        // version that drops its support dir still reaps the stale one);
        // `support_dest` is `Some` only when this version actually
        // materializes one (so the record + footprint hash cover it).
        let cleanup = match kind {
            ArtifactKind::Rule => dest.parent().map(|parent| parent.join(&artifact.name)),
            _ => None,
        };
        let support_dest = staged_support.as_ref().and(cleanup.clone());

        if dest.exists() {
            remove_path(&dest).map_err(|e| target_io(&dest, e))?;
        }
        if let Some(sd) = &cleanup
            && sd.exists()
        {
            remove_path(sd).map_err(|e| target_io(sd, e))?;
        }
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(|e| target_io(parent, e))?;
        }
        client
            .materialize(
                kind,
                &artifact.name,
                &canonical,
                &dest,
                &pinned_str,
                staged_support.as_deref(),
            )
            .map_err(crate::error::Error::from)?;
        fsync_tree(&dest).map_err(|e| target_io(&dest, e))?;
        if let Some(sd) = &support_dest {
            fsync_tree(sd).map_err(|e| target_io(sd, e))?;
        }
        #[cfg(unix)]
        if let Some(parent) = dest.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::File::open(parent)
                .and_then(|f| f.sync_all())
                .map_err(|e| target_io(parent, e))?;
        }
        let installed_hash = footprint_hash(&dest, support_dest.as_deref()).map_err(|e| target_io(&dest, e))?;
        // `dest` / `support_dest` are the non-canonicalized (pre-symlink)
        // forms — the `from_target` caller invariant (§1.5).
        let anchored_target =
            crate::install::path_anchor::AnchoredPath::from_target(&dest, target.scope(), *client, kind, roots)?;
        let anchored_support = match &support_dest {
            Some(sd) => Some(crate::install::path_anchor::AnchoredPath::from_target(
                sd,
                target.scope(),
                *client,
                kind,
                roots,
            )?),
            None => None,
        };
        client_records.push(ClientOutput {
            client: client.to_string(),
            target: anchored_target,
            content_hash: installed_hash,
            support_dir: anchored_support,
        });
    }

    // `outputs` is the single source of truth — no denormalized top-level
    // mirror of the primary client.
    state.record(InstallRecord {
        kind,
        name: artifact.name.clone(),
        pinned: artifact.pinned.clone(),
        outputs: client_records,
    });

    Ok(if recorded.is_some() {
        InstallOutcome::Updated
    } else {
        InstallOutcome::Installed
    })
}

/// Remove `path` whether it is a file or a directory.
fn remove_path(path: &std::path::Path) -> std::io::Result<()> {
    let meta = std::fs::symlink_metadata(path)?;
    if meta.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    }
}

/// fsync a freshly materialized file or directory tree so the rename that
/// publishes it is durable across a crash (Unix only — opening a directory
/// as a file is not portable).
fn fsync_tree(path: &std::path::Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        let meta = std::fs::symlink_metadata(path)?;
        if meta.is_dir() {
            for entry in std::fs::read_dir(path)? {
                fsync_tree(&entry?.path())?;
            }
        }
        std::fs::File::open(path)?.sync_all()
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

fn target_io(path: &std::path::Path, source: std::io::Error) -> InstallError {
    InstallError::without_reference(InstallErrorKind::TargetIo {
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::path::Path;

    use crate::install::path_anchor::{AnchorRoots, AnchoredPath, PathAnchor};
    use crate::lock::grimoire_lock::LockMetadata;
    use crate::lock::lock_version::LockVersion;
    use crate::oci::access::Operation;
    use crate::oci::access::error::AccessError;
    use crate::oci::manifest::{Descriptor, OciManifest};
    use crate::oci::pinned_identifier::PinnedIdentifier;
    use crate::oci::{Algorithm, Digest};

    /// Build `AnchorRoots` rooted at `workspace` for tests.
    fn roots(workspace: &std::path::Path) -> AnchorRoots {
        AnchorRoots {
            workspace: workspace.to_path_buf(),
            grim_home: workspace.to_path_buf(),
            claude_root: None,
            copilot_root: None,
            opencode_skills: None,
        }
    }

    use super::super::materializer::DefaultMaterializer;

    /// A single-layer manifest whose layer digest = sha256(`blob`).
    fn manifest_for(blob: &[u8]) -> OciManifest {
        OciManifest {
            media_type: Some("application/vnd.oci.image.manifest.v1+json".to_string()),
            artifact_type: Some("application/vnd.grimoire.skill.v1".to_string()),
            config_media_type: Some("application/vnd.grimoire.skill.config.v1+json".to_string()),
            layers: vec![Descriptor {
                digest: Algorithm::Sha256.hash(blob),
                media_type: "application/vnd.grimoire.artifact.layer.v1.tar".to_string(),
                size: blob.len() as u64,
            }],
            annotations: std::collections::BTreeMap::new(),
        }
    }

    /// Mock that serves one manifest + its layer blob.
    struct BlobMock {
        blob: Vec<u8>,
    }

    #[async_trait]
    impl OciAccess for BlobMock {
        async fn resolve_digest(&self, _id: &Identifier, _op: Operation) -> Result<Option<Digest>, AccessError> {
            Ok(None)
        }
        async fn fetch_manifest(&self, _id: &PinnedIdentifier) -> Result<Option<OciManifest>, AccessError> {
            Ok(Some(manifest_for(&self.blob)))
        }
        async fn fetch_blob(&self, _repo: &Identifier, _digest: &Digest) -> Result<Option<Vec<u8>>, AccessError> {
            Ok(Some(self.blob.clone()))
        }
        async fn list_tags(&self, _id: &Identifier) -> Result<Option<Vec<String>>, AccessError> {
            Ok(None)
        }
        async fn list_catalog(&self, _registry: &str) -> Result<Vec<String>, AccessError> {
            Ok(Vec::new())
        }
        async fn push_blob(&self, _repo: &Identifier, bytes: &[u8]) -> Result<Digest, AccessError> {
            Ok(Algorithm::Sha256.hash(bytes))
        }
        async fn push_manifest(&self, _repo: &Identifier, _m: &OciManifest) -> Result<Digest, AccessError> {
            Ok(Algorithm::Sha256.hash(b"m"))
        }
        async fn put_tag(&self, _repo: &Identifier, _t: &str, _d: &Digest) -> Result<(), AccessError> {
            Ok(())
        }
    }

    /// Mock that serves a manifest but no layer blob.
    struct MissingMock {
        blob: Vec<u8>,
    }

    #[async_trait]
    impl OciAccess for MissingMock {
        async fn resolve_digest(&self, _id: &Identifier, _op: Operation) -> Result<Option<Digest>, AccessError> {
            Ok(None)
        }
        async fn fetch_manifest(&self, _id: &PinnedIdentifier) -> Result<Option<OciManifest>, AccessError> {
            Ok(Some(manifest_for(&self.blob)))
        }
        async fn fetch_blob(&self, _repo: &Identifier, _digest: &Digest) -> Result<Option<Vec<u8>>, AccessError> {
            Ok(None)
        }
        async fn list_tags(&self, _id: &Identifier) -> Result<Option<Vec<String>>, AccessError> {
            Ok(None)
        }
        async fn list_catalog(&self, _registry: &str) -> Result<Vec<String>, AccessError> {
            Ok(Vec::new())
        }
        async fn push_blob(&self, _repo: &Identifier, bytes: &[u8]) -> Result<Digest, AccessError> {
            Ok(Algorithm::Sha256.hash(bytes))
        }
        async fn push_manifest(&self, _repo: &Identifier, _m: &OciManifest) -> Result<Digest, AccessError> {
            Ok(Algorithm::Sha256.hash(b"m"))
        }
        async fn put_tag(&self, _repo: &Identifier, _t: &str, _d: &Digest) -> Result<(), AccessError> {
            Ok(())
        }
    }

    /// Mock whose manifest's layer digest does not match the served blob
    /// bytes (corrupt-registry simulation).
    struct WrongBlobMock {
        manifest_blob: Vec<u8>,
        served_blob: Vec<u8>,
    }

    #[async_trait]
    impl OciAccess for WrongBlobMock {
        async fn resolve_digest(&self, _id: &Identifier, _op: Operation) -> Result<Option<Digest>, AccessError> {
            Ok(None)
        }
        async fn fetch_manifest(&self, _id: &PinnedIdentifier) -> Result<Option<OciManifest>, AccessError> {
            Ok(Some(manifest_for(&self.manifest_blob)))
        }
        async fn fetch_blob(&self, _repo: &Identifier, _digest: &Digest) -> Result<Option<Vec<u8>>, AccessError> {
            Ok(Some(self.served_blob.clone()))
        }
        async fn list_tags(&self, _id: &Identifier) -> Result<Option<Vec<String>>, AccessError> {
            Ok(None)
        }
        async fn list_catalog(&self, _registry: &str) -> Result<Vec<String>, AccessError> {
            Ok(Vec::new())
        }
        async fn push_blob(&self, _repo: &Identifier, bytes: &[u8]) -> Result<Digest, AccessError> {
            Ok(Algorithm::Sha256.hash(bytes))
        }
        async fn push_manifest(&self, _repo: &Identifier, _m: &OciManifest) -> Result<Digest, AccessError> {
            Ok(Algorithm::Sha256.hash(b"m"))
        }
        async fn put_tag(&self, _repo: &Identifier, _t: &str, _d: &Digest) -> Result<(), AccessError> {
            Ok(())
        }
    }

    fn rule_tar(name: &str, body: &[u8]) -> Vec<u8> {
        let mut b = tar::Builder::new(Vec::new());
        let mut h = tar::Header::new_gnu();
        h.set_size(body.len() as u64);
        h.set_mode(0o644);
        h.set_cksum();
        b.append_data(&mut h, format!("{name}.md"), body).unwrap();
        b.into_inner().unwrap()
    }

    /// A multi-file rule tar: the index `<name>.md` plus `<name>/<rel>`
    /// support entries.
    fn multi_rule_tar(name: &str, index: &[u8], support: &[(&str, &[u8])]) -> Vec<u8> {
        let mut b = tar::Builder::new(Vec::new());
        let mut push = |path: String, body: &[u8]| {
            let mut h = tar::Header::new_gnu();
            h.set_size(body.len() as u64);
            h.set_mode(0o644);
            h.set_cksum();
            b.append_data(&mut h, path, body).unwrap();
        };
        push(format!("{name}.md"), index);
        for (rel, body) in support {
            push(format!("{name}/{rel}"), body);
        }
        b.into_inner().unwrap()
    }

    fn locked_rule(name: &str, blob: &[u8]) -> LockedArtifact {
        let digest = Algorithm::Sha256.hash(blob);
        let id = Identifier::new_registry(name, "localhost:5000").clone_with_digest(digest);
        LockedArtifact::direct(
            name.to_string(),
            ArtifactKind::Rule,
            PinnedIdentifier::try_from(id).unwrap(),
        )
    }

    fn lock_of(rules: Vec<LockedArtifact>) -> GrimoireLock {
        GrimoireLock {
            metadata: LockMetadata {
                lock_version: LockVersion::V1,
                declaration_hash_version: 1,
                declaration_hash: format!("sha256:{}", "d".repeat(64)),
                generated_by: "grim 0.1.0".to_string(),
                generated_at: "2026-01-01T00:00:00Z".to_string(),
            },
            skills: vec![],
            rules,
            agents: vec![],
            bundles: vec![],
        }
    }

    fn arc(m: impl OciAccess + 'static) -> Arc<dyn OciAccess> {
        Arc::new(m)
    }

    #[tokio::test]
    async fn fresh_install_then_already_installed_noop() {
        let dir = tempfile::tempdir().unwrap();
        let blob = rule_tar("rust-style", b"# rust\n");
        let lock = lock_of(vec![locked_rule("rust-style", &blob)]);
        let access = arc(BlobMock { blob: blob.clone() });
        let target = InstallTarget::new(dir.path(), crate::config::scope::ConfigScope::Project, vec![]);
        let mut state = InstallState::load(&dir.path().join("state.json")).unwrap();
        let m = DefaultMaterializer;
        let roots = roots(dir.path());

        let r1 = install_all(&lock, &access, &m, &target, &mut state, &roots, false).await;
        assert_eq!(r1.len(), 1);
        assert_eq!(*r1[0].result.as_ref().unwrap(), InstallOutcome::Installed);
        assert!(dir.path().join(".claude/rules/rust-style.md").is_file());

        // F05: portability contract — the saved record's target must be an
        // AnchoredPath, never an absolute PathBuf. Pins the serialization contract.
        let rec = state.get(crate::oci::ArtifactKind::Rule, "rust-style").unwrap();
        assert_eq!(
            rec.outputs[0].target,
            AnchoredPath {
                anchor: PathAnchor::Workspace,
                relative: ".claude/rules/rust-style.md".to_string(),
            },
            "saved target must be Workspace-anchored relative path, never absolute"
        );

        // Second pass with same lock + intact content ⇒ no-op.
        let r2 = install_all(&lock, &access, &m, &target, &mut state, &roots, false).await;
        assert_eq!(*r2[0].result.as_ref().unwrap(), InstallOutcome::AlreadyInstalled);
    }

    #[tokio::test]
    async fn modified_file_refused_then_forced() {
        let dir = tempfile::tempdir().unwrap();
        let blob = rule_tar("rust-style", b"# rust\n");
        let lock = lock_of(vec![locked_rule("rust-style", &blob)]);
        let access = arc(BlobMock { blob: blob.clone() });
        let target = InstallTarget::new(dir.path(), crate::config::scope::ConfigScope::Project, vec![]);
        let mut state = InstallState::load(&dir.path().join("state.json")).unwrap();
        let m = DefaultMaterializer;
        let roots = roots(dir.path());

        install_all(&lock, &access, &m, &target, &mut state, &roots, false).await;
        // Tamper with the installed file.
        let installed = dir.path().join(".claude/rules/rust-style.md");
        std::fs::write(&installed, b"hand edited\n").unwrap();

        let refused = install_all(&lock, &access, &m, &target, &mut state, &roots, false).await;
        assert!(matches!(
            refused[0].result.as_ref().unwrap(),
            InstallOutcome::Refused { .. }
        ));
        assert_eq!(std::fs::read(&installed).unwrap(), b"hand edited\n");

        let forced = install_all(&lock, &access, &m, &target, &mut state, &roots, true).await;
        assert_eq!(*forced[0].result.as_ref().unwrap(), InstallOutcome::Updated);
        assert_eq!(std::fs::read(&installed).unwrap(), b"# rust\n");
    }

    #[tokio::test]
    async fn changed_pin_reinstalls_as_updated() {
        let dir = tempfile::tempdir().unwrap();
        let blob_v1 = rule_tar("rust-style", b"v1\n");
        let lock_v1 = lock_of(vec![locked_rule("rust-style", &blob_v1)]);
        let target = InstallTarget::new(dir.path(), crate::config::scope::ConfigScope::Project, vec![]);
        let mut state = InstallState::load(&dir.path().join("state.json")).unwrap();
        let m = DefaultMaterializer;
        let roots = roots(dir.path());

        install_all(
            &lock_v1,
            &arc(BlobMock { blob: blob_v1 }),
            &m,
            &target,
            &mut state,
            &roots,
            false,
        )
        .await;

        let blob_v2 = rule_tar("rust-style", b"v2\n");
        let lock_v2 = lock_of(vec![locked_rule("rust-style", &blob_v2)]);
        let r = install_all(
            &lock_v2,
            &arc(BlobMock { blob: blob_v2 }),
            &m,
            &target,
            &mut state,
            &roots,
            false,
        )
        .await;
        assert_eq!(*r[0].result.as_ref().unwrap(), InstallOutcome::Updated);
        assert_eq!(
            std::fs::read(dir.path().join(".claude/rules/rust-style.md")).unwrap(),
            b"v2\n"
        );

        // F05: portability contract — after an update the record's target must
        // still be an AnchoredPath, not an absolute PathBuf.
        let rec = state.get(crate::oci::ArtifactKind::Rule, "rust-style").unwrap();
        assert_eq!(
            rec.outputs[0].target,
            AnchoredPath {
                anchor: PathAnchor::Workspace,
                relative: ".claude/rules/rust-style.md".to_string(),
            },
            "updated record target must be Workspace-anchored relative path, never absolute"
        );
    }

    #[tokio::test]
    async fn missing_blob_is_blob_missing_error() {
        let dir = tempfile::tempdir().unwrap();
        let blob = rule_tar("rust-style", b"# rust\n");
        let lock = lock_of(vec![locked_rule("rust-style", &blob)]);
        let target = InstallTarget::new(dir.path(), crate::config::scope::ConfigScope::Project, vec![]);
        let mut state = InstallState::load(&dir.path().join("state.json")).unwrap();
        let m = DefaultMaterializer;
        let roots = roots(dir.path());

        let r = install_all(
            &lock,
            &arc(MissingMock { blob: blob.clone() }),
            &m,
            &target,
            &mut state,
            &roots,
            false,
        )
        .await;
        let err = r[0].result.as_ref().expect_err("missing blob must error");
        assert!(matches!(
            err,
            crate::error::Error::Install(ie) if matches!(ie.kind, InstallErrorKind::BlobMissing)
        ));
    }

    #[tokio::test]
    async fn blob_digest_mismatch_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let blob = rule_tar("rust-style", b"# rust\n");
        let lock = lock_of(vec![locked_rule("rust-style", &blob)]);
        // The manifest advertises the layer digest of `blob`, but the
        // registry serves `tampered` bytes — a corrupt-registry scenario.
        let wrong = rule_tar("rust-style", b"tampered\n");
        let target = InstallTarget::new(dir.path(), crate::config::scope::ConfigScope::Project, vec![]);
        let mut state = InstallState::load(&dir.path().join("state.json")).unwrap();
        let m = DefaultMaterializer;

        let mock = WrongBlobMock {
            manifest_blob: blob.clone(),
            served_blob: wrong,
        };
        let roots = roots(dir.path());
        let r = install_all(&lock, &arc(mock), &m, &target, &mut state, &roots, false).await;
        let err = r[0].result.as_ref().expect_err("digest mismatch must error");
        assert!(matches!(
            err,
            crate::error::Error::Install(ie) if matches!(ie.kind, InstallErrorKind::BlobDigestMismatch { .. })
        ));
    }

    #[tokio::test]
    async fn multi_file_rule_installs_noop_then_support_drift_refused_then_forced() {
        let dir = tempfile::tempdir().unwrap();
        let blob = multi_rule_tar("my-rule", b"# index\n", &[("examples.md", b"# ex\n")]);
        let lock = lock_of(vec![locked_rule("my-rule", &blob)]);
        let access = arc(BlobMock { blob: blob.clone() });
        let target = InstallTarget::new(dir.path(), crate::config::scope::ConfigScope::Project, vec![]);
        let mut state = InstallState::load(&dir.path().join("state.json")).unwrap();
        let m = DefaultMaterializer;
        let roots = roots(dir.path());

        // Fresh install lands the index and the support file beside it.
        let r1 = install_all(&lock, &access, &m, &target, &mut state, &roots, false).await;
        assert_eq!(*r1[0].result.as_ref().unwrap(), InstallOutcome::Installed);
        let index = dir.path().join(".claude/rules/my-rule.md");
        let support = dir.path().join(".claude/rules/my-rule/examples.md");
        assert!(index.is_file());
        assert!(support.is_file());

        // Intact footprint ⇒ no-op.
        let r2 = install_all(&lock, &access, &m, &target, &mut state, &roots, false).await;
        assert_eq!(*r2[0].result.as_ref().unwrap(), InstallOutcome::AlreadyInstalled);

        // Editing a *support* file (not the index) is detected as drift.
        std::fs::write(&support, b"hand edited\n").unwrap();
        let refused = install_all(&lock, &access, &m, &target, &mut state, &roots, false).await;
        assert!(matches!(
            refused[0].result.as_ref().unwrap(),
            InstallOutcome::Refused { .. }
        ));
        assert_eq!(std::fs::read(&support).unwrap(), b"hand edited\n");

        // Forcing restores the canonical support content.
        let forced = install_all(&lock, &access, &m, &target, &mut state, &roots, true).await;
        assert_eq!(*forced[0].result.as_ref().unwrap(), InstallOutcome::Updated);
        assert_eq!(std::fs::read(&support).unwrap(), b"# ex\n");
    }

    #[tokio::test]
    async fn deleting_the_support_dir_is_drift_not_an_io_error() {
        let dir = tempfile::tempdir().unwrap();
        let blob = multi_rule_tar("my-rule", b"# index\n", &[("examples.md", b"# ex\n")]);
        let lock = lock_of(vec![locked_rule("my-rule", &blob)]);
        let access = arc(BlobMock { blob: blob.clone() });
        let target = InstallTarget::new(dir.path(), crate::config::scope::ConfigScope::Project, vec![]);
        let mut state = InstallState::load(&dir.path().join("state.json")).unwrap();
        let m = DefaultMaterializer;
        let roots = roots(dir.path());

        install_all(&lock, &access, &m, &target, &mut state, &roots, false).await;
        let support = dir.path().join(".claude/rules/my-rule");
        assert!(support.is_dir());

        // The user deletes the whole support dir (index kept).
        std::fs::remove_dir_all(&support).unwrap();

        // Reinstall must see *drift* (Refused), never a hard I/O error.
        let refused = install_all(&lock, &access, &m, &target, &mut state, &roots, false).await;
        assert!(
            matches!(refused[0].result.as_ref().unwrap(), InstallOutcome::Refused { .. }),
            "a deleted support dir is drift, got {:?}",
            refused[0].result
        );

        // Forcing restores the support tree.
        let forced = install_all(&lock, &access, &m, &target, &mut state, &roots, true).await;
        assert_eq!(*forced[0].result.as_ref().unwrap(), InstallOutcome::Updated);
        assert_eq!(std::fs::read(support.join("examples.md")).unwrap(), b"# ex\n");
    }

    #[tokio::test]
    async fn updating_a_rule_that_drops_its_support_dir_reaps_the_stale_dir() {
        let dir = tempfile::tempdir().unwrap();
        let blob_v1 = multi_rule_tar("my-rule", b"# index v1\n", &[("examples.md", b"# ex\n")]);
        let lock_v1 = lock_of(vec![locked_rule("my-rule", &blob_v1)]);
        let target = InstallTarget::new(dir.path(), crate::config::scope::ConfigScope::Project, vec![]);
        let mut state = InstallState::load(&dir.path().join("state.json")).unwrap();
        let m = DefaultMaterializer;
        let roots = roots(dir.path());

        install_all(
            &lock_v1,
            &arc(BlobMock { blob: blob_v1 }),
            &m,
            &target,
            &mut state,
            &roots,
            false,
        )
        .await;
        let support = dir.path().join(".claude/rules/my-rule");
        assert!(support.is_dir(), "v1 installs the support dir");

        // v2 is a plain single-file rule (different digest ⇒ update).
        let blob_v2 = rule_tar("my-rule", b"# index v2\n");
        let lock_v2 = lock_of(vec![locked_rule("my-rule", &blob_v2)]);
        let r = install_all(
            &lock_v2,
            &arc(BlobMock { blob: blob_v2 }),
            &m,
            &target,
            &mut state,
            &roots,
            false,
        )
        .await;
        assert_eq!(*r[0].result.as_ref().unwrap(), InstallOutcome::Updated);

        assert!(dir.path().join(".claude/rules/my-rule.md").is_file());
        assert!(
            !support.exists(),
            "a version that drops its support dir must reap the stale one"
        );
        // The record no longer carries a support dir.
        let rec = state.get(ArtifactKind::Rule, "my-rule").unwrap();
        assert!(rec.outputs.iter().all(|c| c.support_dir.is_none()));
    }

    #[test]
    fn outcome_equality() {
        assert_eq!(InstallOutcome::Installed, InstallOutcome::Installed);
        assert_ne!(InstallOutcome::Installed, InstallOutcome::Updated);
        assert_eq!(InstallOutcome::Skipped("x".into()), InstallOutcome::Skipped("x".into()));
        assert!(matches!(
            InstallOutcome::Refused {
                recorded: Digest::Sha256("a".repeat(64)),
                actual: Digest::Sha256("b".repeat(64)),
            },
            InstallOutcome::Refused { .. }
        ));
        let _ = Path::new("/x");
    }
}
