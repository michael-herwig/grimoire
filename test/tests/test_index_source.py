# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The Grimoire Authors
"""Package-index browse-source acceptance tests (`[[registries]] index = …`).

A ``[[registries]]`` entry sets exactly one of ``url`` / ``index``. An
``index`` entry lists packages from a package index instead of the OCI
``_catalog`` endpoint, over two transports:

- HTTP(S): a compiled static index — ``<base>/all.json``
- git: a shallow clone walking ``index/**/metadata.json``

The index is a phone book: entries carry ``ref`` (registry/repository),
kind, and description — never versions. Search rows therefore surface with
no version data; installs resolve tags live from the registry.
"""
from __future__ import annotations

import json
import subprocess
import threading
from functools import partial
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

import pytest


# ---------------------------------------------------------------------------
# Helpers / fixtures
# ---------------------------------------------------------------------------


def _package(name: str, kind: str, ref: str, description: str) -> dict:
    return {
        "schema": 1,
        "name": name,
        "kind": kind,
        "ref": ref,
        "description": description,
        "repository": "https://github.com/acme/skills",
        "owner": {"github": "acme", "id": 1},
    }


@pytest.fixture()
def http_index(tmp_path: Path):
    """A local static webserver serving an index dist dir (all.json)."""
    root = tmp_path / "index-dist"
    root.mkdir()
    handler = partial(SimpleHTTPRequestHandler, directory=str(root))
    server = ThreadingHTTPServer(("127.0.0.1", 0), handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    yield root, f"http://127.0.0.1:{server.server_address[1]}"
    server.shutdown()


def _write_all_json(root: Path, packages: list[dict]) -> None:
    (root / "all.json").write_text(json.dumps(packages))


def _git_index_repo(tmp_path: Path, packages: list[dict]) -> Path:
    """A local git repository (path ends in ``.git`` so it classifies as a
    git index locator) holding ``index/github.com/<ns>/<pkg>/metadata.json``."""
    repo = tmp_path / "index-repo.git"
    for pkg in packages:
        d = repo / "index" / "github.com" / "acme" / pkg["name"]
        d.mkdir(parents=True)
        (d / "metadata.json").write_text(json.dumps(pkg))
    def git(*args: str) -> None:
        subprocess.run(
            ["git", "-c", "user.email=t@t", "-c", "user.name=t", *args],
            cwd=repo,
            check=True,
            capture_output=True,
        )
    subprocess.run(["git", "init", "-q", str(repo)], check=True, capture_output=True)
    git("add", "-A")
    git("commit", "-q", "-m", "seed")
    return repo


def _index_config(project_dir: Path, locator: str) -> None:
    (project_dir / "grimoire.toml").write_text(
        f'[[registries]]\n'
        f'alias = "hub"\n'
        f'index = "{locator}"\n'
        f'default = true\n'
        f'\n[skills]\n\n[rules]\n'
    )


def _search_rows(runner) -> list[dict]:
    result = runner.run("--format", "json", "search", "--refresh", check=False)
    assert result.returncode == 0, (
        f"index-backed search must exit 0, got {result.returncode}; stderr: {result.stderr}"
    )
    rows = json.loads(result.stdout)
    assert isinstance(rows, list)
    return rows


# ---------------------------------------------------------------------------
# HTTP transport
# ---------------------------------------------------------------------------


def test_search_http_index_lists_packages(grim_at, project_dir: Path, http_index) -> None:
    """``grim search`` against an ``index = http://…`` source lists the
    packages from ``all.json`` — no OCI registry involved at all."""
    root, base = http_index
    _write_all_json(
        root,
        [
            _package("idx-skill", "skill", "ghcr.io/acme/skills/idx-skill", "From the index"),
            _package("idx-rule", "rule", "registry.example/acme/rules/idx-rule", "Rule pointer"),
        ],
    )
    _index_config(project_dir, base)

    rows = _search_rows(grim_at(project_dir))
    repos = [r.get("repo", "") for r in rows]
    assert "ghcr.io/acme/skills/idx-skill" in repos, f"got {repos}"
    assert "registry.example/acme/rules/idx-rule" in repos, f"got {repos}"

    skill = next(r for r in rows if r.get("repo") == "ghcr.io/acme/skills/idx-skill")
    assert skill.get("kind") == "skill"
    assert skill.get("description") == "From the index"
    # Phone-book contract: the index carries no version data.
    assert not skill.get("version"), f"index rows carry no version, got {skill!r}"


def test_search_http_index_filters_by_query(grim_at, project_dir: Path, http_index) -> None:
    root, base = http_index
    _write_all_json(
        root,
        [
            _package("alpha-skill", "skill", "ghcr.io/acme/skills/alpha-skill", "Alpha"),
            _package("beta-rule", "rule", "ghcr.io/acme/rules/beta-rule", "Beta"),
        ],
    )
    _index_config(project_dir, base)

    runner = grim_at(project_dir)
    result = runner.run("--format", "json", "search", "--refresh", "alpha", check=False)
    assert result.returncode == 0, result.stderr
    repos = [r.get("repo", "") for r in json.loads(result.stdout)]
    assert repos == ["ghcr.io/acme/skills/alpha-skill"], f"got {repos}"


def test_search_unreachable_http_index_degrades_to_empty(grim_at, project_dir: Path) -> None:
    """An unreachable index degrades that source to an empty group — the
    browse still exits 0 (same contract as an unreachable registry)."""
    _index_config(project_dir, "http://127.0.0.1:1/absent")
    rows = _search_rows(grim_at(project_dir))
    assert rows == [], f"unreachable index must yield no rows, got {rows}"


# ---------------------------------------------------------------------------
# Git transport
# ---------------------------------------------------------------------------


def test_search_git_index_lists_packages(grim_at, project_dir: Path, tmp_path: Path) -> None:
    """``index = <repo>.git`` shallow-clones the index repository and walks
    ``index/**/metadata.json`` — works against GitHub, GitLab, or any plain
    git host; here a local repository stands in."""
    repo = _git_index_repo(
        tmp_path,
        [
            _package("git-skill", "skill", "ghcr.io/acme/skills/git-skill", "Cloned pointer"),
            _package("git-bundle", "bundle", "gitlab.example/acme/bundles/git-bundle", "Bundle pointer"),
        ],
    )
    _index_config(project_dir, str(repo))

    rows = _search_rows(grim_at(project_dir))
    repos = [r.get("repo", "") for r in rows]
    assert "ghcr.io/acme/skills/git-skill" in repos, f"got {repos}"
    assert "gitlab.example/acme/bundles/git-bundle" in repos, f"got {repos}"


def test_git_index_refresh_picks_up_new_packages(grim_at, project_dir: Path, tmp_path: Path) -> None:
    """A second ``--refresh`` re-clones and surfaces newly announced packages."""
    repo = _git_index_repo(
        tmp_path,
        [_package("first", "skill", "ghcr.io/acme/skills/first", "First")],
    )
    _index_config(project_dir, str(repo))
    runner = grim_at(project_dir)

    assert any("first" in r.get("repo", "") for r in _search_rows(runner))

    d = repo / "index" / "github.com" / "acme" / "second"
    d.mkdir(parents=True)
    (d / "metadata.json").write_text(
        json.dumps(_package("second", "rule", "ghcr.io/acme/rules/second", "Second"))
    )
    subprocess.run(
        ["git", "-c", "user.email=t@t", "-c", "user.name=t", "add", "-A"],
        cwd=repo, check=True, capture_output=True,
    )
    subprocess.run(
        ["git", "-c", "user.email=t@t", "-c", "user.name=t", "commit", "-q", "-m", "announce"],
        cwd=repo, check=True, capture_output=True,
    )

    repos = [r.get("repo", "") for r in _search_rows(runner)]
    assert any("second" in r for r in repos), f"got {repos}"


# ---------------------------------------------------------------------------
# Mixed sources
# ---------------------------------------------------------------------------


def test_index_and_registry_sources_combine(
    grim_at, project_dir: Path, registry: str, http_index
) -> None:
    """A config declaring one registry source and one index source browses
    both — groups aggregate across source kinds."""
    import uuid

    from src.helpers import make_artifact
    from src.registry import REGISTRY_HOST

    ns = f"grim-test/{uuid.uuid4().hex[:12]}"
    make_artifact(
        f"{ns}/reg-skill",
        "skill",
        {"reg-skill/SKILL.md": "---\nname: reg-skill\ndescription: from registry\n---\n# S\n"},
        tag="latest",
    )

    root, base = http_index
    _write_all_json(
        root,
        [_package("hub-skill", "skill", "ghcr.io/acme/skills/hub-skill", "From the index")],
    )

    (project_dir / "grimoire.toml").write_text(
        f'[[registries]]\n'
        f'alias = "reg"\n'
        f'oci = "{REGISTRY_HOST}/{ns}"\n'
        f'default = true\n'
        f'\n'
        f'[[registries]]\n'
        f'alias = "hub"\n'
        f'index = "{base}"\n'
        f'\n[skills]\n\n[rules]\n'
    )

    repos = [r.get("repo", "") for r in _search_rows(grim_at(project_dir))]
    assert any("reg-skill" in r for r in repos), f"registry source missing: {repos}"
    assert any("hub-skill" in r for r in repos), f"index source missing: {repos}"


# ---------------------------------------------------------------------------
# Config surface + validation
# ---------------------------------------------------------------------------


def test_config_registry_add_index_roundtrip(grim_at, project_dir: Path) -> None:
    (project_dir / "grimoire.toml").write_text("[skills]\n\n[rules]\n")
    runner = grim_at(project_dir)

    r = runner.run("config", "registry", "add", "hub", "--index", "https://index.grimoire.rs", check=False)
    assert r.returncode == 0, r.stderr

    r = runner.run("--format", "json", "config", "registry", "show", "hub", check=False)
    assert r.returncode == 0, r.stderr
    shown = json.loads(r.stdout)
    assert shown.get("index") == "https://index.grimoire.rs"
    assert "oci" not in shown or shown["oci"] is None

    r = runner.run("config", "get", "registry.hub.index", check=False)
    assert r.returncode == 0
    assert r.stdout.strip() == "https://index.grimoire.rs"


def test_config_registry_add_requires_exactly_one_source(grim_at, project_dir: Path) -> None:
    (project_dir / "grimoire.toml").write_text("[skills]\n\n[rules]\n")
    runner = grim_at(project_dir)

    # Neither --oci nor --index: usage error 64.
    r = runner.run("config", "registry", "add", "hub", check=False)
    assert r.returncode == 64, f"expected 64, got {r.returncode}: {r.stderr}"

    # Both: rejected at the clap layer (usage error 2 from clap → 64 mapping
    # or clap's own exit; accept any non-zero usage-shaped failure).
    r = runner.run(
        "config", "registry", "add", "hub",
        "--oci", "ghcr.io/acme", "--index", "https://idx", check=False,
    )
    assert r.returncode != 0


def test_config_registry_add_rejects_bad_index_locator(grim_at, project_dir: Path) -> None:
    (project_dir / "grimoire.toml").write_text("[skills]\n\n[rules]\n")
    runner = grim_at(project_dir)
    r = runner.run("config", "registry", "add", "hub", "--index", "ftp://nope", check=False)
    assert r.returncode == 65, f"expected 65, got {r.returncode}: {r.stderr}"


def test_config_set_index_on_oci_entry_rejected(grim_at, project_dir: Path) -> None:
    """oci and index are mutually exclusive — switching source type requires
    an explicit unset first (or rm/add).

    The entry is written with the legacy ``url`` key on purpose: it must
    keep parsing as ``oci`` (serde alias, 0.6.x back-compat).
    """
    (project_dir / "grimoire.toml").write_text(
        '[[registries]]\nalias = "acme"\nurl = "ghcr.io/acme"\n\n[skills]\n\n[rules]\n'
    )
    runner = grim_at(project_dir)
    r = runner.run("config", "set", "registry.acme.index", "https://idx.example", check=False)
    assert r.returncode == 65, f"expected 65, got {r.returncode}: {r.stderr}"

    # Unsetting the only source is refused (the entry would be sourceless).
    r = runner.run("config", "unset", "registry.acme.oci", check=False)
    assert r.returncode == 64, f"expected 64, got {r.returncode}: {r.stderr}"


def test_config_file_with_oci_and_index_rejected(grim_at, project_dir: Path) -> None:
    """A hand-edited entry setting both oci and index fails config parse (78)."""
    (project_dir / "grimoire.toml").write_text(
        '[[registries]]\n'
        'alias = "bad"\n'
        'oci = "ghcr.io/acme"\n'
        'index = "https://idx.example"\n'
        '\n[skills]\n\n[rules]\n'
    )
    runner = grim_at(project_dir)
    r = runner.run("config", "list", check=False)
    assert r.returncode == 78, f"expected 78, got {r.returncode}: {r.stderr}"
