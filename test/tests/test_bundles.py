# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The Grimoire Authors
"""Bundle acceptance tests: expansion, conflict policy, provenance, publish."""
from __future__ import annotations

from pathlib import Path

from src.assertions import assert_dir_exists, assert_not_exists, assert_path_exists
from src.helpers import make_artifact, make_bundle, write_config
from src.registry import REGISTRY_HOST, retag, tag_digest


def _member_skill(unique_repo: str, name: str, body: str = "CR", tag: str = "stable"):
    return make_artifact(
        f"{unique_repo}/{name}",
        "skill",
        {f"{name}/SKILL.md": f"---\nname: {name}\n---\n# {body}\n"},
        tag=tag,
    )


def _member_rule(unique_repo: str, name: str, tag: str = "v1"):
    return make_artifact(
        f"{unique_repo}/{name}",
        "rule",
        {f"{name}.md": "---\npaths: ['**/*.rs']\n---\n# rule\n"},
        tag=tag,
    )


def test_lock_expands_bundle_members(
    grim_at, project_dir: Path, registry: str, unique_repo: str
) -> None:
    sk = _member_skill(unique_repo, "code-review")
    ru = _member_rule(unique_repo, "rust-style")
    bundle = make_bundle(
        f"{unique_repo}/python-stack",
        [("skill", "code-review", sk.fq), ("rule", "rust-style", ru.fq)],
        tag="1.0.0",
    )
    write_config(project_dir, bundles={"python-stack": bundle.fq})
    runner = grim_at(project_dir)

    runner.run("lock")

    lock = (project_dir / "grimoire.lock").read_text()
    # Both members are expanded into the lock, pinned by digest, and carry
    # bundle provenance.
    assert "code-review" in lock
    assert "rust-style" in lock
    assert "@sha256:" in lock
    assert f"bundle = \"{REGISTRY_HOST}/{unique_repo}/python-stack\"" in lock


def test_install_materializes_bundle_members(
    grim_at, project_dir: Path, registry: str, unique_repo: str
) -> None:
    sk = _member_skill(unique_repo, "code-review")
    ru = _member_rule(unique_repo, "rust-style")
    bundle = make_bundle(
        f"{unique_repo}/stack",
        [("skill", "code-review", sk.fq), ("rule", "rust-style", ru.fq)],
        tag="1.0.0",
    )
    write_config(project_dir, bundles={"stack": bundle.fq})
    runner = grim_at(project_dir)

    runner.run("lock")
    rows = runner.json("install")
    assert {r["status"] for r in rows} == {"installed"}

    assert_dir_exists(project_dir / ".claude/skills/code-review")
    assert_path_exists(project_dir / ".claude/skills/code-review/SKILL.md")
    assert_path_exists(project_dir / ".claude/rules/rust-style.md")


def test_status_shows_bundle_provenance(
    grim_at, project_dir: Path, registry: str, unique_repo: str
) -> None:
    sk = _member_skill(unique_repo, "code-review")
    bundle = make_bundle(
        f"{unique_repo}/stack",
        [("skill", "code-review", sk.fq)],
        tag="1.0.0",
    )
    write_config(project_dir, bundles={"stack": bundle.fq})
    runner = grim_at(project_dir)
    runner.run("lock")

    rows = runner.json("status")
    member = next(r for r in rows if r["name"] == "code-review")
    assert member["source"].startswith("bundle:")
    assert f"{unique_repo}/stack" in member["source"]
    # The bundle declaration itself is reported as a bundle-kind row.
    bundle_row = next(r for r in rows if r["kind"] == "bundle")
    assert bundle_row["name"] == "stack"
    assert bundle_row["source"] == "direct"


def test_direct_declaration_wins_over_bundle(
    grim_at, project_dir: Path, registry: str, unique_repo: str
) -> None:
    # Both the bundle and a direct declaration name "code-review", at
    # different versions. The direct declaration must win, with no conflict.
    bundle_member = _member_skill(unique_repo, "code-review", body="bundle", tag="bundled")
    direct_member = make_artifact(
        f"{unique_repo}/code-review",
        "skill",
        {"code-review/SKILL.md": "---\nname: code-review\n---\n# direct\n"},
        tag="direct",
    )
    bundle = make_bundle(
        f"{unique_repo}/stack",
        [("skill", "code-review", bundle_member.fq)],
        tag="1.0.0",
    )
    write_config(
        project_dir,
        skills={"code-review": direct_member.fq},
        bundles={"stack": bundle.fq},
    )
    runner = grim_at(project_dir)
    runner.run("lock")

    # The direct pin wins; the lock entry is NOT marked as a bundle member.
    rows = runner.json("status")
    cr = next(r for r in rows if r["kind"] == "skill" and r["name"] == "code-review")
    assert cr["source"] == "direct"
    assert direct_member.digest in cr["pinned"]


