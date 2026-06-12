# /// script
# requires-python = ">=3.11"
# dependencies = []
# ///
"""Publish first-party catalog packages per catalog/publish.toml.

Drives ``grim release`` for every package listed in the manifest, in
dependency order: skills, rules, and agents first (sorted by name), bundles
last — a bundle's members must exist before anyone resolves the bundle.

The grim binary comes from the ``GRIM_COMMAND`` env var (default: ``grim``
on PATH). Pass ``--dry-run`` to validate + pack + plan without pushing,
``--force`` to move existing exact-version tags.
"""

from __future__ import annotations

import argparse
import os
import re
import subprocess
import sys
import tomllib
from pathlib import Path

CATALOG_DIR = Path(__file__).resolve().parent.parent
SEMVER = re.compile(r"^\d+\.\d+\.\d+$")

# kind -> (manifest table, source path template, registry namespace, extra args)
KINDS: list[tuple[str, str, str, list[str]]] = [
    ("skills", "skills/{name}", "skills", []),
    ("rules", "rules/{name}.md", "rules", ["--kind", "rule"]),
    ("agents", "agents/{name}.md", "agents", ["--kind", "agent"]),
    ("bundles", "bundles/{name}.toml", "bundles", ["--kind", "bundle"]),
]


def load_manifest(path: Path) -> dict:
    with path.open("rb") as fh:
        return tomllib.load(fh)


def plan_releases(manifest: dict) -> list[tuple[Path, str, list[str]]]:
    """Return (source_path, reference, extra_args) per package, in publish order."""
    registry = manifest.get("registry")
    if not isinstance(registry, str) or not registry:
        sys.exit("publish.toml: missing or empty 'registry'")

    releases: list[tuple[Path, str, list[str]]] = []
    for table, path_template, namespace, extra in KINDS:
        entries = manifest.get(table, {})
        if not isinstance(entries, dict):
            sys.exit(f"publish.toml: [{table}] must be a table of name = version")
        for name in sorted(entries):
            version = entries[name]
            if not isinstance(version, str) or not SEMVER.match(version):
                sys.exit(f"publish.toml: [{table}] {name}: bad version {version!r} (want X.Y.Z)")
            source = CATALOG_DIR / path_template.format(name=name)
            if not source.exists():
                sys.exit(f"publish.toml: [{table}] {name}: source missing at {source}")
            releases.append((source, f"{registry}/{namespace}/{name}:{version}", extra))
    if not releases:
        sys.exit("publish.toml: no packages declared")
    return releases


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--dry-run", action="store_true", help="validate + pack + plan, no push")
    parser.add_argument("--force", action="store_true", help="move existing exact-version tags")
    parser.add_argument(
        "--manifest",
        type=Path,
        default=CATALOG_DIR / "publish.toml",
        help="publish manifest path (default: catalog/publish.toml)",
    )
    args = parser.parse_args()

    grim = os.environ.get("GRIM_COMMAND", "grim")
    releases = plan_releases(load_manifest(args.manifest))

    mode = "dry-run" if args.dry_run else "publish"
    print(f"{mode}: {len(releases)} package(s) via {grim}")
    for source, reference, extra in releases:
        cmd = [grim, "release", str(source), reference, *extra]
        if args.dry_run:
            cmd.append("--dry-run")
        if args.force:
            cmd.append("--force")
        print(f"==> {' '.join(cmd)}")
        result = subprocess.run(cmd)
        if result.returncode != 0:
            print(f"release failed for {reference} (exit {result.returncode})", file=sys.stderr)
            return result.returncode
    return 0


if __name__ == "__main__":
    sys.exit(main())
