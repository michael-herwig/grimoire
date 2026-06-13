# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The Grimoire Authors
"""Install-state portability acceptance tests (T13).

These tests encode the contracts of the anchor-relativized install state
introduced by plan_install_state_portability.md.  They are written from the
design record (§1–§6, UX scenarios §2, tasks T11–T13) and MUST FAIL until
T11/T12 are implemented — that failure is the proof that the spec is real.

Six test scenarios:

1. no-collision  — two projects that canonicalize to the same path under a
                   shared GRIM_HOME each get their own
                   ``<workspace>/.grimoire/state.json``.
2. portability   — install, move the project dir, ``grim status`` still
                   resolves and reports ``installed``.
3. drift-anchor  — install then edit a support-dir file; ``grim status``
                   reports ``modified`` (drift preserved through anchoring).
4. traversal     — hand-corrupt the stored ``relative`` with ``../``: a
                   mutating command exits 65 (DataError); ``grim status``
                   degrades the row to ``missing`` and exits 0.
5. fresh-clone   — no ``.grimoire/state.json`` → ``grim install`` creates it
                   and exits 0.
6. gitignore     — after the first project install ``.grimoire/.gitignore``
                   exists with contents ``*``; the consumer root ``.gitignore``
                   is unchanged; a second install does not overwrite a
                   hand-edited ``.grimoire/.gitignore``.
"""
from __future__ import annotations

import hashlib
import json
import os
import shutil
import sys
from pathlib import Path

import pytest

from src.helpers import make_artifact, write_config
from src.runner import GrimRunner


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _rule_artifact(unique_repo: str, name: str = "rust-style") -> object:
    """Push a minimal single-file rule artifact and return its fq ref."""
    art = make_artifact(
        f"{unique_repo}/{name}",
        "rule",
        {f"{name}.md": "---\npaths: ['**/*.rs']\n---\n# canonical\n"},
        tag="v1",
    )
    return art


def _multifile_rule_artifact(unique_repo: str, name: str = "my-rule") -> object:
    """Push a multi-file rule (index + support dir) and return its artifact."""
    files = {
        f"{name}.md": (
            "---\npaths: ['**/*.rs']\n---\n"
            f"# {name}\nSee [{name}/examples.md](./{name}/examples.md)\n"
        ),
        f"{name}/examples.md": "# Examples\nworked example\n",
    }
    return make_artifact(
        f"{unique_repo}/{name}",
        "rule",
        files,
        tag="v1",
    )


def _install_rule(grim_at, project_dir: Path, art) -> GrimRunner:
    """Write config, lock, and install; return the runner."""
    runner = grim_at(project_dir)
    write_config(project_dir, rules={"rust-style": art.fq})
    runner.run("lock", check=False)
    runner.run("install", check=False)
    return runner


def _install_multifile_rule(grim_at, project_dir: Path, art, name: str = "my-rule") -> GrimRunner:
    runner = grim_at(project_dir)
    write_config(project_dir, rules={name: art.fq})
    runner.run("lock", check=False)
    runner.run("install", check=False)
    return runner


# ---------------------------------------------------------------------------
# T13-1: No collision — two projects share one GRIM_HOME, never collide
# ---------------------------------------------------------------------------


