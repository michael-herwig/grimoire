# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The Grimoire Authors
"""`grim publish --announce` acceptance tests.

--announce records published packages in a package-index git repository:
clone → write index/<host>/<ns>/<pkg>/metadata.json → commit on a
deterministic topic branch → push → open the PR/MR via the forge API
(GitHub/GitLab), via git push options (token-less GitLab), or leave the
pushed branch (plain git host).

Hermetic setup: the announce target is a real-looking
`https://git.example.test/acme/index.git` URL that a `GIT_CONFIG_*`
insteadOf rewrite points at a local bare repository — the host derivation
and forge project-path parsing see the URL, git clone/push see the local
repo. Forge APIs are a local HTTP server injected via `[announce] api_url`
(or the CI env's `CI_API_V4_URL`). Explicit `owner_id` keeps most tests
free of owner-lookup traffic.
"""
from __future__ import annotations

import json
import subprocess
import sys
import threading
import uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

import pytest

from src.registry import REGISTRY_HOST

INDEX_URL = "https://git.example.test/acme/index.git"
INDEX_HOST = "git.example.test"
MR_URL = "https://git.example.test/acme/index/-/merge_requests/7"
PR_URL = "https://git.example.test/acme/index/pull/7"
TOKEN = "glpat-test-secret-value"


def _write(p: Path, body: str) -> None:
    p.parent.mkdir(parents=True, exist_ok=True)
    p.write_text(body)


def _make_skill_source(project_dir: Path, name: str, description: str) -> None:
    _write(
        project_dir / "skills" / name / "SKILL.md",
        f"---\nname: {name}\ndescription: {description}\n"
        f"metadata:\n  repository: https://github.com/acme/{name}\n---\n# {name}\n",
    )


def _git(cwd: Path, *args: str) -> str:
    result = subprocess.run(
        ["git", "-c", "user.email=t@t", "-c", "user.name=t", *args],
        cwd=cwd,
        check=True,
        capture_output=True,
        text=True,
    )
    return result.stdout


def _bare_index_repo(tmp_path: Path) -> Path:
    """A seeded bare repository standing in for a custom index host."""
    seed = tmp_path / "index-seed"
    seed.mkdir()
    (seed / "README.md").write_text("# index\n")
    subprocess.run(["git", "init", "-q", str(seed)], check=True, capture_output=True)
    _git(seed, "add", "-A")
    _git(seed, "commit", "-q", "-m", "seed")
    bare = tmp_path / "index.git"
    subprocess.run(
        ["git", "clone", "--bare", "-q", str(seed), str(bare)],
        check=True,
        capture_output=True,
    )
    return bare


def _index_remote(tmp_path: Path, runner) -> Path:
    """Rewrite INDEX_URL to a local bare repo for every git grim spawns."""
    bare = _bare_index_repo(tmp_path)
    runner.env.update(
        {
            "GIT_CONFIG_COUNT": "1",
            "GIT_CONFIG_KEY_0": f"url.{bare}.insteadOf",
            "GIT_CONFIG_VALUE_0": INDEX_URL,
        }
    )
    return bare


def _manifest(
    project_dir: Path,
    ns: str,
    name: str,
    repository: str,
    *,
    owner_id: int | None = 42,
    host: str | None = None,
    forge: str | None = None,
    api_url: str | None = None,
) -> None:
    announce = [f'repository = "{repository}"', 'namespace = "acme"']
    if owner_id is not None:
        announce.append(f"owner_id = {owner_id}")
    if host is not None:
        announce.append(f'host = "{host}"')
    if forge is not None:
        announce.append(f'forge = "{forge}"')
    if api_url is not None:
        announce.append(f'api_url = "{api_url}"')
    _write(
        project_dir / "publish.toml",
        f'registry = "{REGISTRY_HOST}"\n'
        f'repository_prefix = "{ns}"\n'
        f"\n[announce]\n" + "\n".join(announce) + "\n"
        f"\n[skills.{name}]\n"
        f'version = "0.1.0"\n',
    )


