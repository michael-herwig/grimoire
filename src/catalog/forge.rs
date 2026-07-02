// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! Forge resolution for `grim publish --announce` — which forge API (if
//! any) fronts the index git repository, and how to talk to it.
//!
//! A *forge* is the API flavor of the git host: GitHub (github.com or a
//! GitHub Enterprise instance), GitLab (gitlab.com or self-hosted), or
//! plain git (no vendor API). The kind is decoupled from the host name so
//! enterprise instances work without host constants. Resolution order:
//! explicit `[announce] forge` > the CI environment (only when the CI
//! server host equals the announce target host — a GitLab pipeline
//! announcing to a GitHub index must not inherit GitLab credentials) >
//! the github.com convention > plain.
//!
//! All forge traffic is REST — no `gh`/`glab` CLI dependency. Tokens are
//! sent as request headers only and never logged.

use serde::{Deserialize, Serialize};

use super::index_announce::AnnounceError;

/// The API flavor of the git host an index repository lives on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ForgeKind {
    /// github.com or a GitHub Enterprise instance (REST API v3).
    #[serde(rename = "github")]
    GitHub,
    /// gitlab.com or a self-hosted GitLab instance (REST API v4).
    #[serde(rename = "gitlab")]
    GitLab,
    /// A plain git host without a (known) forge API.
    Plain,
}

/// Snapshot of the CI/env variables forge resolution reads.
///
/// A plain struct rather than ambient `std::env` reads so resolution is
/// unit-testable (env mutation is `unsafe` in edition 2024 and the crate
/// forbids unsafe).
#[derive(Debug, Default, Clone)]
pub struct CiEnv {
    /// `GRIM_ANNOUNCE_TOKEN` — explicit announce token, always wins.
    pub announce_token: Option<String>,
    /// `GITHUB_ACTIONS` truthy.
    pub github_actions: bool,
    /// `GITHUB_SERVER_URL` (e.g. `https://github.example.corp`).
    pub github_server_url: Option<String>,
    /// `GITHUB_API_URL`.
    pub github_api_url: Option<String>,
    /// `GITHUB_REPOSITORY_OWNER` — namespace default in Actions.
    pub github_repository_owner: Option<String>,
    /// `GH_TOKEN` else `GITHUB_TOKEN`.
    pub github_token: Option<String>,
    /// `GITLAB_CI` truthy.
    pub gitlab_ci: bool,
    /// `CI_SERVER_HOST`.
    pub ci_server_host: Option<String>,
    /// `CI_API_V4_URL`.
    pub ci_api_v4_url: Option<String>,
    /// `CI_PROJECT_NAMESPACE` — namespace default in GitLab CI.
    pub ci_project_namespace: Option<String>,
    /// `GITLAB_TOKEN` (never `CI_JOB_TOKEN` — it cannot open MRs).
    pub gitlab_token: Option<String>,
}

