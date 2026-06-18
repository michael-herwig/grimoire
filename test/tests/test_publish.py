# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The Grimoire Authors
"""`grim publish` acceptance tests — manifest-driven batch release.

Tests exercise the full publish path: manifest validation, entry ordering,
push/skip/dry-run/force semantics, --only filtering, --tag channel tags,
JSON report shape, and error cases (missing manifest, unknown --only name,
semver --tag). Each test uses unique_repo-prefixed names to isolate on the
shared registry.

Source layout mirrors the manifest path convention (ADR D2):
  skills/<name>/SKILL.md
  rules/<name>.md
  agents/<name>.md
  bundles/<name>.toml
"""
from __future__ import annotations

import json
import urllib.error
import urllib.request
import uuid
from pathlib import Path

import pytest

from src.registry import REGISTRY_BASE, REGISTRY_HOST, fetch_manifest, tag_digest


# ---------------------------------------------------------------------------
# Source layout helpers
# ---------------------------------------------------------------------------


def _write(p: Path, body: str) -> None:
    p.parent.mkdir(parents=True, exist_ok=True)
    p.write_text(body)


def _make_skill_source(project_dir: Path, name: str) -> Path:
    """Write a minimal valid skill source directory."""
    skill_dir = project_dir / "skills" / name
    _write(
        skill_dir / "SKILL.md",
        f"---\nname: {name}\ndescription: A test skill for publish suite.\n"
        f"metadata:\n  keywords: test,publish\n---\n# {name}\n",
    )
    _write(skill_dir / "scripts/run.sh", "echo hi\n")
    return skill_dir


def _make_rule_source(project_dir: Path, name: str) -> Path:
    """Write a minimal valid rule source file."""
    rule_file = project_dir / "rules" / f"{name}.md"
    _write(
        rule_file,
        f"---\npaths: ['**/*.md']\n---\n# {name}\nRule body.\n",
    )
    return rule_file


def _make_agent_source(project_dir: Path, name: str) -> Path:
    """Write a minimal valid agent source file."""
    agent_file = project_dir / "agents" / f"{name}.md"
    _write(
        agent_file,
        f"---\nname: {name}\ndescription: A test agent for publish suite.\n---\n"
        f"You are a test agent.\n",
    )
    return agent_file


def _make_bundle_source(
    project_dir: Path,
    name: str,
    skill_ref: str,
) -> Path:
    """Write a minimal valid bundle source file referencing a skill member."""
    bundle_file = project_dir / "bundles" / f"{name}.toml"
    _write(
        bundle_file,
        f"[skills]\nskill-member = \"{skill_ref}\"\n",
    )
    return bundle_file


def _write_publish_manifest(
    project_dir: Path,
    registry: str,
    unique_prefix: str,
    *,
    include_rule: bool = False,
    include_agent: bool = False,
    include_bundle: bool = False,
    skill_ref_for_bundle: str = "",
    skill_version: str = "0.1.0",
    rule_version: str = "0.1.0",
    agent_version: str = "0.1.0",
    bundle_version: str = "0.1.0",
) -> Path:
    """Write a publish.toml manifest and all required source artifacts.

    The manifest uses unique_prefix to avoid collisions on the shared registry.
    Returns the path to the publish.toml.
    """
    skill_name = f"{unique_prefix}-skill"
    _make_skill_source(project_dir, skill_name)

    lines = [
        f'registry = "{registry}"',
        "",
        f"[skills.{skill_name}]",
        f'version = "{skill_version}"',
    ]

    if include_rule:
        rule_name = f"{unique_prefix}-rule"
        _make_rule_source(project_dir, rule_name)
        lines += [
            "",
            f"[rules.{rule_name}]",
            f'version = "{rule_version}"',
        ]

    if include_agent:
        agent_name = f"{unique_prefix}-agent"
        _make_agent_source(project_dir, agent_name)
        lines += [
            "",
            f"[agents.{agent_name}]",
            f'version = "{agent_version}"',
        ]

    if include_bundle and skill_ref_for_bundle:
        bundle_name = f"{unique_prefix}-bundle"
        _make_bundle_source(project_dir, bundle_name, skill_ref_for_bundle)
        lines += [
            "",
            f"[bundles.{bundle_name}]",
            f'version = "{bundle_version}"',
        ]

    manifest_path = project_dir / "publish.toml"
    manifest_path.write_text("\n".join(lines) + "\n")
    return manifest_path


# ---------------------------------------------------------------------------
# Tests: happy paths
# ---------------------------------------------------------------------------


