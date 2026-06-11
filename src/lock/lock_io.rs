// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! Lock file I/O: capped load + atomic save with `generated_at`
//! preservation.
//!
//! `generated_at` is preserved verbatim when the resolved content of
//! every artifact (registry, repository, digest — the advisory tag is
//! ignored via [`PinnedIdentifier::eq_content`]) is unchanged between two
//! lock writes, and the comparison is order-independent. When content
//! differs the timestamp is "now"; if "now" collides with the previous
//! timestamp string it is bumped by one second so downstream diffs see a
//! change.

use std::path::Path;

use crate::config;
use crate::lock::grimoire_lock::GrimoireLock;
use crate::lock::lock_error::{LockError, LockErrorKind};
use crate::lock::locked_artifact::LockedArtifact;
use crate::store::atomic_write::atomic_write;

/// Current UTC time as an RFC3339 string (`%Y-%m-%dT%H:%M:%SZ`).
pub fn now_rfc3339() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// Load a lock from `path`, enforcing the 64 KiB cap.
///
/// # Errors
///
/// [`LockErrorKind::Io`] (including not-found), [`LockErrorKind::FileTooLarge`],
/// or any parse/version error — all with `path` context.
pub fn load(path: &Path) -> Result<GrimoireLock, LockError> {
    let content = read_capped(path)?;
    GrimoireLock::from_toml_str(&content).map_err(|e| LockError::new(path, e.kind))
}

/// Atomically save `lock` to `path`.
///
/// `generated_at` is preserved from `previous` when every artifact's
/// pinned content is unchanged (order-independent, tag-agnostic). If
/// content changed but the new timestamp equals the previous string, it
/// is bumped by one second so the change is observable.
///
/// # Errors
///
/// Serialization or I/O failure with `path` context.
pub fn save(path: &Path, lock: &GrimoireLock, previous: Option<&GrimoireLock>) -> Result<(), LockError> {
    let mut to_write = lock.clone();
    if let Some(prev) = previous {
        if content_equal(&to_write, prev) {
            to_write.metadata.generated_at = prev.metadata.generated_at.clone();
        } else if to_write.metadata.generated_at <= prev.metadata.generated_at {
            to_write.metadata.generated_at =
                bump_one_second(&prev.metadata.generated_at).unwrap_or_else(|| to_write.metadata.generated_at.clone());
        }
    }

    let serialized = to_write.to_toml_string().map_err(|e| LockError::new(path, e.kind))?;
    atomic_write(path, serialized.as_bytes()).map_err(|e| LockError::new(path, LockErrorKind::Io(e)))
}

/// Read a lock file with the shared 64 KiB cap, mapping config-tier I/O /
/// size errors onto the lock-tier taxonomy.
fn read_capped(path: &Path) -> Result<String, LockError> {
    config::read_capped(path).map_err(|e| {
        let kind = match e.kind {
            config::ConfigErrorKind::Io(io) => LockErrorKind::Io(io),
            config::ConfigErrorKind::FileTooLarge { size, limit } => LockErrorKind::FileTooLarge { size, limit },
            // `read_capped` only ever yields Io / FileTooLarge.
            other => LockErrorKind::Io(std::io::Error::other(other.to_string())),
        };
        LockError::new(path, kind)
    })
}

/// Whether two locks have the same resolved content (artifact set by
/// kind/name and pinned digest), ignoring artifact order and advisory
/// tags. `generated_at` and the other metadata are intentionally not
/// compared — only the resolved pins drive timestamp preservation.
fn content_equal(a: &GrimoireLock, b: &GrimoireLock) -> bool {
    lists_content_equal(&a.skills, &b.skills)
        && lists_content_equal(&a.rules, &b.rules)
        && lists_content_equal(&a.agents, &b.agents)
}

fn lists_content_equal(a: &[LockedArtifact], b: &[LockedArtifact]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut a_sorted: Vec<&LockedArtifact> = a.iter().collect();
    let mut b_sorted: Vec<&LockedArtifact> = b.iter().collect();
    a_sorted.sort_by(|x, y| x.name.cmp(&y.name));
    b_sorted.sort_by(|x, y| x.name.cmp(&y.name));
    a_sorted
        .iter()
        .zip(b_sorted.iter())
        .all(|(x, y)| x.name == y.name && x.pinned.eq_content(&y.pinned))
}