class _ForgeApi:
    """Minimal fake forge API (GitHub + GitLab routes), recording requests."""

    def __init__(self, conflict: bool = False) -> None:
        api = self
        self.requests: list[tuple[str, str]] = []
        self.conflict = conflict

        class Handler(BaseHTTPRequestHandler):
            def _reply(self, code: int, body: object) -> None:
                payload = json.dumps(body).encode()
                self.send_response(code)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(payload)))
                self.end_headers()
                self.wfile.write(payload)

            def do_GET(self) -> None:  # noqa: N802 (http.server API)
                api.requests.append(("GET", self.path))
                if "/namespaces/" in self.path:
                    self._reply(200, {"kind": "group", "id": 44, "full_path": "acme"})
                elif "/merge_requests?" in self.path:
                    self._reply(200, [{"web_url": MR_URL}])
                elif "/pulls?" in self.path:
                    self._reply(200, [{"html_url": PR_URL}])
                elif "/projects/" in self.path or "/repos/" in self.path:
                    self._reply(200, {"default_branch": "main"})
                else:
                    self._reply(404, {})

            def do_POST(self) -> None:  # noqa: N802 (http.server API)
                api.requests.append(("POST", self.path))
                self.rfile.read(int(self.headers.get("Content-Length") or 0))
                if self.path.endswith("/merge_requests"):
                    self._reply(409 if api.conflict else 201, {"web_url": MR_URL})
                elif self.path.endswith("/pulls"):
                    self._reply(422 if api.conflict else 201, {"html_url": PR_URL})
                else:
                    self._reply(404, {})

            def log_message(self, *args: object) -> None:
                pass

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        threading.Thread(target=self.server.serve_forever, daemon=True).start()
        self.url = f"http://127.0.0.1:{self.server.server_port}"

    def close(self) -> None:
        self.server.shutdown()


@pytest.fixture
def forge_api():
    apis: list[_ForgeApi] = []

    def make(conflict: bool = False) -> _ForgeApi:
        api = _ForgeApi(conflict)
        apis.append(api)
        return api

    yield make
    for api in apis:
        api.close()


def _announce_branch(bare: Path) -> str:
    branches = _git(bare, "branch", "--list", "announce/*")
    assert "announce/acme-" in branches, f"topic branch missing: {branches!r}"
    return branches.strip().lstrip("* ").strip()


# ── plain git host (no forge API, no token) ────────────────────────────────


def test_publish_announce_pushes_branch_with_metadata(
    grim_at, project_dir: Path, registry: str, tmp_path: Path
) -> None:
    """Plain host: pointers land under the URL-derived index/<host>/ path
    with the generic owner.login key. The bare repo does not advertise push
    options, so this also exercises the plain-push retry after the
    merge_request.create options push is rejected."""
    ns = f"grim-test/{uuid.uuid4().hex[:12]}"
    name = "ann-skill"
    _make_skill_source(project_dir, name, "Announce me.")
    _manifest(project_dir, ns, name, INDEX_URL)

    runner = grim_at(project_dir)
    bare = _index_remote(tmp_path, runner)
    result = runner.run("publish", "--announce", check=False)
    assert result.returncode == 0, f"publish --announce failed: {result.stderr}"
    assert "announced:" in result.stderr, result.stderr

    branch = _announce_branch(bare)
    blob = _git(bare, "show", f"{branch}:index/{INDEX_HOST}/acme/{name}/metadata.json")
    meta = json.loads(blob)
    assert meta["schema"] == 1
    assert meta["name"] == name
    assert meta["kind"] == "skill"
    assert meta["ref"] == f"{REGISTRY_HOST}/{ns}/{name}", meta
    assert meta["description"] == "Announce me."
    assert meta["owner"] == {"login": "acme", "id": 42}
    assert meta["repository"] == f"https://github.com/acme/{name}"


