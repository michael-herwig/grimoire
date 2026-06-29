# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The Grimoire Authors
"""Package-deprecation acceptance tests (issue #15).

Three surfaces, end-to-end against the real registry:

1. **Publish** — an authored `deprecated` notice (skill/agent `metadata`,
   rule top-level, bundle TOML) lands in the `com.grimoire.deprecated`
   manifest annotation via the real `annotations_for_*` path; an empty /
   absent notice emits no annotation.
2. **Search** — a deprecated catalog entry exposes a `deprecated` message
   field in JSON and a comma-separated `deprecated` suffix in the plain
   `Status` cell.
3. **Add** — acquiring a deprecated reference warns on stderr (the install
   still succeeds).
"""
from __future__ import annotations

from pathlib import Path

from src.helpers import make_artifact, write_config
from src.registry import REGISTRY_HOST, fetch_manifest

DEPRECATED = "com.grimoire.deprecated"


def _write(p: Path, body: str) -> None:
    p.parent.mkdir(parents=True, exist_ok=True)
    p.write_text(body)


# ── 1. publish path: authored notice → annotation ──────────────────────


def test_skill_deprecated_becomes_annotation(
    grim_at, project_dir: Path, registry: str, unique_repo: str
) -> None:
    skill = project_dir / "code-review"
    _write(
        skill / "SKILL.md",
        "---\nname: code-review\ndescription: d\n"
        "metadata:\n  deprecated: use acme/code-review-2 instead\n---\n# CR\n",
    )
    repo_path = f"{unique_repo}/code-review"
    grim_at(project_dir).json("release", str(skill), f"{registry}/{repo_path}:1.0.0")

    ann = fetch_manifest(repo_path, "1.0.0")["annotations"]
    assert ann[DEPRECATED] == "use acme/code-review-2 instead"


def test_rule_deprecated_becomes_annotation(
    grim_at, project_dir: Path, registry: str, unique_repo: str
) -> None:
    rule = project_dir / "rust-style.md"
    _write(
        rule,
        "---\npaths: ['**/*.rs']\ndeprecated: superseded by rust-style-2\n"
        "---\n# Rust Style\nbody\n",
    )
    repo_path = f"{unique_repo}/rust-style"
    grim_at(project_dir).json("release", str(rule), f"{registry}/{repo_path}:1.0.0")

    ann = fetch_manifest(repo_path, "1.0.0")["annotations"]
    assert ann[DEPRECATED] == "superseded by rust-style-2"


def test_agent_deprecated_becomes_annotation(
    grim_at, project_dir: Path, registry: str, unique_repo: str
) -> None:
    agent = project_dir / "reviewer.md"
    _write(
        agent,
        "---\nname: reviewer\ndescription: d\n"
        "metadata:\n  deprecated: no longer maintained\n---\nbody\n",
    )
    repo_path = f"{unique_repo}/reviewer"
    grim_at(project_dir).json(
        "release", "--kind", "agent", str(agent), f"{registry}/{repo_path}:1.0.0"
    )

    ann = fetch_manifest(repo_path, "1.0.0")["annotations"]
    assert ann[DEPRECATED] == "no longer maintained"


def test_bundle_deprecated_becomes_annotation(
    grim_at, project_dir: Path, registry: str, unique_repo: str
) -> None:
    bundle = project_dir / "python-stack.toml"
    _write(
        bundle,
        'deprecated = "migrate to python-stack-2"\n\n'
        "[skills]\n"
        'code-review = "ghcr.io/acme/code-review:1"\n',
    )
    repo_path = f"{unique_repo}/python-stack"
    grim_at(project_dir).json("release", str(bundle), f"{registry}/{repo_path}:1.0.0")

    ann = fetch_manifest(repo_path, "1.0.0")["annotations"]
    assert ann[DEPRECATED] == "migrate to python-stack-2"


def test_non_deprecated_skill_omits_annotation(
    grim_at, project_dir: Path, registry: str, unique_repo: str
) -> None:
    """An empty notice is treated as not-deprecated and emits no annotation."""
    skill = project_dir / "fresh"
    _write(
        skill / "SKILL.md",
        "---\nname: fresh\ndescription: d\nmetadata:\n  deprecated: '   '\n---\n# F\n",
    )
    repo_path = f"{unique_repo}/fresh"
    grim_at(project_dir).json("release", str(skill), f"{registry}/{repo_path}:1.0.0")

    ann = fetch_manifest(repo_path, "1.0.0")["annotations"]
    assert DEPRECATED not in ann


