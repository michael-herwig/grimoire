# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The Grimoire Authors
"""`grim config registry` acceptance tests.

Additional coverage (review-fix round 1):
- Security regression: injection payloads in URL / default_registry rejected or
  produce no extra registries in the written config.
- D2: unset registry.<alias> removes the whole entry; set registry.<alias>.default
  true; unset registry.<alias>.oci exits 64.

Specification-phase suite: every test expresses expected behavior from
``adr_grim_config_command.md`` and ``plan_grim_config.md``.  All tests
FAIL against the Phase-3 stubs (``run`` body is ``unimplemented!()``).

Behaviors covered:
- ``registry add`` / ``list`` / ``show`` / ``use`` / ``rm`` lifecycle.
- At-most-one-default invariant: ``registry use <alias>`` clears all prior
  defaults, leaving exactly one.
- ``registry add`` with a duplicate alias exits 64 (UsageError).
- ``registry rm`` / ``show`` / ``use`` with a missing alias exits 64.
- Dotted ``config get registry.<alias>.oci`` returns the URL.
- Dotted ``config set registry.<alias>.oci`` on a MISSING alias exits 64
  (creation is ``registry add`` only — keeps validation in one path).
- End-to-end: ``config registry add`` at project scope then ``grim add``
  with an alias-qualified ref resolves and installs the artifact.
- End-to-end (global): ``--global registry add corp --oci <host>`` writes
  to ``$GRIM_HOME/grimoire.toml``.
"""
from __future__ import annotations

import tomllib  # stdlib (Python 3.11+)
from pathlib import Path

from src.helpers import make_artifact, write_config
from src.registry import REGISTRY_HOST
from src.runner import GrimRunner


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _minimal_global_config(grim_home: Path) -> None:
    """Write a minimal valid ``grimoire.toml`` in ``$GRIM_HOME``."""
    (grim_home / "grimoire.toml").write_text("[skills]\n\n[rules]\n")


# ---------------------------------------------------------------------------
# Lifecycle tests
# ---------------------------------------------------------------------------


def test_registry_add_then_list_shows_entry(
    grim_at: object,
    project_dir: Path,
) -> None:
    """``registry add`` adds an entry visible in ``registry list``.

    After adding ``acme`` with URL ``ghcr.io/acme``, ``registry list``
    must return a JSON array that contains an entry with alias ``acme``
    and the configured URL.

    Traces to ADR: ``grim config registry add <alias> --oci <ref>``.
    """
    write_config(project_dir)
    runner: GrimRunner = grim_at(project_dir)  # type: ignore[call-arg]

    runner.run("config", "registry", "add", "acme", "--oci", "ghcr.io/acme")

    result = runner.json("config", "registry", "list")

    assert isinstance(result, list), (
        f"registry list --format json must return a JSON array; got: {type(result)}"
    )
    aliases = [r.get("alias") for r in result if isinstance(r, dict)]
    assert "acme" in aliases, (
        f"registry list must include 'acme' after add; aliases: {aliases}"
    )
    urls = [r.get("oci") for r in result if isinstance(r, dict)]
    assert "ghcr.io/acme" in urls, (
        f"registry list must include the configured URL; urls: {urls}"
    )


def test_registry_add_legacy_url_flag_and_key_still_work(
    grim_at: object,
    project_dir: Path,
) -> None:
    """0.6.x back-compat: ``--url`` is a hidden alias for ``--oci`` and the
    dotted field ``registry.<alias>.url`` maps to ``oci``.

    End-to-end alias proof: add via the legacy flag, read back via both the
    new and the legacy dotted key.
    """
    write_config(project_dir)
    runner: GrimRunner = grim_at(project_dir)  # type: ignore[call-arg]

    runner.run("config", "registry", "add", "legacy", "--url", "ghcr.io/legacy")

    new_key = runner.plain("config", "get", "registry.legacy.oci")
    assert new_key.stdout.strip() == "ghcr.io/legacy", (
        f"registry.legacy.oci must return the value added via --url; got: {new_key.stdout!r}"
    )
    old_key = runner.plain("config", "get", "registry.legacy.url")
    assert old_key.stdout.strip() == "ghcr.io/legacy", (
        f"legacy dotted key registry.legacy.url must keep working; got: {old_key.stdout!r}"
    )