def test_publish_announce_is_repeatable(
    grim_at, project_dir: Path, registry: str, tmp_path: Path
) -> None:
    """A re-run (packages already pushed → skipped) still announces cleanly
    onto the same deterministic topic branch."""
    ns = f"grim-test/{uuid.uuid4().hex[:12]}"
    name = "ann-repeat"
    _make_skill_source(project_dir, name, "Repeatable.")
    _manifest(project_dir, ns, name, INDEX_URL)

    runner = grim_at(project_dir)
    bare = _index_remote(tmp_path, runner)
    first = runner.run("publish", "--announce", check=False)
    assert first.returncode == 0, first.stderr
    second = runner.run("publish", "--announce", check=False)
    assert second.returncode == 0, second.stderr

    branches = [
        b.strip().lstrip("* ").strip()
        for b in _git(bare, "branch", "--list", "announce/*").splitlines()
    ]
    assert len(branches) == 1, f"identical content must reuse one branch: {branches}"


def test_publish_announce_dry_run_touches_nothing(
    grim_at, project_dir: Path, registry: str, tmp_path: Path
) -> None:
    ns = f"grim-test/{uuid.uuid4().hex[:12]}"
    name = "ann-dry"
    _make_skill_source(project_dir, name, "Dry.")
    _manifest(project_dir, ns, name, INDEX_URL)

    runner = grim_at(project_dir)
    bare = _index_remote(tmp_path, runner)
    result = runner.run("publish", "--announce", "--dry-run", check=False)
    assert result.returncode == 0, result.stderr
    assert "announce: skipped (dry run)" in result.stderr

    branches = _git(bare, "branch", "--list", "announce/*")
    assert branches.strip() == "", f"dry run must not push: {branches!r}"


@pytest.mark.skipif(sys.platform == "win32", reason="shell hook fixture")
def test_publish_announce_push_options_reach_the_server(
    grim_at, project_dir: Path, registry: str, tmp_path: Path
) -> None:
    """A server that advertises push options receives merge_request.create
    (the mechanism GitLab uses to open the MR server-side without a token)."""
    ns = f"grim-test/{uuid.uuid4().hex[:12]}"
    name = "ann-opts"
    _make_skill_source(project_dir, name, "Options.")
    _manifest(project_dir, ns, name, INDEX_URL)

    runner = grim_at(project_dir)
    bare = _index_remote(tmp_path, runner)
    _git(bare, "config", "receive.advertisePushOptions", "true")
    seen = bare / "push-options.txt"
    hook = bare / "hooks" / "post-receive"
    hook.write_text(f'#!/bin/sh\necho "${{GIT_PUSH_OPTION_0:-none}}" > "{seen}"\n')
    hook.chmod(0o755)

    result = runner.run("publish", "--announce", check=False)
    assert result.returncode == 0, result.stderr
    assert seen.read_text().strip() == "merge_request.create", (
        f"push option not received: {seen.read_text()!r}"
    )


# ── misconfiguration exits usage (64) ──────────────────────────────────────


def test_publish_announce_local_path_requires_host(
    grim_at, project_dir: Path, registry: str, tmp_path: Path
) -> None:
    """A locator without a derivable host (a local path) needs an explicit
    `[announce] host` — exit 64 names the key."""
    ns = f"grim-test/{uuid.uuid4().hex[:12]}"
    name = "ann-nohost"
    _make_skill_source(project_dir, name, "No host.")
    bare = _bare_index_repo(tmp_path)
    _manifest(project_dir, ns, name, str(bare))

    runner = grim_at(project_dir)
    result = runner.run("publish", "--announce", check=False)
    assert result.returncode == 64, f"expected 64, got {result.returncode}: {result.stderr}"
    assert "[announce] host" in result.stderr


def test_publish_announce_plain_host_requires_owner_id(
    grim_at, project_dir: Path, registry: str, tmp_path: Path
) -> None:
    """Without a forge API to resolve it from, owner_id must be explicit."""
    ns = f"grim-test/{uuid.uuid4().hex[:12]}"
    name = "ann-noowner"
    _make_skill_source(project_dir, name, "No owner.")
    _manifest(project_dir, ns, name, INDEX_URL, owner_id=None)

    runner = grim_at(project_dir)
    _index_remote(tmp_path, runner)
    result = runner.run("publish", "--announce", check=False)
    assert result.returncode == 64, f"expected 64, got {result.returncode}: {result.stderr}"
    assert "owner_id" in result.stderr


