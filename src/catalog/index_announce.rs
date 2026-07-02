// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! Package-index announcement — the write side of [`super::index_source`].
//!
//! `grim publish --announce` records published packages in a package-index
//! git repository: clone, write `index/<host>/<ns>/<pkg>/metadata.json`
//! pointers, commit on a deterministic topic branch, push, and open the
//! pull/merge request through the resolved forge API ([`super::forge`],
//! GitHub or GitLab — enterprise instances included, no CLI dependency).
//! A GitLab host without an API token gets the MR via git push options
//! (`-o merge_request.create`); a plain git host gets the pushed branch.
//!
//! The announced metadata is the phone-book pointer only (name, kind,
//! tagless ref, description, ownership) — never versions. Re-announcing
//! unchanged content is detected via `git status` and reported as
//! [`AnnounceOutcome::UpToDate`] without a push.

use std::path::Path;

use super::forge::{ForgeContext, ForgeKind};

/// The default public index announcements target.
pub const DEFAULT_INDEX_REPO: &str = "https://github.com/grimoire-rs/index";

/// Derive the index host path segment (`index/<host>/…`) from the index
/// repository locator: strip a `git+` transport prefix, normalize the
/// remote shape (https / ssh / scp-like, credentials and ports stripped),
/// and take the lowercased host. `None` for locators without a host (a
/// local path, `file://`) — those need an explicit `[announce] host`.
pub fn index_host(repo_url: &str) -> Option<String> {
    let url = repo_url.strip_prefix("git+").unwrap_or(repo_url);
    let https = crate::oci::git_provenance::normalize_remote_url(url)?;
    https.strip_prefix("https://")?.split('/').next().map(str::to_lowercase)
}

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
    /// The index host path segment — pointers land under
    /// `index/<host>/<namespace>/`.
    pub host: String,
    /// The `index/<host>/<namespace>/` the packages land under.
    pub namespace: String,
    /// The namespace's numeric owner id on the index host (resolved by the
    /// caller — explicitly configured or looked up via the forge API).
    pub owner_id: u64,
    /// The resolved forge fronting the index repository.
    pub forge: ForgeContext,
    /// The packages to announce.
    pub packages: Vec<AnnouncePackage>,
}