def test_registry_add_then_show_returns_fields(
    grim_at: object,
    project_dir: Path,
) -> None:
    """``registry show <alias>`` returns all fields for a specific registry.

    The JSON object must contain ``alias``, ``oci``, and ``default``.

    Traces to ADR: ``grim config registry show <alias>`` → one-row table
    (Alias | Type | Source | Default); JSON ``{"alias":"…","oci":"…","default":bool}``.
    """
    write_config(project_dir)
    runner: GrimRunner = grim_at(project_dir)  # type: ignore[call-arg]

    runner.run("config", "registry", "add", "acme", "--oci", "ghcr.io/acme")

    result = runner.json("config", "registry", "show", "acme")

    assert result.get("alias") == "acme", (
        f"show must return alias='acme'; got: {result!r}"
    )
    assert result.get("oci") == "ghcr.io/acme", (
        f"show must return the configured URL; got: {result!r}"
    )
    assert "default" in result, (
        f"show must include the 'default' field; got: {result!r}"
    )


def test_registry_use_transfers_default_at_most_one(
    grim_at: object,
    project_dir: Path,
) -> None:
    """``registry use <alias>`` makes exactly one registry the default.

    After ``registry add acme --default`` and ``registry add corp``,
    ``registry use corp`` must set ``corp`` as the default and clear
    ``acme``'s default flag.  At most one entry may carry ``default=true``.

    Traces to ADR: "``registry use`` sets target ``default=true`` and
    clears it on all others (enforces at-most-one before write)".
    """
    write_config(project_dir)
    runner: GrimRunner = grim_at(project_dir)  # type: ignore[call-arg]

    runner.run(
        "config", "registry", "add", "acme",
        "--oci", "ghcr.io/acme", "--default",
    )
    runner.run(
        "config", "registry", "add", "corp",
        "--oci", "registry.corp/team",
    )

    runner.run("config", "registry", "use", "corp")

    result = runner.json("config", "registry", "list")
    assert isinstance(result, list), "registry list must return a JSON array"

    defaults = [r for r in result if r.get("default") is True]
    assert len(defaults) == 1, (
        f"exactly one registry must be the default after 'use'; "
        f"got {len(defaults)} defaults: {defaults}"
    )
    assert defaults[0].get("alias") == "corp", (
        f"'corp' must be the new default; got: {defaults[0]!r}"
    )


# ---------------------------------------------------------------------------
# Error-path tests (UsageError 64)
# ---------------------------------------------------------------------------


def test_registry_add_duplicate_alias_exits_64(
    grim_at: object,
    project_dir: Path,
) -> None:
    """``registry add`` with an already-existing alias exits 64 (UsageError).

    To change a registry URL, use dotted ``set registry.<alias>.oci`` or
    ``rm`` + ``add`` — there is no overwrite/upsert verb.

    Traces to ADR: "``registry add`` of an existing alias → UsageError 64".
    """
    write_config(project_dir)
    runner: GrimRunner = grim_at(project_dir)  # type: ignore[call-arg]

    runner.run("config", "registry", "add", "acme", "--oci", "ghcr.io/acme")

    result = runner.run(
        "config", "registry", "add", "acme", "--oci", "ghcr.io/acme-v2",
        check=False,
    )
    assert result.returncode == 64, (
        f"duplicate registry add must exit 64 (UsageError); "
        f"got {result.returncode}; stderr: {result.stderr.strip()}"
    )