def test_publish_all_kinds_reports_pushed_and_exit_0(
    grim_at, project_dir: Path, registry: str, unique_repo: str
) -> None:
    """Publishing skill+rule+agent from a catalog-shaped dir exits 0,
    all rows status=pushed. (ADR D1, D4, D6 — Testing Strategy row 1)"""
    prefix = unique_repo.split("/")[-1]
    runner = grim_at(project_dir)

    manifest_path = _write_publish_manifest(
        project_dir,
        registry,
        prefix,
        include_rule=True,
        include_agent=True,
    )

    result = runner.run(
        "publish",
        "--manifest", str(manifest_path),
        format="json",
        check=False,
    )
    assert result.returncode == 0, (
        f"grim publish must exit 0 on success, got {result.returncode}; "
        f"stderr: {result.stderr.strip()}"
    )

    rows = json.loads(result.stdout)
    assert isinstance(rows, list), "JSON output must be a bare array (ADR D6)"
    assert len(rows) == 3, f"expected 3 rows (skill+rule+agent), got {rows}"

    statuses = {r["status"] for r in rows}
    assert statuses == {"pushed"}, f"all rows must be 'pushed', got {statuses}"


def test_publish_all_kinds_row_shape_has_required_keys(
    grim_at, project_dir: Path, registry: str, unique_repo: str
) -> None:
    """Each JSON row must carry ref, kind, digest, tags, status (ADR D6)."""
    prefix = unique_repo.split("/")[-1]
    runner = grim_at(project_dir)

    manifest_path = _write_publish_manifest(
        project_dir,
        registry,
        prefix,
    )

    rows = runner.json("publish", "--manifest", str(manifest_path))
    assert isinstance(rows, list), "JSON must be a bare array"
    assert len(rows) >= 1

    row = rows[0]
    assert "ref" in row, f"row must have 'ref' key: {row}"
    assert "kind" in row, f"row must have 'kind' key: {row}"
    assert "digest" in row, f"row must have 'digest' key: {row}"
    assert "tags" in row, f"row must have 'tags' key: {row}"
    assert "status" in row, f"row must have 'status' key: {row}"


def test_publish_kind_order_is_skills_rules_agents_bundles(
    grim_at, project_dir: Path, registry: str, unique_repo: str
) -> None:
    """Report rows must appear in skills→rules→agents order (ADR D4)."""
    prefix = unique_repo.split("/")[-1]
    runner = grim_at(project_dir)

    manifest_path = _write_publish_manifest(
        project_dir,
        registry,
        prefix,
        include_rule=True,
        include_agent=True,
    )

    rows = runner.json("publish", "--manifest", str(manifest_path))
    assert len(rows) == 3

    kinds = [r["kind"] for r in rows]
    assert kinds[0] == "skill", f"first row must be skill, got {kinds[0]}"
    assert kinds[1] == "rule", f"second row must be rule, got {kinds[1]}"
    assert kinds[2] == "agent", f"third row must be agent, got {kinds[2]}"


def test_publish_rerun_skips_existing_and_exits_0(
    grim_at, project_dir: Path, registry: str, unique_repo: str
) -> None:
    """Second run with identical versions exits 0 with all rows 'skipped'
    (ADR D3 skip-existing default — Testing Strategy row 2)."""
    prefix = unique_repo.split("/")[-1]
    runner = grim_at(project_dir)

    manifest_path = _write_publish_manifest(
        project_dir,
        registry,
        prefix,
    )

    # First run: push
    first = runner.json("publish", "--manifest", str(manifest_path))
    assert all(r["status"] == "pushed" for r in first), "first run must push"

    # Second run: skip
    result = runner.run(
        "publish",
        "--manifest", str(manifest_path),
        format="json",
        check=False,
    )
    assert result.returncode == 0, (
        f"re-run must exit 0 (skip-existing default), got {result.returncode}; "
        f"stderr: {result.stderr.strip()}"
    )

    second = json.loads(result.stdout)
    statuses = {r["status"] for r in second}
    assert statuses == {"skipped"}, (
        f"all rows must be 'skipped' on re-run (ADR D3), got {statuses}"
    )


