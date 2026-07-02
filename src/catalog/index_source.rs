// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! Package-index catalog source.
//!
//! When a `[[registries]]` entry sets `index` instead of `url`, the browse
//! listing comes from a package index rather than the OCI `_catalog`
//! endpoint (which GHCR, GitLab SaaS, and Docker Hub gate or omit). Two
//! transports:
//!
//! - **HTTP(S)** — a compiled static index (`<base>/all.json`), e.g.
//!   `https://index.grimoire.rs` served from GitHub Pages or any webserver.
//! - **Git** — a shallow clone of the index repository, walking
//!   `index/**/metadata.json`. Works against GitHub, GitLab, or any
//!   plain git host — no vendor API needed.
//!
//! The index is a *phone book, not a catalog*: entries are pointers
//! (`ref` = `registry/repository`) plus display metadata. Versions are
//! never stored in the index — grim resolves tags live from the registry
//! at install time, so an index-backed [`CatalogEntry`] carries no
//! `latest_tag`/`version`.

use std::path::Path;

use serde::Deserialize;

use crate::catalog::catalog_error::CatalogError;
use crate::catalog::registry_catalog::CatalogEntry;
use crate::config::registry_resolve::SourceKind;

/// HTTP fetch timeout for the compiled index.
const HTTP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// One package pointer as published in the index (`all.json` element or a
/// single `metadata.json`). Unknown fields are tolerated so index schema
/// additions never break older grim binaries.
#[derive(Debug, Deserialize)]
struct IndexPackage {
    /// Metadata schema version; only `1` is consumed today.
    schema: u32,
    /// Package name (equals the index directory name; unused here — the
    /// repository path from `ref` names the row).
    #[allow(dead_code)]
    name: String,
    /// `skill` / `rule` / `agent` / `bundle`.
    kind: String,
    /// OCI reference (`registry/repository`, no tag) grim resolves against.
    r#ref: String,
    /// One-line description shown in `grim search`.
    #[serde(default)]
    description: Option<String>,
    /// Source repository URL.
    #[serde(default)]
    repository: Option<String>,
}

impl IndexPackage {
    /// Project into a [`CatalogEntry`], or `None` when the `ref` carries no
    /// `registry/repository` split or the schema version is unknown.
    fn into_entry(self, fetched_at: &str) -> Option<CatalogEntry> {
        if self.schema != 1 {
            tracing::warn!(
                "skipping index entry '{}': unsupported schema {}",
                self.r#ref,
                self.schema
            );
            return None;
        }
        let (registry, repository) = self.r#ref.split_once('/')?;
        if registry.is_empty() || repository.is_empty() {
            return None;
        }
        Some(CatalogEntry {
            registry: registry.to_string(),
            repository: repository.to_string(),
            kind: Some(self.kind),
            description: self.description,
            summary: None,
            keywords: Vec::new(),
            // Same HTTPS prefix guard as the manifest read-back path.
            repository_url: self.repository.filter(|r| r.starts_with("https://")),
            revision: None,
            created: None,
            deprecated: None,
            // Phone-book contract: no version data in the index; tags are
            // resolved live from the registry at install time.
            latest_tag: None,
            version: None,
            fetched_at: fetched_at.to_string(),
        })
    }
}

/// Fetch the package list for `locator` over the transport `kind`.
///
/// `git_dir` is the per-locator shallow-clone directory (git transport
/// only); `cache_path` provides error context (the catalog cache file the
/// build is for).
///
/// # Errors
///
/// [`CatalogError`] for an HTTP transport/status failure, a git subprocess
/// failure, or an index-content parse failure.
pub async fn fetch_index_entries(
    locator: &str,
    kind: SourceKind,
    git_dir: &Path,
    cache_path: &Path,
    fetched_at: &str,
) -> Result<Vec<CatalogEntry>, CatalogError> {
    let packages = match kind {
        SourceKind::IndexGit => fetch_git(locator, git_dir, cache_path).await?,
        // `Registry` never reaches this module; treat defensively as HTTP.
        SourceKind::IndexHttp | SourceKind::Registry => fetch_http(locator, cache_path).await?,
    };
    Ok(packages.into_iter().filter_map(|p| p.into_entry(fetched_at)).collect())
}

