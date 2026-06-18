# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The Grimoire Authors
"""`grim add` kind-inference and name-override acceptance tests.

`grim add <reference>` now requires only the reference.  When `--kind` is
omitted, the kind is inferred from the manifest's OCI `artifactType`.  When
`--name` is omitted, the binding name defaults to the reference's last path
segment.  Both flags remain overridable.  A reference that cannot be
resolved yields exit 65 (DataError / KindInferenceFailed).
"""
from __future__ import annotations

from pathlib import Path

from src.helpers import make_artifact, write_config
from src.registry import fetch_manifest


def test_add_infers_kind_and_name_from_manifest(
    grim_at, project_dir: Path, registry: str, unique_repo: str
) -> None:
    """Kind and name are inferred from the published manifest when omitted."""
    ru = make_artifact(
        f"{unique_repo}/rust-style",
        "rule",
        {"rust-style.md": "---\npaths: ['**/*.rs']\n---\n# Rust Style\n"},
        tag="v1",
    )
    write_config(project_dir)
    runner = grim_at(project_dir)

    out = runner.json("add", ru.fq)
    assert out["kind"] == "rule", (
        f"kind must be inferred as 'rule' from the manifest annotation, got {out['kind']!r}"
    )
    assert out["name"] == "rust-style", (
        f"name must default to the last path segment 'rust-style', got {out['name']!r}"
    )
    assert out["status"] == "added"
    assert "@sha256:" in out["pinned"]


def test_published_manifest_types_kind_via_artifact_type(
    grim_at, project_dir: Path, registry: str, unique_repo: str
) -> None:
    """The wire contract (`adr_oci_empty_config_compat.md`): kind rides on
    `artifactType` and is mirrored into the `com.grimoire.kind` annotation; the
    config descriptor is the OCI empty type (GitLab-allowlist-safe), not a
    custom Grimoire config type. `grim add` infers the kind from `artifactType`."""
    repo = f"{unique_repo}/rust-style"
    ru = make_artifact(
        repo,
        "rule",
        {"rust-style.md": "---\npaths: ['**/*.rs']\n---\n# Rust Style\n"},
        tag="v1",
    )

    manifest = fetch_manifest(repo, "v1")
    assert manifest["artifactType"] == "application/vnd.grimoire.rule.v1", (
        f"manifest must carry the Grimoire artifactType, got {manifest.get('artifactType')!r}"
    )
    assert manifest["config"]["mediaType"] == "application/vnd.oci.empty.v1+json", (
        f"config descriptor must be the OCI empty type, got {manifest['config']['mediaType']!r}"
    )
    assert manifest.get("annotations", {}).get("com.grimoire.kind") == "rule", (
        f"manifest must carry the com.grimoire.kind fallback annotation, "
        f"got {manifest.get('annotations', {})!r}"
    )

    # End-to-end: kind inference works off the type alone (no annotation).
    write_config(project_dir)
    out = grim_at(project_dir).json("add", ru.fq)
    assert out["kind"] == "rule"


def test_add_name_override_replaces_inferred_name(
    grim_at, project_dir: Path, registry: str, unique_repo: str
) -> None:
    """--name overrides the default segment-based name."""
    sk = make_artifact(
        f"{unique_repo}/code-review",
        "skill",
        {"code-review/SKILL.md": "---\nname: code-review\ndescription: d\n---\n# CR\n"},
        tag="stable",
    )
    write_config(project_dir)
    runner = grim_at(project_dir)

    out = runner.json("add", sk.fq, "--name", "cr")
    assert out["name"] == "cr", (
        f"--name 'cr' must override the default segment name, got {out['name']!r}"
    )
    assert out["kind"] == "skill"

    # The config binding name must match the --name value.
    # The FQ reference in the value still contains "code-review" (that's the
    # repo path), but the KEY must be "cr", not "code-review".
    cfg = (project_dir / "grimoire.toml").read_text()
    skills_section = cfg.split("[skills]")[1].split("[rules]")[0]
    assert 'cr = "' in skills_section, (
        f"config skills section must have key 'cr', got:\n{skills_section}"
    )
    assert not skills_section.strip().startswith("code-review"), (
        "config skills key must be 'cr', not 'code-review'"
    )


def test_add_kind_override_still_works(
    grim_at, project_dir: Path, registry: str, unique_repo: str
) -> None:
    """Explicit --kind still overrides inference."""
    sk = make_artifact(
        f"{unique_repo}/code-review",
        "skill",
        {"code-review/SKILL.md": "---\nname: code-review\ndescription: d\n---\n# CR\n"},
        tag="stable",
    )
    write_config(project_dir)
    runner = grim_at(project_dir)

    # Pass --kind explicitly (even if it matches what would be inferred).
    out = runner.json("add", sk.fq, "--kind", "skill")
    assert out["kind"] == "skill"
    assert out["name"] == "code-review"
    assert out["status"] == "added"


def test_add_missing_reference_kind_inference_fails_exit_65(
    grim_at, project_dir: Path, registry: str, unique_repo: str
) -> None:
    """A reference that does not resolve fails kind inference: exit 65."""
    write_config(project_dir)
    runner = grim_at(project_dir)

    missing_ref = f"{registry}/{unique_repo}/missing:latest"
    result = runner.run("add", missing_ref, check=False)
    assert result.returncode == 65, (
        f"kind inference failure for an unresolvable reference must exit 65, "
        f"got {result.returncode}; stderr: {result.stderr}"
    )