impl CiEnv {
    /// Read the snapshot from the process environment.
    pub fn from_env() -> Self {
        let var = |key: &str| std::env::var(key).ok().filter(|v| !v.is_empty());
        let truthy = |key: &str| {
            std::env::var(key)
                .is_ok_and(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"))
        };
        Self {
            announce_token: var("GRIM_ANNOUNCE_TOKEN"),
            github_actions: truthy("GITHUB_ACTIONS"),
            github_server_url: var("GITHUB_SERVER_URL"),
            github_api_url: var("GITHUB_API_URL"),
            github_repository_owner: var("GITHUB_REPOSITORY_OWNER"),
            github_token: var("GH_TOKEN").or_else(|| var("GITHUB_TOKEN")),
            gitlab_ci: truthy("GITLAB_CI"),
            ci_server_host: var("CI_SERVER_HOST"),
            ci_api_v4_url: var("CI_API_V4_URL"),
            ci_project_namespace: var("CI_PROJECT_NAMESPACE"),
            gitlab_token: var("GITLAB_TOKEN"),
        }
    }
}

/// The resolved forge: kind, API endpoint, credential, CI namespace hint.
#[derive(Debug, Clone)]
pub struct ForgeContext {
    /// The resolved forge kind.
    pub kind: ForgeKind,
    /// API base URL without a trailing slash (`https://api.github.com`,
    /// `https://gitlab.example.com/api/v4`). `None` for plain git hosts.
    pub api_url: Option<String>,
    /// API token. Sent as a request header only — never logged.
    pub token: Option<String>,
    /// Namespace default contributed by a host-matched CI environment.
    pub ci_namespace: Option<String>,
}

/// A host-matched CI contribution (kind + endpoint + token + namespace).
struct CiCandidate {
    kind: ForgeKind,
    api_url: Option<String>,
    token: Option<String>,
    namespace: Option<String>,
}

/// Resolve the forge for an announce targeting `host`.
///
/// `host` is the index host path segment (`index/<host>/…`), already
/// derived from the repository URL or set explicitly. The CI environment
/// contributes API URL / token / namespace **only** when its server host
/// equals `host` and its forge kind survived resolution — explicit config
/// always overrides.
pub fn resolve(explicit: Option<ForgeKind>, api_url_override: Option<String>, host: &str, env: &CiEnv) -> ForgeContext {
    let ci = ci_candidate(host, env);
    let kind = explicit
        .or(ci.as_ref().map(|c| c.kind))
        .unwrap_or(if host.eq_ignore_ascii_case("github.com") {
            ForgeKind::GitHub
        } else {
            ForgeKind::Plain
        });
    // A CI candidate of a different kind than the resolved one contributes
    // nothing (e.g. explicit `forge = "gitlab"` inside GitHub Actions).
    let ci = ci.filter(|c| c.kind == kind);

    let api_url = match kind {
        ForgeKind::Plain => None,
        _ => api_url_override
            .or_else(|| ci.as_ref().and_then(|c| c.api_url.clone()))
            .or_else(|| conventional_api_url(kind, host)),
    };
    let token = env
        .announce_token
        .clone()
        .or_else(|| ci.as_ref().and_then(|c| c.token.clone()));
    let ci_namespace = ci.and_then(|c| c.namespace);
    ForgeContext {
        kind,
        api_url,
        token,
        ci_namespace,
    }
}

/// The CI environment's contribution, gated on a server-host match.
fn ci_candidate(host: &str, env: &CiEnv) -> Option<CiCandidate> {
    if env.github_actions {
        let server_host = env
            .github_server_url
            .as_deref()
            .and_then(host_of_url)
            .unwrap_or_else(|| "github.com".to_string());
        if server_host.eq_ignore_ascii_case(host) {
            return Some(CiCandidate {
                kind: ForgeKind::GitHub,
                api_url: env.github_api_url.clone(),
                token: env.github_token.clone(),
                namespace: env.github_repository_owner.clone(),
            });
        }
    }
    if env.gitlab_ci
        && env
            .ci_server_host
            .as_deref()
            .is_some_and(|h| h.eq_ignore_ascii_case(host))
    {
        return Some(CiCandidate {
            kind: ForgeKind::GitLab,
            api_url: env.ci_api_v4_url.clone(),
            token: env.gitlab_token.clone(),
            namespace: env.ci_project_namespace.clone(),
        });
    }
    None
}

/// The conventional API base for a forge kind on `host`. github.com's API
/// lives on its own host; GitHub Enterprise serves `/api/v3`, GitLab
/// `/api/v4` — both on the instance host.
fn conventional_api_url(kind: ForgeKind, host: &str) -> Option<String> {
    match kind {
        ForgeKind::GitHub if host.eq_ignore_ascii_case("github.com") => Some("https://api.github.com".to_string()),
        ForgeKind::GitHub => Some(format!("https://{host}/api/v3")),
        ForgeKind::GitLab => Some(format!("https://{host}/api/v4")),
        ForgeKind::Plain => None,
    }
}

/// The host segment of an `https://host/...` (or other schemed) URL.
fn host_of_url(url: &str) -> Option<String> {
    let normalized = crate::oci::git_provenance::normalize_remote_url(url.trim_end_matches('/')).or_else(|| {
        // normalize_remote_url requires a path; a bare server URL
        // (`https://github.example.corp`) has none — take the
        // authority directly.
        let rest = url.strip_prefix("https://").or_else(|| url.strip_prefix("http://"))?;
        let host = rest.split('/').next()?;
        (!host.is_empty()).then(|| format!("https://{host}/x"))
    })?;
    normalized
        .strip_prefix("https://")?
        .split('/')
        .next()
        .map(str::to_lowercase)
}

/// Percent-encode a path segment (RFC 3986 unreserved characters pass
/// through). Encodes `/` too — GitLab project paths and namespaces embed
/// in a single path segment (`platform%2Fai`).
fn encode_segment(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for byte in raw.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(byte as char);
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// Shared HTTP client for forge API calls: 30s timeout, grim user-agent.
fn client() -> Result<reqwest::Client, reqwest::Error> {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .user_agent(concat!("grim/", env!("CARGO_PKG_VERSION")))
        .build()
}

/// Attach the forge-appropriate auth header (GitHub: `Authorization:
/// Bearer`; GitLab: `PRIVATE-TOKEN`) when a token is present.
fn authorize(ctx: &ForgeContext, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
    let request = match ctx.kind {
        ForgeKind::GitHub => request.header("Accept", "application/vnd.github+json"),
        _ => request,
    };
    match (&ctx.token, ctx.kind) {
        (Some(token), ForgeKind::GitHub) => request.header("Authorization", format!("Bearer {token}")),
        (Some(token), ForgeKind::GitLab) => request.header("PRIVATE-TOKEN", token.as_str()),
        _ => request,
    }
}

/// Look up the namespace's numeric owner id on the forge: the immutable
/// account id on GitHub, the namespace id (uniform for users and nested
/// groups) on GitLab.
///
/// # Errors
///
/// [`AnnounceError::OwnerLookup`] when the forge has no API, the request
/// fails, or the response carries no numeric id.
pub async fn lookup_owner_id(ctx: &ForgeContext, namespace: &str) -> Result<u64, AnnounceError> {
    let wrap = |source: Box<dyn std::error::Error + Send + Sync>| AnnounceError::OwnerLookup {
        namespace: namespace.to_string(),
        source,
    };
    let api = ctx
        .api_url
        .as_deref()
        .ok_or_else(|| wrap("plain git host has no owner API — set `[announce] owner_id`".into()))?;
    let url = match ctx.kind {
        ForgeKind::GitHub => format!("{api}/users/{}", encode_segment(namespace)),
        ForgeKind::GitLab => format!("{api}/namespaces/{}", encode_segment(namespace)),
        // Plain never carries an api_url, so the check above already
        // returned — kept as an error rather than a panic macro.
        ForgeKind::Plain => {
            return Err(wrap(
                "plain git host has no owner API — set `[announce] owner_id`".into(),
            ));
        }
    };
    let response = authorize(ctx, client().map_err(|e| wrap(e.into()))?.get(url))
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|e| wrap(e.into()))?;
    let body: serde_json::Value = response.json().await.map_err(|e| wrap(e.into()))?;
    body.get("id")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| wrap("response carries no numeric id".into()))
}

