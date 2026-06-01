# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The Grimoire Authors
"""Acceptance tests for ``grim login`` / ``grim logout``.

Every test isolates the docker config into a per-test ``DOCKER_CONFIG``
tempdir — the user's real ``~/.docker`` is never touched. Helper-backed
tests drop a ``docker-credential-test`` Python script onto a tempdir,
prepend it to ``PATH``, and point ``credsStore`` at ``test``.

Exit codes follow ``quality-rust-exit_codes.md`` (sysexits-aligned):
usage 64, config 78, success 0.
"""
from __future__ import annotations

import base64
import json
import os
import stat
import subprocess
import sys
from pathlib import Path

import pytest

from src.runner import GrimRunner

# ---------------------------------------------------------------------------
# Fixtures
# ---------------------------------------------------------------------------


@pytest.fixture()
def docker_config(tmp_path: Path) -> Path:
    """An isolated ``$DOCKER_CONFIG`` directory."""
    d = tmp_path / "docker"
    d.mkdir()
    return d


_MOCK_HELPER = """\
#!/usr/bin/env python3
import json, os, sys

store = os.environ["DOCKER_CREDENTIAL_TEST_STORE"]


def load():
    try:
        with open(store) as fh:
            return json.load(fh)
    except FileNotFoundError:
        return {}


def save(data):
    with open(store, "w") as fh:
        json.dump(data, fh)


action = sys.argv[1] if len(sys.argv) > 1 else ""
data = load()

if action == "store":
    req = json.load(sys.stdin)
    data[req["ServerURL"]] = {"Username": req["Username"], "Secret": req["Secret"]}
    save(data)
elif action == "get":
    server = sys.stdin.read().strip()
    entry = data.get(server)
    if entry is None:
        print("credentials not found in native keychain")
        sys.exit(1)
    print(json.dumps({"ServerURL": server, "Username": entry["Username"], "Secret": entry["Secret"]}))
elif action == "erase":
    server = sys.stdin.read().strip()
    data.pop(server, None)
    save(data)
elif action == "list":
    print(json.dumps({k: v["Username"] for k, v in data.items()}))
else:
    sys.exit(2)
"""


@pytest.fixture()
def credential_helper(tmp_path: Path, docker_config: Path) -> dict[str, str]:
    """A ``docker-credential-test`` helper on PATH + a ``credsStore`` config.

    Returns the extra environment the runner needs and writes the seed
    config.json. The helper persists credentials to a JSON file named by
    ``DOCKER_CREDENTIAL_TEST_STORE``.
    """
    bin_dir = tmp_path / "helper-bin"
    bin_dir.mkdir()
    helper = bin_dir / "docker-credential-test"
    helper.write_text(_MOCK_HELPER)
    helper.chmod(helper.stat().st_mode | stat.S_IEXEC | stat.S_IXGRP | stat.S_IXOTH)

    (docker_config / "config.json").write_text(json.dumps({"credsStore": "test"}))

    store_file = tmp_path / "helper-store.json"
    return {
        "PATH": f"{bin_dir}{os.pathsep}{os.environ.get('PATH', '')}",
        "DOCKER_CREDENTIAL_TEST_STORE": str(store_file),
        "_STORE_FILE": str(store_file),
    }


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _login(
    grim: GrimRunner,
    *args: str,
    docker_config: Path,
    stdin: str | None = None,
    extra_env: dict[str, str] | None = None,
    fmt: str | None = None,
) -> subprocess.CompletedProcess[str]:
    env = dict(grim.env)
    env["DOCKER_CONFIG"] = str(docker_config)
    if extra_env:
        env.update({k: v for k, v in extra_env.items() if not k.startswith("_")})
    cmd = [str(grim.binary)]
    if fmt:
        cmd += ["--format", fmt]
    cmd += ["login", *args]
    return subprocess.run(
        cmd,
        input=stdin,
        stdin=subprocess.DEVNULL if stdin is None else None,
        capture_output=True,
        text=True,
        env=env,
    )


def _logout(
    grim: GrimRunner,
    *args: str,
    docker_config: Path,
    extra_env: dict[str, str] | None = None,
) -> subprocess.CompletedProcess[str]:
    env = dict(grim.env)
    env["DOCKER_CONFIG"] = str(docker_config)
    if extra_env:
        env.update({k: v for k, v in extra_env.items() if not k.startswith("_")})
    return subprocess.run(
        [str(grim.binary), "logout", *args],
        stdin=subprocess.DEVNULL,
        capture_output=True,
        text=True,
        env=env,
    )


def _read_config(docker_config: Path) -> dict:
    path = docker_config / "config.json"
    return json.loads(path.read_text()) if path.exists() else {}


# ---------------------------------------------------------------------------
# Plaintext-store path (no native helper)
# ---------------------------------------------------------------------------


def test_login_plaintext_writes_base64_entry(grim: GrimRunner, docker_config: Path) -> None:
    res = _login(
        grim,
        "-u", "alice", "--password-stdin", "--allow-insecure-store", "ghcr.io",
        docker_config=docker_config,
        stdin="hunter2\n",
    )
    assert res.returncode == 0, res.stderr
    cfg = _read_config(docker_config)
    auth = cfg["auths"]["ghcr.io"]["auth"]
    assert base64.b64decode(auth).decode() == "alice:hunter2"