/// GET `<base>/all.json` (or the locator itself when it already names a
/// `.json` document) and parse the package array.
async fn fetch_http(locator: &str, cache_path: &Path) -> Result<Vec<IndexPackage>, CatalogError> {
    let base = locator.trim_end_matches('/');
    let url = if base.ends_with(".json") {
        base.to_string()
    } else {
        format!("{base}/all.json")
    };

    let client = reqwest::Client::builder()
        .timeout(HTTP_TIMEOUT)
        .user_agent(concat!("grim/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| CatalogError::index_fetch(cache_path, locator, e))?;
    let response = client
        .get(&url)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|e| CatalogError::index_fetch(cache_path, locator, e))?;
    let bytes = response
        .bytes()
        .await
        .map_err(|e| CatalogError::index_fetch(cache_path, locator, e))?;
    serde_json::from_slice(&bytes).map_err(|e| CatalogError::index_fetch(cache_path, locator, e))
}

/// Shallow-clone the index repository and walk `index/**/metadata.json`.
///
/// A fresh `--depth 1` clone lands in a temp sibling and atomically
/// replaces the previous clone — simpler and more robust than fetch/reset
/// against force-pushed or re-rooted index repos, and cheap under the
/// catalog TTL (one clone per locator per hour).
async fn fetch_git(locator: &str, git_dir: &Path, cache_path: &Path) -> Result<Vec<IndexPackage>, CatalogError> {
    let url = locator.strip_prefix("git+").unwrap_or(locator).to_string();
    let tmp = git_dir.with_extension("tmp");

    if let Some(parent) = git_dir.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| CatalogError::io(cache_path, e))?;
    }
    // Best-effort cleanup of a previous interrupted clone.
    let _ = tokio::fs::remove_dir_all(&tmp).await;

    let output = tokio::process::Command::new("git")
        .arg("clone")
        .arg("--depth")
        .arg("1")
        .arg("--quiet")
        .arg(&url)
        .arg(&tmp)
        // Never hang a browse on an interactive credential prompt; a
        // private index needs ambient git credentials (helper / ssh agent).
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .await
        .map_err(|e| CatalogError::io(cache_path, e))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(CatalogError::io(
            cache_path,
            std::io::Error::other(format!("git clone of index '{url}' failed: {}", stderr.trim())),
        ));
    }

    let _ = tokio::fs::remove_dir_all(git_dir).await;
    tokio::fs::rename(&tmp, git_dir)
        .await
        .map_err(|e| CatalogError::io(cache_path, e))?;

    // Walk on the blocking pool — recursive std::fs, never on a worker.
    let root = git_dir.join("index");
    let cache = cache_path.to_path_buf();
    let locator = locator.to_string();
    tokio::task::spawn_blocking(move || walk_metadata(&root, &cache, &locator))
        .await
        .map_err(|e| CatalogError::io(cache_path, std::io::Error::other(e)))?
}

/// Collect every `metadata.json` under `root` (recursive), skipping
/// unparseable files with a warning so one bad entry never hides the rest.
fn walk_metadata(root: &Path, cache_path: &Path, locator: &str) -> Result<Vec<IndexPackage>, CatalogError> {
    let mut packages = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            // A missing `index/` tree is an empty index, not an error.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(CatalogError::io(cache_path, e)),
        };
        for entry in entries {
            let entry = entry.map_err(|e| CatalogError::io(cache_path, e))?;
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.file_name().is_some_and(|n| n == "metadata.json") {
                match std::fs::read(&path)
                    .map_err(|e| CatalogError::io(cache_path, e))
                    .and_then(|bytes| {
                        serde_json::from_slice::<IndexPackage>(&bytes)
                            .map_err(|e| CatalogError::index_fetch(cache_path, locator, e))
                    }) {
                    Ok(pkg) => packages.push(pkg),
                    Err(e) => tracing::warn!("skipping unreadable index entry {}: {e}", path.display()),
                }
            }
        }
    }
    Ok(packages)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pkg(json: &str) -> IndexPackage {
        serde_json::from_str(json).expect("valid index package")
    }

    #[test]
    fn package_maps_to_catalog_entry() {
        let p = pkg(r#"{
            "schema": 1,
            "name": "grim-usage",
            "kind": "skill",
            "ref": "ghcr.io/grimoire-rs/skills/grim-usage",
            "description": "Drive the grim CLI",
            "repository": "https://github.com/grimoire-rs/grimoire",
            "owner": {"github": "grimoire-rs", "id": 1}
        }"#);
        let e = p.into_entry("2026-01-01T00:00:00Z").expect("maps");
        assert_eq!(e.registry, "ghcr.io");
        assert_eq!(e.repository, "grimoire-rs/skills/grim-usage");
        assert_eq!(e.kind.as_deref(), Some("skill"));
        assert_eq!(e.description.as_deref(), Some("Drive the grim CLI"));
        assert_eq!(
            e.repository_url.as_deref(),
            Some("https://github.com/grimoire-rs/grimoire")
        );
        assert_eq!(e.latest_tag, None, "phone book carries no version data");
        assert_eq!(e.version, None);
    }

    #[test]
    fn unknown_schema_is_skipped() {
        let p = pkg(r#"{"schema": 2, "name": "x", "kind": "skill", "ref": "h/r"}"#);
        assert!(p.into_entry("t").is_none());
    }

    #[test]
    fn hostless_ref_is_skipped() {
        let p = pkg(r#"{"schema": 1, "name": "x", "kind": "skill", "ref": "just-a-name"}"#);
        assert!(p.into_entry("t").is_none());
    }

    #[test]
    fn non_https_repository_is_dropped() {
        let p = pkg(r#"{"schema": 1, "name": "x", "kind": "rule", "ref": "h/r", "repository": "http://plain"}"#);
        let e = p.into_entry("t").expect("maps");
        assert_eq!(e.repository_url, None);
    }
}
