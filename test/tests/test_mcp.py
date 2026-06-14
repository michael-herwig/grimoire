# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The Grimoire Authors
"""Acceptance tests for the `grim mcp` local STDIO server.

Drives the server over a real JSON-RPC-over-stdio handshake (the same wire
protocol an MCP client speaks) and asserts the advertised tool surface and
that a read tool's payload matches the equivalent `grim ... --format json`
output. The server exits on stdin EOF, so a single piped request batch runs
the whole session.
"""
from __future__ import annotations

import json
import subprocess
from collections.abc import Callable
from pathlib import Path

from src.runner import GrimRunner

_PROTOCOL = "2025-06-18"


def _drive(runner: GrimRunner, cwd: Path, requests: list[dict], *, allow_writes: bool = False) -> dict[int, dict]:
    """Run `grim [--offline] mcp` feeding `requests`, return responses by id.

    `--offline` keeps the embedded catalog browse off the network (a cold
    cache degrades to empty), so the test is hermetic.
    """
    args = [str(runner.binary), "--offline", "mcp"]
    if allow_writes:
        args.append("--allow-writes")
    payload = "".join(json.dumps(r) + "\n" for r in requests)
    result = subprocess.run(
        args,
        input=payload,
        capture_output=True,
        text=True,
        env=runner.env,
        cwd=str(cwd),
        timeout=60,
    )
    responses: dict[int, dict] = {}
    for line in result.stdout.splitlines():
        line = line.strip()
        if not line:
            continue
        msg = json.loads(line)
        if isinstance(msg.get("id"), int):
            responses[msg["id"]] = msg
    return responses


def _initialize(idx: int) -> dict:
    return {
        "jsonrpc": "2.0",
        "id": idx,
        "method": "initialize",
        "params": {
            "protocolVersion": _PROTOCOL,
            "capabilities": {},
            "clientInfo": {"name": "pytest", "version": "0"},
        },
    }


def test_mcp_initialize_and_lists_read_tools(
    grim_at: Callable[[Path], GrimRunner], project_dir: Path
) -> None:
    (project_dir / "grimoire.toml").write_text("[skills]\n")
    runner = grim_at(project_dir)

    responses = _drive(
        runner,
        project_dir,
        [
            _initialize(1),
            {"jsonrpc": "2.0", "method": "notifications/initialized"},
            {"jsonrpc": "2.0", "id": 2, "method": "tools/list"},
        ],
    )

    assert responses[1]["result"]["serverInfo"]["name"] == "grim"
    names = {t["name"] for t in responses[2]["result"]["tools"]}
    assert "grim_search" in names
    assert "grim_status" in names
    # Read-only by default: mutating tools are not advertised.
    assert not (names & {"grim_add", "grim_install", "grim_update", "grim_uninstall"})


def test_mcp_search_tool_matches_cli_json(
    grim_at: Callable[[Path], GrimRunner], project_dir: Path
) -> None:
    (project_dir / "grimoire.toml").write_text("[skills]\n")
    runner = grim_at(project_dir)

    responses = _drive(
        runner,
        project_dir,
        [
            _initialize(1),
            {"jsonrpc": "2.0", "method": "notifications/initialized"},
            {
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/call",
                "params": {"name": "grim_search", "arguments": {"query": "review"}},
            },
        ],
    )

    call = responses[2]["result"]
    assert call["isError"] is False
    mcp_payload = json.loads(call["content"][0]["text"])

    # The MCP tool reuses `command::search::run`, so its JSON equals the CLI's
    # `grim search --format json` output for the same scope/query.
    cli_payload = runner.json("--offline", "search", "review")
    assert mcp_payload == cli_payload