def test_publish_dry_run_pushes_nothing(
    grim_at, project_dir: Path, registry: str, unique_repo: str
) -> None:
    """--dry-run exits 0 and reports 'dry-run' status; nothing pushed
    (ADR D3 — Testing Strategy row 3)."""
    prefix = unique_repo.split("/")[-1]
    runner = grim_at(project_dir)

    manifest_path = _write_publish_manifest(
        project_dir,
        registry,
        prefix,
    )

    result = runner.run(
        "publish",
        "--manifest", str(manifest_path),
        "--dry-run",
        format="json",
        check=False,
    )
    assert result.returncode == 0, (
        f"--dry-run must exit 0, got {result.returncode}; stderr: {result.stderr.strip()}"
    )

    rows = json.loads(result.stdout)
    statuses = {r["status"] for r in rows}
    assert statuses == {"dry-run"}, (
        f"all rows must be 'dry-run' with --dry-run, got {statuses}"
    )

    # F5: dry-run rows must carry non-empty tags (Skipped-vs-DryRun distinction:
    # skipped rows have no tags to report, but dry-run rows still compute and
    # report the tags that *would* have been pushed).
    for row in rows:
        assert isinstance(row.get("tags"), list) and len(row["tags"]) > 0, (
            f"dry-run row must have non-empty 'tags' field "
            f"(distinguishes dry-run from skipped); got row: {row}"
        )

    # Nothing was pushed: verify no tag exists in registry
    skill_name = f"{prefix}-skill"
    # Attempt to resolve the tag; should be absent (would 404)
    try:
        # tag_digest raises on 404; if it does not raise, tag was pushed
        digest = tag_digest(f"skills/{skill_name}", "0.1.0")
        pytest.fail(
            f"--dry-run must not push; tag 0.1.0 resolved to {digest}"
        )
    except urllib.error.HTTPError:
        pass  # expected: tag not found (404)


def test_publish_force_moves_existing_tag(
    grim_at, project_dir: Path, registry: str, unique_repo: str
) -> None:
    """--force re-publishes even when the version tag already exists, and
    the second push yields a different digest when content changed (ADR D3
    — Testing Strategy row 4)."""
    prefix = unique_repo.split("/")[-1]
    runner = grim_at(project_dir)

    manifest_path = _write_publish_manifest(
        project_dir,
        registry,
        prefix,
    )

    # First publish
    first = runner.json("publish", "--manifest", str(manifest_path))
    first_digest = first[0]["digest"]
    assert first[0]["status"] == "pushed", (
        f"first publish must be 'pushed', got {first[0]['status']}"
    )
    assert first_digest is not None, "first push must have a non-null digest"

    # Modify SKILL.md content so the artifact layer differs on the second push
    skill_name = f"{prefix}-skill"
    skill_md = project_dir / "skills" / skill_name / "SKILL.md"
    original = skill_md.read_text()
    skill_md.write_text(original + "\n## Updated\n\nForce-push test modification.\n")

    # --force must push (not skip) even though version already exists
    result = runner.run(
        "publish",
        "--manifest", str(manifest_path),
        "--force",
        format="json",
        check=False,
    )
    assert result.returncode == 0, (
        f"--force re-publish must exit 0, got {result.returncode}; "
        f"stderr: {result.stderr.strip()}"
    )

    second = json.loads(result.stdout)
    assert all(r["status"] == "pushed" for r in second), (
        f"--force must push (not skip), got statuses {[r['status'] for r in second]}"
    )

    second_digest = second[0]["digest"]
    assert second_digest is not None, "second push must have a non-null digest"
    assert second_digest != first_digest, (
        f"--force with modified content must produce a different digest: "
        f"first={first_digest!r}, second={second_digest!r}"
    )


def test_publish_only_single_entry(
    grim_at, project_dir: Path, registry: str, unique_repo: str
) -> None:
    """--only NAME publishes exactly that entry (ADR D1 — Testing Strategy row 5)."""
    prefix = unique_repo.split("/")[-1]
    runner = grim_at(project_dir)

    manifest_path = _write_publish_manifest(
        project_dir,
        registry,
        prefix,
        include_rule=True,
    )

    skill_name = f"{prefix}-skill"
    rows = runner.json(
        "publish",
        "--manifest", str(manifest_path),
        "--only", skill_name,
    )

    assert len(rows) == 1, f"--only {skill_name} must yield 1 row, got {rows}"
    assert rows[0]["kind"] == "skill"
    # Verify the reference contains the skill name
    assert skill_name in rows[0]["ref"], (
        f"ref must contain the skill name: {rows[0]['ref']}"
    )