/// `iso` + 1 second, preserving the `%Y-%m-%dT%H:%M:%SZ` shape. `None`
/// if `iso` is not RFC3339 — callers fall back to their own timestamp.
fn bump_one_second(iso: &str) -> Option<String> {
    let parsed = chrono::DateTime::parse_from_rfc3339(iso).ok()?;
    let bumped = parsed.checked_add_signed(chrono::Duration::seconds(1))?;
    Some(
        bumped
            .with_timezone(&chrono::Utc)
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::FILE_SIZE_LIMIT_BYTES;
    use crate::lock::grimoire_lock::LockMetadata;
    use crate::lock::lock_version::LockVersion;
    use crate::oci::{ArtifactKind, Digest, Identifier, PinnedIdentifier};

    fn sha(byte: char) -> String {
        std::iter::repeat_n(byte, 64).collect()
    }

    fn pinned(repo: &str, tag: Option<&str>, byte: char) -> PinnedIdentifier {
        let mut id = Identifier::new_registry(repo, "ghcr.io");
        if let Some(t) = tag {
            id = id.clone_with_tag(t);
        }
        let id = id.clone_with_digest(Digest::Sha256(sha(byte)));
        PinnedIdentifier::try_from(id).unwrap()
    }

    fn artifact(name: &str, p: PinnedIdentifier) -> LockedArtifact {
        LockedArtifact::direct(name.to_string(), ArtifactKind::Skill, p)
    }

    fn lock_with(generated_at: &str, skills: Vec<LockedArtifact>) -> GrimoireLock {
        GrimoireLock {
            metadata: LockMetadata {
                lock_version: LockVersion::V1,
                declaration_hash_version: 1,
                declaration_hash: format!("sha256:{}", sha('d')),
                generated_by: LockMetadata::generated_by_current(),
                generated_at: generated_at.to_string(),
            },
            skills,
            rules: vec![],
            agents: vec![],
        }
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("grimoire.lock");
        let lock = lock_with(
            "2026-04-19T00:00:00Z",
            vec![artifact("code-review", pinned("acme/code-review", Some("stable"), 'a'))],
        );
        save(&path, &lock, None).unwrap();
        let loaded = load(&path).unwrap();
        assert_eq!(loaded.skills.len(), 1);
        assert_eq!(loaded.skills[0].pinned.tag(), None, "advisory tag stripped on disk");
    }

    #[test]
    fn deterministic_double_save_is_byte_identical() {
        let dir = tempfile::tempdir().unwrap();
        let p1 = dir.path().join("a.lock");
        let p2 = dir.path().join("b.lock");
        let lock = lock_with(
            "2026-04-19T00:00:00Z",
            vec![
                artifact("zeta", pinned("acme/zeta", None, '2')),
                artifact("alpha", pinned("acme/alpha", None, '1')),
            ],
        );
        save(&p1, &lock, None).unwrap();
        save(&p2, &lock, None).unwrap();
        assert_eq!(std::fs::read(&p1).unwrap(), std::fs::read(&p2).unwrap());
    }

    #[test]
    fn generated_at_preserved_when_content_unchanged() {
        let prev = lock_with("2026-01-01T00:00:00Z", vec![artifact("x", pinned("acme/x", None, 'a'))]);
        let next = lock_with("2099-12-31T23:59:59Z", vec![artifact("x", pinned("acme/x", None, 'a'))]);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("grimoire.lock");
        save(&path, &next, Some(&prev)).unwrap();
        assert_eq!(load(&path).unwrap().metadata.generated_at, "2026-01-01T00:00:00Z");
    }

    #[test]
    fn generated_at_preserved_when_only_tag_changes() {
        let prev = lock_with(
            "2026-01-01T00:00:00Z",
            vec![artifact("x", pinned("acme/x", Some("3.28"), 'a'))],
        );
        let next = lock_with(
            "2099-12-31T23:59:59Z",
            vec![artifact("x", pinned("acme/x", Some("3.29"), 'a'))],
        );
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("grimoire.lock");
        save(&path, &next, Some(&prev)).unwrap();
        assert_eq!(load(&path).unwrap().metadata.generated_at, "2026-01-01T00:00:00Z");
    }

    #[test]
    fn generated_at_preserved_when_order_differs() {
        let a = artifact("x", pinned("acme/x", None, 'a'));
        let b = artifact("y", pinned("acme/y", None, 'b'));
        let prev = lock_with("2026-01-01T00:00:00Z", vec![a.clone(), b.clone()]);
        let next = lock_with("2099-12-31T23:59:59Z", vec![b, a]);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("grimoire.lock");
        save(&path, &next, Some(&prev)).unwrap();
        assert_eq!(load(&path).unwrap().metadata.generated_at, "2026-01-01T00:00:00Z");
    }

    #[test]
    fn generated_at_updated_when_agent_content_differs() {
        // The agents list participates in content equality: an agent pin
        // change must refresh the timestamp.
        let mut prev = lock_with("2026-01-01T00:00:00Z", vec![]);
        prev.agents = vec![LockedArtifact::direct(
            "rev".to_string(),
            ArtifactKind::Agent,
            pinned("acme/rev", None, 'a'),
        )];
        let mut next = lock_with("2026-06-01T12:00:00Z", vec![]);
        next.agents = vec![LockedArtifact::direct(
            "rev".to_string(),
            ArtifactKind::Agent,
            pinned("acme/rev", None, 'b'),
        )];
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("grimoire.lock");
        save(&path, &next, Some(&prev)).unwrap();
        assert_ne!(load(&path).unwrap().metadata.generated_at, "2026-01-01T00:00:00Z");
    }

    #[test]
    fn generated_at_updated_when_content_differs() {
        let prev = lock_with("2026-01-01T00:00:00Z", vec![artifact("x", pinned("acme/x", None, 'a'))]);
        let next = lock_with("2026-06-01T12:00:00Z", vec![artifact("x", pinned("acme/x", None, 'b'))]);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("grimoire.lock");
        save(&path, &next, Some(&prev)).unwrap();
        assert_ne!(load(&path).unwrap().metadata.generated_at, "2026-01-01T00:00:00Z");
    }

    #[test]
    fn generated_at_bumped_when_content_differs_but_timestamp_collides() {
        let prev = lock_with("2026-01-01T00:00:00Z", vec![artifact("x", pinned("acme/x", None, 'a'))]);
        // Same (≤) timestamp, different content ⇒ must bump +1s.
        let next = lock_with("2026-01-01T00:00:00Z", vec![artifact("x", pinned("acme/x", None, 'b'))]);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("grimoire.lock");
        save(&path, &next, Some(&prev)).unwrap();
        assert_eq!(load(&path).unwrap().metadata.generated_at, "2026-01-01T00:00:01Z");
    }

    #[cfg(unix)]
    #[test]
    fn save_preserves_original_on_write_failure() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("lockdir");
        std::fs::create_dir(&sub).unwrap();
        let path = sub.join("grimoire.lock");

        let seed = lock_with("2026-01-01T00:00:00Z", vec![artifact("x", pinned("acme/x", None, 'a'))]);
        save(&path, &seed, None).unwrap();
        let original = std::fs::read(&path).unwrap();

        let perms = std::fs::metadata(&sub).unwrap().permissions();
        std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o555)).unwrap();

        let clobber = lock_with("2099-01-01T00:00:00Z", vec![artifact("y", pinned("acme/y", None, 'b'))]);
        let err = save(&path, &clobber, None);
        std::fs::set_permissions(&sub, perms).unwrap();
        assert!(err.is_err());
        assert_eq!(std::fs::read(&path).unwrap(), original);
    }

    #[cfg(unix)]
    #[test]
    fn save_caps_permissions_at_0o644() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("grimoire.lock");
        let lock = lock_with("2026-01-01T00:00:00Z", vec![artifact("x", pinned("acme/x", None, 'a'))]);
        save(&path, &lock, None).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o666)).unwrap();
        save(&path, &lock, Some(&lock)).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o644);
    }

    #[test]
    fn load_rejects_oversized() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("grimoire.lock");
        let padding = "# pad pad pad pad pad pad pad pad pad pad pad pad\n".repeat(2200);
        let body = format!(
            "{padding}\n[metadata]\nlock_version = 1\ndeclaration_hash_version = 1\n\
             declaration_hash = \"sha256:{a}\"\ngenerated_by = \"grim 0.1.0\"\n\
             generated_at = \"2026-04-19T00:00:00Z\"\n",
            a = sha('a')
        );
        assert!(body.len() as u64 > FILE_SIZE_LIMIT_BYTES);
        std::fs::write(&path, &body).unwrap();
        let err = load(&path).expect_err("oversize rejects");
        assert!(matches!(err.kind, LockErrorKind::FileTooLarge { .. }));
    }
}
