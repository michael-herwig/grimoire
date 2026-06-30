# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The Grimoire Authors
"""`grim config` settings acceptance tests (get/set/unset/list).

Additional coverage (review-fix round 1):
- D2: group_by_type set→get→unset, tree_separators round-trip, list on empty.
- F4: Concurrency smoke — two simultaneous ``config set`` calls.


Specification-phase suite: every test expresses expected behavior from
``adr_grim_config_command.md`` and ``plan_grim_config.md``.  All tests
FAIL against the Phase-3 stubs (``run`` body is ``unimplemented!()``).

Behaviors covered:
- ``set`` → ``get`` round-trips for ``options.clients``,
  ``options.tui.default_view``, and ``options.default_registry`` at both
  project scope and ``--global`` (writes ``$GRIM_HOME/grimoire.toml``).
- ``get`` of a valid-but-unset key exits 1 with no stdout (git-compatible).
- Unknown dotted key (typo) exits 64 (UsageError).
- Invalid enum value exits 65 (DataError).
- ``unset`` removes a key (subsequent ``get`` exits 1).
- ``list`` outputs ``key=value`` lines; ``--show-origin`` adds the origin.
- ``--format json`` shapes for ``set`` write-confirmation and ``list``.
- ``--global`` writes to ``$GRIM_HOME/grimoire.toml``, never the project
  config (scope separation invariant).
"""
from __future__ import annotations

import json
import subprocess
import threading
from pathlib import Path

import tomllib  # stdlib (Python 3.11+)

from src.helpers import write_config
from src.runner import GrimRunner


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _minimal_global_config(grim_home: Path) -> None:
    """Write a minimal valid ``grimoire.toml`` in ``$GRIM_HOME``.

    ``grim config --global set`` requires the global config file to exist
    (scope resolution fails with NotFound 79 when absent).  This helper
    provides the minimal valid skeleton so tests focus on config-command
    behavior, not on config file creation.
    """
    (grim_home / "grimoire.toml").write_text("[skills]\n\n[rules]\n")


# ---------------------------------------------------------------------------
# Round-trip tests — project scope
# ---------------------------------------------------------------------------


def test_set_get_round_trip_options_clients_project_scope(
    grim_at: object,
    project_dir: Path,
) -> None:
    """``set`` then ``get`` returns the value for ``options.clients`` at
    project scope.

    The value is a comma-separated client list (``claude,opencode``).  The
    plain ``get`` output must be the bare value — no key name, no table —
    on stdout with exit 0.

    Traces to ADR: key-namespace table row ``options.clients``.
    """
    write_config(project_dir)
    runner: GrimRunner = grim_at(project_dir)  # type: ignore[call-arg]

    runner.run("config", "set", "options.clients", "claude,opencode")

    result = runner.plain("config", "get", "options.clients")
    assert result.returncode == 0, (
        f"get of a set key must exit 0; got {result.returncode}\n"
        f"stderr: {result.stderr.strip()}"
    )
    # Plain output is the bare value; both client names must appear.
    assert "claude" in result.stdout, (
        f"plain get must include 'claude' in output; got: {result.stdout!r}"
    )
    assert "opencode" in result.stdout, (
        f"plain get must include 'opencode' in output; got: {result.stdout!r}"
    )


def test_set_get_round_trip_tui_default_view_project_scope(
    grim_at: object,
    project_dir: Path,
) -> None:
    """``set`` then ``get`` returns the value for ``options.tui.default_view``.

    ``tree`` is a valid enum variant.  The plain ``get`` output must
    contain the string ``tree`` and exit 0.

    Traces to ADR: key-namespace table row ``options.tui.default_view``,
    valid values ``flat`` | ``tree``.
    """
    write_config(project_dir)
    runner: GrimRunner = grim_at(project_dir)  # type: ignore[call-arg]

    runner.run("config", "set", "options.tui.default_view", "tree")

    result = runner.plain("config", "get", "options.tui.default_view")
    assert result.returncode == 0, (
        f"get of set tui.default_view must exit 0; got {result.returncode}\n"
        f"stderr: {result.stderr.strip()}"
    )
    assert "tree" in result.stdout, (
        f"plain get must return 'tree'; got: {result.stdout!r}"
    )