def test_publish_announce_unreachable_index_exits_unavailable(
    grim_at, project_dir: Path, registry: str, tmp_path: Path
) -> None:
    """A failing announce after a successful publish exits 69 (the packages
    ARE published; only the announcement needs a retry)."""
    ns = f"grim-test/{uuid.uuid4().hex[:12]}"
    name = "ann-fail"
    _make_skill_source(project_dir, name, "Unreachable index.")
    # host set explicitly so the failure is the clone, not host derivation
    _manifest(project_dir, ns, name, str(tmp_path / "no-such-repo.git"), host=INDEX_HOST)

    runner = grim_at(project_dir)
    result = runner.run("publish", "--announce", check=False)
    assert result.returncode == 69, f"expected 69, got {result.returncode}: {result.stderr}"
    assert "announce failed" in result.stderr


# ── forge APIs (fake server via api_url / CI env) ──────────────────────────


def test_publish_announce_gitlab_forge_opens_mr_via_api(
    grim_at, project_dir: Path, registry: str, tmp_path: Path, forge_api
) -> None:
    ns = f"grim-test/{uuid.uuid4().hex[:12]}"
    name = "ann-mr"
    _make_skill_source(project_dir, name, "MR me.")
    api = forge_api()
    _manifest(project_dir, ns, name, INDEX_URL, forge="gitlab", api_url=api.url)

    runner = grim_at(project_dir)
    bare = _index_remote(tmp_path, runner)
    runner.env["GRIM_ANNOUNCE_TOKEN"] = TOKEN
    result = runner.run("publish", "--announce", check=False)
    assert result.returncode == 0, result.stderr
    assert f"announced: {MR_URL}" in result.stderr, result.stderr
    assert ("POST", "/projects/acme%2Findex/merge_requests") in api.requests, api.requests
    _announce_branch(bare)  # the branch is pushed before the MR opens
    assert TOKEN not in result.stdout + result.stderr, "token must never be printed"


def test_publish_announce_gitlab_conflict_reuses_open_mr(
    grim_at, project_dir: Path, registry: str, tmp_path: Path, forge_api
) -> None:
    """A 409 (MR already open for the branch) reuses the existing MR URL."""
    ns = f"grim-test/{uuid.uuid4().hex[:12]}"
    name = "ann-409"
    _make_skill_source(project_dir, name, "Conflict.")
    api = forge_api(conflict=True)
    _manifest(project_dir, ns, name, INDEX_URL, forge="gitlab", api_url=api.url)

    runner = grim_at(project_dir)
    _index_remote(tmp_path, runner)
    runner.env["GRIM_ANNOUNCE_TOKEN"] = TOKEN
    result = runner.run("publish", "--announce", check=False)
    assert result.returncode == 0, result.stderr
    assert f"announced: {MR_URL}" in result.stderr, result.stderr
    assert any(m == "GET" and "/merge_requests?" in p for m, p in api.requests), api.requests


def test_publish_announce_github_forge_opens_pr_via_api(
    grim_at, project_dir: Path, registry: str, tmp_path: Path, forge_api
) -> None:
    ns = f"grim-test/{uuid.uuid4().hex[:12]}"
    name = "ann-pr"
    _make_skill_source(project_dir, name, "PR me.")
    api = forge_api()
    _manifest(project_dir, ns, name, INDEX_URL, forge="github", api_url=api.url)

    runner = grim_at(project_dir)
    bare = _index_remote(tmp_path, runner)
    runner.env["GRIM_ANNOUNCE_TOKEN"] = TOKEN
    result = runner.run("publish", "--announce", check=False)
    assert result.returncode == 0, result.stderr
    assert f"announced: {PR_URL}" in result.stderr, result.stderr
    assert ("POST", "/repos/acme/index/pulls") in api.requests, api.requests
    branch = _announce_branch(bare)
    # GitHub-forge pointers keep the spec-v1 owner.github key.
    blob = _git(bare, "show", f"{branch}:index/{INDEX_HOST}/acme/{name}/metadata.json")
    assert json.loads(blob)["owner"] == {"github": "acme", "id": 42}