def test_mcp_status_tool_returns_payload(
    grim_at: Callable[[Path], GrimRunner], project_dir: Path
) -> None:
    """``grim_status`` MCP tool returns a valid payload matching ``grim status --format json``.

    Drives initialize + tools/call grim_status over stdio and asserts:
    - ``isError`` is ``False``
    - The content parses as a JSON array (the same shape ``grim status``
      emits)
    - The payload exactly equals what ``grim status --format json`` returns
      for the same scope (same GRIM_HOME, same project dir)

    Both the MCP server and the CLI route through ``command::status::run``,
    so they are one source of truth.
    """
    (project_dir / "grimoire.toml").write_text("[skills]\n")
    runner = grim_at(project_dir)

    responses = _drive(
        runner,
        project_dir,
        [
            _initialize(1),
            {"jsonrpc": "2.0", "method": "notifications/initialized"},
            {
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/call",
                "params": {"name": "grim_status", "arguments": {}},
            },
        ],
    )

    call = responses[2]["result"]
    assert call["isError"] is False, (
        f"grim_status tool must not return an error, got: {call!r}"
    )
    # Content is a JSON string wrapping the status array.
    mcp_payload = json.loads(call["content"][0]["text"])
    assert isinstance(mcp_payload, list), (
        f"grim_status payload must be a JSON array, got: {mcp_payload!r}"
    )

    # The MCP payload must exactly match the CLI's --format json output for
    # the same scope. Both go through command::status::run; one source of truth.
    cli_payload = runner.json("status")
    assert mcp_payload == cli_payload, (
        f"MCP grim_status payload must equal CLI grim status --format json;\n"
        f"  MCP: {mcp_payload!r}\n"
        f"  CLI: {cli_payload!r}"
    )


def test_mcp_allow_writes_tool_surface_unchanged(
    grim_at: Callable[[Path], GrimRunner], project_dir: Path
) -> None:
    """``grim mcp --allow-writes`` advertises no additional tools beyond read-only mode.

    Write tools (grim_add, grim_install, grim_update, grim_uninstall) are
    gated behind ``--allow-writes`` in the design, but are NOT YET IMPLEMENTED
    (v1 ships only the two read tools ``grim_search`` and ``grim_status``).

    This test documents and locks the current shipped state: the tool set
    is identical whether or not ``--allow-writes`` is passed. When write
    tools are implemented, this test MUST be updated deliberately to reflect
    the new advertised tool surface — it serves as a change-detector.
    """
    (project_dir / "grimoire.toml").write_text("[skills]\n")
    runner = grim_at(project_dir)

    list_request = [
        _initialize(1),
        {"jsonrpc": "2.0", "method": "notifications/initialized"},
        {"jsonrpc": "2.0", "id": 2, "method": "tools/list"},
    ]

    # Read-only mode (default).
    read_only_responses = _drive(runner, project_dir, list_request, allow_writes=False)
    read_only_names = {t["name"] for t in read_only_responses[2]["result"]["tools"]}

    # --allow-writes mode.
    allow_writes_responses = _drive(runner, project_dir, list_request, allow_writes=True)
    allow_writes_names = {t["name"] for t in allow_writes_responses[2]["result"]["tools"]}

    # Both modes must expose the two read tools.
    assert "grim_search" in read_only_names
    assert "grim_status" in read_only_names
    assert "grim_search" in allow_writes_names
    assert "grim_status" in allow_writes_names

    # Write tools are NOT YET IMPLEMENTED: the tool surface must be identical
    # in both modes. When write tools land, update this assertion to describe
    # the new set.
    assert read_only_names == allow_writes_names, (
        f"--allow-writes must not add or remove tools until write tools are "
        f"implemented; read-only: {sorted(read_only_names)}, "
        f"allow-writes: {sorted(allow_writes_names)}"
    )


def test_mcp_search_ignores_agent_supplied_registry(
    grim_at: Callable[[Path], GrimRunner], project_dir: Path
) -> None:
    """An agent-supplied ``registry`` argument is inert (CWE-918 boundary).

    ``grim_search`` deliberately exposes no per-call registry override — the
    server browses only its configured registry set. A prompt-injected agent
    that adds a ``registry`` pointing at an unconfigured host must not be able
    to redirect the browse: the extra argument is harmlessly ignored, so the
    result is identical to a call without it.
    """
    (project_dir / "grimoire.toml").write_text("[skills]\n")
    runner = grim_at(project_dir)

    def _search(arguments: dict) -> dict:
        responses = _drive(
            runner,
            project_dir,
            [
                _initialize(1),
                {"jsonrpc": "2.0", "method": "notifications/initialized"},
                {
                    "jsonrpc": "2.0",
                    "id": 2,
                    "method": "tools/call",
                    "params": {"name": "grim_search", "arguments": arguments},
                },
            ],
        )
        return responses[2]["result"]

    baseline = _search({"query": "review"})
    injected = _search({"query": "review", "registry": "evil.example.com"})

    # The injected registry must not turn into an error that reveals a reach
    # to the unconfigured host, and must not change the result: the field is
    # simply not part of the tool's input surface.
    assert injected["isError"] is False, (
        f"an extra registry arg must be ignored, not error; got: {injected!r}"
    )
    assert injected["content"][0]["text"] == baseline["content"][0]["text"], (
        "an agent-supplied registry must have no effect on the browse result"
    )