# ── 2. search highlight ────────────────────────────────────────────────


def test_search_highlights_deprecated_entry(
    grim_at, project_dir: Path, registry: str, unique_repo: str
) -> None:
    make_artifact(
        f"{unique_repo}/old-skill",
        "skill",
        {"old-skill/SKILL.md": "---\nname: old-skill\n---\n# old\n"},
        tag="latest",
        annotations={
            "org.opencontainers.image.description": "An old reviewer",
            DEPRECATED: "use new-skill instead",
        },
    )
    runner = grim_at(project_dir)

    # JSON: the deprecation message rides a dedicated field.
    rows = runner.json(
        "search", unique_repo, "--registry", f"{REGISTRY_HOST}/{unique_repo}", "--refresh"
    )
    entry = next(r for r in rows if r["repo"].endswith(f"{unique_repo}/old-skill"))
    assert entry["deprecated"] == "use new-skill instead"

    # Plain: the Status cell gains a comma-separated `deprecated` suffix.
    plain = runner.plain(
        "search", unique_repo, "--registry", f"{REGISTRY_HOST}/{unique_repo}"
    )
    assert "deprecated" in plain.stdout, plain.stdout


def test_search_non_deprecated_entry_has_null_field(
    grim_at, project_dir: Path, registry: str, unique_repo: str
) -> None:
    make_artifact(
        f"{unique_repo}/fresh-skill",
        "skill",
        {"fresh-skill/SKILL.md": "---\nname: fresh-skill\n---\n# fresh\n"},
        tag="latest",
        annotations={"org.opencontainers.image.description": "A current reviewer"},
    )
    rows = grim_at(project_dir).json(
        "search", unique_repo, "--registry", f"{REGISTRY_HOST}/{unique_repo}", "--refresh"
    )
    entry = next(r for r in rows if r["repo"].endswith(f"{unique_repo}/fresh-skill"))
    assert entry["deprecated"] is None


# ── 3. add acquisition warning ─────────────────────────────────────────


def test_add_warns_on_deprecated_reference(
    grim_at, project_dir: Path, registry: str, unique_repo: str
) -> None:
    dep = make_artifact(
        f"{unique_repo}/old-rule",
        "rule",
        {"old-rule.md": "---\npaths: ['**/*.rs']\n---\n# old\n"},
        tag="v1",
        annotations={DEPRECATED: "use new-rule instead"},
    )
    write_config(project_dir)
    runner = grim_at(project_dir)

    result = runner.run("add", dep.fq)
    assert result.returncode == 0, f"add of a deprecated artifact still succeeds; {result.stderr}"
    assert "deprecated" in result.stderr.lower(), (
        f"add must warn that the artifact is deprecated; stderr was:\n{result.stderr}"
    )
    assert "use new-rule instead" in result.stderr, result.stderr


def test_add_explicit_kind_warns_on_deprecated_reference(
    grim_at, project_dir: Path, registry: str, unique_repo: str
) -> None:
    """The explicit-`--kind` path skips inference but still warns (it fetches
    the manifest best-effort just for the notice)."""
    dep = make_artifact(
        f"{unique_repo}/old-rule",
        "rule",
        {"old-rule.md": "---\npaths: ['**/*.rs']\n---\n# old\n"},
        tag="v1",
        annotations={DEPRECATED: "use new-rule instead"},
    )
    write_config(project_dir)
    result = grim_at(project_dir).run("add", "--kind", "rule", dep.fq)
    assert result.returncode == 0, result.stderr
    assert "deprecated" in result.stderr.lower(), result.stderr
    assert "use new-rule instead" in result.stderr, result.stderr


def test_add_no_warning_for_current_reference(
    grim_at, project_dir: Path, registry: str, unique_repo: str
) -> None:
    cur = make_artifact(
        f"{unique_repo}/cur-rule",
        "rule",
        {"cur-rule.md": "---\npaths: ['**/*.rs']\n---\n# cur\n"},
        tag="v1",
    )
    write_config(project_dir)
    result = grim_at(project_dir).run("add", cur.fq)
    assert result.returncode == 0
    assert "deprecated" not in result.stderr.lower(), result.stderr
