# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The Grimoire Authors
"""`grim add` default-registry acceptance tests.

The default registry is a pure CLI-input convenience: short references are
expanded against it, but the resolved fully-qualified name (registry host
included) is what gets persisted in both ``grimoire.toml`` and
``grimoire.lock``.  Two resolution sources are tested:

1. ``GRIM_DEFAULT_REGISTRY`` environment variable.
2. ``[options].default_registry`` in the project config.
"""
from __future__ import annotations

from pathlib import Path

from src.helpers import make_artifact, write_config
from src.registry import REGISTRY_HOST


def test_add_env_default_registry_persists_fq_name(
    grim_at, project_dir: Path, registry: str, unique_repo: str
) -> None:
    """Env-sourced default registry: config+lock carry the fully-qualified name."""
    sk = make_artifact(
        f"{unique_repo}/code-review",
        "skill",
        {"code-review/SKILL.md": "---\nname: code-review\ndescription: d\n---\n# CR\n"},
        tag="stable",
    )
    write_config(project_dir)
    runner = grim_at(project_dir)
    # Inject the default registry via env; use a SHORT reference (no host).
    runner.env["GRIM_DEFAULT_REGISTRY"] = REGISTRY_HOST

    short_ref = f"{unique_repo}/code-review:stable"
    out = runner.json("add", short_ref)
    assert out["kind"] == "skill"
    assert out["name"] == "code-review"
    assert out["status"] == "added"

    # Both config and lock must persist the FULLY-QUALIFIED name (host present).
    cfg_text = (project_dir / "grimoire.toml").read_text()
    assert f"{REGISTRY_HOST}/" in cfg_text, (
        f"grimoire.toml must contain the registry host '{REGISTRY_HOST}/', "
        f"got:\n{cfg_text}"
    )

    lock_text = (project_dir / "grimoire.lock").read_text()
    assert f"{REGISTRY_HOST}/" in lock_text, (
        f"grimoire.lock must contain the registry host '{REGISTRY_HOST}/', "
        f"got:\n{lock_text}"
    )


def test_add_env_default_registry_beats_config_default(
    grim_at, project_dir: Path, registry: str, unique_repo: str
) -> None:
    """Registry precedence: ``GRIM_DEFAULT_REGISTRY`` wins over the config
    ``[options].default_registry``.

    The config declares a bogus host; the env names the real registry. The
    short reference must expand against the env value (so resolution succeeds
    and the persisted FQ name carries the real host), proving env beats config
    in the reordered precedence chain.
    """
    sk = make_artifact(
        f"{unique_repo}/code-review",
        "skill",
        {"code-review/SKILL.md": "---\nname: code-review\ndescription: d\n---\n# CR\n"},
        tag="stable",
    )
    wrong_host = "wrong-registry.invalid:5000"
    cfg_path = project_dir / "grimoire.toml"
    cfg_path.write_text(
        f'[options]\ndefault_registry = "{wrong_host}"\n\n[skills]\n\n[rules]\n'
    )

    runner = grim_at(project_dir)
    # The env names the REAL registry; it must win over the config default.
    runner.env["GRIM_DEFAULT_REGISTRY"] = REGISTRY_HOST

    short_ref = f"{unique_repo}/code-review:stable"
    out = runner.json("add", short_ref)
    assert out["kind"] == "skill"
    assert out["name"] == "code-review"
    assert out["status"] == "added"

    # The resolved skill binding must expand against the env (real) host,
    # proving env beats config. The bogus `default_registry` line still
    # round-trips in `[options]` (add preserves options), so assert on the
    # skill ENTRY line, not the whole file.
    cfg_text = (project_dir / "grimoire.toml").read_text()
    skill_line = next(
        (line for line in cfg_text.splitlines() if line.startswith("code-review")),
        "",
    )
    assert f"{REGISTRY_HOST}/" in skill_line, (
        f"the skill binding must use the env registry host '{REGISTRY_HOST}/', "
        f"got skill line: {skill_line!r}\nfull config:\n{cfg_text}"
    )
    assert wrong_host not in skill_line, (
        f"the bogus config registry '{wrong_host}' must not win on the skill "
        f"binding, got skill line: {skill_line!r}"
    )

    # The lock must record the env (real) host, and never the bogus one.
    lock_text = (project_dir / "grimoire.lock").read_text()
    assert f"{REGISTRY_HOST}/" in lock_text, (
        f"grimoire.lock must use the env registry host '{REGISTRY_HOST}/', got:\n{lock_text}"
    )
    assert wrong_host not in lock_text, (
        f"the bogus config registry '{wrong_host}' must not appear in the lock, got:\n{lock_text}"
    )


def test_add_config_default_registry_persists_fq_name(
    grim_at, project_dir: Path, registry: str, unique_repo: str
) -> None:
    """Config-sourced default registry: config+lock carry the fully-qualified name.

    ``GRIM_DEFAULT_REGISTRY`` is NOT set; only the ``[options].default_registry``
    entry in ``grimoire.toml`` provides the default.
    """
    sk = make_artifact(
        f"{unique_repo}/code-review",
        "skill",
        {"code-review/SKILL.md": "---\nname: code-review\ndescription: d\n---\n# CR\n"},
        tag="stable",
    )
    # Write a grimoire.toml with [options].default_registry directly; the
    # write_config helper does not emit [options], so we write it manually.
    cfg_path = project_dir / "grimoire.toml"
    cfg_path.write_text(
        f'[options]\ndefault_registry = "{REGISTRY_HOST}"\n\n[skills]\n\n[rules]\n'
    )

    runner = grim_at(project_dir)
    # Deliberately do NOT set GRIM_DEFAULT_REGISTRY so the config option
    # is the only source.
    runner.env.pop("GRIM_DEFAULT_REGISTRY", None)

    short_ref = f"{unique_repo}/code-review:stable"
    out = runner.json("add", short_ref)
    assert out["kind"] == "skill"
    assert out["name"] == "code-review"
    assert out["status"] == "added"

    # Config after `grim add` re-serialises the declared set (registry host
    # must be in the skill entry).
    cfg_text = (project_dir / "grimoire.toml").read_text()
    assert f"{REGISTRY_HOST}/" in cfg_text, (
        f"grimoire.toml must contain the registry host '{REGISTRY_HOST}/', "
        f"got:\n{cfg_text}"
    )

    lock_text = (project_dir / "grimoire.lock").read_text()
    assert f"{REGISTRY_HOST}/" in lock_text, (
        f"grimoire.lock must contain the registry host '{REGISTRY_HOST}/', "
        f"got:\n{lock_text}"
    )