def test_set_get_round_trip_options_default_registry_project_scope(
    grim_at: object,
    project_dir: Path,
) -> None:
    """``set`` then ``get`` returns the value for ``options.default_registry``.

    The legacy ``[options].default_registry`` field is string-valued;
    get/set must be allowed per the ADR (though ``registry use`` is
    the preferred modern path).

    Traces to ADR: key-namespace table row ``options.default_registry``.
    """
    write_config(project_dir)
    runner: GrimRunner = grim_at(project_dir)  # type: ignore[call-arg]

    runner.run("config", "set", "options.default_registry", "ghcr.io/acme")

    result = runner.plain("config", "get", "options.default_registry")
    assert result.returncode == 0, (
        f"get of set default_registry must exit 0; got {result.returncode}\n"
        f"stderr: {result.stderr.strip()}"
    )
    assert "ghcr.io/acme" in result.stdout, (
        f"plain get must return the registry URL; got: {result.stdout!r}"
    )


# ---------------------------------------------------------------------------
# Round-trip tests — global scope
# ---------------------------------------------------------------------------


def test_set_get_round_trip_options_clients_global_scope(
    grim_binary: Path,
    grim_home: Path,
) -> None:
    """``--global set`` then ``--global get`` round-trips ``options.clients``.

    The global config file is ``$GRIM_HOME/grimoire.toml``.  Both the set
    and get must target it, not any project config.

    Traces to ADR: ``--global`` flag selects ``$GRIM_HOME/grimoire.toml``.
    """
    _minimal_global_config(grim_home)
    runner = GrimRunner(grim_binary, grim_home)

    runner.run("config", "--global", "set", "options.clients", "claude")

    result = runner.plain("config", "--global", "get", "options.clients")
    assert result.returncode == 0, (
        f"--global get of set key must exit 0; got {result.returncode}\n"
        f"stderr: {result.stderr.strip()}"
    )
    assert "claude" in result.stdout, (
        f"plain --global get must return the set value; got: {result.stdout!r}"
    )


# ---------------------------------------------------------------------------
# Error-path tests
# ---------------------------------------------------------------------------


def test_get_valid_but_unset_key_exits_1_with_empty_stdout(
    grim_at: object,
    project_dir: Path,
) -> None:
    """``get`` of a valid-but-unset key exits 1 and emits nothing on stdout.

    This is the git-compatible script contract:
    ``grim config get options.clients || echo default``

    Traces to ADR: "get of a valid-but-unset key → exit 1 (Failure), no
    stdout".
    """
    write_config(project_dir)  # no options set — all keys unset
    runner: GrimRunner = grim_at(project_dir)  # type: ignore[call-arg]

    result = runner.run("config", "get", "options.clients", check=False)

    assert result.returncode == 1, (
        f"get of valid-but-unset key must exit 1, got {result.returncode}; "
        f"stderr: {result.stderr.strip()}"
    )
    assert result.stdout.strip() == "", (
        f"get of unset key must produce no stdout; got: {result.stdout!r}"
    )


def test_get_unknown_key_exits_64(
    grim_at: object,
    project_dir: Path,
) -> None:
    """``get`` of a key with an unknown root segment exits 64 (UsageError).

    ``optins.clients`` is a typo; the valid root is ``options``.  The
    command must reject it before attempting a config file read.

    Traces to ADR: "Unknown key name … → UsageError 64".
    """
    write_config(project_dir)
    runner: GrimRunner = grim_at(project_dir)  # type: ignore[call-arg]

    result = runner.run("config", "get", "optins.clients", check=False)

    assert result.returncode == 64, (
        f"unknown key must exit 64 (UsageError), got {result.returncode}; "
        f"stderr: {result.stderr.strip()}"
    )