def test_no_collision_two_projects_share_grim_home(
    grim_binary: Path,
    grim_home: Path,
    tmp_path: Path,
    registry: str,
    unique_repo: str,
) -> None:
    """Two projects sharing one GRIM_HOME each get their own
    ``<workspace>/.grimoire/state.json`` (§1.6, UX §2(b)).

    Under the old scheme both would hash ``/workspace/grimoire.toml`` to the
    SAME sha256 filename, colliding.  Under the new scheme the state file
    LOCATION is the key — they never collide regardless of how similar the
    config paths look.
    """
    art = _rule_artifact(unique_repo)

    # Two sibling workspace directories.
    ws_a = tmp_path / "project-a"
    ws_b = tmp_path / "project-b"
    ws_a.mkdir()
    ws_b.mkdir()

    runner_a = GrimRunner(grim_binary, grim_home, cwd=ws_a)
    runner_b = GrimRunner(grim_binary, grim_home, cwd=ws_b)

    # Set up and install in both workspaces.
    write_config(ws_a, rules={"rust-style": art.fq})
    runner_a.run("lock", check=False)
    runner_a.run("install", check=False)

    write_config(ws_b, rules={"rust-style": art.fq})
    runner_b.run("lock", check=False)
    runner_b.run("install", check=False)

    # Each workspace must have its OWN .grimoire/state.json.
    state_a = ws_a / ".grimoire" / "state.json"
    state_b = ws_b / ".grimoire" / "state.json"
    assert state_a.is_file(), (
        f"project-a must have its own .grimoire/state.json at {state_a}"
    )
    assert state_b.is_file(), (
        f"project-b must have its own .grimoire/state.json at {state_b}"
    )

    # Both state files are independent (separate paths, no shared mutable file).
    assert state_a != state_b, "the two state files must be at different paths"

    # Both projects report installed correctly.
    rows_a = runner_a.json("status")
    rows_b = runner_b.json("status")
    assert rows_a[0]["state"] == "installed", (
        f"project-a must report installed after install, got {rows_a}"
    )
    assert rows_b[0]["state"] == "installed", (
        f"project-b must report installed after install, got {rows_b}"
    )


# ---------------------------------------------------------------------------
# T13-2: Portability — move project dir, status still resolves
# ---------------------------------------------------------------------------


def test_status_resolves_after_project_dir_move(
    grim_binary: Path,
    grim_home: Path,
    tmp_path: Path,
    unique_repo: str,
) -> None:
    """Install in ``<orig>``, move the entire directory to ``<moved>``,
    then ``grim status`` from the moved location still reports ``installed``
    (§1.1 Workspace anchor + §2(a) portability contract).

    The state.json travels with the workspace (inside ``.grimoire/``), and
    the ``Workspace`` anchor resolves relative to wherever the workspace is
    now — not where it was when installed.
    """
    art = _rule_artifact(unique_repo)

    orig = tmp_path / "original-workspace"
    orig.mkdir()

    runner_orig = GrimRunner(grim_binary, grim_home, cwd=orig)
    write_config(orig, rules={"rust-style": art.fq})
    runner_orig.run("lock", check=False)
    runner_orig.run("install", check=False)

    # Verify installed in original location.
    rows_before = runner_orig.json("status")
    assert rows_before[0]["state"] == "installed", (
        f"must be installed before move, got {rows_before}"
    )

    # Move the entire project directory to a new path.
    moved = tmp_path / "moved-workspace"
    shutil.move(str(orig), str(moved))

    # grim status from the moved location must still resolve correctly.
    runner_moved = GrimRunner(grim_binary, grim_home, cwd=moved)
    rows_after = runner_moved.json("status")

    assert rows_after[0]["state"] == "installed", (
        f"after moving the workspace, grim status must still report "
        f"installed (portable Workspace anchor), got {rows_after}"
    )


# ---------------------------------------------------------------------------
# T13-3: Drift preserved through anchoring — edit support-dir file
# ---------------------------------------------------------------------------


def test_status_reports_modified_after_support_dir_edit(
    grim_at,
    project_dir: Path,
    registry: str,
    unique_repo: str,
) -> None:
    """Edit a support-dir file after install → ``grim status`` reports
    ``modified`` (§1.2 support_dir anchoring, §1.3 ClientOutput.current_hash,
    §6 status.rs contract).

    This verifies drift detection works correctly through the AnchoredPath
    indirection: the content_hash was recorded at install time, and the
    current footprint no longer matches it.
    """
    art = _multifile_rule_artifact(unique_repo)
    runner = _install_multifile_rule(grim_at, project_dir, art)

    # Verify initially installed.
    rows_before = runner.json("status")
    by_name_before = {r["name"]: r for r in rows_before}
    assert by_name_before["my-rule"]["state"] == "installed", (
        f"must be installed before edit, got {rows_before}"
    )

    # Hand-edit a file inside the support dir (not the index).
    support_file = project_dir / ".claude" / "rules" / "my-rule" / "examples.md"
    assert support_file.is_file(), (
        f"support dir file must exist after install at {support_file}"
    )
    support_file.write_text("tampered by test\n")

    # grim status must detect the drift even though it was a support-dir file.
    rows_after = runner.json("status")
    by_name_after = {r["name"]: r for r in rows_after}
    assert by_name_after["my-rule"]["state"] == "modified", (
        f"editing a support-dir file must be detected as drift via "
        f"anchored footprint hash, got {rows_after}"
    )

    # status is read-only — must exit 0 even when drift is detected.
    result = runner.run("--format", "json", "status", check=False)
    assert result.returncode == 0, (
        f"grim status must exit 0 (read-only state), got {result.returncode}"
    )