/// What the announce achieved.
#[derive(Debug, PartialEq, Eq)]
pub enum AnnounceOutcome {
    /// A pull/merge request was opened — via the forge API, or by a forge
    /// honoring `merge_request.create` push options.
    PullRequest {
        /// The PR/MR URL.
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
        let relative = format!(
            "index/{}/{}/{}/metadata.json",
            request.host, request.namespace, pkg.name
        );
        rendered.push((
            relative,
            metadata_json(pkg, &request.namespace, request.owner_id, request.forge.kind),
        ));
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

    // Force-push our own topic branch (deterministic name ⇒ safe to move),
    // then open the change request the best way the forge allows.
    let api_capable =
        request.forge.token.is_some() && matches!(request.forge.kind, ForgeKind::GitHub | ForgeKind::GitLab);
    if api_capable || request.forge.kind == ForgeKind::GitHub {
        git(Some(&clone), "push", &["push", "--quiet", "--force", "origin", &branch]).await?;
        if api_capable
            && let Some(url) =
                super::forge::create_change_request(&request.forge, &request.repo_url, &branch, &message).await
        {
            return Ok(AnnounceOutcome::PullRequest { url });
        }
        return Ok(AnnounceOutcome::BranchPushed { branch });
    }

    // GitLab without a token, or a plain git host: ask the server to open
    // the MR via push options (native GitLab feature, harmless elsewhere).
    // A server without push-options support fails the whole push — retry
    // once as a plain push rather than sniffing localized git stderr.
    let title_option = format!("merge_request.title={message}");
    let options_push = git_stderr(
        &clone,
        "push",
        &[
            "push",
            "--force",
            "-o",
            "merge_request.create",
            "-o",
            &title_option,
            "origin",
            &branch,
        ],
    )
    .await;
    match options_push {
        Ok(stderr) => Ok(match merge_request_url(&stderr) {
            Some(url) => AnnounceOutcome::PullRequest { url },
            None => AnnounceOutcome::BranchPushed { branch },
        }),
        Err(_) => {
            // `?` propagates the retry's error — same root cause when the
            // push itself (not the options) is broken.
            git(Some(&clone), "push", &["push", "--quiet", "--force", "origin", &branch]).await?;
            Ok(AnnounceOutcome::BranchPushed { branch })
        }
    }
}

/// The created/updated MR URL from a `merge_request.create` push's stderr:
/// an `http(s)://` token containing `/merge_requests/` whose final path
/// segment is all digits. Deliberately rejects the `/merge_requests/new?…`
/// *suggestion* URL a plain GitLab push prints.
fn merge_request_url(push_stderr: &str) -> Option<String> {
    push_stderr
        .split_whitespace()
        .find(|token| {
            (token.starts_with("https://") || token.starts_with("http://"))
                && token.contains("/merge_requests/")
                && token
                    .rsplit('/')
                    .next()
                    .is_some_and(|last| !last.is_empty() && last.bytes().all(|b| b.is_ascii_digit()))
        })
        .map(str::to_string)
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

/// Render the metadata.json pointer for `pkg` (index spec v1).
///
/// The owner key is `github` for GitHub-forge pointers (spec-v1 compatible
/// with the default index's validator) and the generic `login` for any
/// other host — the pointer's `index/<host>/` segment carries the forge
/// context.
fn metadata_json(pkg: &AnnouncePackage, namespace: &str, owner_id: u64, forge: ForgeKind) -> String {
    let owner_key = match forge {
        ForgeKind::GitHub => "github",
        ForgeKind::GitLab | ForgeKind::Plain => "login",
    };
    let mut value = serde_json::json!({
        "schema": 1,
        "name": pkg.name,
        "kind": pkg.kind,
        "ref": pkg.reference,
        "description": pkg.description,
        "owner": { owner_key: namespace, "id": owner_id },
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
    let output = git_output_impl(Some(cwd), action, args).await?;
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Run a git subprocess in `cwd` and return its stderr — where git remotes
/// print their responses (e.g. GitLab's "View merge request" line after a
/// `merge_request.create` push option).
async fn git_stderr(cwd: &Path, action: &'static str, args: &[&str]) -> Result<String, AnnounceError> {
    let output = git_output_impl(Some(cwd), action, args).await?;
    Ok(String::from_utf8_lossy(&output.stderr).into_owned())
}

async fn git_output_impl(
    cwd: Option<&Path>,
    action: &'static str,
    args: &[&str],
) -> Result<std::process::Output, AnnounceError> {
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
    Ok(output)
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
        let rendered = metadata_json(&pkg("code-review"), "acme", 42, ForgeKind::GitHub);
        let value: serde_json::Value = serde_json::from_str(&rendered).expect("valid JSON");
        assert_eq!(value["schema"], 1);
        assert_eq!(value["name"], "code-review");
        assert_eq!(value["kind"], "skill");
        assert_eq!(value["ref"], "ghcr.io/acme/skills/code-review");
        assert_eq!(value["owner"]["github"], "acme");
        assert_eq!(value["owner"]["id"], 42);
        assert!(value["owner"].get("login").is_none());
        assert_eq!(value["repository"], "https://github.com/acme/skills");
        assert!(rendered.ends_with('\n'), "trailing newline for clean diffs");
    }

    #[test]
    fn metadata_json_uses_generic_login_key_off_github() {
        for forge in [ForgeKind::GitLab, ForgeKind::Plain] {
            let rendered = metadata_json(&pkg("x"), "platform/ai", 44, forge);
            let value: serde_json::Value = serde_json::from_str(&rendered).expect("valid JSON");
            assert_eq!(value["owner"]["login"], "platform/ai", "{forge:?}");
            assert_eq!(value["owner"]["id"], 44, "{forge:?}");
            assert!(value["owner"].get("github").is_none(), "{forge:?}");
        }
    }

    #[test]
    fn metadata_json_omits_absent_repository() {
        let mut p = pkg("x");
        p.repository_url = None;
        let value: serde_json::Value = serde_json::from_str(&metadata_json(&p, "acme", 1, ForgeKind::GitHub)).unwrap();
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

    #[test]
    fn index_host_derives_from_locator_shapes() {
        for (locator, expected) in [
            ("https://github.com/grimoire-rs/index", Some("github.com")),
            (
                "https://gitlab.example.com/platform/index.git",
                Some("gitlab.example.com"),
            ),
            (
                "git+https://gitlab.example.com/platform/index.git",
                Some("gitlab.example.com"),
            ),
            ("ssh://git@gitlab.corp:2222/platform/index.git", Some("gitlab.corp")),
            ("git@GitLab.Example.com:platform/index.git", Some("gitlab.example.com")),
            (
                "https://oauth2:token@gitlab.example.com/g/index.git",
                Some("gitlab.example.com"),
            ),
            ("/tmp/local-index.git", None),
            ("file:///tmp/local-index.git", None),
        ] {
            assert_eq!(index_host(locator).as_deref(), expected, "{locator}");
        }
    }

    #[test]
    fn merge_request_url_extracts_created_mr_only() {
        let created = "remote: View merge request for announce/acme-12345678:\n\
                       remote:   https://gitlab.example.com/platform/index/-/merge_requests/7\n";
        assert_eq!(
            merge_request_url(created).as_deref(),
            Some("https://gitlab.example.com/platform/index/-/merge_requests/7")
        );

        let suggestion = "remote: To create a merge request for announce/acme-12345678, visit:\n\
                          remote:   https://gitlab.example.com/platform/index/-/merge_requests/new?merge_request%5Bsource_branch%5D=announce\n";
        assert_eq!(merge_request_url(suggestion), None);
        assert_eq!(merge_request_url(""), None);
    }
}