def test_set_invalid_tui_default_view_value_exits_65(
    grim_at: object,
    project_dir: Path,
) -> None:
    """``set options.tui.default_view bogus`` exits 65 (DataError).

    ``bogus`` is not a valid ``DefaultView`` enum variant (valid: ``flat``,
    ``tree``).  The command must reject bad enum values before writing.

    Traces to ADR: "Invalid value format (bad enum …) → DataError 65".
    """
    write_config(project_dir)
    runner: GrimRunner = grim_at(project_dir)  # type: ignore[call-arg]

    result = runner.run(
        "config", "set", "options.tui.default_view", "bogus", check=False
    )

    assert result.returncode == 65, (
        f"invalid enum value must exit 65 (DataError), got {result.returncode}; "
        f"stderr: {result.stderr.strip()}"
    )


# ---------------------------------------------------------------------------
# Unset test
# ---------------------------------------------------------------------------


def test_unset_removes_previously_set_key(
    grim_at: object,
    project_dir: Path,
) -> None:
    """``unset`` of a key makes the subsequent ``get`` exit 1 (unset contract).

    Sequence: set → get (exit 0) → unset → get (exit 1, no stdout).

    Traces to ADR: ``grim config unset <key>`` removes a key; subsequent
    get of the now-absent key must return exit 1.
    """
    write_config(project_dir)
    runner: GrimRunner = grim_at(project_dir)  # type: ignore[call-arg]

    runner.run("config", "set", "options.clients", "claude")

    # Verify the key is set before unset.
    set_result = runner.run("config", "get", "options.clients", check=False)
    assert set_result.returncode == 0, "pre-condition: key must be set before unset"

    runner.run("config", "unset", "options.clients")

    # After unset the key must behave as if it was never set.
    unset_result = runner.run("config", "get", "options.clients", check=False)
    assert unset_result.returncode == 1, (
        f"get after unset must exit 1, got {unset_result.returncode}; "
        f"stderr: {unset_result.stderr.strip()}"
    )
    assert unset_result.stdout.strip() == "", (
        f"get after unset must produce no stdout; got: {unset_result.stdout!r}"
    )


# ---------------------------------------------------------------------------
# List tests
# ---------------------------------------------------------------------------


def test_list_plain_contains_key_and_value(
    grim_at: object,
    project_dir: Path,
) -> None:
    """``list`` in plain mode shows all effective key=value pairs.

    After setting ``options.clients``, ``grim config list`` must include
    the key name and its value in stdout.  Exit must be 0.

    Traces to ADR: "list: plain ``key=value`` lines; one table per
    invocation".
    """
    write_config(project_dir)
    runner: GrimRunner = grim_at(project_dir)  # type: ignore[call-arg]

    runner.run("config", "set", "options.clients", "claude")

    result = runner.plain("config", "list")
    assert result.returncode == 0, (
        f"list must exit 0; got {result.returncode}; "
        f"stderr: {result.stderr.strip()}"
    )
    assert "options.clients" in result.stdout, (
        f"list output must contain the key name; got:\n{result.stdout}"
    )
    assert "claude" in result.stdout, (
        f"list output must contain the value; got:\n{result.stdout}"
    )


# ---------------------------------------------------------------------------
# JSON output shape tests
# ---------------------------------------------------------------------------


def test_set_json_write_confirmation_carries_action_key_value_scope(
    grim_at: object,
    project_dir: Path,
) -> None:
    """``set`` with ``--format json`` returns a write-confirmation object
    with ``action``, ``key``, ``value``, and ``scope`` fields.

    Traces to ADR: ConfigWriteReport JSON shape
    ``{"action":"…","key":"…","value":"…","scope":"…"}``.
    """
    write_config(project_dir)
    runner: GrimRunner = grim_at(project_dir)  # type: ignore[call-arg]

    # runner.json() fails against stub (binary panics) → test fails.
    # After implementation it returns the write-confirmation object.
    result = runner.json("config", "set", "options.clients", "claude")

    assert "action" in result, (
        f"write-confirmation JSON must have 'action' field; got: {result!r}"
    )
    assert result.get("key") == "options.clients", (
        f"'key' field must be the dotted key; got: {result.get('key')!r}"
    )
    assert "claude" in str(result.get("value", "")), (
        f"'value' field must contain the new value; got: {result.get('value')!r}"
    )
    assert result.get("scope") == "project", (
        f"'scope' field must be 'project' for project-scope set; "
        f"got: {result.get('scope')!r}"
    )