def test_publish_announce_owner_id_resolves_via_gitlab_api(
    grim_at, project_dir: Path, registry: str, tmp_path: Path, forge_api
) -> None:
    """Without an explicit owner_id, a tokened GitLab forge resolves the
    namespace id from /namespaces/<path>."""
    ns = f"grim-test/{uuid.uuid4().hex[:12]}"
    name = "ann-ownerapi"
    _make_skill_source(project_dir, name, "Owner lookup.")
    api = forge_api()
    _manifest(project_dir, ns, name, INDEX_URL, owner_id=None, forge="gitlab", api_url=api.url)

    runner = grim_at(project_dir)
    bare = _index_remote(tmp_path, runner)
    runner.env["GRIM_ANNOUNCE_TOKEN"] = TOKEN
    result = runner.run("publish", "--announce", check=False)
    assert result.returncode == 0, result.stderr
    assert ("GET", "/namespaces/acme") in api.requests, api.requests
    branch = _announce_branch(bare)
    blob = _git(bare, "show", f"{branch}:index/{INDEX_HOST}/acme/{name}/metadata.json")
    assert json.loads(blob)["owner"] == {"login": "acme", "id": 44}


# ── CI environment auto-detection (host-match gated) ───────────────────────


def test_publish_announce_gitlab_ci_env_autoconfigures(
    grim_at, project_dir: Path, registry: str, tmp_path: Path, forge_api
) -> None:
    """In GitLab CI with a matching server host, forge/api/token come from
    the environment — zero `[announce]` forge config needed."""
    ns = f"grim-test/{uuid.uuid4().hex[:12]}"
    name = "ann-cienv"
    _make_skill_source(project_dir, name, "CI env.")
    api = forge_api()
    _manifest(project_dir, ns, name, INDEX_URL)

    runner = grim_at(project_dir)
    _index_remote(tmp_path, runner)
    runner.env.update(
        {
            "GITLAB_CI": "true",
            "CI_SERVER_HOST": INDEX_HOST,
            "CI_API_V4_URL": api.url,
            "GITLAB_TOKEN": TOKEN,
        }
    )
    result = runner.run("publish", "--announce", check=False)
    assert result.returncode == 0, result.stderr
    assert f"announced: {MR_URL}" in result.stderr, result.stderr
    assert ("POST", "/projects/acme%2Findex/merge_requests") in api.requests, api.requests
    assert TOKEN not in result.stdout + result.stderr, "token must never be printed"


def test_publish_announce_ci_env_host_mismatch_is_ignored(
    grim_at, project_dir: Path, registry: str, tmp_path: Path, forge_api
) -> None:
    """A GitLab pipeline announcing to a foreign host must not inherit the
    GitLab CI credentials or API — the announce degrades to a plain push."""
    ns = f"grim-test/{uuid.uuid4().hex[:12]}"
    name = "ann-mismatch"
    _make_skill_source(project_dir, name, "Mismatch.")
    api = forge_api()
    _manifest(project_dir, ns, name, INDEX_URL)

    runner = grim_at(project_dir)
    bare = _index_remote(tmp_path, runner)
    runner.env.update(
        {
            "GITLAB_CI": "true",
            "CI_SERVER_HOST": "other.example.test",
            "CI_API_V4_URL": api.url,
            "GITLAB_TOKEN": TOKEN,
        }
    )
    result = runner.run("publish", "--announce", check=False)
    assert result.returncode == 0, result.stderr
    assert api.requests == [], f"mismatched CI host must not reach the API: {api.requests}"
    _announce_branch(bare)
    assert TOKEN not in result.stdout + result.stderr, "token must never be printed"
