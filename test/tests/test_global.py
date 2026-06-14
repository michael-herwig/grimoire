# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The Grimoire Authors
"""`--global` scope acceptance tests.

The global scope operates on ``$GRIM_HOME/grimoire.toml`` and its own
lock, fully independent of any project config (the two are never merged).

Global installs land in vendor-native user-level discovery paths:
- Claude:    skills → ``$HOME/.claude/skills/<name>/``
             rules  → ``$HOME/.claude/rules/<name>.md``
- OpenCode:  skills → ``$XDG_CONFIG_HOME/opencode/skills/<name>/`` (default ``$HOME/.config/opencode/skills/``)
             rules  → ``$GRIM_HOME/.opencode/rules/<name>.md`` (loaded via absolute glob in
                       ``$XDG_CONFIG_HOME/opencode/opencode.json``)
- Copilot:   skills → ``$HOME/.copilot/skills/<name>/``
             rules  → ``$GRIM_HOME/.github/instructions/<name>.instructions.md`` (inert for Copilot)

Vendor env-var overrides (tested at the bottom of this file):
- ``CLAUDE_CONFIG_DIR`` replaces the entire ``~/.claude`` tree (skills + rules)
- ``OPENCODE_CONFIG_DIR`` is the preferred skills install target over the XDG default
- ``COPILOT_HOME`` replaces ``~/.copilot`` for Copilot skills
- ``OPENCODE_CONFIG`` is the global ``opencode.json`` edit target (file path)
- ``XDG_CONFIG_HOME`` drives the OpenCode skills root and config location
- empty values are treated as unset
"""
from __future__ import annotations

import json
from pathlib import Path

from src.helpers import make_artifact
from src.registry import retag
from src.runner import GrimRunner


def test_global_scope_is_independent_of_project(
    grim_binary: Path, grim_home: Path, registry: str, unique_repo: str
) -> None:
    repo = f"{unique_repo}/global-rule"
    ru = make_artifact(
        repo,
        "rule",
        {"global-rule.md": "---\npaths: ['**']\n---\n# global\n"},
        tag="v1",
    )
    # Global config under $GRIM_HOME, no project config anywhere.
    (grim_home / "grimoire.toml").write_text(
        f'[rules]\nglobal-rule = "{ru.fq}"\n'
    )
    runner = GrimRunner(grim_binary, grim_home)

    lock_rows = runner.json("lock", "--global")
    assert lock_rows[0]["name"] == "global-rule"
    assert (grim_home / "grimoire.lock").is_file()
    assert "@sha256:" in (grim_home / "grimoire.lock").read_text()

    install_rows = runner.json("install", "--global")
    assert install_rows[0]["status"] == "installed"
    # Global Claude rules land in the vendor-native ~/.claude/rules/ path,
    # not under $GRIM_HOME/.claude/.
    assert (runner.home / ".claude/rules/global-rule.md").is_file(), (
        "global Claude rule must materialize in $HOME/.claude/rules/"
    )
    assert not (grim_home / ".claude/rules/global-rule.md").exists(), (
        "global rule must NOT land under $GRIM_HOME/.claude/ (old layout)"
    )

    status_rows = runner.json("status", "--global")
    assert status_rows[0]["state"] == "installed"


def test_global_install_without_lock_exits_79(
    grim_binary: Path, grim_home: Path, registry: str, unique_repo: str
) -> None:
    repo = f"{unique_repo}/r"
    ru = make_artifact(repo, "rule", {"r.md": "# r\n"}, tag="v1")
    (grim_home / "grimoire.toml").write_text(f'[rules]\nr = "{ru.fq}"\n')
    runner = GrimRunner(grim_binary, grim_home)

    result = runner.run("install", "--global", check=False)
    assert result.returncode == 79, (
        f"global install without a lock must exit 79, got "
        f"{result.returncode}; {result.stderr}"
    )