# ---------------------------------------------------------------------------
# T13-4: Traversal — hand-corrupt relative with ../ → exit 65 / missing
# ---------------------------------------------------------------------------


def test_corrupt_relative_traversal_exits_65_on_mutating_command(
    grim_at,
    project_dir: Path,
    registry: str,
    unique_repo: str,
) -> None:
    """A hand-corrupted ``relative`` containing ``../`` in the state file
    causes a mutating command to exit 65 (DataError) — AnchorError
    TraversalAttempt → DataError mapping (§3, error taxonomy).

    Verifies Layer-1 path-traversal guard fires for install/update/uninstall
    rather than silently accepting an escape attempt.
    """
    art = _rule_artifact(unique_repo)
    runner = _install_rule(grim_at, project_dir, art)

    # Locate and corrupt the state file.
    state_path = project_dir / ".grimoire" / "state.json"
    assert state_path.is_file(), (
        f"state.json must exist after install at {state_path}"
    )

    raw = json.loads(state_path.read_text())
    # Find the first output and corrupt its relative with a traversal sequence.
    corrupted = False
    for record in raw.get("records", []):
        for output in record.get("outputs", []):
            target = output.get("target", {})
            original_relative = target.get("relative", "")
            if original_relative:
                # Prepend ../ to manufacture a traversal attempt.
                target["relative"] = "../" + original_relative
                corrupted = True
                break
        if corrupted:
            break

    assert corrupted, "failed to find a relative field to corrupt in state.json"
    state_path.write_text(json.dumps(raw))

    # A mutating command (install) must exit 65 (DataError / TraversalAttempt).
    result = runner.run("install", check=False)
    assert result.returncode == 65, (
        f"corrupt relative with ../ must cause mutating command to exit 65 "
        f"(DataError / TraversalAttempt), got {result.returncode}; "
        f"stderr: {result.stderr}"
    )


def test_corrupt_relative_traversal_status_degrades_to_missing(
    grim_at,
    project_dir: Path,
    registry: str,
    unique_repo: str,
) -> None:
    """A hand-corrupted ``relative`` containing ``../`` makes ``grim status``
    degrade that row to ``missing`` and exit 0 (§3 read-only contract, §6
    status.rs: AnchorError → Missing, never ``?``-propagated).
    """
    art = _rule_artifact(unique_repo)
    runner = _install_rule(grim_at, project_dir, art)

    state_path = project_dir / ".grimoire" / "state.json"
    raw = json.loads(state_path.read_text())

    corrupted = False
    for record in raw.get("records", []):
        for output in record.get("outputs", []):
            target = output.get("target", {})
            original_relative = target.get("relative", "")
            if original_relative:
                target["relative"] = "../" + original_relative
                corrupted = True
                break
        if corrupted:
            break

    assert corrupted, "failed to find a relative field to corrupt in state.json"
    state_path.write_text(json.dumps(raw))

    # grim status must exit 0 (read-only — never propagates AnchorError).
    result = runner.run("--format", "json", "status", check=False)
    assert result.returncode == 0, (
        f"grim status must exit 0 even with a corrupt relative, "
        f"got {result.returncode}; stderr: {result.stderr}"
    )

    rows = json.loads(result.stdout)
    assert len(rows) >= 1, "status must return at least one row"
    # The corrupted record must degrade to missing (not raise an error, not
    # silently disappear, not remain installed).
    states = {r["state"] for r in rows}
    assert "missing" in states, (
        f"corrupt relative must degrade to missing in status output, "
        f"got states {states}"
    )
    # Crucially, it must NOT report installed or modified.
    assert "installed" not in states, (
        "a record with a corrupt relative path must not report as installed"
    )