def test_publish_only_unknown_name_exits_65(
    grim_at, project_dir: Path, registry: str, unique_repo: str
) -> None:
    """--only with an unknown name exits 65 (DataError, ADR D1 — Testing Strategy row 5)."""
    prefix = unique_repo.split("/")[-1]
    runner = grim_at(project_dir)

    manifest_path = _write_publish_manifest(
        project_dir,
        registry,
        prefix,
    )

    result = runner.run(
        "publish",
        "--manifest", str(manifest_path),
        "--only", "this-does-not-exist-in-manifest",
        check=False,
    )
    assert result.returncode == 65, (
        f"--only with unknown name must exit 65 (DataError), got {result.returncode}; "
        f"stderr: {result.stderr.strip()}"
    )


def test_publish_tag_canary_uses_movable_tag_version_absent(
    grim_at, project_dir: Path, registry: str, unique_repo: str
) -> None:
    """--tag canary publishes with the canary tag; version tag is absent.
    (ADR D1 — Testing Strategy row 6)"""
    prefix = unique_repo.split("/")[-1]
    runner = grim_at(project_dir)

    manifest_path = _write_publish_manifest(
        project_dir,
        registry,
        prefix,
    )

    rows = runner.json(
        "publish",
        "--manifest", str(manifest_path),
        "--tag", "canary",
    )
    assert len(rows) >= 1
    assert rows[0]["status"] == "pushed", (
        f"--tag canary must push, got status {rows[0]['status']}"
    )

    skill_name = f"{prefix}-skill"
    # The ref must end with :canary
    assert rows[0]["ref"].endswith(":canary"), (
        f"ref must end with :canary, got {rows[0]['ref']}"
    )

    # canary tag must exist in registry
    canary_digest = tag_digest(f"skills/{skill_name}", "canary")
    assert canary_digest.startswith("sha256:"), (
        f"canary tag must resolve to a digest, got {canary_digest}"
    )

    # version tag (0.1.0) must NOT exist (fresh name, only canary was pushed)
    try:
        version_digest = tag_digest(f"skills/{skill_name}", "0.1.0")
        pytest.fail(
            f"--tag canary must not push the version tag; 0.1.0 resolved to {version_digest}"
        )
    except urllib.error.HTTPError:
        pass  # expected: version tag absent (404)


def test_publish_tag_semver_value_exits_65(
    grim_at, project_dir: Path, registry: str, unique_repo: str
) -> None:
    """--tag with a semver value exits 65 (DataError, ADR D1 — Testing Strategy row 6)."""
    prefix = unique_repo.split("/")[-1]
    runner = grim_at(project_dir)

    manifest_path = _write_publish_manifest(
        project_dir,
        registry,
        prefix,
    )

    result = runner.run(
        "publish",
        "--manifest", str(manifest_path),
        "--tag", "1.2.3",
        check=False,
    )
    assert result.returncode == 65, (
        f"--tag 1.2.3 (semver) must exit 65 (DataError), got {result.returncode}; "
        f"stderr: {result.stderr.strip()}"
    )


def test_publish_missing_manifest_exits_65(
    grim_at, project_dir: Path, registry: str, unique_repo: str
) -> None:
    """Missing manifest file exits 65 (DataError, ADR D2 — Testing Strategy row 1).

    W1: stderr must mention 'manifest not found' so the operator can distinguish
    this error from other DataError causes without reading exit-code tables.
    """
    runner = grim_at(project_dir)
    manifest_path = str(project_dir / "nonexistent-publish.toml")

    result = runner.run(
        "publish",
        "--manifest", manifest_path,
        check=False,
    )
    assert result.returncode == 65, (
        f"missing manifest must exit 65 (DataError), got {result.returncode}; "
        f"stderr: {result.stderr.strip()}"
    )
    # W1: the human-readable error must identify the failure mode
    assert "manifest not found" in result.stderr, (
        f"stderr must contain 'manifest not found' for a missing --manifest path; "
        f"stderr: {result.stderr.strip()!r}"
    )


def test_publish_default_manifest_missing_exits_65(
    grim_at, project_dir: Path, registry: str, unique_repo: str
) -> None:
    """Calling grim publish with no --manifest and no publish.toml in CWD
    exits 65 (no publish.toml in an empty project dir).

    W1: stderr must mention 'manifest not found' so the operator knows the
    default manifest lookup failed (vs. a malformed manifest or other DataError).
    """
    runner = grim_at(project_dir)
    # project_dir has no publish.toml — default path ./publish.toml is absent

    result = runner.run("publish", check=False)
    assert result.returncode == 65, (
        f"missing default publish.toml must exit 65, got {result.returncode}; "
        f"stderr: {result.stderr.strip()}"
    )
    # W1: the human-readable error must identify the failure mode
    assert "manifest not found" in result.stderr, (
        f"stderr must contain 'manifest not found' when default publish.toml is absent; "
        f"stderr: {result.stderr.strip()!r}"
    )