def test_registry_rm_removes_entry_then_show_exits_64(
    grim_at: object,
    project_dir: Path,
) -> None:
    """``registry rm`` removes the entry; subsequent ``show`` exits 64.

    After ``rm``, the alias is gone: ``show`` must return UsageError 64,
    not a stale result.

    Traces to ADR: ``grim config registry rm <alias>``; "alias not found
    → UsageError 64".
    """
    write_config(project_dir)
    runner: GrimRunner = grim_at(project_dir)  # type: ignore[call-arg]

    runner.run("config", "registry", "add", "acme", "--oci", "ghcr.io/acme")
    runner.run("config", "registry", "rm", "acme")

    result = runner.run("config", "registry", "show", "acme", check=False)
    assert result.returncode == 64, (
        f"show after rm must exit 64 (alias no longer exists); "
        f"got {result.returncode}; stderr: {result.stderr.strip()}"
    )


def test_registry_show_missing_alias_exits_64(
    grim_at: object,
    project_dir: Path,
) -> None:
    """``registry show`` with an unknown alias exits 64 (UsageError).

    Traces to ADR: "Registry alias not found on get/show/rm/use … →
    UsageError 64".
    """
    write_config(project_dir)
    runner: GrimRunner = grim_at(project_dir)  # type: ignore[call-arg]

    result = runner.run(
        "config", "registry", "show", "nonexistent", check=False
    )
    assert result.returncode == 64, (
        f"show of missing alias must exit 64; "
        f"got {result.returncode}; stderr: {result.stderr.strip()}"
    )


def test_registry_rm_missing_alias_exits_64(
    grim_at: object,
    project_dir: Path,
) -> None:
    """``registry rm`` with an unknown alias exits 64 (UsageError).

    Traces to ADR: "Registry alias not found on get/show/rm/use … →
    UsageError 64".
    """
    write_config(project_dir)
    runner: GrimRunner = grim_at(project_dir)  # type: ignore[call-arg]

    result = runner.run(
        "config", "registry", "rm", "nonexistent", check=False
    )
    assert result.returncode == 64, (
        f"rm of missing alias must exit 64; "
        f"got {result.returncode}; stderr: {result.stderr.strip()}"
    )


def test_registry_use_missing_alias_exits_64(
    grim_at: object,
    project_dir: Path,
) -> None:
    """``registry use`` with an unknown alias exits 64 (UsageError).

    Traces to ADR: "Registry alias not found on get/show/rm/use … →
    UsageError 64".
    """
    write_config(project_dir)
    runner: GrimRunner = grim_at(project_dir)  # type: ignore[call-arg]

    result = runner.run(
        "config", "registry", "use", "nonexistent", check=False
    )
    assert result.returncode == 64, (
        f"use of missing alias must exit 64; "
        f"got {result.returncode}; stderr: {result.stderr.strip()}"
    )


# ---------------------------------------------------------------------------
# Dotted-key access to registry fields
# ---------------------------------------------------------------------------


def test_dotted_get_registry_alias_url_returns_url(
    grim_at: object,
    project_dir: Path,
) -> None:
    """``config get registry.<alias>.oci`` returns the URL via dotted-key.

    After ``registry add acme --oci ghcr.io/acme``, the dotted key
    ``registry.acme.oci`` must return ``ghcr.io/acme`` and exit 0.

    Traces to ADR: key-namespace table row ``registry.<alias>.oci``.
    """
    write_config(project_dir)
    runner: GrimRunner = grim_at(project_dir)  # type: ignore[call-arg]

    runner.run("config", "registry", "add", "acme", "--oci", "ghcr.io/acme")

    result = runner.plain("config", "get", "registry.acme.oci")
    assert result.returncode == 0, (
        f"dotted get of registry URL must exit 0; got {result.returncode}; "
        f"stderr: {result.stderr.strip()}"
    )
    assert "ghcr.io/acme" in result.stdout, (
        f"dotted get must return the URL; got: {result.stdout!r}"
    )