# ---------------------------------------------------------------------------
# T13-5: Fresh clone — no .grimoire/state.json → grim install creates it
# ---------------------------------------------------------------------------


def test_fresh_clone_install_creates_state_json(
    grim_at,
    project_dir: Path,
    registry: str,
    unique_repo: str,
) -> None:
    """In a fresh clone (no ``.grimoire/state.json``), ``grim install``
    creates the file and exits 0 (§1.6 project_state_path, fresh-clone
    scenario from §2(a)).

    This is the normal first-install path. The state file must not exist
    before the install and must exist and be valid JSON after.
    """
    art = _rule_artifact(unique_repo)

    state_path = project_dir / ".grimoire" / "state.json"
    # Pre-condition: state.json must not exist (fresh clone).
    assert not state_path.exists(), (
        f".grimoire/state.json must not exist before first install, "
        f"but found it at {state_path}"
    )

    runner = grim_at(project_dir)
    write_config(project_dir, rules={"rust-style": art.fq})
    runner.run("lock", check=False)
    result = runner.run("install", check=False)

    assert result.returncode == 0, (
        f"grim install on a fresh clone must exit 0, "
        f"got {result.returncode}; stderr: {result.stderr}"
    )

    # Post-condition: state.json must exist and be valid JSON.
    assert state_path.is_file(), (
        f"grim install must create .grimoire/state.json at {state_path}"
    )
    try:
        state = json.loads(state_path.read_text())
    except json.JSONDecodeError as exc:
        pytest.fail(f".grimoire/state.json is not valid JSON after install: {exc}")

    assert state.get("version") == 2, (
        f"state.json must be V2 format after install, got version "
        f"{state.get('version')}"
    )


# ---------------------------------------------------------------------------
# T13-6: Self-managed gitignore
# ---------------------------------------------------------------------------


def test_first_install_creates_grimoire_gitignore(
    grim_at,
    project_dir: Path,
    registry: str,
    unique_repo: str,
) -> None:
    """After the first project install, ``.grimoire/.gitignore`` must exist
    with contents ``*`` (self-managed gitignore, §1.6 self-managed gitignore,
    round-2 Q1 decision: uv/pixi pattern).
    """
    art = _rule_artifact(unique_repo)
    runner = grim_at(project_dir)
    write_config(project_dir, rules={"rust-style": art.fq})
    runner.run("lock", check=False)
    runner.run("install", check=False)

    gitignore_path = project_dir / ".grimoire" / ".gitignore"
    assert gitignore_path.is_file(), (
        f".grimoire/.gitignore must be created on first install at "
        f"{gitignore_path}"
    )
    contents = gitignore_path.read_text()
    assert contents.strip() == "*", (
        f".grimoire/.gitignore must contain exactly '*', got: {contents!r}"
    )


def test_first_install_does_not_touch_root_gitignore(
    grim_at,
    project_dir: Path,
    registry: str,
    unique_repo: str,
) -> None:
    """The consumer's root ``.gitignore`` must be UNCHANGED by ``grim install``
    (§1.6: grim never edits the consumer root .gitignore).
    """
    art = _rule_artifact(unique_repo)

    # Write a pre-existing root .gitignore with distinct content.
    root_gitignore = project_dir / ".gitignore"
    original_content = "*.log\nbuild/\ndist/\n"
    root_gitignore.write_text(original_content)

    runner = grim_at(project_dir)
    write_config(project_dir, rules={"rust-style": art.fq})
    runner.run("lock", check=False)
    runner.run("install", check=False)

    assert root_gitignore.read_text() == original_content, (
        "grim install must not modify the consumer's root .gitignore"
    )