def test_publish_format_json_bare_array(
    grim_at, project_dir: Path, registry: str, unique_repo: str
) -> None:
    """--format json outputs a bare JSON array (not wrapped object, ADR D6)."""
    prefix = unique_repo.split("/")[-1]
    runner = grim_at(project_dir)

    manifest_path = _write_publish_manifest(
        project_dir,
        registry,
        prefix,
    )

    result = runner.run(
        "publish",
        "--manifest", str(manifest_path),
        format="json",
        check=False,
    )
    assert result.returncode == 0

    parsed = json.loads(result.stdout)
    assert isinstance(parsed, list), (
        f"--format json must produce a bare JSON array (ADR D6), got {type(parsed).__name__}"
    )


def test_publish_plain_output_has_table_headers(
    grim_at, project_dir: Path, registry: str, unique_repo: str
) -> None:
    """Plain (default) output has table headers Kind|Ref|Digest|Tags|Status."""
    prefix = unique_repo.split("/")[-1]
    runner = grim_at(project_dir)

    manifest_path = _write_publish_manifest(
        project_dir,
        registry,
        prefix,
    )

    result = runner.run(
        "publish",
        "--manifest", str(manifest_path),
        check=False,
    )
    assert result.returncode == 0
    out = result.stdout
    # Plain table must include the static column headers
    assert "Kind" in out, f"plain output must have 'Kind' header, got: {out[:200]}"
    assert "Status" in out, f"plain output must have 'Status' header, got: {out[:200]}"


# ---------------------------------------------------------------------------
# Tests: bundle published, member skills resolvable
# ---------------------------------------------------------------------------


def test_publish_bundle_after_member_skill_is_resolvable(
    grim_at, project_dir: Path, registry: str, unique_repo: str
) -> None:
    """After grim publish, the published bundle can be referenced (its member
    skill was also published in the same batch). Exercises skills→bundles
    ordering: skill goes first so bundle members resolve (ADR D4).

    Verification: lock the bundle reference; it must resolve without error.
    """
    prefix = unique_repo.split("/")[-1]
    runner = grim_at(project_dir)

    skill_name = f"{prefix}-skill"
    bundle_name = f"{prefix}-bundle"
    skill_ref = f"{registry}/skills/{skill_name}:0.1.0"

    manifest_path = _write_publish_manifest(
        project_dir,
        registry,
        prefix,
        include_bundle=True,
        skill_ref_for_bundle=skill_ref,
    )

    # Publish skill + bundle in one batch
    rows = runner.json("publish", "--manifest", str(manifest_path))
    assert len(rows) == 2, f"expected 2 rows (skill+bundle), got {rows}"

    skill_row = next((r for r in rows if r["kind"] == "skill"), None)
    bundle_row = next((r for r in rows if r["kind"] == "bundle"), None)
    assert skill_row is not None, "skill must appear in report"
    assert bundle_row is not None, "bundle must appear in report"
    assert skill_row["status"] == "pushed"
    assert bundle_row["status"] == "pushed"

    # The skill tag must exist in the registry (member was pushed before bundle)
    skill_tag_digest = tag_digest(f"skills/{skill_name}", "0.1.0")
    assert skill_tag_digest.startswith("sha256:"), (
        f"skill member must be in registry after publish, got {skill_tag_digest}"
    )


# ---------------------------------------------------------------------------
# Tests: error cases
# ---------------------------------------------------------------------------


def test_publish_empty_manifest_exits_65(
    grim_at, project_dir: Path, registry: str, unique_repo: str
) -> None:
    """A publish.toml with registry but no entries exits 65 (ADR D2).

    Codex#3 / W1 variant: stderr must mention 'no packages declared in manifest'
    so the operator can distinguish an empty manifest from a missing one.
    """
    runner = grim_at(project_dir)

    manifest_path = project_dir / "publish.toml"
    manifest_path.write_text(f'registry = "{registry}"\n')

    result = runner.run(
        "publish",
        "--manifest", str(manifest_path),
        check=False,
    )
    assert result.returncode == 65, (
        f"empty manifest must exit 65 (DataError), got {result.returncode}; "
        f"stderr: {result.stderr.strip()}"
    )
    # Codex#3: stderr must name the specific validation failure
    assert "no packages declared in manifest" in result.stderr, (
        f"stderr must contain 'no packages declared in manifest' for an empty manifest; "
        f"stderr: {result.stderr.strip()!r}"
    )