def test_get_json_format_when_key_is_set(
    grim_at: object,
    project_dir: Path,
) -> None:
    """``get --format json`` returns an object with ``key``, ``value``,
    ``set``, and ``scope`` when the key is set.

    Traces to ADR / F1/W1: ConfigGetReport JSON shape
    ``{"key":"…","value":"…","set":true,"scope":"…"}``.
    """
    write_config(project_dir)
    runner: GrimRunner = grim_at(project_dir)  # type: ignore[call-arg]

    runner.run("config", "set", "options.clients", "opencode")

    result = runner.json("config", "get", "options.clients")

    assert result.get("key") == "options.clients", (
        f"JSON get must carry the queried key; got: {result!r}"
    )
    value = result.get("value")
    assert value is not None and "opencode" in str(value), (
        f"JSON get must carry the value when set; got value={value!r}"
    )
    assert result.get("set") is True, (
        f"JSON get must have 'set': true when key is set; got: {result!r}"
    )
    assert result.get("scope") == "project", (
        f"JSON get must have 'scope': 'project' for project-scope get; "
        f"got: {result!r}"
    )


def test_list_json_format_is_parseable_array(
    grim_at: object,
    project_dir: Path,
) -> None:
    """``list --format json`` returns a JSON array of key/value entry objects.

    Traces to ADR / ConfigListReport doc: JSON format is an array of
    ``{"key":"…","value":"…"}`` objects (not wrapped in a parent object).
    """
    write_config(project_dir)
    runner: GrimRunner = grim_at(project_dir)  # type: ignore[call-arg]

    runner.run("config", "set", "options.clients", "claude")

    result = runner.json("config", "list")

    assert isinstance(result, list), (
        f"list --format json must return a JSON array; got: {type(result)}"
    )
    keys = [e.get("key") for e in result if isinstance(e, dict)]
    assert "options.clients" in keys, (
        f"JSON list must contain 'options.clients' entry; keys: {keys}"
    )


# ---------------------------------------------------------------------------
# Scope isolation test
# ---------------------------------------------------------------------------


def test_global_flag_writes_grim_home_config_not_project_config(
    grim_at: object,
    project_dir: Path,
    grim_home: Path,
) -> None:
    """``--global`` writes to ``$GRIM_HOME/grimoire.toml``, never the project.

    Setting a value at global scope must not appear in the project config.
    The project and global configs are distinct files, never merged.

    Traces to ADR: "Two scopes, **never merged**"; "``--global`` selects
    ``$GRIM_HOME/grimoire.toml``".
    """
    write_config(project_dir)
    _minimal_global_config(grim_home)
    runner: GrimRunner = grim_at(project_dir)  # type: ignore[call-arg]

    runner.run("config", "--global", "set", "options.clients", "opencode")

    # Global config must contain the new value.
    global_cfg = (grim_home / "grimoire.toml").read_text()
    assert "opencode" in global_cfg, (
        f"$GRIM_HOME/grimoire.toml must contain the globally-set value; "
        f"got:\n{global_cfg}"
    )

    # Project config must be unchanged — it must NOT contain "opencode".
    project_cfg = (project_dir / "grimoire.toml").read_text()
    assert "opencode" not in project_cfg, (
        f"project grimoire.toml must not be modified by --global set; "
        f"got:\n{project_cfg}"
    )


# ---------------------------------------------------------------------------
# D2: group_by_type coverage
# ---------------------------------------------------------------------------