def test_dotted_set_registry_url_on_missing_alias_exits_64(
    grim_at: object,
    project_dir: Path,
) -> None:
    """``config set registry.<alias>.oci`` on an absent alias exits 64.

    Registry entries are created ONLY via ``registry add`` to keep the
    url-required validation invariant in one path.  A dotted ``set`` on a
    non-existent alias must not silently create a half-built entry.

    Traces to ADR: "Dotted ``set registry.<alias>.<field>`` requires the
    entry to already exist … create only via ``registry add``".
    """
    write_config(project_dir)
    runner: GrimRunner = grim_at(project_dir)  # type: ignore[call-arg]

    result = runner.run(
        "config", "set", "registry.phantom.oci", "ghcr.io/phantom",
        check=False,
    )
    assert result.returncode == 64, (
        f"dotted set on missing alias must exit 64 (UsageError); "
        f"got {result.returncode}; stderr: {result.stderr.strip()}"
    )


# ---------------------------------------------------------------------------
# End-to-end: config registry add → grim add resolves alias
# ---------------------------------------------------------------------------


def test_registry_add_then_grim_add_resolves_alias(
    grim_at: object,
    project_dir: Path,
    registry: str,
    unique_repo: str,
) -> None:
    """A registry added via ``grim config`` is used to resolve alias refs.

    After ``config registry add corp --oci <host>/<ns> --default``, a
    ``grim add corp/<repo>:tag`` must expand the alias to the full URL
    and succeed.

    This end-to-end path tests that:
    1. ``grim config registry add`` correctly writes the ``[[registries]]``
       entry into ``grimoire.toml``.
    2. The existing alias-expansion logic in ``grim add`` works with entries
       written by the config command (not only hand-authored configs).

    Traces to ADR: end-to-end test scenario in Testing Strategy section.
    """
    ns = unique_repo
    make_artifact(
        f"{ns}/corp-skill",
        "skill",
        {
            "corp-skill/SKILL.md": (
                "---\nname: corp-skill\ndescription: config e2e\n---\n# corp\n"
            )
        },
        tag="v1",
    )

    write_config(project_dir)
    runner: GrimRunner = grim_at(project_dir)  # type: ignore[call-arg]
    runner.env.pop("GRIM_DEFAULT_REGISTRY", None)

    # Register the test registry under the "corp" alias.
    runner.run(
        "config", "registry", "add", "corp",
        "--oci", f"{REGISTRY_HOST}/{ns}", "--default",
    )

    # Add via the alias-qualified reference; grim must expand corp → the URL.
    out = runner.json("add", "corp/corp-skill:v1")

    assert out.get("status") == "added", (
        f"grim add via config-registered alias must succeed; got: {out!r}"
    )
    assert out.get("kind") == "skill", (
        f"kind must be 'skill'; got: {out!r}"
    )


# ---------------------------------------------------------------------------
# End-to-end (global scope)
# ---------------------------------------------------------------------------


def test_global_registry_add_writes_grim_home_config(
    grim_binary: Path,
    grim_home: Path,
) -> None:
    """``--global registry add`` writes the entry to ``$GRIM_HOME/grimoire.toml``.

    The written config must contain the alias name and the configured URL
    so that subsequent global-scope commands (lock, install) can use it.

    Traces to ADR: ``--global`` flag selects ``$GRIM_HOME/grimoire.toml``;
    "``--global registry add corp --oci <REGISTRY_HOST>`` then a short-id
    resolves against it".
    """
    _minimal_global_config(grim_home)
    runner = GrimRunner(grim_binary, grim_home)
    runner.env.pop("GRIM_DEFAULT_REGISTRY", None)

    runner.run(
        "config", "--global", "registry", "add", "corp",
        "--oci", f"{REGISTRY_HOST}/grim-test/e2e", "--default",
    )

    cfg_text = (grim_home / "grimoire.toml").read_text()
    assert "corp" in cfg_text, (
        f"$GRIM_HOME/grimoire.toml must contain the alias 'corp'; "
        f"got:\n{cfg_text}"
    )
    assert REGISTRY_HOST in cfg_text, (
        f"$GRIM_HOME/grimoire.toml must contain the registry host; "
        f"got:\n{cfg_text}"
    )