def test_second_install_does_not_overwrite_hand_edited_grimoire_gitignore(
    grim_at,
    project_dir: Path,
    registry: str,
    unique_repo: str,
) -> None:
    """A second install must not overwrite a hand-edited ``.grimoire/.gitignore``
    (§1.6: idempotent — never overwrites a user-edited one).
    """
    art = _rule_artifact(unique_repo)
    runner = grim_at(project_dir)
    write_config(project_dir, rules={"rust-style": art.fq})
    runner.run("lock", check=False)
    runner.run("install", check=False)

    gitignore_path = project_dir / ".grimoire" / ".gitignore"
    assert gitignore_path.is_file(), (
        ".grimoire/.gitignore must exist after first install"
    )

    # User hand-edits the gitignore to preserve something specific.
    hand_edited_content = "# keep state.json visible for debugging\n!state.json\n*\n"
    gitignore_path.write_text(hand_edited_content)

    # Second install.
    runner.run("install", check=False)

    # The hand-edited content must survive.
    assert gitignore_path.read_text() == hand_edited_content, (
        "second install must not overwrite a hand-edited .grimoire/.gitignore"
    )


# ---------------------------------------------------------------------------
# F08: Traversal on uninstall — corrupt relative with ../ → exit 65
# ---------------------------------------------------------------------------


def test_corrupt_relative_traversal_exits_65_on_uninstall(
    grim_at,
    project_dir: Path,
    registry: str,
    unique_repo: str,
) -> None:
    """A hand-corrupted ``relative`` containing ``../`` in the state file
    causes ``grim uninstall`` (a mutating command) to exit 65 (DataError) —
    mirrors the existing install traversal test but exercises the uninstall
    path (§3, error taxonomy: AnchorError TraversalAttempt → DataError 65).

    This ensures the Layer-1 path-traversal guard fires for uninstall, not
    just install, when an untrusted stored relative is encountered.
    """
    art = _rule_artifact(unique_repo)
    runner = _install_rule(grim_at, project_dir, art)

    state_path = project_dir / ".grimoire" / "state.json"
    assert state_path.is_file(), (
        f"state.json must exist after install at {state_path}"
    )

    raw = json.loads(state_path.read_text())

    # Corrupt the target.relative of the first output with a traversal sequence.
    corrupted = False
    for record in raw.get("records", []):
        for output in record.get("outputs", []):
            target = output.get("target", {})
            original_relative = target.get("relative", "")
            if original_relative:
                target["relative"] = "../" + original_relative
                corrupted = True
                break
        if corrupted:
            break

    assert corrupted, "failed to find a relative field to corrupt in state.json"
    state_path.write_text(json.dumps(raw))

    # grim uninstall must exit 65 (DataError / TraversalAttempt) when the
    # stored relative contains a path-traversal sequence.
    result = runner.run("--format", "json", "uninstall", "rule", "rust-style", check=False)
    assert result.returncode == 65, (
        f"corrupt relative with ../ must cause grim uninstall to exit 65 "
        f"(DataError / TraversalAttempt), got {result.returncode}; "
        f"stderr: {result.stderr}"
    )


# ---------------------------------------------------------------------------
# F09: Symlinked workspace — status resolves through a symlink to the workspace
# ---------------------------------------------------------------------------


@pytest.mark.skipif(sys.platform == "win32", reason="os.symlink to dirs requires elevation on Windows")
def test_status_resolves_through_symlinked_workspace(
    grim_at,
    project_dir: Path,
    registry: str,
    unique_repo: str,
    tmp_path: Path,
) -> None:
    """Install in a real workspace dir, symlink to it, run ``grim status
    --config <symlink>/grimoire.toml``: the artifact resolves as installed.

    Pins the §1.5 non-canonicalized-abs invariant end-to-end: store-time
    uses the non-canonicalized path (built via ``root.join(relative)``
    against the non-symlinked workspace), so the relative round-trips
    cleanly when resolved under the original (canonical) workspace — even
    when the config is discovered via a symlinked path.
    """
    art = _rule_artifact(unique_repo)
    runner = _install_rule(grim_at, project_dir, art)

    # Verify it is installed from the real directory first.
    rows_before = runner.json("status")
    assert rows_before[0]["state"] == "installed", (
        f"must be installed in original workspace before symlink test, got {rows_before}"
    )

    # Create a symlink to the project_dir.
    symlink_path = tmp_path / "symlinked-workspace"
    os.symlink(str(project_dir), str(symlink_path))
    assert symlink_path.is_symlink(), "symlink creation must succeed"

    # Run grim status using --config pointing into the symlinked workspace.
    symlinked_config = symlink_path / "grimoire.toml"
    result = runner.run(
        "--format", "json",
        "--config", str(symlinked_config),
        "status",
        check=False,
    )
    assert result.returncode == 0, (
        f"grim status --config <symlink>/grimoire.toml must exit 0, "
        f"got {result.returncode}; stderr: {result.stderr}"
    )

    rows = json.loads(result.stdout)
    assert len(rows) >= 1, "status via symlinked config must return at least one row"
    # The artifact must resolve as installed (§1.5: store-time non-canonicalized
    # abs round-trips cleanly regardless of how the config is discovered).
    states = {r["state"] for r in rows}
    assert "installed" in states, (
        f"artifact must resolve as installed when config discovered via a "
        f"symlinked workspace path (§1.5 non-canonicalized-abs invariant), "
        f"got states {states}"
    )