/// The authenticated GitHub login (`GET /user`), as a namespace default.
/// Best-effort: `None` without a token or on any API failure.
pub async fn github_login(ctx: &ForgeContext) -> Option<String> {
    if ctx.kind != ForgeKind::GitHub || ctx.token.is_none() {
        return None;
    }
    let api = ctx.api_url.as_deref()?;
    let response = authorize(ctx, client().ok()?.get(format!("{api}/user")))
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .ok()?;
    let body: serde_json::Value = response.json().await.ok()?;
    body.get("login")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
}

/// Open (or find) the pull/merge request for a pushed announce branch via
/// the forge API. Best-effort by contract: every failure degrades to
/// `None` and the caller reports the pushed branch instead — a failed PR
/// is never worse than today's plain push.
pub async fn create_change_request(ctx: &ForgeContext, repo_url: &str, branch: &str, title: &str) -> Option<String> {
    let (Some(_), Some(api)) = (&ctx.token, ctx.api_url.clone()) else {
        return None;
    };
    let project = project_path(repo_url)?;
    let result = match ctx.kind {
        ForgeKind::GitHub => github_pull_request(ctx, &api, &project, branch, title).await,
        ForgeKind::GitLab => gitlab_merge_request(ctx, &api, &project, branch, title).await,
        ForgeKind::Plain => return None,
    };
    match result {
        Ok(url) => Some(url),
        Err(detail) => {
            tracing::info!(
                "forge API did not open the change request ({detail}); the branch is pushed — open it manually"
            );
            None
        }
    }
}

/// The forge project path (`owner/repo`, `group/subgroup/project`) from
/// the index repository URL.
fn project_path(repo_url: &str) -> Option<String> {
    let url = repo_url.strip_prefix("git+").unwrap_or(repo_url);
    let https = crate::oci::git_provenance::normalize_remote_url(url)?;
    let (_, path) = https.strip_prefix("https://")?.split_once('/')?;
    (!path.is_empty()).then(|| path.to_string())
}

