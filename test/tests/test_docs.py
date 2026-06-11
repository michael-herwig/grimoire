# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The Grimoire Authors
"""Structural guards for the user documentation under ``docs/src/``.

Doc pages cross-link heavily (command reference, concepts, vendor
registries), and a renamed page, dropped section, or stale ``SUMMARY.md``
entry breaks readers silently — mdBook renders a broken link without
failing the build. These tests pin the structural invariants:

- every page on disk is reachable from ``SUMMARY.md`` and vice versa,
- every internal link points at an existing page,
- every internal anchor (``./page.md#section`` or same-page ``#section``)
  resolves to an explicit ``{#anchor}`` or a heading on the target page.

Runnable doc *workflows* (the quickstart chain etc.) are exercised
against the real binary in ``test_workflows.py``; this module checks
structure only.
"""
from __future__ import annotations

import re
from pathlib import Path

import pytest

from src.helpers import PROJECT_ROOT

_DOCS_DIR = PROJECT_ROOT / "docs" / "src"

# Inline links and reference-style definitions to a local page, with an
# optional anchor: `](./page.md#a)`, `]: ./page.md`, ...
_INTERNAL_LINK = re.compile(
    r"(?:\]\(|\]:\s*)\./(?P<page>[A-Za-z0-9._-]+\.md)(?:#(?P<anchor>[A-Za-z0-9._-]+))?"
)
# Same-page anchor links: `](#section)`.
_LOCAL_LINK = re.compile(r"\]\(#(?P<anchor>[A-Za-z0-9._-]+)\)")
# Explicit `{#anchor}` markers on headings.
_EXPLICIT_ANCHOR = re.compile(r"\{#(?P<anchor>[A-Za-z0-9._-]+)\}")
_HEADING = re.compile(r"^#{1,6}\s+(?P<text>.+?)\s*$", re.MULTILINE)


def _pages() -> list[Path]:
    return sorted(p for p in _DOCS_DIR.glob("*.md") if p.name != "SUMMARY.md")


def _strip_code_blocks(text: str) -> str:
    """Drop fenced code blocks — links inside them are samples, not links."""
    return re.sub(r"```.*?```", "", text, flags=re.DOTALL)


def _slugify(heading: str) -> str:
    """Approximate mdBook's auto-generated heading id.

    Lowercase; alphanumerics, ``-`` and ``_`` survive; whitespace becomes
    ``-``; everything else is dropped (matching mdBook's id generation
    closely enough for the anchors used in this book).
    """
    text = _EXPLICIT_ANCHOR.sub("", heading)
    text = text.replace("`", "").replace("*", "").strip().lower()
    out: list[str] = []
    for ch in text:
        if ch.isalnum() or ch in "-_":
            out.append(ch)
        elif ch.isspace():
            out.append("-")
    return "".join(out)


def _anchors(page: Path) -> set[str]:
    body = _strip_code_blocks(page.read_text())
    anchors = {m.group("anchor") for m in _EXPLICIT_ANCHOR.finditer(body)}
    anchors |= {_slugify(m.group("text")) for m in _HEADING.finditer(body)}
    return anchors


def test_summary_matches_pages_on_disk() -> None:
    """``SUMMARY.md`` lists exactly the pages that exist."""
    summary = (_DOCS_DIR / "SUMMARY.md").read_text()
    listed = {m.group("page") for m in _INTERNAL_LINK.finditer(summary)}
    on_disk = {p.name for p in _pages()}
    assert listed == on_disk, (
        f"SUMMARY.md and docs/src/ drifted apart — "
        f"only in SUMMARY: {sorted(listed - on_disk)}, "
        f"only on disk: {sorted(on_disk - listed)}"
    )


@pytest.mark.parametrize("page", _pages(), ids=lambda p: p.name)
def test_internal_links_resolve(page: Path) -> None:
    """Every internal link on the page hits an existing page and anchor."""
    body = _strip_code_blocks(page.read_text())
    problems: list[str] = []
    for m in _INTERNAL_LINK.finditer(body):
        target = _DOCS_DIR / m.group("page")
        if not target.is_file():
            problems.append(f"{m.group(0)!r}: page does not exist")
            continue
        anchor = m.group("anchor")
        if anchor and anchor not in _anchors(target):
            problems.append(
                f"{m.group(0)!r}: no '{{#{anchor}}}' or matching heading "
                f"in {target.name}"
            )
    for m in _LOCAL_LINK.finditer(body):
        anchor = m.group("anchor")
        if anchor not in _anchors(page):
            problems.append(f"(#{anchor}): no such anchor on this page")
    assert not problems, f"{page.name}: " + "; ".join(problems)