def test_global_install_claude_skill_lands_in_home_dot_claude(
    grim_binary: Path, grim_home: Path, registry: str, unique_repo: str
) -> None:
    """A globally-installed Claude skill materializes in ``$HOME/.claude/skills/``."""
    repo = f"{unique_repo}/my-skill"
    sk = make_artifact(
        repo,
        "skill",
        {"my-skill/SKILL.md": "---\nname: my-skill\ndescription: test skill\n---\n# body\n"},
        tag="v1",
    )
    (grim_home / "grimoire.toml").write_text(f'[skills]\nmy-skill = "{sk.fq}"\n')
    runner = GrimRunner(grim_binary, grim_home)
    runner.json("lock", "--global")

    install_rows = runner.json("install", "--global")
    assert install_rows[0]["status"] == "installed"

    skill_dir = runner.home / ".claude/skills/my-skill"
    assert skill_dir.is_dir(), (
        f"global Claude skill must materialize in $HOME/.claude/skills/; got nothing at {skill_dir}"
    )
    assert (skill_dir / "SKILL.md").is_file()
    # Must NOT land under $GRIM_HOME/.claude/ (old layout).
    assert not (grim_home / ".claude/skills/my-skill").exists(), (
        "global skill must NOT land under $GRIM_HOME/.claude/ (old layout)"
    )


def test_global_install_claude_rule_lands_in_home_dot_claude(
    grim_binary: Path, grim_home: Path, registry: str, unique_repo: str
) -> None:
    """A globally-installed Claude rule materializes in ``$HOME/.claude/rules/``."""
    repo = f"{unique_repo}/my-rule"
    ru = make_artifact(
        repo,
        "rule",
        {"my-rule.md": "---\npaths: ['**/*.rs']\n---\n# Rust style\n"},
        tag="v1",
    )
    (grim_home / "grimoire.toml").write_text(f'[rules]\nmy-rule = "{ru.fq}"\n')
    runner = GrimRunner(grim_binary, grim_home)
    runner.json("lock", "--global")

    install_rows = runner.json("install", "--global")
    assert install_rows[0]["status"] == "installed"

    rule_file = runner.home / ".claude/rules/my-rule.md"
    assert rule_file.is_file(), (
        f"global Claude rule must materialize in $HOME/.claude/rules/; got nothing at {rule_file}"
    )
    content = rule_file.read_text()
    assert "Rust style" in content


def test_global_install_opencode_skill_lands_in_xdg_config(
    grim_binary: Path, grim_home: Path, registry: str, unique_repo: str
) -> None:
    """A globally-installed OpenCode skill materializes in ``$XDG_CONFIG_HOME/opencode/skills/``."""
    repo = f"{unique_repo}/oc-skill"
    sk = make_artifact(
        repo,
        "skill",
        {"oc-skill/SKILL.md": "---\nname: oc-skill\ndescription: opencode skill\n---\n# body\n"},
        tag="v1",
    )
    (grim_home / "grimoire.toml").write_text(f'[skills]\noc-skill = "{sk.fq}"\n')
    runner = GrimRunner(grim_binary, grim_home)
    runner.json("lock", "--global")

    install_rows = runner.json("install", "--global", "--client", "opencode")
    assert install_rows[0]["status"] == "installed"

    # Skills go to $XDG_CONFIG_HOME/opencode/skills/ (set to $HOME/.config by runner)
    skill_dir = runner.home / ".config/opencode/skills/oc-skill"
    assert skill_dir.is_dir(), (
        f"global OpenCode skill must materialize in $XDG_CONFIG_HOME/opencode/skills/; "
        f"got nothing at {skill_dir}"
    )
    assert (skill_dir / "SKILL.md").is_file()


