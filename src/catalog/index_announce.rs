// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! Package-index announcement — the write side of [`super::index_source`].
//!
//! `grim publish --announce` records published packages in a package-index
//! git repository: clone, write `index/github.com/<ns>/<pkg>/metadata.json`
//! pointers, commit on a deterministic topic branch, push, and (on
//! github.com, when the `gh` CLI is available) open a pull request. Any
//! other git host gets the pushed branch — open the merge request there
//! (GitLab et al. print a ready-made MR URL on push).
//!
//! The announced metadata is the phone-book pointer only (name, kind,
//! tagless ref, description, ownership) — never versions. Re-announcing
//! unchanged content is detected via `git status` and reported as
//! [`AnnounceOutcome::UpToDate`] without a push.

use std::path::Path;

/// The default public index announcements target.
pub const DEFAULT_INDEX_REPO: &str = "https://github.com/grimoire-rs/index";

/// One package pointer to announce.
#[derive(Debug, Clone)]
pub struct AnnouncePackage {
    /// Package name (the index directory name).
    pub name: String,
    /// `skill` / `rule` / `agent` / `bundle`.
    pub kind: String,
    /// Tagless OCI reference (`registry/repository`).
    pub reference: String,
    /// One-line description shown in `grim search`.
    pub description: String,
    /// HTTPS source-repository URL, if known.
    pub repository_url: Option<String>,
}

/// The announce request: where, as whom, and what.
#[derive(Debug)]
pub struct AnnounceRequest {
    /// The index git repository (https clone URL or local path).
    pub repo_url: String,
    /// The `index/github.com/<namespace>/` the packages land under.
    pub namespace: String,
    /// The namespace's numeric GitHub account id. `None` ⇒ resolved live
    /// from the GitHub API.
    pub owner_id: Option<u64>,
    /// The packages to announce.
    pub packages: Vec<AnnouncePackage>,
}

/// What the announce achieved.
#[derive(Debug, PartialEq, Eq)]
pub enum AnnounceOutcome {
    /// A pull request was opened (github.com + `gh`).
    PullRequest {
        /// The PR URL as printed by `gh`.
        url: String,
    },
    /// The topic branch was pushed; open the merge request on the host.
    BranchPushed {
        /// The pushed branch name.
        branch: String,
    },
    /// The index already carries exactly this metadata — nothing to do.
    UpToDate,
}