# ---------------------------------------------------------------------------
# Security regression (S1 + S3): injection payloads must be rejected or
# produce no extra registries in the written config.
# ---------------------------------------------------------------------------


def _count_registries(cfg_path: Path) -> int:
    """Return the number of ``[[registries]]`` entries in the parsed config."""
    with cfg_path.open("rb") as f:
        data = tomllib.load(f)
    return len(data.get("registries", []))


def test_injection_via_registry_add_url_is_rejected_or_harmless(
    grim_at: object,
    project_dir: Path,
) -> None:
    """A ``[[registries]]`` injection payload in a registry URL must not
    produce extra entries in the written ``grimoire.toml``.

    Either the command rejects the value (exit 65 for control chars) OR the
    written file round-trips to the same registry count.  In either case,
    ``registry list`` must not reveal any additional (injected) registry.

    Traces to S1 + S3: TOML-escape all written string values;
    reject control characters in URLs before writing.
    """
    write_config(project_dir)
    runner: GrimRunner = grim_at(project_dir)  # type: ignore[call-arg]

    # Payload that would create a second [[registries]] block if not escaped.
    injection = 'legit.io"\n[[registries]]\nalias="evil"\nurl="attacker.io"'

    result = runner.run(
        "config", "registry", "add", "probe",
        "--oci", injection,
        check=False,
    )

    cfg_path = project_dir / "grimoire.toml"
    if result.returncode == 0:
        # Command accepted the value; the file must still parse cleanly and
        # contain exactly one registry (the one we added).
        try:
            count = _count_registries(cfg_path)
        except tomllib.TOMLDecodeError as exc:
            raise AssertionError(
                f"grimoire.toml is invalid TOML after injection attempt:\n"
                f"{cfg_path.read_text()}\nError: {exc}"
            ) from exc
        assert count == 1, (
            f"injection must not create extra [[registries]] entries; "
            f"got {count} entries.\nConfig:\n{cfg_path.read_text()}"
        )
    else:
        # Command rejected the value (exit 65 = DataError expected for control chars).
        assert result.returncode == 65, (
            f"injection URL with control chars must exit 65 (DataError); "
            f"got {result.returncode}; stderr: {result.stderr.strip()}"
        )


def test_injection_via_set_default_registry_is_rejected(
    grim_at: object,
    project_dir: Path,
) -> None:
    """A ``default_registry`` injection payload must not corrupt ``grimoire.toml``.

    Traces to S1 + S3: ``options.default_registry`` is TOML-escaped on write;
    control characters in the value are rejected before writing.
    """
    write_config(project_dir)
    runner: GrimRunner = grim_at(project_dir)  # type: ignore[call-arg]

    injection = 'legit.io"\ndefault_registry = "pwned'

    result = runner.run(
        "config", "set", "options.default_registry", injection,
        check=False,
    )

    cfg_path = project_dir / "grimoire.toml"
    if result.returncode == 0:
        try:
            with cfg_path.open("rb") as f:
                parsed = tomllib.load(f)
        except tomllib.TOMLDecodeError as exc:
            raise AssertionError(
                f"grimoire.toml is invalid TOML after injection: {exc}\n"
                f"Content:\n{cfg_path.read_text()}"
            ) from exc
        options = parsed.get("options", {})
        assert options.get("default_registry") != "pwned", (
            "injection must not set default_registry to 'pwned'"
        )
    else:
        assert result.returncode == 65, (
            f"injection value with newline must exit 65 (DataError); "
            f"got {result.returncode}"
        )