def test_global_install_opencode_rule_stays_in_grim_home_and_registers_glob(
    grim_binary: Path, grim_home: Path, registry: str, unique_repo: str
) -> None:
    """A globally-installed OpenCode rule writes to ``$GRIM_HOME/.opencode/rules/``
    and registers an absolute glob in ``$XDG_CONFIG_HOME/opencode/opencode.json``."""
    repo = f"{unique_repo}/oc-rule"
    ru = make_artifact(
        repo,
        "rule",
        {"oc-rule.md": "---\npaths: ['**']\n---\n# OpenCode rule\n"},
        tag="v1",
    )
    (grim_home / "grimoire.toml").write_text(f'[rules]\noc-rule = "{ru.fq}"\n')
    runner = GrimRunner(grim_binary, grim_home)
    runner.json("lock", "--global")

    install_rows = runner.json("install", "--global", "--client", "opencode")
    assert install_rows[0]["status"] == "installed"

    # Rule file stays under $GRIM_HOME/.opencode/rules/ (loaded via absolute glob)
    rule_file = grim_home / ".opencode/rules/oc-rule.md"
    assert rule_file.is_file(), (
        f"global OpenCode rule must stay in $GRIM_HOME/.opencode/rules/; "
        f"got nothing at {rule_file}"
    )

    # The absolute glob must be registered in $XDG_CONFIG_HOME/opencode/opencode.json
    opencode_cfg = runner.home / ".config/opencode/opencode.json"
    assert opencode_cfg.is_file(), (
        f"opencode.json must be created at $XDG_CONFIG_HOME/opencode/opencode.json; "
        f"got nothing at {opencode_cfg}"
    )
    cfg = json.loads(opencode_cfg.read_text())
    instructions = cfg.get("instructions", [])
    assert any(str(grim_home) in entry for entry in instructions), (
        f"opencode.json instructions must contain an absolute glob pointing at $GRIM_HOME; "
        f"instructions={instructions}"
    )


def test_global_install_copilot_skill_lands_in_home_dot_copilot(
    grim_binary: Path, grim_home: Path, registry: str, unique_repo: str
) -> None:
    """A globally-installed Copilot skill materializes in ``$HOME/.copilot/skills/``."""
    repo = f"{unique_repo}/cp-skill"
    sk = make_artifact(
        repo,
        "skill",
        {"cp-skill/SKILL.md": "---\nname: cp-skill\ndescription: copilot skill\n---\n# body\n"},
        tag="v1",
    )
    (grim_home / "grimoire.toml").write_text(f'[skills]\ncp-skill = "{sk.fq}"\n')
    runner = GrimRunner(grim_binary, grim_home)
    runner.json("lock", "--global")

    install_rows = runner.json("install", "--global", "--client", "copilot")
    assert install_rows[0]["status"] == "installed"

    skill_dir = runner.home / ".copilot/skills/cp-skill"
    assert skill_dir.is_dir(), (
        f"global Copilot skill must materialize in $HOME/.copilot/skills/; "
        f"got nothing at {skill_dir}"
    )
    assert (skill_dir / "SKILL.md").is_file()


# ---------------------------------------------------------------------------
# Vendor env-var directory overrides
# ---------------------------------------------------------------------------


def test_global_claude_install_honors_claude_config_dir(
    grim_binary: Path, grim_home: Path, registry: str, unique_repo: str
) -> None:
    """``$CLAUDE_CONFIG_DIR`` replaces the entire ``~/.claude`` tree, so a
    global Claude skill AND rule both land under it instead of ``$HOME``."""
    repo_s = f"{unique_repo}/env-skill"
    repo_r = f"{unique_repo}/env-rule"
    sk = make_artifact(
        repo_s,
        "skill",
        {"env-skill/SKILL.md": "---\nname: env-skill\ndescription: env override\n---\n# body\n"},
        tag="v1",
    )
    ru = make_artifact(
        repo_r,
        "rule",
        {"env-rule.md": "---\npaths: ['**/*.rs']\n---\n# env rule\n"},
        tag="v1",
    )
    (grim_home / "grimoire.toml").write_text(
        f'[skills]\nenv-skill = "{sk.fq}"\n\n[rules]\nenv-rule = "{ru.fq}"\n'
    )
    runner = GrimRunner(grim_binary, grim_home)
    config_dir = grim_home.parent / "claude-config"
    runner.env["CLAUDE_CONFIG_DIR"] = str(config_dir)
    runner.json("lock", "--global")

    install_rows = runner.json("install", "--global")
    assert all(r["status"] == "installed" for r in install_rows)

    assert (config_dir / "skills/env-skill/SKILL.md").is_file(), (
        "skill must land in $CLAUDE_CONFIG_DIR/skills/"
    )
    assert (config_dir / "rules/env-rule.md").is_file(), (
        "rule must land in $CLAUDE_CONFIG_DIR/rules/"
    )
    # Default location must stay untouched.
    assert not (runner.home / ".claude/skills/env-skill").exists()
    assert not (runner.home / ".claude/rules/env-rule.md").exists()