/// `POST /repos/{project}/pulls`, reusing an existing open PR on 422.
async fn github_pull_request(
    ctx: &ForgeContext,
    api: &str,
    project: &str,
    branch: &str,
    title: &str,
) -> Result<String, String> {
    let client = client().map_err(|e| e.to_string())?;
    let base: serde_json::Value = authorize(ctx, client.get(format!("{api}/repos/{project}")))
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|e| e.to_string())?
        .json()
        .await
        .map_err(|e| e.to_string())?;
    let default_branch = base
        .get("default_branch")
        .and_then(serde_json::Value::as_str)
        .ok_or("repository response carries no default branch")?;

    let response = authorize(ctx, client.post(format!("{api}/repos/{project}/pulls")))
        .json(&serde_json::json!({
            "title": title,
            "head": branch,
            "base": default_branch,
            "body": "Automated announcement via `grim publish --announce`.",
        }))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if response.status().as_u16() == 422 {
        // Likely "a pull request already exists" — the force-push above
        // already updated it; find and reuse its URL.
        let owner = project.split('/').next().unwrap_or_default();
        let existing: serde_json::Value = authorize(
            ctx,
            client
                .get(format!("{api}/repos/{project}/pulls"))
                .query(&[("head", format!("{owner}:{branch}")), ("state", "open".to_string())]),
        )
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|e| e.to_string())?
        .json()
        .await
        .map_err(|e| e.to_string())?;
        return existing
            .get(0)
            .and_then(|pr| pr.get("html_url"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| "pull request exists but could not be located".to_string());
    }
    let body: serde_json::Value = response
        .error_for_status()
        .map_err(|e| e.to_string())?
        .json()
        .await
        .map_err(|e| e.to_string())?;
    body.get("html_url")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| "pull request response carries no URL".to_string())
}