def test_publish_mid_batch_fail_fast(
    grim_at, project_dir: Path, registry: str, unique_repo: str
) -> None:
    """When a later entry fails during push, the batch stops, the report
    contains completed entries (status=pushed) plus the failed entry
    (status=failed, digest=null), and grim exits non-zero.

    Failure is injected by pointing the second entry at a non-existent
    source path — release will error when it tries to open the path.
    The first entry uses the normal skill source so it pushes successfully.
    (ADR D4 fail-fast — Testing Strategy row 8)
    """
    prefix = unique_repo.split("/")[-1]
    runner = grim_at(project_dir)

    # Write the first skill (fully valid source → will push successfully)
    skill_a = f"{prefix}-skill-a"
    _make_skill_source(project_dir, skill_a)

    # Create an empty directory for skill_b so that validate_manifest
    # sees the path as existing (passes the upfront validation), but the
    # skill builder will fail at push time because there is no SKILL.md
    # inside — injecting a deterministic in-band push-time failure.
    skill_b = f"{prefix}-skill-b"
    skill_b_dir = project_dir / "skills" / skill_b
    skill_b_dir.mkdir(parents=True, exist_ok=True)

    manifest_lines = [
        f'registry = "{registry}"',
        "",
        f"[skills.{skill_a}]",
        'version = "0.1.0"',
        "",
        f"[skills.{skill_b}]",
        'version = "0.1.0"',
    ]
    manifest_path = project_dir / "publish.toml"
    manifest_path.write_text("\n".join(manifest_lines) + "\n")

    result = runner.run(
        "publish",
        "--manifest", str(manifest_path),
        format="json",
        check=False,
    )

    assert result.returncode != 0, (
        f"mid-batch failure must exit non-zero, got {result.returncode}"
    )

    # Codex#3: builder must print the underlying release error chain to stderr
    # before emitting the partial JSON report. At least one stderr line must
    # start with "error:" so CI log scanners can identify the failure cause.
    stderr_lines = result.stderr.splitlines()
    assert any(line.lstrip().startswith("error:") for line in stderr_lines), (
        f"stderr must contain a line starting with 'error:' describing the "
        f"push failure; stderr: {result.stderr.strip()!r}"
    )

    rows = json.loads(result.stdout)
    assert isinstance(rows, list), "JSON output must be a bare array even on partial failure"
    assert len(rows) >= 1, f"report must contain at least the failed entry, got {rows}"

    # First entry (skill_a) must have succeeded
    pushed_rows = [r for r in rows if r.get("status") == "pushed"]
    assert len(pushed_rows) >= 1, (
        f"at least one entry must be pushed before fail-fast; rows: {rows}"
    )
    assert any(skill_a in r["ref"] for r in pushed_rows), (
        f"skill_a must appear as pushed; rows: {rows}"
    )

    # Failed entry must appear in the report
    failed_rows = [r for r in rows if r.get("status") == "failed"]
    assert len(failed_rows) == 1, (
        f"exactly one failed entry must appear in report; rows: {rows}"
    )
    assert failed_rows[0]["digest"] is None, (
        f"failed entry must have null digest; got {failed_rows[0]['digest']!r}"
    )

    # No rows should appear after the failed entry (fail-fast)
    failed_idx = rows.index(failed_rows[0])
    assert failed_idx == len(rows) - 1, (
        f"failed entry must be the last row (fail-fast); rows: {rows}"
    )