def test_global_opencode_skill_honors_opencode_config_dir(
    grim_binary: Path, grim_home: Path, registry: str, unique_repo: str
) -> None:
    """``$OPENCODE_CONFIG_DIR`` (OpenCode's additive scan dir) wins over the
    XDG default for global skill installs."""
    repo = f"{unique_repo}/oc-env-skill"
    sk = make_artifact(
        repo,
        "skill",
        {"oc-env-skill/SKILL.md": "---\nname: oc-env-skill\ndescription: env override\n---\n# body\n"},
        tag="v1",
    )
    (grim_home / "grimoire.toml").write_text(f'[skills]\noc-env-skill = "{sk.fq}"\n')
    runner = GrimRunner(grim_binary, grim_home)
    config_dir = grim_home.parent / "opencode-config"
    runner.env["OPENCODE_CONFIG_DIR"] = str(config_dir)
    runner.json("lock", "--global")

    install_rows = runner.json("install", "--global", "--client", "opencode")
    assert install_rows[0]["status"] == "installed"

    assert (config_dir / "skills/oc-env-skill/SKILL.md").is_file(), (
        "skill must land in $OPENCODE_CONFIG_DIR/skills/"
    )
    assert not (runner.home / ".config/opencode/skills/oc-env-skill").exists(), (
        "XDG default must stay untouched when OPENCODE_CONFIG_DIR is set"
    )


def test_global_copilot_skill_honors_copilot_home(
    grim_binary: Path, grim_home: Path, registry: str, unique_repo: str
) -> None:
    """``$COPILOT_HOME`` replaces ``~/.copilot`` entirely for global skills."""
    repo = f"{unique_repo}/cp-env-skill"
    sk = make_artifact(
        repo,
        "skill",
        {"cp-env-skill/SKILL.md": "---\nname: cp-env-skill\ndescription: env override\n---\n# body\n"},
        tag="v1",
    )
    (grim_home / "grimoire.toml").write_text(f'[skills]\ncp-env-skill = "{sk.fq}"\n')
    runner = GrimRunner(grim_binary, grim_home)
    copilot_home = grim_home.parent / "copilot-home"
    runner.env["COPILOT_HOME"] = str(copilot_home)
    runner.json("lock", "--global")

    install_rows = runner.json("install", "--global", "--client", "copilot")
    assert install_rows[0]["status"] == "installed"

    assert (copilot_home / "skills/cp-env-skill/SKILL.md").is_file(), (
        "skill must land in $COPILOT_HOME/skills/"
    )
    assert not (runner.home / ".copilot/skills/cp-env-skill").exists(), (
        "default ~/.copilot must stay untouched when COPILOT_HOME is set"
    )


def test_global_uninstall_removes_files_from_env_override_dir(
    grim_binary: Path, grim_home: Path, registry: str, unique_repo: str
) -> None:
    """Uninstall uses the recorded absolute path: files installed under an
    env-override dir are removed even though resolution re-runs later."""
    repo = f"{unique_repo}/env-un-skill"
    sk = make_artifact(
        repo,
        "skill",
        {"env-un-skill/SKILL.md": "---\nname: env-un-skill\ndescription: x\n---\n# body\n"},
        tag="v1",
    )
    (grim_home / "grimoire.toml").write_text(f'[skills]\nenv-un-skill = "{sk.fq}"\n')
    runner = GrimRunner(grim_binary, grim_home)
    config_dir = grim_home.parent / "claude-config-un"
    runner.env["CLAUDE_CONFIG_DIR"] = str(config_dir)
    runner.json("lock", "--global")
    runner.json("install", "--global")
    assert (config_dir / "skills/env-un-skill/SKILL.md").is_file(), (
        "install step must have written the skill before uninstall can be tested"
    )

    runner.json("uninstall", "skill", "env-un-skill", "--global")
    assert not (config_dir / "skills/env-un-skill").exists(), (
        "uninstall must remove the env-override install dir"
    )