def test_agreeing_bundles_coalesce(
    grim_at, project_dir: Path, registry: str, unique_repo: str
) -> None:
    sk = _member_skill(unique_repo, "code-review")
    bundle_a = make_bundle(
        f"{unique_repo}/stack-a",
        [("skill", "code-review", sk.fq)],
        tag="1.0.0",
    )
    bundle_b = make_bundle(
        f"{unique_repo}/stack-b",
        [("skill", "code-review", sk.fq)],
        tag="1.0.0",
    )
    write_config(project_dir, bundles={"a": bundle_a.fq, "b": bundle_b.fq})
    runner = grim_at(project_dir)
    runner.run("lock")

    rows = runner.json("status")
    members = [r for r in rows if r["kind"] == "skill" and r["name"] == "code-review"]
    assert len(members) == 1, "identical members from two bundles coalesce"


def test_disagreeing_bundles_fail_closed(
    grim_at, project_dir: Path, registry: str, unique_repo: str
) -> None:
    member_a = _member_skill(unique_repo, "code-review-a", tag="stable")
    member_b = _member_skill(unique_repo, "code-review-b", tag="stable")
    # Both bundles bind the SAME member name to DIFFERENT identifiers.
    bundle_a = make_bundle(
        f"{unique_repo}/stack-a",
        [("skill", "code-review", member_a.fq)],
        tag="1.0.0",
    )
    bundle_b = make_bundle(
        f"{unique_repo}/stack-b",
        [("skill", "code-review", member_b.fq)],
        tag="1.0.0",
    )
    write_config(project_dir, bundles={"a": bundle_a.fq, "b": bundle_b.fq})
    runner = grim_at(project_dir)

    result = runner.run("lock", check=False)
    # A bundle conflict is a misconfiguration of the user's declaration.
    assert result.returncode == 78, "a bundle conflict is a config error"
    assert "conflict" in (result.stderr + result.stdout).lower()


def test_add_bundle_declares_and_locks(
    grim_at, project_dir: Path, registry: str, unique_repo: str
) -> None:
    sk = _member_skill(unique_repo, "code-review")
    bundle = make_bundle(
        f"{unique_repo}/stack",
        [("skill", "code-review", sk.fq)],
        tag="1.0.0",
    )
    write_config(project_dir)
    runner = grim_at(project_dir)

    out = runner.json("add", "bundle", "stack", bundle.fq)
    assert out["kind"] == "bundle"
    assert out["name"] == "stack"

    cfg = (project_dir / "grimoire.toml").read_text()
    assert "[bundles]" in cfg
    assert "stack" in cfg
    # The member was expanded into the lock.
    lock = (project_dir / "grimoire.lock").read_text()
    assert "code-review" in lock


def test_remove_bundle_drops_members(
    grim_at, project_dir: Path, registry: str, unique_repo: str
) -> None:
    sk = _member_skill(unique_repo, "code-review")
    bundle = make_bundle(
        f"{unique_repo}/stack",
        [("skill", "code-review", sk.fq)],
        tag="1.0.0",
    )
    write_config(project_dir, bundles={"stack": bundle.fq})
    runner = grim_at(project_dir)
    runner.run("lock")
    assert "code-review" in (project_dir / "grimoire.lock").read_text()

    out = runner.json("remove", "bundle", "stack")
    assert out["status"] == "removed"

    cfg = (project_dir / "grimoire.toml").read_text()
    assert "stack" not in cfg
    lock = (project_dir / "grimoire.lock").read_text()
    assert "code-review" not in lock, "removing the bundle drops its members"


def test_remove_bundle_keeps_sibling_at_same_repo(
    grim_at, project_dir: Path, registry: str, unique_repo: str
) -> None:
    # Two bundles at the SAME repo but different tags, with disjoint
    # members. Removing one must not evict the other's members.
    sk_a = _member_skill(unique_repo, "skill-a")
    sk_b = _member_skill(unique_repo, "skill-b")
    bundle_repo = f"{unique_repo}/stack"
    make_bundle(bundle_repo, [("skill", "skill-a", sk_a.fq)], tag="1.0")
    make_bundle(bundle_repo, [("skill", "skill-b", sk_b.fq)], tag="2.0")
    write_config(
        project_dir,
        bundles={
            "stack-v1": f"{REGISTRY_HOST}/{bundle_repo}:1.0",
            "stack-v2": f"{REGISTRY_HOST}/{bundle_repo}:2.0",
        },
    )
    runner = grim_at(project_dir)
    runner.run("lock")

    runner.json("remove", "bundle", "stack-v1")
    lock = (project_dir / "grimoire.lock").read_text()
    assert "skill-a" not in lock, "the removed bundle's member is dropped"
    assert "skill-b" in lock, "the sibling bundle's member at the same repo is preserved"