/// `POST /projects/{path}/merge_requests`, reusing the open MR on 409.
async fn gitlab_merge_request(
    ctx: &ForgeContext,
    api: &str,
    project: &str,
    branch: &str,
    title: &str,
) -> Result<String, String> {
    let client = client().map_err(|e| e.to_string())?;
    let encoded = encode_segment(project);
    let base: serde_json::Value = authorize(ctx, client.get(format!("{api}/projects/{encoded}")))
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|e| e.to_string())?
        .json()
        .await
        .map_err(|e| e.to_string())?;
    let default_branch = base
        .get("default_branch")
        .and_then(serde_json::Value::as_str)
        .ok_or("project response carries no default branch")?;

    let response = authorize(ctx, client.post(format!("{api}/projects/{encoded}/merge_requests")))
        .json(&serde_json::json!({
            "source_branch": branch,
            "target_branch": default_branch,
            "title": title,
            "description": "Automated announcement via `grim publish --announce`.",
            "squash": true,
            "remove_source_branch": true,
        }))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if response.status().as_u16() == 409 {
        // An MR for this source branch is already open — the force-push
        // above already updated it; find and reuse its URL.
        let existing: serde_json::Value = authorize(
            ctx,
            client
                .get(format!("{api}/projects/{encoded}/merge_requests"))
                .query(&[("source_branch", branch), ("state", "opened")]),
        )
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|e| e.to_string())?
        .json()
        .await
        .map_err(|e| e.to_string())?;
        return existing
            .get(0)
            .and_then(|mr| mr.get("web_url"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| "merge request exists but could not be located".to_string());
    }
    let body: serde_json::Value = response
        .error_for_status()
        .map_err(|e| e.to_string())?
        .json()
        .await
        .map_err(|e| e.to_string())?;
    body.get("web_url")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| "merge request response carries no URL".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gitlab_ci_env(host: &str) -> CiEnv {
        CiEnv {
            gitlab_ci: true,
            ci_server_host: Some(host.to_string()),
            ci_api_v4_url: Some(format!("https://{host}/api/v4")),
            ci_project_namespace: Some("platform".to_string()),
            gitlab_token: Some("glpat-x".to_string()),
            ..CiEnv::default()
        }
    }

    fn github_actions_env(server: &str) -> CiEnv {
        CiEnv {
            github_actions: true,
            github_server_url: Some(server.to_string()),
            github_api_url: Some(format!("{server}/api/v3")),
            github_repository_owner: Some("acme".to_string()),
            github_token: Some("ghp-x".to_string()),
            ..CiEnv::default()
        }
    }

    #[test]
    fn explicit_forge_wins_over_ci_and_convention() {
        let ctx = resolve(
            Some(ForgeKind::Plain),
            None,
            "gitlab.example.com",
            &gitlab_ci_env("gitlab.example.com"),
        );
        assert_eq!(ctx.kind, ForgeKind::Plain);
        assert_eq!(ctx.api_url, None, "plain forge never carries an API URL");
        assert_eq!(ctx.ci_namespace, None, "kind-mismatched CI contributes nothing");
    }

    #[test]
    fn gitlab_ci_matching_host_contributes_api_token_namespace() {
        let ctx = resolve(None, None, "gitlab.example.com", &gitlab_ci_env("gitlab.example.com"));
        assert_eq!(ctx.kind, ForgeKind::GitLab);
        assert_eq!(ctx.api_url.as_deref(), Some("https://gitlab.example.com/api/v4"));
        assert_eq!(ctx.token.as_deref(), Some("glpat-x"));
        assert_eq!(ctx.ci_namespace.as_deref(), Some("platform"));
    }

    #[test]
    fn ci_host_mismatch_contributes_nothing() {
        // A GitLab pipeline announcing to a github.com index must not
        // inherit GitLab credentials or API config.
        let ctx = resolve(None, None, "github.com", &gitlab_ci_env("gitlab.example.com"));
        assert_eq!(ctx.kind, ForgeKind::GitHub, "github.com convention still applies");
        assert_eq!(ctx.api_url.as_deref(), Some("https://api.github.com"));
        assert_eq!(ctx.token, None);
        assert_eq!(ctx.ci_namespace, None);
    }

    #[test]
    fn github_actions_on_enterprise_host_matches() {
        let ctx = resolve(
            None,
            None,
            "github.example.corp",
            &github_actions_env("https://github.example.corp"),
        );
        assert_eq!(ctx.kind, ForgeKind::GitHub);
        assert_eq!(ctx.api_url.as_deref(), Some("https://github.example.corp/api/v3"));
        assert_eq!(ctx.token.as_deref(), Some("ghp-x"));
        assert_eq!(ctx.ci_namespace.as_deref(), Some("acme"));
    }

    #[test]
    fn enterprise_github_api_convention_without_ci() {
        let ctx = resolve(Some(ForgeKind::GitHub), None, "github.example.corp", &CiEnv::default());
        assert_eq!(ctx.api_url.as_deref(), Some("https://github.example.corp/api/v3"));
        assert_eq!(ctx.token, None);
    }

    #[test]
    fn announce_token_beats_ci_token_and_api_override_beats_ci() {
        let mut env = gitlab_ci_env("gitlab.example.com");
        env.announce_token = Some("explicit".to_string());
        let ctx = resolve(
            None,
            Some("https://proxy.example.com/api/v4".to_string()),
            "gitlab.example.com",
            &env,
        );
        assert_eq!(ctx.token.as_deref(), Some("explicit"));
        assert_eq!(ctx.api_url.as_deref(), Some("https://proxy.example.com/api/v4"));
    }

    #[test]
    fn unknown_host_without_ci_resolves_plain() {
        let ctx = resolve(None, None, "git.example.test", &CiEnv::default());
        assert_eq!(ctx.kind, ForgeKind::Plain);
        assert_eq!(ctx.api_url, None);
        assert_eq!(ctx.token, None);
    }

    #[test]
    fn announce_token_applies_even_without_ci() {
        let env = CiEnv {
            announce_token: Some("t".to_string()),
            ..CiEnv::default()
        };
        let ctx = resolve(Some(ForgeKind::GitLab), None, "gitlab.example.com", &env);
        assert_eq!(ctx.token.as_deref(), Some("t"));
    }

    #[test]
    fn encode_segment_escapes_slashes_and_specials() {
        assert_eq!(encode_segment("platform/ai"), "platform%2Fai");
        assert_eq!(encode_segment("a-b.c_d~e"), "a-b.c_d~e");
        assert_eq!(encode_segment("sp ace"), "sp%20ace");
    }

    #[test]
    fn project_path_from_url_shapes() {
        assert_eq!(
            project_path("https://gitlab.example.com/platform/ai/index.git").as_deref(),
            Some("platform/ai/index")
        );
        assert_eq!(
            project_path("git+ssh://git@gitlab.example.com:2222/platform/index.git").as_deref(),
            Some("platform/index")
        );
        assert_eq!(project_path("/tmp/local-index.git"), None);
    }

    #[test]
    fn host_of_url_handles_bare_server_urls() {
        assert_eq!(host_of_url("https://github.com").as_deref(), Some("github.com"));
        assert_eq!(
            host_of_url("https://github.example.corp/").as_deref(),
            Some("github.example.corp")
        );
    }
}