# ---------------------------------------------------------------------------
# F15: Strengthen no-collision — legacy sha file seeded, migration reaps it
# ---------------------------------------------------------------------------


def test_no_collision_reaps_legacy_sha_file_on_install(
    grim_binary: Path,
    grim_home: Path,
    tmp_path: Path,
    registry: str,
    unique_repo: str,
) -> None:
    """Seed a legacy ``$GRIM_HOME/state/projects/<sha>.json`` (the V1 sha256
    formula), run ``grim install``, and assert:

    1. The new ``<workspace>/.grimoire/state.json`` is created (migration
       persisted to the new location).
    2. The legacy sha file is reaped (gone after the first mutating command).

    This directly proves the collision class is resolved: the old scheme
    produced ``sha256(canonicalize(config_path))`` as the filename, so two
    containers mounting the same ``/workspace`` would hash to the SAME file.
    The new scheme uses ``<workspace>/.grimoire/state.json`` (location = key),
    and the migration reaps the old sha file on first mutating command.

    Requirement: T12 "seeded legacy … after install, new state.json exists,
    old file gone" (§5 migration persistence).
    """
    art = _rule_artifact(unique_repo)

    ws = tmp_path / "legacy-workspace"
    ws.mkdir()

    # Write a real grimoire.toml so the config exists on disk and the
    # canonicalized-path formula used by the legacy writer and the reap step
    # agree (canonicalize falls back to the raw path when the file is absent,
    # but a real file proves the canonical formula round-trips).
    write_config(ws, rules={"rust-style": art.fq})

    # Compute the legacy sha256 path the same way the V1 writer did:
    # sha256(canonicalize(config_path)).hex + ".json" under
    # $GRIM_HOME/state/projects/.
    config_path = ws / "grimoire.toml"
    canonical_str = str(config_path.resolve())
    sha = hashlib.sha256(canonical_str.encode()).hexdigest()
    legacy_dir = grim_home / "state" / "projects"
    legacy_dir.mkdir(parents=True, exist_ok=True)
    legacy_file = legacy_dir / f"{sha}.json"

    # Seed a minimal V1 state file at the legacy path.  The content is a
    # placeholder record (the real install will overwrite via migrate+save).
    v1_placeholder = json.dumps({
        "version": 1,
        "records": [],
    })
    legacy_file.write_text(v1_placeholder)
    assert legacy_file.is_file(), "legacy sha file must exist before install"

    new_state_path = ws / ".grimoire" / "state.json"
    assert not new_state_path.exists(), "new state.json must be absent before install"

    runner = GrimRunner(grim_binary, grim_home, cwd=ws)
    runner.run("lock", check=False)
    runner.run("install", check=False)

    # 1. The new V2 state file must exist after the first mutating command.
    assert new_state_path.is_file(), (
        f"grim install must create .grimoire/state.json at the new location "
        f"({new_state_path}); old collision class is fixed (location = key)"
    )

    # 2. The legacy sha file must be reaped.
    assert not legacy_file.exists(), (
        f"grim install must reap the legacy projects/<sha>.json at {legacy_file}; "
        f"this proves the migration relocated state AND cleaned up the old file"
    )
