# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The Grimoire Authors
"""`grim publish --announce` acceptance tests.

--announce records published packages in a package-index git repository:
clone → write index/github.com/<ns>/<pkg>/metadata.json → commit on a
deterministic topic branch → push. On non-github.com hosts (here: a local
bare repository standing in for GitLab / self-hosted git) the outcome is
the pushed branch; a maintainer merges it. Ownership fields come from the
manifest's [announce] table (owner_id explicit ⇒ no GitHub API call —
hermetic).
"""
from __future__ import annotations

import json
import subprocess
import uuid
from pathlib import Path

from src.registry import REGISTRY_HOST


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


def _manifest(project_dir: Path, ns: str, name: str, index_repo: Path) -> None:
    _write(
        project_dir / "publish.toml",
        f'registry = "{REGISTRY_HOST}"\n'
        f'repository_prefix = "{ns}"\n'
        f"\n"
        f"[announce]\n"
        f'repository = "{index_repo}"\n'
        f'namespace = "acme"\n'
        f"owner_id = 42\n"
        f"\n"
        f"[skills.{name}]\n"
        f'version = "0.1.0"\n',
    )


def test_publish_announce_pushes_branch_with_metadata(
    grim_at, project_dir: Path, registry: str, tmp_path: Path
) -> None:
    ns = f"grim-test/{uuid.uuid4().hex[:12]}"
    name = "ann-skill"
    _make_skill_source(project_dir, name, "Announce me.")
    bare = _bare_index_repo(tmp_path)
    _manifest(project_dir, ns, name, bare)

    runner = grim_at(project_dir)
    result = runner.run("publish", "--announce", check=False)
    assert result.returncode == 0, f"publish --announce failed: {result.stderr}"
    assert "announced:" in result.stderr, result.stderr

    branches = _git(bare, "branch", "--list", "announce/*")
    assert "announce/acme-" in branches, f"topic branch missing: {branches!r}"
    branch = branches.strip().lstrip("* ").strip()

    blob = _git(bare, "show", f"{branch}:index/github.com/acme/{name}/metadata.json")
    meta = json.loads(blob)
    assert meta["schema"] == 1
    assert meta["name"] == name
    assert meta["kind"] == "skill"
    assert meta["ref"] == f"{REGISTRY_HOST}/{ns}/{name}", meta
    assert meta["description"] == "Announce me."
    assert meta["owner"] == {"github": "acme", "id": 42}
    assert meta["repository"] == f"https://github.com/acme/{name}"


def test_publish_announce_is_repeatable(
    grim_at, project_dir: Path, registry: str, tmp_path: Path
) -> None:
    """A re-run (packages already pushed → skipped) still announces cleanly
    onto the same deterministic topic branch."""
    ns = f"grim-test/{uuid.uuid4().hex[:12]}"
    name = "ann-repeat"
    _make_skill_source(project_dir, name, "Repeatable.")
    bare = _bare_index_repo(tmp_path)
    _manifest(project_dir, ns, name, bare)

    runner = grim_at(project_dir)
    first = runner.run("publish", "--announce", check=False)
    assert first.returncode == 0, first.stderr
    second = runner.run("publish", "--announce", check=False)
    assert second.returncode == 0, second.stderr

    branches = [b.strip().lstrip("* ").strip() for b in _git(bare, "branch", "--list", "announce/*").splitlines()]
    assert len(branches) == 1, f"identical content must reuse one branch: {branches}"


def test_publish_announce_dry_run_touches_nothing(
    grim_at, project_dir: Path, registry: str, tmp_path: Path
) -> None:
    ns = f"grim-test/{uuid.uuid4().hex[:12]}"
    name = "ann-dry"
    _make_skill_source(project_dir, name, "Dry.")
    bare = _bare_index_repo(tmp_path)
    _manifest(project_dir, ns, name, bare)

    runner = grim_at(project_dir)
    result = runner.run("publish", "--announce", "--dry-run", check=False)
    assert result.returncode == 0, result.stderr
    assert "announce: skipped (dry run)" in result.stderr

    branches = _git(bare, "branch", "--list", "announce/*")
    assert branches.strip() == "", f"dry run must not push: {branches!r}"


def test_publish_announce_unreachable_index_exits_unavailable(
    grim_at, project_dir: Path, registry: str, tmp_path: Path
) -> None:
    """A failing announce after a successful publish exits 69 (the packages
    ARE published; only the announcement needs a retry)."""
    ns = f"grim-test/{uuid.uuid4().hex[:12]}"
    name = "ann-fail"
    _make_skill_source(project_dir, name, "Unreachable index.")
    _manifest(project_dir, ns, name, tmp_path / "no-such-repo.git")

    runner = grim_at(project_dir)
    result = runner.run("publish", "--announce", check=False)
    assert result.returncode == 69, f"expected 69, got {result.returncode}: {result.stderr}"
    assert "announce failed" in result.stderr