def test_global_empty_env_override_is_treated_as_unset(
    grim_binary: Path, grim_home: Path, registry: str, unique_repo: str
) -> None:
    """``CLAUDE_CONFIG_DIR=""`` must behave exactly like an unset variable:
    the install lands in the default ``~/.claude`` tree, never in a path
    built from an empty string (which would resolve relative to CWD)."""
    repo = f"{unique_repo}/empty-env-skill"
    sk = make_artifact(
        repo,
        "skill",
        {"empty-env-skill/SKILL.md": "---\nname: empty-env-skill\ndescription: x\n---\n# body\n"},
        tag="v1",
    )
    (grim_home / "grimoire.toml").write_text(f'[skills]\nempty-env-skill = "{sk.fq}"\n')
    runner = GrimRunner(grim_binary, grim_home)
    runner.env["CLAUDE_CONFIG_DIR"] = ""
    runner.json("lock", "--global")

    install_rows = runner.json("install", "--global")
    assert install_rows[0]["status"] == "installed"

    assert (runner.home / ".claude/skills/empty-env-skill/SKILL.md").is_file(), (
        "empty CLAUDE_CONFIG_DIR must fall back to the default ~/.claude tree"
    )


def test_global_update_rematerializes_into_env_override_dir(
    grim_binary: Path, grim_home: Path, registry: str, unique_repo: str
) -> None:
    """``grim update --global`` re-resolves the install target from the
    current environment — with ``CLAUDE_CONFIG_DIR`` set, the updated file
    must land in the override dir, not the ``$HOME`` default."""
    repo = f"{unique_repo}/env-up-skill"
    v1 = make_artifact(
        repo,
        "skill",
        {"env-up-skill/SKILL.md": "---\nname: env-up-skill\ndescription: x\n---\nv1\n"},
        tag="stable",
    )
    (grim_home / "grimoire.toml").write_text(f'[skills]\nenv-up-skill = "{v1.fq}"\n')
    runner = GrimRunner(grim_binary, grim_home)
    config_dir = grim_home.parent / "claude-config-up"
    runner.env["CLAUDE_CONFIG_DIR"] = str(config_dir)
    runner.json("lock", "--global")
    runner.json("install", "--global")
    installed = config_dir / "skills/env-up-skill/SKILL.md"
    assert installed.is_file() and "v1" in installed.read_text()

    # Roll the floating tag onto v2, then update.
    v2 = make_artifact(
        repo,
        "skill",
        {"env-up-skill/SKILL.md": "---\nname: env-up-skill\ndescription: x\n---\nv2\n"},
        tag="2.0.0",
    )
    retag(repo, "stable", v2.digest)

    rows = runner.json("update", "--global")
    assert rows[0]["action"] == "updated"
    assert "v2" in installed.read_text(), (
        "update must rematerialize into $CLAUDE_CONFIG_DIR, not the $HOME default"
    )
    assert not (runner.home / ".claude/skills/env-up-skill").exists(), (
        "update must not fall back to the default ~/.claude tree"
    )