/// Announce-tier failures.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AnnounceError {
    /// A git subprocess failed.
    #[error("git {action} failed: {detail}")]
    Git {
        /// The git verb that failed (`clone`, `push`, …).
        action: &'static str,
        /// Trimmed stderr of the failing invocation.
        detail: String,
    },
    /// Local I/O around the working clone failed.
    #[error("I/O error during announce")]
    Io(#[from] std::io::Error),
    /// The namespace's GitHub account id could not be resolved.
    #[error("GitHub account lookup failed for '{namespace}'")]
    OwnerLookup {
        /// The namespace whose id lookup failed.
        namespace: String,
        /// The transport / parse cause.
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
}

/// Announce `request.packages` to the index repository.
///
/// # Errors
///
/// [`AnnounceError`] for a git clone/commit/push failure, a local I/O
/// failure, or a failed owner-id lookup.
pub async fn announce(request: &AnnounceRequest) -> Result<AnnounceOutcome, AnnounceError> {
    let owner_id = match request.owner_id {
        Some(id) => id,
        None => lookup_owner_id(&request.namespace).await?,
    };

    let workdir = tempfile::tempdir()?;
    let clone = workdir.path().join("index");
    git(
        None,
        "clone",
        &[
            "clone",
            "--depth",
            "1",
            "--quiet",
            &request.repo_url,
            &clone.display().to_string(),
        ],
    )
    .await?;

    // Deterministic topic branch: same package set + content ⇒ same branch,
    // so a retried announce force-updates its own branch instead of
    // littering. Fold REPO-RELATIVE paths — the clone lands in a fresh
    // tempdir every run, so absolute paths would break determinism.
    let mut rendered: Vec<(String, String)> = Vec::new();
    for pkg in &request.packages {
        let relative = format!("index/github.com/{}/{}/metadata.json", request.namespace, pkg.name);
        rendered.push((relative, metadata_json(pkg, &request.namespace, owner_id)));
    }
    let branch = branch_name(&request.namespace, &rendered);

    git(Some(&clone), "checkout", &["checkout", "--quiet", "-b", &branch]).await?;
    for (relative, content) in &rendered {
        let path = clone.join(relative);
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(path, content).await?;
    }

    let status = git_output(&clone, "status", &["status", "--porcelain"]).await?;
    if status.trim().is_empty() {
        return Ok(AnnounceOutcome::UpToDate);
    }

    git(Some(&clone), "add", &["add", "-A"]).await?;
    let names: Vec<&str> = request.packages.iter().map(|p| p.name.as_str()).collect();
    let message = format!("announce: {}", names.join(", "));
    git(
        Some(&clone),
        "commit",
        &[
            "-c",
            "user.name=grim",
            "-c",
            "user.email=announce@grimoire.rs",
            "commit",
            "--quiet",
            "-m",
            &message,
        ],
    )
    .await?;

    // Force-push our own topic branch (deterministic name ⇒ safe to move).
    git(Some(&clone), "push", &["push", "--quiet", "--force", "origin", &branch]).await?;

    // On github.com with `gh` available, open the PR right away.
    if request.repo_url.contains("github.com")
        && let Some(url) = try_gh_pr(&clone, &branch, &message).await
    {
        return Ok(AnnounceOutcome::PullRequest { url });
    }
    Ok(AnnounceOutcome::BranchPushed { branch })
}

/// Read the pointer metadata (description + HTTPS source URL) back from
/// the just-published artifact's manifest annotations: representative tag
/// → digest → manifest, over the access seam. Every failure degrades to
/// `None` — announce still proceeds with a fallback description.
pub async fn pointer_metadata(
    access: &dyn crate::oci::access::OciAccess,
    id: &crate::oci::Identifier,
) -> (Option<String>, Option<String>) {
    let tags = match access.list_tags(id).await {
        Ok(Some(tags)) => tags,
        _ => return (None, None),
    };
    let Some(tag) = crate::catalog::registry_catalog::pick_latest_tag(&tags) else {
        return (None, None);
    };
    let tagged = id.clone_with_tag(tag);
    let digest = match access
        .resolve_digest(&tagged, crate::oci::access::Operation::Query)
        .await
    {
        Ok(Some(d)) => d,
        _ => return (None, None),
    };
    let Ok(pinned) = crate::oci::PinnedIdentifier::try_from(tagged.clone_with_digest(digest)) else {
        return (None, None);
    };
    let Ok(Some(manifest)) = access.fetch_manifest(&pinned).await else {
        return (None, None);
    };
    (
        manifest
            .annotations
            .get("org.opencontainers.image.description")
            .cloned(),
        manifest
            .annotations
            .get("org.opencontainers.image.source")
            .filter(|s| s.starts_with("https://"))
            .cloned(),
    )
}

/// Resolve the namespace's GitHub login when none is configured: the
/// authenticated `gh` user, if the CLI is present and logged in.
pub async fn gh_login() -> Option<String> {
    let output = tokio::process::Command::new("gh")
        .args(["api", "user", "--jq", ".login"])
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let login = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!login.is_empty()).then_some(login)
}

/// GET the namespace's immutable numeric account id from the GitHub API.
async fn lookup_owner_id(namespace: &str) -> Result<u64, AnnounceError> {
    let wrap = |source: Box<dyn std::error::Error + Send + Sync>| AnnounceError::OwnerLookup {
        namespace: namespace.to_string(),
        source,
    };
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .user_agent(concat!("grim/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| wrap(e.into()))?;
    let response = client
        .get(format!("https://api.github.com/users/{namespace}"))
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|e| wrap(e.into()))?;
    let body: serde_json::Value = response.json().await.map_err(|e| wrap(e.into()))?;
    body.get("id")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| wrap("response carries no numeric id".into()))
}

/// Render the metadata.json pointer for `pkg` (index spec v1).
fn metadata_json(pkg: &AnnouncePackage, namespace: &str, owner_id: u64) -> String {
    let mut value = serde_json::json!({
        "schema": 1,
        "name": pkg.name,
        "kind": pkg.kind,
        "ref": pkg.reference,
        "description": pkg.description,
        "owner": { "github": namespace, "id": owner_id },
    });
    if let Some(repo) = &pkg.repository_url {
        value["repository"] = serde_json::Value::String(repo.clone());
    }
    let mut out = serde_json::to_string_pretty(&value).unwrap_or_default();
    out.push('\n');
    out
}