def test_group_by_type_set_get_unset(
    grim_at: object,
    project_dir: Path,
) -> None:
    """``set options.tui.group_by_type true`` → ``get`` exits 0 with 'true';
    ``unset`` → subsequent ``get`` exits 1 (treated as unset when false).

    Traces to ADR / F2/D1: ``group_by_type`` returns ``None`` when ``false``
    so ``get`` and ``list`` treat it the same as an absent key.
    """
    write_config(project_dir)
    runner: GrimRunner = grim_at(project_dir)  # type: ignore[call-arg]

    runner.run("config", "set", "options.tui.group_by_type", "true")

    result = runner.plain("config", "get", "options.tui.group_by_type")
    assert result.returncode == 0, (
        f"get of set group_by_type must exit 0; got {result.returncode}"
    )
    assert "true" in result.stdout, (
        f"get must return 'true'; got: {result.stdout!r}"
    )

    runner.run("config", "unset", "options.tui.group_by_type")

    after = runner.run("config", "get", "options.tui.group_by_type", check=False)
    assert after.returncode == 1, (
        f"get of unset group_by_type must exit 1; got {after.returncode}"
    )
    assert after.stdout.strip() == "", (
        f"get after unset must produce no stdout; got: {after.stdout!r}"
    )


# ---------------------------------------------------------------------------
# D2: tree_separators round-trip
# ---------------------------------------------------------------------------


def test_tree_separators_set_get_round_trip(
    grim_at: object,
    project_dir: Path,
) -> None:
    """``set options.tui.tree_separators /,-`` → ``get`` returns the value.

    Traces to ADR / D2: tree_separators round-trip via config get/set.
    """
    write_config(project_dir)
    runner: GrimRunner = grim_at(project_dir)  # type: ignore[call-arg]

    runner.run("config", "set", "options.tui.tree_separators", "/,-")

    result = runner.plain("config", "get", "options.tui.tree_separators")
    assert result.returncode == 0, (
        f"get of set tree_separators must exit 0; got {result.returncode}"
    )
    assert "/" in result.stdout, (
        f"get must include '/'; got: {result.stdout!r}"
    )
    assert "-" in result.stdout, (
        f"get must include '-'; got: {result.stdout!r}"
    )


# ---------------------------------------------------------------------------
# FIX 3: empty/whitespace client segment rejected (exit 65)
# ---------------------------------------------------------------------------


def test_set_clients_empty_segment_exits_65(
    grim_at: object,
    project_dir: Path,
) -> None:
    """``config set options.clients`` with an empty segment exits 65 (DataError).

    ``claude, ,opencode`` splits on ``,`` and trims to ``['claude', '', 'opencode']``.
    The empty segment must be rejected before writing so the config never
    holds a blank client name that silently installs nothing.

    Traces to FIX 3: reject any empty/whitespace-only segment → exit 65.
    """
    write_config(project_dir)
    runner: GrimRunner = grim_at(project_dir)  # type: ignore[call-arg]

    result = runner.run(
        "config", "set", "options.clients", "claude, ,opencode",
        check=False,
    )
    assert result.returncode == 65, (
        f"empty segment in clients must exit 65 (DataError); "
        f"got {result.returncode}; stderr: {result.stderr.strip()}"
    )


# ---------------------------------------------------------------------------
# FIX A regression: zero-width separator rejected at CLI (no lockout)
# ---------------------------------------------------------------------------


def test_set_tree_separators_zero_width_char_exits_65(
    grim_at: object,
    project_dir: Path,
) -> None:
    """``config set options.tui.tree_separators`` with U+200B exits 65 (DataError).

    U+200B ZERO WIDTH SPACE passes the single-char and control/whitespace checks
    but has display width 0. Before FIX A the CLI accepted it, wrote the config,
    and every subsequent ``grim`` invocation failed with ConfigError 78 — with
    no CLI recovery path because ``config unset`` also reads the config first.

    The fix mirrors the load-time ``validate_tree_separators`` check, so the
    CLI parser and the loader accept exactly the same set.

    Traces to FIX A: parse_tree_separators mirrors validate_tree_separators.
    """
    write_config(project_dir)
    runner: GrimRunner = grim_at(project_dir)  # type: ignore[call-arg]

    zwsp = "​"  # U+200B ZERO WIDTH SPACE — invisible, display width 0
    result = runner.run(
        "config", "set", "options.tui.tree_separators", zwsp,
        check=False,
    )
    assert result.returncode == 65, (
        f"zero-width separator must exit 65 (DataError); "
        f"got {result.returncode}; stderr: {result.stderr.strip()}"
    )
    # Config must not have been written with the bad separator.
    cfg_path = project_dir / "grimoire.toml"
    if cfg_path.exists():
        with cfg_path.open("rb") as f:
            data = tomllib.load(f)
        seps = data.get("options", {}).get("tui", {}).get("tree_separators", [])
        assert zwsp not in seps, (
            f"zero-width char must not be written to config; got seps={seps!r}"
        )