def test_global_opencode_rule_honors_opencode_config_file(
    grim_binary: Path, grim_home: Path, registry: str, unique_repo: str
) -> None:
    """``$OPENCODE_CONFIG`` (OpenCode's custom config *file* path) is the
    edit target for global rule registration — the managed glob must land
    there, not in the XDG default, and deregister from there too."""
    repo = f"{unique_repo}/oc-cfg-rule"
    ru = make_artifact(
        repo,
        "rule",
        {"oc-cfg-rule.md": "---\npaths: ['**']\n---\n# rule\n"},
        tag="v1",
    )
    (grim_home / "grimoire.toml").write_text(f'[rules]\noc-cfg-rule = "{ru.fq}"\n')
    runner = GrimRunner(grim_binary, grim_home)
    custom_cfg = grim_home.parent / "custom" / "oc.json"
    custom_cfg.parent.mkdir(parents=True)
    runner.env["OPENCODE_CONFIG"] = str(custom_cfg)
    runner.json("lock", "--global")

    install_rows = runner.json("install", "--global", "--client", "opencode")
    assert install_rows[0]["status"] == "installed"

    assert custom_cfg.is_file(), (
        "managed instructions glob must be registered in $OPENCODE_CONFIG"
    )
    instructions = json.loads(custom_cfg.read_text()).get("instructions", [])
    assert any(str(grim_home) in e for e in instructions), (
        f"absolute glob missing from $OPENCODE_CONFIG; instructions={instructions}"
    )
    assert not (runner.home / ".config/opencode/opencode.json").exists(), (
        "XDG-default opencode.json must stay untouched when OPENCODE_CONFIG is set"
    )

    # Deregistration converges on the same file.
    runner.json("uninstall", "rule", "oc-cfg-rule", "--global")
    cfg_after = json.loads(custom_cfg.read_text())
    assert "instructions" not in cfg_after, (
        f"managed glob must deregister from $OPENCODE_CONFIG; got {cfg_after}"
    )


def test_global_opencode_honors_custom_xdg_config_home(
    grim_binary: Path, grim_home: Path, registry: str, unique_repo: str
) -> None:
    """A custom ``$XDG_CONFIG_HOME`` (different from the ``~/.config``
    default) drives BOTH the OpenCode skills root and the ``opencode.json``
    edit target — proving grim reads the variable instead of hardcoding
    ``~/.config``."""
    repo_s = f"{unique_repo}/xdg-skill"
    repo_r = f"{unique_repo}/xdg-rule"
    sk = make_artifact(
        repo_s,
        "skill",
        {"xdg-skill/SKILL.md": "---\nname: xdg-skill\ndescription: x\n---\n# body\n"},
        tag="v1",
    )
    ru = make_artifact(
        repo_r,
        "rule",
        {"xdg-rule.md": "---\npaths: ['**']\n---\n# rule\n"},
        tag="v1",
    )
    (grim_home / "grimoire.toml").write_text(
        f'[skills]\nxdg-skill = "{sk.fq}"\n\n[rules]\nxdg-rule = "{ru.fq}"\n'
    )
    runner = GrimRunner(grim_binary, grim_home)
    xdg = grim_home.parent / "custom-xdg"
    runner.env["XDG_CONFIG_HOME"] = str(xdg)
    runner.json("lock", "--global")

    install_rows = runner.json("install", "--global", "--client", "opencode")
    assert all(r["status"] == "installed" for r in install_rows)

    assert (xdg / "opencode/skills/xdg-skill/SKILL.md").is_file(), (
        "skill must land in $XDG_CONFIG_HOME/opencode/skills/"
    )
    cfg = xdg / "opencode/opencode.json"
    assert cfg.is_file(), (
        "opencode.json must be created under the custom $XDG_CONFIG_HOME"
    )
    instructions = json.loads(cfg.read_text()).get("instructions", [])
    assert any(str(grim_home) in e for e in instructions)
    # The ~/.config default must stay untouched.
    assert not (runner.home / ".config/opencode").exists(), (
        "default ~/.config must stay untouched when XDG_CONFIG_HOME points elsewhere"
    )


# ---------------------------------------------------------------------------
# Codex global scope
# ---------------------------------------------------------------------------


def _codex_agent_doc(name: str = "cx-agent") -> str:
    return (
        f"---\nname: {name}\ndescription: A codex agent.\nmodel: gpt-5\n---\n"
        f"# {name}\nCodex body text.\n"
    )