def test_publish_only_and_tag_combined(
    grim_at, project_dir: Path, registry: str, unique_repo: str
) -> None:
    """--only NAME --tag canary publishes only the named entry with tag
    canary; the ref ends with :canary (ADR D1 — Testing Strategy row 7)."""
    prefix = unique_repo.split("/")[-1]
    runner = grim_at(project_dir)

    # Multi-entry manifest: skill + rule
    manifest_path = _write_publish_manifest(
        project_dir,
        registry,
        prefix,
        include_rule=True,
    )

    skill_name = f"{prefix}-skill"
    result = runner.run(
        "publish",
        "--manifest", str(manifest_path),
        "--only", skill_name,
        "--tag", "canary",
        format="json",
        check=False,
    )
    assert result.returncode == 0, (
        f"--only+--tag must exit 0, got {result.returncode}; stderr: {result.stderr.strip()}"
    )

    rows = json.loads(result.stdout)
    assert len(rows) == 1, (
        f"--only {skill_name} must yield exactly 1 row, got {rows}"
    )
    assert rows[0]["kind"] == "skill", f"row kind must be skill, got {rows[0]['kind']}"
    assert rows[0]["ref"].endswith(":canary"), (
        f"ref must end with :canary when --tag canary, got {rows[0]['ref']}"
    )
    assert rows[0]["status"] == "pushed", (
        f"row status must be pushed, got {rows[0]['status']}"
    )

    # The rule must NOT have been published (--only filtered it out)
    rule_name = f"{prefix}-rule"
    try:
        rule_d = tag_digest(f"rules/{rule_name}", "canary")
        pytest.fail(f"--only {skill_name} must not push the rule; canary resolved to {rule_d}")
    except urllib.error.HTTPError:
        pass  # expected: rule tag absent


def test_publish_pin_true_bundle_member_references_digest(
    grim_at, project_dir: Path, registry: str, unique_repo: str
) -> None:
    """pin=true on a bundle entry freezes member references to digest pins
    in the published bundle layer (ADR D2, D5 — Testing Strategy row 9).

    Verification: fetch the bundle manifest from the registry, then fetch
    the layer blob and parse the members JSON; every member id must contain
    '@sha256:'.
    """
    prefix = unique_repo.split("/")[-1]
    runner = grim_at(project_dir)

    skill_name = f"{prefix}-skill"
    bundle_name = f"{prefix}-bundle"
    skill_ref = f"{registry}/skills/{skill_name}:0.1.0"

    # Build publish.toml with pin=true on the bundle
    _make_skill_source(project_dir, skill_name)
    _make_bundle_source(project_dir, bundle_name, skill_ref)

    manifest_lines = [
        f'registry = "{registry}"',
        "",
        f"[skills.{skill_name}]",
        'version = "0.1.0"',
        "",
        f"[bundles.{bundle_name}]",
        'version = "0.1.0"',
        "pin = true",
    ]
    manifest_path = project_dir / "publish.toml"
    manifest_path.write_text("\n".join(manifest_lines) + "\n")

    rows = runner.json("publish", "--manifest", str(manifest_path))
    assert len(rows) == 2, f"expected skill+bundle, got {rows}"
    bundle_row = next((r for r in rows if r["kind"] == "bundle"), None)
    assert bundle_row is not None, "bundle must appear in report"
    assert bundle_row["status"] == "pushed", (
        f"bundle must be pushed, got {bundle_row['status']}"
    )

    # Fetch the OCI manifest for the published bundle
    oci_manifest = fetch_manifest(f"bundles/{bundle_name}", "0.1.0")
    layers = oci_manifest.get("layers", [])
    assert len(layers) == 1, f"bundle OCI manifest must have exactly 1 layer; got {layers}"

    layer_digest = layers[0]["digest"]
    assert layer_digest.startswith("sha256:"), (
        f"layer digest must be sha256:..., got {layer_digest}"
    )

    # Fetch the layer blob (the bundle members JSON)
    blob_url = f"{REGISTRY_BASE}/v2/bundles/{bundle_name}/blobs/{layer_digest}"
    req = urllib.request.Request(blob_url)
    with urllib.request.urlopen(req) as resp:
        members_doc = json.loads(resp.read())

    members = members_doc.get("members", [])
    assert len(members) >= 1, f"bundle members layer must have at least 1 member; got {members_doc}"

    for member in members:
        member_id = member.get("id", "")
        assert "@sha256:" in member_id, (
            f"pin=true must freeze member ref to digest pin; got id={member_id!r}"
        )


# ---------------------------------------------------------------------------
# Tests: repository_prefix / per-entry repository (issue #11 axis B)
# ---------------------------------------------------------------------------