# ---------------------------------------------------------------------------
# D2: dotted unset registry.<alias> removes whole entry
# ---------------------------------------------------------------------------


def test_unset_registry_alias_removes_whole_entry(
    grim_at: object,
    project_dir: Path,
) -> None:
    """``config unset registry.<alias>`` removes the entire registry entry.

    After ``registry add acme`` and ``unset registry.acme``, the
    ``registry list`` output must not contain acme.

    Traces to D2: ``unset registry.<alias>`` removes the whole entry.
    """
    write_config(project_dir)
    runner: GrimRunner = grim_at(project_dir)  # type: ignore[call-arg]

    runner.run("config", "registry", "add", "acme", "--oci", "ghcr.io/acme")

    runner.run("config", "unset", "registry.acme")

    result = runner.json("config", "registry", "list")
    aliases = [r.get("alias") for r in result if isinstance(r, dict)]
    assert "acme" not in aliases, (
        f"unset registry.acme must remove the entry; remaining aliases: {aliases}"
    )


# ---------------------------------------------------------------------------
# FIX B regression: injection via grim add tag must not create [[registries]]
# ---------------------------------------------------------------------------


def test_injection_via_grim_add_tag_is_rejected(
    grim_at: object,
    project_dir: Path,
) -> None:
    """An injection payload in a ``grim add`` tag must not produce extra
    ``[[registries]]`` entries in ``grimoire.toml``.

    Before FIX B the four artifact-table VALUE writes in ``write_config``
    used raw ``"{name} = \\"{id}\\""`` formatting.  A tag containing a
    literal ``"`` + newline could inject additional TOML sections, e.g.::

        tag"
        [[registries]]
        alias="evil"
        url="attacker.io"
        # <rest of tag>

    The fix wraps every value through ``toml::Value::String(id.to_string())``
    so the output is always properly TOML-escaped.  Normal identifiers are
    byte-identical after escaping, so existing round-trip tests still pass.

    Traces to FIX B: all four artifact-table writes in write_config are
    TOML-escaped.
    """
    write_config(project_dir)
    runner: GrimRunner = grim_at(project_dir)  # type: ignore[call-arg]

    # Injection payload in the tag position — would create a second [[registries]]
    # block if the value were written unescaped.
    injection_ref = 'ghcr.io/x/y:tag"\n[[registries]]\nalias="evil"\nurl="attacker.io"\n# '

    result = runner.run(
        "add", injection_ref, "--kind", "skill",
        check=False,
    )

    cfg_path = project_dir / "grimoire.toml"
    if cfg_path.exists():
        try:
            with cfg_path.open("rb") as f:
                data = tomllib.load(f)
        except Exception as exc:
            raise AssertionError(
                f"grimoire.toml is invalid TOML after injection attempt — "
                f"injection succeeded:\n{cfg_path.read_text()}\nError: {exc}"
            ) from exc
        registry_count = len(data.get("registries", []))
        assert registry_count == 0, (
            f"injection must not create [[registries]] entries; "
            f"got {registry_count}.\nConfig:\n{cfg_path.read_text()}"
        )
    else:
        # No config file written — command must have exited non-zero.
        assert result.returncode != 0, (
            f"grim add with injection payload must either reject (non-zero) "
            f"or write a clean config with 0 registries; "
            f"got exit {result.returncode} with no config file"
        )


# ---------------------------------------------------------------------------
# D2: set registry.<alias>.default true
# ---------------------------------------------------------------------------