# ---------------------------------------------------------------------------
# D2: list on empty config
# ---------------------------------------------------------------------------


def test_list_on_empty_config_exits_0_with_empty_output(
    grim_at: object,
    project_dir: Path,
) -> None:
    """``config list`` on a config with no options set exits 0 with no rows.

    Traces to ADR / D2: list on empty config → exit 0, empty.
    """
    write_config(project_dir)  # minimal config, no [options] table
    runner: GrimRunner = grim_at(project_dir)  # type: ignore[call-arg]

    result = runner.json("config", "list")

    assert result == [], f"list on empty config must be empty array; got {result!r}"
    assert len(result) == 0, (
        f"list on empty config must have zero entries; got: {result!r}"
    )


# ---------------------------------------------------------------------------
# F4: Concurrency smoke
# ---------------------------------------------------------------------------


def test_concurrent_config_set_produces_valid_toml(
    grim_at: object,
    project_dir: Path,
    grim_binary: Path,
    grim_home: Path,
) -> None:
    """Two simultaneous ``config set`` calls leave ``grimoire.toml`` valid.

    Spawns two subprocesses in parallel; after both finish asserts the
    config file is parseable TOML and contains at least one of the two
    expected values (last-writer-wins is acceptable, but the file must
    never be corrupted).

    Traces to ADR / F4: concurrency smoke — file-lock prevents partial-write
    corruption; ``ConfigFileLock`` must ensure at-most-one writer at a time.
    """
    import os

    write_config(project_dir)
    runner: GrimRunner = grim_at(project_dir)  # type: ignore[call-arg]

    errors: list[str] = []

    # Exit codes: 0 = success, 75 = TempFail (lock contention — expected when
    # the other writer holds the advisory flock). Any other non-zero is a failure.
    LOCK_CONTENTION = 75
    successes: list[str] = []
    hard_errors: list[str] = []

    def run_set(key: str, value: str) -> None:
        cmd = [str(grim_binary), "config", "set", key, value]
        r = subprocess.run(
            cmd,
            capture_output=True,
            text=True,
            env=runner.env,
            cwd=str(project_dir),
        )
        if r.returncode == 0:
            successes.append(f"{key}={value}")
        elif r.returncode != LOCK_CONTENTION:
            hard_errors.append(
                f"{key}={value}: rc={r.returncode} stderr={r.stderr.strip()}"
            )

    t1 = threading.Thread(target=run_set, args=("options.default_registry", "ghcr.io/a"))
    t2 = threading.Thread(target=run_set, args=("options.clients", "claude"))
    t1.start()
    t2.start()
    t1.join()
    t2.join()

    assert not hard_errors, f"concurrent set commands failed unexpectedly: {hard_errors}"

    # The file must be valid TOML regardless of which writer won.
    cfg_text = (project_dir / "grimoire.toml").read_text()
    try:
        parsed = tomllib.loads(cfg_text)
    except tomllib.TOMLDecodeError as exc:
        raise AssertionError(
            f"grimoire.toml is not valid TOML after concurrent writes:\n{cfg_text}\nError: {exc}"
        ) from exc

    # At least one writer must have succeeded (lock contention = one winner).
    assert successes, "at least one concurrent set must have succeeded"
    options = parsed.get("options", {})
    has_registry = options.get("default_registry") == "ghcr.io/a"
    has_clients = options.get("clients") == ["claude"]
    assert has_registry or has_clients, (
        f"the winning writer's value must appear in the config; options={options!r}"
    )
