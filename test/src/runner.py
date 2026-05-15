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
    host state into ``grim``.
    """

    def __init__(self, binary: Path, grim_home: Path):
        self.binary = binary
        self.grim_home = grim_home
        self.env: dict[str, str] = {
            "GRIM_HOME": str(grim_home),
            "PATH": os.environ.get("PATH", ""),
            "HOME": os.environ.get("HOME", str(Path.home())),
        }
        # Windows needs these for subprocess spawning and executable resolution
        for key in ("SYSTEMROOT", "TEMP", "TMP", "PATHEXT"):
            if key in os.environ:
                self.env[key] = os.environ[key]

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