def test_release_bundle_pin_freezes_members(
    grim_at, project_dir: Path, registry: str, unique_repo: str
) -> None:
    # A member published at :stable (digest A).
    member = _member_skill(unique_repo, "code-review", body="v1", tag="stable")
    member_v1_digest = tag_digest(f"{unique_repo}/code-review", "stable")

    # Author a bundle source referencing the floating member tag, and
    # release it with --pin so the member is frozen to digest A.
    bundle_src = project_dir / "stack.toml"
    bundle_src.write_text(f'[skills]\ncode-review = "{member.fq}"\n')
    runner = grim_at(project_dir)
    runner.run("release", str(bundle_src), f"{registry}/{unique_repo}/stack:1.0.0", "--pin")

    # The member tag rolls forward to new content (digest B).
    member_v2 = make_artifact(
        f"{unique_repo}/code-review",
        "skill",
        {"code-review/SKILL.md": "---\nname: code-review\n---\n# v2\n"},
        tag="2.0.0",
    )
    retag(f"{unique_repo}/code-review", "stable", member_v2.digest)

    # A consumer of the pinned bundle still resolves the member to digest A,
    # because --pin baked the digest into the published bundle.
    consumer = project_dir / "consumer"
    consumer.mkdir()
    write_config(consumer, bundles={"stack": f"{REGISTRY_HOST}/{unique_repo}/stack:1.0.0"})
    crunner = grim_at(consumer)
    crunner.run("lock")
    lock = (consumer / "grimoire.lock").read_text()
    assert member_v1_digest in lock, "pinned member stays frozen despite the tag move"


def test_update_prunes_dropped_bundle_member(
    grim_at, project_dir: Path, registry: str, unique_repo: str
) -> None:
    # A bundle at a floating tag with two members; the consumer installs
    # both, then the bundle rolls forward and drops one member.
    sk_a = _member_skill(unique_repo, "skill-a")
    sk_b = _member_skill(unique_repo, "skill-b")
    bundle_repo = f"{unique_repo}/stack"
    make_bundle(
        bundle_repo,
        [("skill", "skill-a", sk_a.fq), ("skill", "skill-b", sk_b.fq)],
        tag="rolling",
    )
    write_config(project_dir, bundles={"stack": f"{REGISTRY_HOST}/{bundle_repo}:rolling"})
    runner = grim_at(project_dir)

    runner.run("lock")
    runner.run("install")
    assert_dir_exists(project_dir / ".claude/skills/skill-a")
    assert_dir_exists(project_dir / ".claude/skills/skill-b")

    # The bundle tag rolls forward to a version that no longer includes
    # skill-b.
    make_bundle(bundle_repo, [("skill", "skill-a", sk_a.fq)], tag="rolling")

    rows = runner.json("update")
    dropped = next(r for r in rows if r["name"] == "skill-b")
    assert dropped["action"] == "removed"
    assert dropped["new"] is None, "a pruned row has no new pin"

    # skill-b is gone from disk and the lock; skill-a stays.
    assert_dir_exists(project_dir / ".claude/skills/skill-a")
    assert_not_exists(project_dir / ".claude/skills/skill-b")
    lock = (project_dir / "grimoire.lock").read_text()
    assert "skill-a" in lock
    assert "skill-b" not in lock, "the dropped member leaves the lock"


def test_update_keeps_modified_dropped_member_without_force(
    grim_at, project_dir: Path, registry: str, unique_repo: str
) -> None:
    # A locally modified member that the bundle later drops must be
    # preserved on a plain update and only pruned with --force, mirroring
    # the install integrity gate.
    sk_a = _member_skill(unique_repo, "skill-a")
    sk_b = _member_skill(unique_repo, "skill-b")
    bundle_repo = f"{unique_repo}/stack"
    make_bundle(
        bundle_repo,
        [("skill", "skill-a", sk_a.fq), ("skill", "skill-b", sk_b.fq)],
        tag="rolling",
    )
    write_config(project_dir, bundles={"stack": f"{REGISTRY_HOST}/{bundle_repo}:rolling"})
    runner = grim_at(project_dir)
    runner.run("lock")
    runner.run("install")

    # Hand-edit the member, then drop it from the bundle.
    edited = project_dir / ".claude/skills/skill-b/SKILL.md"
    edited.write_text(edited.read_text() + "\n# local edit\n")
    make_bundle(bundle_repo, [("skill", "skill-a", sk_a.fq)], tag="rolling")

    # A plain update preserves the modified orphan and flags it.
    rows = runner.json("update")
    kept = next(r for r in rows if r["name"] == "skill-b")
    assert kept["action"] == "kept-modified"
    assert_path_exists(edited)
    assert "# local edit" in edited.read_text(), "the user's edit survives"

    # --force prunes it despite the modification.
    rows = runner.json("update", "--force")
    pruned = next(r for r in rows if r["name"] == "skill-b")
    assert pruned["action"] == "removed"
    assert_not_exists(project_dir / ".claude/skills/skill-b")


def test_remote_flag_is_removed(grim_at, project_dir: Path) -> None:
    # The access-mode collapse removed `--remote`; it must no longer parse.
    write_config(project_dir)
    runner = grim_at(project_dir)
    result = runner.run("--remote", "status", check=False)
    assert result.returncode != 0, "the removed --remote flag must be rejected"
