# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The Grimoire Authors
"""Black-box smoke tests for the ``grim`` CLI.

These exercise the universal CLI contract that any clap-based binary must
honor, independent of Grimoire's domain features:

- ``grim --version`` prints a version string and exits 0
- ``grim --help`` prints usage and exits 0
- ``grim help`` (subcommand form) prints usage and exits 0
- bare ``grim`` exits cleanly
- an unknown subcommand is rejected with a non-zero exit code

They are intentionally lenient about exact wording so they keep passing
against a minimal scaffold while still catching real regressions.
"""
from __future__ import annotations

import re
import subprocess

from src.runner import GrimRunner


def test_version_prints_semver(grim: GrimRunner) -> None:
    """``grim --version`` exits 0 and prints a MAJOR.MINOR.PATCH string."""
    result = grim.run("--version", check=False)

    assert result.returncode == 0, (
        f"`grim --version` should exit 0, got rc={result.returncode}; "
        f"stderr={result.stderr!r}"
    )
    # clap's default --version output is "<bin> <semver>"; just require a
    # semver-shaped token to appear somewhere in stdout.
    assert re.search(r"\d+\.\d+\.\d+", result.stdout), (
        f"stdout should contain a MAJOR.MINOR.PATCH version, "
        f"got {result.stdout!r}"
    )


def test_help_prints_usage(grim: GrimRunner) -> None:
    """``grim --help`` exits 0 and prints a usage section."""
    result = grim.run("--help", check=False)

    assert result.returncode == 0, (
        f"`grim --help` should exit 0, got rc={result.returncode}; "
        f"stderr={result.stderr!r}"
    )
    assert "Usage:" in result.stdout, (
        f"`grim --help` should print a 'Usage:' section, "
        f"got stdout={result.stdout!r}"
    )


def test_help_subcommand_prints_usage(grim: GrimRunner) -> None:
    """``grim help`` (subcommand form) exits 0 and prints a usage section."""
    result = grim.run("help", check=False)

    assert result.returncode == 0, (
        f"`grim help` should exit 0, got rc={result.returncode}; "
        f"stderr={result.stderr!r}"
    )
    assert "Usage:" in result.stdout, (
        f"`grim help` should print a 'Usage:' section, "
        f"got stdout={result.stdout!r}"
    )


def test_unknown_subcommand_exits_nonzero(grim: GrimRunner) -> None:
    """An unknown subcommand is rejected with a non-zero exit code.

    A minimal clap app surfaces this as a usage error. Grimoire maps clap
    parse failures to sysexits ``EX_USAGE`` (64); we assert the strong
    non-zero contract and additionally the conventional 64 when present.
    """
    result = subprocess.run(
        [str(grim.binary), "definitely-not-a-real-subcommand"],
        capture_output=True,
        text=True,
        env=grim.env,
    )

    assert result.returncode != 0, (
        f"unknown subcommand must exit non-zero, got rc={result.returncode}; "
        f"stdout={result.stdout!r} stderr={result.stderr!r}"
    )
    assert result.returncode == 64, (
        f"unknown subcommand should map to EX_USAGE (64), "
        f"got {result.returncode}; stderr={result.stderr!r}"
    )


def test_bare_invocation_exits_cleanly(grim: GrimRunner) -> None:
    """Bare ``grim`` (no args) must not crash.

    clap apps with subcommands typically print help and exit 0 (or 2) when
    invoked with no arguments. We only require a clean, well-defined exit
    (0 or the clap usage code) and that *something* is written.
    """
    result = grim.run(check=False)

    assert result.returncode in (0, 2, 64), (
        f"bare `grim` should exit cleanly (0/2/64), got rc={result.returncode}; "
        f"stderr={result.stderr!r}"
    )
    assert (result.stdout + result.stderr).strip(), (
        "bare `grim` should print help or a usage hint, got empty output"
    )