@pytest.mark.skipif(sys.platform == "win32", reason="POSIX file mode check")
def test_login_plaintext_config_is_owner_only(grim: GrimRunner, docker_config: Path) -> None:
    _login(
        grim,
        "-u", "u", "--password-stdin", "--allow-insecure-store", "ghcr.io",
        docker_config=docker_config,
        stdin="p\n",
    )
    mode = (docker_config / "config.json").stat().st_mode & 0o777
    assert mode == 0o600, f"credentials file must be 0600, got {oct(mode)}"


def test_login_refused_without_helper_or_optin(grim: GrimRunner, docker_config: Path) -> None:
    res = _login(
        grim,
        "-u", "u", "--password-stdin", "ghcr.io",
        docker_config=docker_config,
        stdin="p\n",
    )
    assert res.returncode == 78, res.stderr  # ConfigError
    assert "allow-insecure-store" in res.stderr or "credential helper" in res.stderr


def test_login_canonicalizes_registry_key(grim: GrimRunner, docker_config: Path) -> None:
    res = _login(
        grim,
        "-u", "u", "--password-stdin", "--allow-insecure-store", "https://ghcr.io/v1/",
        docker_config=docker_config,
        stdin="p\n",
    )
    assert res.returncode == 0, res.stderr
    cfg = _read_config(docker_config)
    assert "ghcr.io" in cfg["auths"], cfg


# ---------------------------------------------------------------------------
# Usage / input errors
# ---------------------------------------------------------------------------


def test_login_noninteractive_requires_password_stdin(grim: GrimRunner, docker_config: Path) -> None:
    # No --password-stdin and stdin is /dev/null (not a TTY) → usage error.
    res = _login(grim, "-u", "u", "--allow-insecure-store", "ghcr.io", docker_config=docker_config)
    assert res.returncode == 64, res.stderr  # UsageError
    assert "--password-stdin" in res.stderr


def test_login_rejects_password_value_flag(grim: GrimRunner, docker_config: Path) -> None:
    # CWE-214: there is no --password VALUE flag; clap rejects it at parse.
    res = _login(grim, "--password", "secret", "ghcr.io", docker_config=docker_config)
    assert res.returncode == 64, res.stderr


def test_login_empty_password_stdin_is_usage_error(grim: GrimRunner, docker_config: Path) -> None:
    res = _login(
        grim,
        "-u", "u", "--password-stdin", "--allow-insecure-store", "ghcr.io",
        docker_config=docker_config,
        stdin="",
    )
    assert res.returncode == 64, res.stderr


# ---------------------------------------------------------------------------
# JSON output
# ---------------------------------------------------------------------------


def test_login_json_output(grim: GrimRunner, docker_config: Path) -> None:
    res = _login(
        grim,
        "-u", "alice", "--password-stdin", "--allow-insecure-store", "ghcr.io",
        docker_config=docker_config,
        stdin="p\n",
        fmt="json",
    )
    assert res.returncode == 0, res.stderr
    payload = json.loads(res.stdout)
    assert payload == {"registry": "ghcr.io", "username": "alice"}


# ---------------------------------------------------------------------------
# Logout
# ---------------------------------------------------------------------------


def test_logout_removes_plaintext_entry(grim: GrimRunner, docker_config: Path) -> None:
    _login(
        grim,
        "-u", "u", "--password-stdin", "--allow-insecure-store", "ghcr.io",
        docker_config=docker_config,
        stdin="p\n",
    )
    res = _logout(grim, "ghcr.io", docker_config=docker_config)
    assert res.returncode == 0, res.stderr
    assert "ghcr.io" not in _read_config(docker_config).get("auths", {})


def test_logout_noop_when_nothing_stored(grim: GrimRunner, docker_config: Path) -> None:
    res = _logout(grim, "ghcr.io", docker_config=docker_config)
    assert res.returncode == 0, res.stderr


# ---------------------------------------------------------------------------
# Native-helper path (mock docker-credential-test)
# ---------------------------------------------------------------------------


@pytest.mark.skipif(sys.platform == "win32", reason="mock helper is a POSIX script")
def test_login_via_helper_stores_credential(grim: GrimRunner, docker_config: Path, credential_helper: dict) -> None:
    res = _login(
        grim,
        "-u", "alice", "--password-stdin", "ghcr.io",
        docker_config=docker_config,
        stdin="s3cret\n",
        extra_env=credential_helper,
    )
    assert res.returncode == 0, res.stderr
    # Credential landed in the helper's backing store, NOT in plaintext auths.
    store = json.loads(Path(credential_helper["_STORE_FILE"]).read_text())
    assert store["ghcr.io"] == {"Username": "alice", "Secret": "s3cret"}
    assert "auths" not in _read_config(docker_config)


@pytest.mark.skipif(sys.platform == "win32", reason="mock helper is a POSIX script")
def test_logout_via_helper_erases_credential(grim: GrimRunner, docker_config: Path, credential_helper: dict) -> None:
    _login(
        grim,
        "-u", "alice", "--password-stdin", "ghcr.io",
        docker_config=docker_config,
        stdin="s3cret\n",
        extra_env=credential_helper,
    )
    res = _logout(grim, "ghcr.io", docker_config=docker_config, extra_env=credential_helper)
    assert res.returncode == 0, res.stderr
    store = json.loads(Path(credential_helper["_STORE_FILE"]).read_text())
    assert "ghcr.io" not in store
