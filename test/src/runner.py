from __future__ import annotations

import json
import os
import platform
import subprocess
from pathlib import Path
from typing import Any


# ---------------------------------------------------------------------------
# Platform helpers
# ---------------------------------------------------------------------------

_ARCH_MAP = {
    "x86_64": "amd64",
    "amd64": "amd64",
    "aarch64": "arm64",
    "arm64": "arm64",
}


def current_platform() -> str:
    """Return the current platform as ``<os>/<arch>`` (e.g. ``linux/amd64``)."""
    system = platform.system().lower()
    machine = platform.machine().lower()
    arch = _ARCH_MAP.get(machine, machine)
    return f"{system}/{arch}"


# ---------------------------------------------------------------------------
# GrimRunner
# ---------------------------------------------------------------------------


class GrimRunner:
    """Wraps the ``grim`` binary with per-test environment isolation.

    Each instance carries its own minimal environment so tests never leak
    host state into ``grim``.  In particular, ``HOME`` is an isolated
    temporary directory (sibling of ``grim_home``, at ``<parent>/home``)
    so that global-scope installs that target vendor-native user-level
    discovery paths (``~/.claude/``, ``~/.config/opencode/``, etc.) write
    into the isolated tree and never touch the developer's real home
    directory.  The isolated home is exposed via the ``home`` property.
    ``XDG_CONFIG_HOME`` is set explicitly to ``<home>/.config`` for
    determinism (overrides any ambient value).
    """

    def __init__(self, binary: Path, grim_home: Path, cwd: Path | None = None):
        self.binary = binary
        self.grim_home = grim_home
        self.cwd = cwd
        # Isolated home: sibling of grim_home so it lives in the same
        # per-test tmp_path tree and is cleaned up automatically.
        self._home = grim_home.parent / "home"
        self._home.mkdir(parents=True, exist_ok=True)
        self.env: dict[str, str] = {
            "GRIM_HOME": str(grim_home),
            "PATH": os.environ.get("PATH", ""),
            "HOME": str(self._home),
            "XDG_CONFIG_HOME": str(self._home / ".config"),
        }
        # Windows needs these for subprocess spawning and executable resolution
        for key in ("SYSTEMROOT", "TEMP", "TMP", "PATHEXT"):
            if key in os.environ:
                self.env[key] = os.environ[key]
        # Propagate the test registry host as an insecure-registry exception when
        # it differs from the built-in defaults (grim only allows plain HTTP on
        # localhost and localhost:5000 by default; any other port needs an explicit
        # GRIM_INSECURE_REGISTRIES entry so grim does not attempt HTTPS).
        _test_host = os.environ.get("GRIM_TEST_REGISTRY_HOST", "")
        _builtin_http = {"localhost", "localhost:5000", "127.0.0.1", "127.0.0.1:5000"}
        if _test_host and _test_host not in _builtin_http:
            self.env["GRIM_INSECURE_REGISTRIES"] = _test_host

    @property
    def home(self) -> Path:
        """The isolated home directory for this runner instance.

        Global-scope installs write vendor-native artifacts here (e.g.
        ``~/.claude/skills/``, ``~/.config/opencode/``).  Tests assert
        against this path instead of the real ``$HOME``.
        """
        return self._home

    def run(
        self,
        *args: str,
        format: str | None = None,
        check: bool = True,
        log_level: str | None = None,
    ) -> subprocess.CompletedProcess[str]:
        """Run ``grim`` with the given arguments."""
        cmd: list[str] = [str(self.binary)]
        if format:
            cmd += ["--format", format]
        if log_level:
            cmd += ["--log-level", log_level]
        cmd += list(args)
        result = subprocess.run(
            cmd,
            capture_output=True,
            text=True,
            env=self.env,
            cwd=str(self.cwd) if self.cwd else None,
        )
        if check and result.returncode != 0:
            raise AssertionError(
                f"grim {' '.join(args)} failed (rc={result.returncode})\n"
                f"stderr: {result.stderr.strip()}"
            )
        return result

    def json(self, *args: str, **kwargs: Any) -> Any:
        """Run ``grim`` and parse stdout as JSON."""
        result = self.run(*args, format="json", **kwargs)
        return json.loads(result.stdout)

    def plain(self, *args: str, **kwargs: Any) -> subprocess.CompletedProcess[str]:
        """Run ``grim`` without ``--format`` (plain text output)."""
        return self.run(*args, format=None, **kwargs)