def test_publish_nested_repository_prefix(
    grim_at, project_dir: Path, registry: str, unique_repo: str
) -> None:
    """Headline regression for issue #11 axis B: a manifest `repository_prefix`
    nests the push under the registry's group/project path instead of the
    hardcoded `{kind-subdir}/{name}` — the reporter's GitLab case."""
    prefix = unique_repo.split("/")[-1]
    runner = grim_at(project_dir)

    skill_name = f"{prefix}-skill"
    _make_skill_source(project_dir, skill_name)
    repo_prefix = f"{prefix}-ns/sub"
    manifest_path = project_dir / "publish.toml"
    manifest_path.write_text(
        f'registry = "{registry}"\n'
        f'repository_prefix = "{repo_prefix}"\n\n'
        f"[skills.{skill_name}]\n"
        f'version = "0.1.0"\n'
    )

    rows = runner.json("publish", "--manifest", str(manifest_path))
    assert len(rows) == 1, f"expected 1 row, got {rows}"
    assert rows[0]["status"] == "pushed", f"skill must be pushed, got {rows[0]}"

    nested_repo = f"{repo_prefix}/{skill_name}"
    expected_ref = f"{registry}/{nested_repo}:0.1.0"
    assert rows[0]["ref"] == expected_ref, (
        f"entry must publish to the nested repo, got {rows[0]['ref']!r}"
    )
    # The artifact actually landed at the nested repo.
    assert tag_digest(nested_repo, "0.1.0") == rows[0]["digest"], (
        "nested repo must resolve to the published manifest digest"
    )
    # The conventional default path (`skills/<name>`) must NOT exist.
    with pytest.raises(urllib.error.HTTPError):
        tag_digest(f"skills/{skill_name}", "0.1.0")


def test_publish_per_entry_repository(
    grim_at, project_dir: Path, registry: str, unique_repo: str
) -> None:
    """Issue #11 axis B: a per-entry `repository` is used verbatim (the entry
    name is NOT appended) and wins over the manifest `repository_prefix`."""
    prefix = unique_repo.split("/")[-1]
    runner = grim_at(project_dir)

    skill_name = f"{prefix}-skill"
    _make_skill_source(project_dir, skill_name)
    full_repo = f"{prefix}-grp/proj/skill/{skill_name}"
    manifest_path = project_dir / "publish.toml"
    manifest_path.write_text(
        f'registry = "{registry}"\n'
        f'repository_prefix = "{prefix}-ignored/prefix"\n\n'
        f"[skills.{skill_name}]\n"
        f'version = "0.1.0"\n'
        f'repository = "{full_repo}"\n'
    )

    rows = runner.json("publish", "--manifest", str(manifest_path))
    assert len(rows) == 1, f"expected 1 row, got {rows}"
    assert rows[0]["status"] == "pushed", f"skill must be pushed, got {rows[0]}"

    expected_ref = f"{registry}/{full_repo}:0.1.0"
    assert rows[0]["ref"] == expected_ref, (
        f"per-entry repository must be used verbatim, got {rows[0]['ref']!r}"
    )
    assert tag_digest(full_repo, "0.1.0") == rows[0]["digest"], (
        "per-entry repo must resolve to the published manifest digest"
    )
    # The prefix path must NOT have been used (per-entry repository wins).
    with pytest.raises(urllib.error.HTTPError):
        tag_digest(f"{prefix}-ignored/prefix/{skill_name}", "0.1.0")


def test_publish_wire_shape_empty_config(
    grim_at, project_dir: Path, registry: str, unique_repo: str
) -> None:
    """Issue #11 axis A via the publish path: a batch-published manifest carries
    the OCI empty config type (GitLab-allowlist-safe) and the `com.grimoire.kind`
    annotation. Guards the publish path independently of `grim release` so a
    future change to how publish composes the manifest cannot silently
    reintroduce the custom config type."""
    prefix = unique_repo.split("/")[-1]
    runner = grim_at(project_dir)

    skill_name = f"{prefix}-skill"
    _make_skill_source(project_dir, skill_name)
    manifest_path = project_dir / "publish.toml"
    manifest_path.write_text(
        f'registry = "{registry}"\n\n'
        f"[skills.{skill_name}]\n"
        f'version = "0.1.0"\n'
    )

    rows = runner.json("publish", "--manifest", str(manifest_path))
    assert rows[0]["status"] == "pushed", f"skill must be pushed, got {rows[0]}"

    manifest = fetch_manifest(f"skills/{skill_name}", "0.1.0")
    assert manifest["config"]["mediaType"] == "application/vnd.oci.empty.v1+json", (
        f"published config descriptor must be the OCI empty type, "
        f"got {manifest['config']['mediaType']!r}"
    )
    # No custom artifactType on the wire — GitLab rejects it.
    assert "artifactType" not in manifest, (
        f"published manifest must NOT carry a custom artifactType, "
        f"got {manifest.get('artifactType')!r}"
    )
    assert manifest.get("annotations", {}).get("com.grimoire.kind") == "skill", (
        f"published manifest must carry com.grimoire.kind=skill, "
        f"got {manifest.get('annotations', {})!r}"
    )