def test_set_registry_alias_default_true(
    grim_at: object,
    project_dir: Path,
) -> None:
    """``config set registry.<alias>.default true`` marks the registry as default.

    Traces to D2: ``set registry.<alias>.default true``.
    """
    write_config(project_dir)
    runner: GrimRunner = grim_at(project_dir)  # type: ignore[call-arg]

    runner.run("config", "registry", "add", "acme", "--oci", "ghcr.io/acme")
    runner.run("config", "registry", "add", "corp", "--oci", "ghcr.io/corp")

    runner.run("config", "set", "registry.acme.default", "true")

    result = runner.json("config", "registry", "list")
    defaults = [r for r in result if r.get("default") is True]
    assert len(defaults) == 1, (
        f"exactly one registry must be the default; got: {defaults!r}"
    )
    assert defaults[0].get("alias") == "acme", (
        f"acme must be the default after set; got: {defaults[0]!r}"
    )


# ---------------------------------------------------------------------------
# D2: unset registry.<alias>.oci exits 64
# ---------------------------------------------------------------------------


def test_unset_registry_alias_url_exits_64(
    grim_at: object,
    project_dir: Path,
) -> None:
    """``config unset registry.<alias>.oci`` exits 64 (UsageError).

    The URL is required for a registry entry; it cannot be unset without
    removing the whole entry via ``registry rm`` or ``unset registry.<alias>``.

    Traces to D2: ``unset registry.<alias>.oci`` → exit 64.
    """
    write_config(project_dir)
    runner: GrimRunner = grim_at(project_dir)  # type: ignore[call-arg]

    runner.run("config", "registry", "add", "acme", "--oci", "ghcr.io/acme")

    result = runner.run(
        "config", "unset", "registry.acme.oci", check=False
    )
    assert result.returncode == 64, (
        f"unset registry.acme.oci must exit 64 (UsageError); "
        f"got {result.returncode}; stderr: {result.stderr.strip()}"
    )


# ---------------------------------------------------------------------------
# FIX 1: malformed alias exits 64, not 78
# ---------------------------------------------------------------------------


def test_registry_add_slash_alias_exits_64(
    grim_at: object,
    project_dir: Path,
) -> None:
    """``registry add`` with a slash in the alias exits 64 (UsageError), not 78.

    Before FIX 1, ``registry add "a/b" --oci x`` pushed the entry and let
    ``validate_registries`` reject it → ``RegistryInvalid`` → exit 78
    (ConfigError).  A bad CLI argument must exit 64 (UsageError).

    Traces to FIX 1: pre-validate alias at command boundary before building
    the entry.
    """
    write_config(project_dir)
    runner: GrimRunner = grim_at(project_dir)  # type: ignore[call-arg]

    result = runner.run(
        "config", "registry", "add", "a/b", "--oci", "ghcr.io/test",
        check=False,
    )
    assert result.returncode == 64, (
        f"alias with '/' must exit 64 (UsageError); "
        f"got {result.returncode}; stderr: {result.stderr.strip()}"
    )


# ---------------------------------------------------------------------------
# FIX 2: dotted aliases stay addressable via dotted key
# ---------------------------------------------------------------------------


def test_dotted_alias_roundtrips_via_get(
    grim_at: object,
    project_dir: Path,
) -> None:
    """``registry add a.b`` then ``config get registry.a.b.oci`` returns the URL.

    Before FIX 2, ``parse_key`` split at the FIRST dot, so
    ``registry.a.b.oci`` → alias=``a``, field=``b.url`` → unknown field
    error.  After fixing parse_key to split at the RIGHTMOST dot, aliases
    containing dots are fully addressable.

    Traces to FIX 2: parse_key splits at rightmost dot for registry keys.
    """
    write_config(project_dir)
    runner: GrimRunner = grim_at(project_dir)  # type: ignore[call-arg]

    runner.run("config", "registry", "add", "a.b", "--oci", "ghcr.io/dotted")

    result = runner.plain("config", "get", "registry.a.b.oci")
    assert result.returncode == 0, (
        f"config get registry.a.b.oci must exit 0; got {result.returncode}; "
        f"stderr: {result.stderr.strip()}"
    )
    assert "ghcr.io/dotted" in result.stdout, (
        f"get must return the URL; got: {result.stdout!r}"
    )