/// Deterministic topic branch: `announce/<ns>-<hash8>` over the rendered
/// (repo-relative path, content) set, so identical content re-announces
/// onto the same branch.
fn branch_name(namespace: &str, rendered: &[(String, String)]) -> String {
    let mut folded = String::new();
    for (relative, content) in rendered {
        folded.push_str(relative);
        folded.push('\0');
        folded.push_str(content);
    }
    let hash = crate::oci::digest::Algorithm::Sha256.hash(&folded).hex()[..8].to_string();
    format!("announce/{namespace}-{hash}")
}

/// Run a git subprocess, mapping a nonzero exit to [`AnnounceError::Git`].
async fn git(cwd: Option<&Path>, action: &'static str, args: &[&str]) -> Result<(), AnnounceError> {
    git_output_impl(cwd, action, args).await.map(|_| ())
}

/// Run a git subprocess in `cwd` and return its stdout.
async fn git_output(cwd: &Path, action: &'static str, args: &[&str]) -> Result<String, AnnounceError> {
    git_output_impl(Some(cwd), action, args).await
}

async fn git_output_impl(cwd: Option<&Path>, action: &'static str, args: &[&str]) -> Result<String, AnnounceError> {
    let mut cmd = tokio::process::Command::new("git");
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    let output = cmd
        .args(args)
        // Never hang on an interactive credential prompt.
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .await?;
    if !output.status.success() {
        return Err(AnnounceError::Git {
            action,
            detail: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Best-effort `gh pr create` for a pushed branch; `None` when `gh` is
/// unavailable, unauthenticated, or the PR cannot be created (e.g. one
/// already exists — the push above still updated it).
async fn try_gh_pr(clone: &Path, branch: &str, title: &str) -> Option<String> {
    let output = tokio::process::Command::new("gh")
        .current_dir(clone)
        .args([
            "pr",
            "create",
            "--head",
            branch,
            "--title",
            title,
            "--body",
            "Automated announcement via `grim publish --announce`.",
        ])
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        tracing::info!(
            "gh pr create did not succeed ({}); the branch is pushed — open the PR manually",
            String::from_utf8_lossy(&output.stderr).trim()
        );
        return None;
    }
    let url = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!url.is_empty()).then_some(url)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pkg(name: &str) -> AnnouncePackage {
        AnnouncePackage {
            name: name.to_string(),
            kind: "skill".to_string(),
            reference: format!("ghcr.io/acme/skills/{name}"),
            description: "A test pointer".to_string(),
            repository_url: Some("https://github.com/acme/skills".to_string()),
        }
    }

    #[test]
    fn metadata_json_matches_index_spec() {
        let rendered = metadata_json(&pkg("code-review"), "acme", 42);
        let value: serde_json::Value = serde_json::from_str(&rendered).expect("valid JSON");
        assert_eq!(value["schema"], 1);
        assert_eq!(value["name"], "code-review");
        assert_eq!(value["kind"], "skill");
        assert_eq!(value["ref"], "ghcr.io/acme/skills/code-review");
        assert_eq!(value["owner"]["github"], "acme");
        assert_eq!(value["owner"]["id"], 42);
        assert_eq!(value["repository"], "https://github.com/acme/skills");
        assert!(rendered.ends_with('\n'), "trailing newline for clean diffs");
    }

    #[test]
    fn metadata_json_omits_absent_repository() {
        let mut p = pkg("x");
        p.repository_url = None;
        let value: serde_json::Value = serde_json::from_str(&metadata_json(&p, "acme", 1)).unwrap();
        assert!(value.get("repository").is_none());
    }

    #[test]
    fn branch_name_is_deterministic_and_content_sensitive() {
        let a = vec![("index/x/metadata.json".to_string(), "one".to_string())];
        let b = vec![("index/x/metadata.json".to_string(), "two".to_string())];
        assert_eq!(branch_name("acme", &a), branch_name("acme", &a));
        assert_ne!(branch_name("acme", &a), branch_name("acme", &b));
        assert!(branch_name("acme", &a).starts_with("announce/acme-"));
    }
}