def test_global_install_codex_skill_lands_in_home_dot_agents(
    grim_binary: Path, grim_home: Path, registry: str, unique_repo: str
) -> None:
    """A globally-installed Codex skill materializes at
    ``$HOME/.agents/skills/<name>/`` — the cross-vendor open standard."""
    repo = f"{unique_repo}/cx-skill"
    sk = make_artifact(
        repo,
        "skill",
        {"cx-skill/SKILL.md": "---\nname: cx-skill\ndescription: codex skill\n---\n# body\n"},
        tag="v1",
    )
    (grim_home / "grimoire.toml").write_text(f'[skills]\ncx-skill = "{sk.fq}"\n')
    runner = GrimRunner(grim_binary, grim_home)
    runner.json("lock", "--global")

    install_rows = runner.json("install", "--global", "--client", "codex")
    assert install_rows[0]["status"] == "installed"

    skill_dir = runner.home / ".agents/skills/cx-skill"
    assert (skill_dir / "SKILL.md").is_file(), (
        f"global Codex skill must materialize at $HOME/.agents/skills/; nothing at {skill_dir}"
    )
    # NOT under $CODEX_HOME-style or $GRIM_HOME layouts.
    assert not (runner.home / ".codex/skills/cx-skill").exists()
    assert not (grim_home / ".agents/skills/cx-skill").exists()


def test_global_install_codex_agent_lands_in_home_dot_codex(
    grim_binary: Path, grim_home: Path, registry: str, unique_repo: str
) -> None:
    """A globally-installed Codex agent materializes at
    ``$HOME/.codex/agents/<name>.toml`` when ``CODEX_HOME`` is unset."""
    import tomllib

    repo = f"{unique_repo}/cx-agent"
    ag = make_artifact(repo, "agent", {"cx-agent.md": _codex_agent_doc("cx-agent")}, tag="v1")
    (grim_home / "grimoire.toml").write_text(f'[agents]\ncx-agent = "{ag.fq}"\n')
    runner = GrimRunner(grim_binary, grim_home)
    runner.json("lock", "--global")

    install_rows = runner.json("install", "--global", "--client", "codex")
    assert install_rows[0]["status"] == "installed"

    toml_file = runner.home / ".codex/agents/cx-agent.toml"
    assert toml_file.is_file(), (
        f"global Codex agent must materialize at $HOME/.codex/agents/; nothing at {toml_file}"
    )
    parsed = tomllib.loads(toml_file.read_text())
    assert parsed["name"] == "cx-agent"
    assert "Codex body text." in parsed["developer_instructions"]


def test_global_codex_home_relocates_agent_but_not_skill(
    grim_binary: Path, grim_home: Path, registry: str, unique_repo: str
) -> None:
    """``CODEX_HOME`` relocates the Codex **agent** root but NOT skills:
    the agent lands under ``$CODEX_HOME/agents`` while the skill stays at the
    ``$HOME/.agents/skills`` cross-vendor standard."""
    sk = make_artifact(
        f"{unique_repo}/cx2-skill",
        "skill",
        {"cx2-skill/SKILL.md": "---\nname: cx2-skill\ndescription: s\n---\n# body\n"},
        tag="v1",
    )
    ag = make_artifact(
        f"{unique_repo}/cx2-agent",
        "agent",
        {"cx2-agent.md": _codex_agent_doc("cx2-agent")},
        tag="v1",
    )
    (grim_home / "grimoire.toml").write_text(
        f'[skills]\ncx2-skill = "{sk.fq}"\n[agents]\ncx2-agent = "{ag.fq}"\n'
    )
    codex_home = grim_home.parent / "codex_home"
    runner = GrimRunner(grim_binary, grim_home)
    runner.env["CODEX_HOME"] = str(codex_home)
    runner.json("lock", "--global")

    install_rows = runner.json("install", "--global", "--client", "codex")
    assert all(r["status"] == "installed" for r in install_rows), install_rows

    # Agent follows $CODEX_HOME.
    assert (codex_home / "agents/cx2-agent.toml").is_file(), (
        "CODEX_HOME must relocate the Codex agent root"
    )
    assert not (runner.home / ".codex/agents/cx2-agent.toml").exists(), (
        "with CODEX_HOME set, the agent must NOT land in the ~/.codex default"
    )
    # Skill stays at $HOME/.agents/skills — CODEX_HOME does not move it.
    assert (runner.home / ".agents/skills/cx2-skill/SKILL.md").is_file(), (
        "CODEX_HOME must NOT relocate Codex skills (cross-vendor $HOME standard)"
    )
    assert not (codex_home / "skills/cx2-skill").exists(), (
        "Codex skills must never land under $CODEX_HOME"
    )
