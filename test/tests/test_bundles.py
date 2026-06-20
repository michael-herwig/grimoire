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

    # Kind inferred as `bundle` from the manifest annotation; name defaults
    # to the reference's last segment (`stack`).
    out = runner.json("add", bundle.fq)
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


def _shared_member_setup(project_dir: Path, unique_repo: str):
    """Two bundles at DIFFERENT repos agreeing on one shared member, plus a
    member exclusive to each, declared as ``a`` and ``b``."""
    shared = _member_skill(unique_repo, "shared-skill")
    only_a = _member_skill(unique_repo, "only-a")
    only_b = _member_skill(unique_repo, "only-b")
    bundle_a = make_bundle(
        f"{unique_repo}/stack-a",
        [("skill", "shared-skill", shared.fq), ("skill", "only-a", only_a.fq)],
        tag="1.0.0",
    )
    bundle_b = make_bundle(
        f"{unique_repo}/stack-b",
        [("skill", "shared-skill", shared.fq), ("skill", "only-b", only_b.fq)],
        tag="1.0.0",
    )
    write_config(project_dir, bundles={"a": bundle_a.fq, "b": bundle_b.fq})


def test_remove_bundle_keeps_member_shared_with_other_bundle(
    grim_at, project_dir: Path, registry: str, unique_repo: str
) -> None:
    # Bundle `a` expands first (BTreeMap order), so the shared member's
    # provenance historically recorded only `a`. Removing `a` must NOT evict
    # the shared member — bundle `b` still declares it.
    _shared_member_setup(project_dir, unique_repo)
    runner = grim_at(project_dir)
    runner.run("lock")

    runner.json("remove", "bundle", "a")
    lock = (project_dir / "grimoire.lock").read_text()
    assert "only-a" not in lock, "the removed bundle's exclusive member is dropped"
    assert "shared-skill" in lock, "a member still held by another bundle survives"
    assert "only-b" in lock, "the sibling bundle's members are untouched"

    # Removing the LAST holder finally drops the shared member.
    runner.json("remove", "bundle", "b")
    lock = (project_dir / "grimoire.lock").read_text()
    assert "shared-skill" not in lock, "the last holder's removal evicts the member"
    assert "only-b" not in lock


def test_remove_bundle_shared_member_reverse_order(
    grim_at, project_dir: Path, registry: str, unique_repo: str
) -> None:
    # Same setup, but remove `b` (the NON-provenance-holder) first. The
    # outcome must be symmetric — eviction must not depend on which bundle
    # happened to stamp the coalesced lock entry.
    _shared_member_setup(project_dir, unique_repo)
    runner = grim_at(project_dir)
    runner.run("lock")

    runner.json("remove", "bundle", "b")
    lock = (project_dir / "grimoire.lock").read_text()
    assert "only-b" not in lock, "the removed bundle's exclusive member is dropped"
    assert "shared-skill" in lock, "a member still held by another bundle survives"
    assert "only-a" in lock

    runner.json("remove", "bundle", "a")
    lock = (project_dir / "grimoire.lock").read_text()
    assert "shared-skill" not in lock, "the last holder's removal evicts the member"


def test_remove_duplicate_binding_keeps_members(
    grim_at, project_dir: Path, registry: str, unique_repo: str
) -> None:
    # The SAME bundle (repo AND tag) declared under two binding names.
    # Removing one binding must not evict the members — the other binding
    # still declares the identical bundle.
    sk = _member_skill(unique_repo, "code-review")
    bundle = make_bundle(
        f"{unique_repo}/stack",
        [("skill", "code-review", sk.fq)],
        tag="1.0.0",
    )
    write_config(project_dir, bundles={"first": bundle.fq, "second": bundle.fq})
    runner = grim_at(project_dir)
    runner.run("lock")

    runner.json("remove", "bundle", "first")
    lock = (project_dir / "grimoire.lock").read_text()
    assert "code-review" in lock, "members survive while a duplicate binding remains"

    runner.json("remove", "bundle", "second")
    lock = (project_dir / "grimoire.lock").read_text()
    assert "code-review" not in lock, "removing the last binding evicts the members"


def test_status_shows_all_bundle_provenances_for_shared_member(
    grim_at, project_dir: Path, registry: str, unique_repo: str
) -> None:
    _shared_member_setup(project_dir, unique_repo)
    runner = grim_at(project_dir)
    runner.run("lock")

    rows = runner.json("status")
    member = next(r for r in rows if r["name"] == "shared-skill")
    assert member["source"].startswith("bundle:")
    assert f"{unique_repo}/stack-a" in member["source"], "first contributor listed"
    assert f"{unique_repo}/stack-b" in member["source"], "second contributor listed"
    # A shared member coalesces to ONE row even with two contributors.
    assert sum(1 for r in rows if r["name"] == "shared-skill") == 1


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


def test_remove_direct_keeps_artifact_held_by_bundle(
    grim_at, project_dir: Path, registry: str, unique_repo: str
) -> None:
    # The artifact is declared DIRECTLY and a declared bundle names it at
    # the SAME identifier. Removing the direct declaration must keep the
    # lock entry — the bundle still provides it (provenance flips from
    # direct to the bundle), with no network round-trip.
    sk = _member_skill(unique_repo, "code-review")
    bundle = make_bundle(
        f"{unique_repo}/stack",
        [("skill", "code-review", sk.fq)],
        tag="1.0.0",
    )
    write_config(
        project_dir,
        skills={"code-review": sk.fq},
        bundles={"stack": bundle.fq},
    )
    runner = grim_at(project_dir)
    runner.run("lock")

    rows = runner.json("status")
    before = next(r for r in rows if r["name"] == "code-review")
    assert before["source"] == "direct", "direct declaration wins while declared"

    runner.json("remove", "skill", "code-review")

    rows = runner.json("status")
    after = next((r for r in rows if r["name"] == "code-review"), None)
    assert after is not None, "the artifact survives — the bundle still holds it"
    assert after["source"].startswith("bundle:"), "provenance flips to the bundle"
    assert f"{unique_repo}/stack" in after["source"]
    assert after["state"] != "stale", "same-identifier flip needs no re-resolution"


def test_remove_direct_with_bundle_id_mismatch_goes_stale(
    grim_at, project_dir: Path, registry: str, unique_repo: str
) -> None:
    # The bundle names the artifact at a DIFFERENT identifier than the
    # direct declaration. While the direct pin wins, the bundle's variant
    # is never resolved — removing the direct declaration cannot produce
    # the correct pin offline. grim must NOT launder the lock: the entry is
    # dropped and the lock left stale so the next operation demands a
    # re-resolve instead of silently omitting the artifact.
    direct = make_artifact(
        f"{unique_repo}/code-review",
        "skill",
        {"code-review/SKILL.md": "---\nname: code-review\n---\n# direct\n"},
        tag="direct",
    )
    bundled = make_artifact(
        f"{unique_repo}/code-review",
        "skill",
        {"code-review/SKILL.md": "---\nname: code-review\n---\n# bundled\n"},
        tag="bundled",
    )
    bundle = make_bundle(
        f"{unique_repo}/stack",
        [("skill", "code-review", bundled.fq)],
        tag="1.0.0",
    )
    write_config(
        project_dir,
        skills={"code-review": direct.fq},
        bundles={"stack": bundle.fq},
    )
    runner = grim_at(project_dir)
    runner.run("lock")

    result = runner.run("remove", "skill", "code-review")
    assert "lock" in (result.stderr or "").lower(), "the user is told to re-resolve"

    # The lock is honestly stale: status surfaces it instead of reporting a
    # fresh lock that silently lost the bundle's variant.
    rows = runner.json("status")
    states = {r["name"]: r["state"] for r in rows}
    assert states.get("stack") == "stale", f"bundle row must surface staleness: {states}"

    # `grim lock` heals: the bundle's variant resolves in.
    runner.run("lock")
    rows = runner.json("status")
    healed = next(r for r in rows if r["name"] == "code-review")
    assert healed["source"].startswith("bundle:")
    assert bundled.digest in healed["pinned"]


def test_remove_standalone_skill_held_by_bundle_at_floating_tag_marks_stale_not_lost(
    grim_at, project_dir: Path, registry: str, unique_repo: str
) -> None:
    # Real-world repro: a skill is declared standalone at floating tag
    # :latest and a declared bundle pins the same skill at a different
    # floating tag :0. While the direct declaration wins, the bundle member
    # at :0 is never resolved. On `grim remove`, grim cannot prove which
    # content :0 resolves to offline, so the lock goes stale. The artifact
    # must NOT disappear — it is still provided by the bundle, awaiting a
    # `grim lock` re-resolve. The user must see an explanatory note that
    # names the bundle and says "stale" + "grim lock", not a dead-end error.
    #
    # Step 1: publish the skill at two different floating tags — same content
    # is fine; the id mismatch is on the tag string, not the digest.
    skill_at_zero = make_artifact(
        f"{unique_repo}/grim-authoring",
        "skill",
        {"grim-authoring/SKILL.md": "---\nname: grim-authoring\n---\n# v0\n"},
        tag="0",
    )
    skill_at_latest = make_artifact(
        f"{unique_repo}/grim-authoring",
        "skill",
        {"grim-authoring/SKILL.md": "---\nname: grim-authoring\n---\n# latest\n"},
        tag="latest",
    )

    # Step 2: publish a bundle that pins the member at :0.
    bundle = make_bundle(
        f"{unique_repo}/grim-essentials",
        [("skill", "grim-authoring", skill_at_zero.fq)],
        tag="1",
    )

    # Step 3: declare the skill standalone at :latest AND the bundle.
    write_config(
        project_dir,
        skills={"grim-authoring": skill_at_latest.fq},
        bundles={"grim-essentials": bundle.fq},
    )
    runner = grim_at(project_dir)
    runner.run("lock")

    rows = runner.json("status")
    before = next(r for r in rows if r["name"] == "grim-authoring")
    assert before["source"] == "direct", "standalone wins while declared at :latest"

    # Step 4: remove the standalone declaration.
    result = runner.run("remove", "skill", "grim-authoring")

    # Step 5: the stderr note must be explanatory — names the bundle, says
    # "stale" and "grim lock" — not a dead-end error phrase.
    stderr = result.stderr or ""
    assert "grim lock" in stderr.lower(), (
        f"note must tell user to run `grim lock`: {stderr!r}"
    )
    assert "stale" in stderr.lower(), (
        f"note must say the lock is stale: {stderr!r}"
    )
    assert "grim-essentials" in stderr, (
        f"note must name the bundle so the user knows what provides it: {stderr!r}"
    )

    # Step 6: the artifact is NOT lost — the bundle row is stale (awaiting
    # re-resolve), but status still surfaces the bundle row, not silence.
    rows = runner.json("status")
    states = {r["name"]: r["state"] for r in rows}
    assert states.get("grim-essentials") == "stale", (
        f"bundle row must surface staleness: {states}"
    )

    # Step 7: `grim lock` heals — the artifact resolves in from the bundle.
    runner.run("lock")
    rows = runner.json("status")
    healed = next((r for r in rows if r["name"] == "grim-authoring"), None)
    assert healed is not None, (
        "artifact must reappear after `grim lock` re-resolves the bundle member"
    )
    assert healed["source"].startswith("bundle:"), (
        "provenance must flip to the bundle after heal"
    )
    assert skill_at_zero.digest in healed["pinned"], (
        "the healed entry must be pinned to the bundle member's (:0) content digest"
    )


def test_uninstall_direct_keeps_lock_entry_held_by_bundle(
    grim_at, project_dir: Path, registry: str, unique_repo: str
) -> None:
    # `grim uninstall` deletes the files (explicit user intent), but the
    # lock entry survives when a declared bundle still names the artifact
    # at the same identifier — the next install rematerializes it.
    sk = _member_skill(unique_repo, "code-review")
    bundle = make_bundle(
        f"{unique_repo}/stack",
        [("skill", "code-review", sk.fq)],
        tag="1.0.0",
    )
    write_config(
        project_dir,
        skills={"code-review": sk.fq},
        bundles={"stack": bundle.fq},
    )
    (project_dir / ".claude").mkdir(exist_ok=True)
    runner = grim_at(project_dir)
    runner.run("lock")
    runner.run("install")
    assert_dir_exists(project_dir / ".claude" / "skills" / "code-review")

    runner.json("uninstall", "skill", "code-review")

    assert_not_exists(project_dir / ".claude" / "skills" / "code-review")
    rows = runner.json("status")
    after = next((r for r in rows if r["name"] == "code-review"), None)
    assert after is not None, "the lock entry survives via the bundle"
    assert after["source"].startswith("bundle:")
    assert after["state"] == "missing", "files deleted, still desired"


def test_release_bundle_with_agent_member(
    grim_at, project_dir: Path, registry: str, unique_repo: str
) -> None:
    # An AUTHORED bundle (.toml with an [agents] table) released through
    # `grim release` must carry the agent member onto the wire — the
    # wire-level make_bundle helper bypasses the authoring path and would
    # not catch a dropped table.
    sk = _member_skill(unique_repo, "code-review")
    agent = make_artifact(
        f"{unique_repo}/reviewer",
        "agent",
        {"reviewer.md": "---\nname: reviewer\ndescription: Reviews diffs.\n---\n# r\n"},
        tag="stable",
    )
    bundle_src = project_dir / "stack.toml"
    bundle_src.write_text(
        f'[skills]\ncode-review = "{sk.fq}"\n\n[agents]\nreviewer = "{agent.fq}"\n'
    )
    runner = grim_at(project_dir)
    runner.run("release", str(bundle_src), f"{registry}/{unique_repo}/stack:1.0.0")

    consumer = project_dir / "consumer"
    consumer.mkdir()
    write_config(consumer, bundles={"stack": f"{REGISTRY_HOST}/{unique_repo}/stack:1.0.0"})
    crunner = grim_at(consumer)
    crunner.run("lock")

    rows = crunner.json("status")
    agent_row = next((r for r in rows if r["kind"] == "agent" and r["name"] == "reviewer"), None)
    assert agent_row is not None, "the authored [agents] member must expand into the lock"
    assert agent_row["source"].startswith("bundle:")


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


def test_update_installs_added_bundle_member(
    grim_at, project_dir: Path, registry: str, unique_repo: str
) -> None:
    # A bundle at a floating tag rolls forward and ADDS a new member; the
    # plain update must install the added member to disk and lock it.
    sk_a = _member_skill(unique_repo, "skill-a")
    sk_b = _member_skill(unique_repo, "skill-b")
    bundle_repo = f"{unique_repo}/stack"
    # Initially the bundle carries only skill-a.
    make_bundle(bundle_repo, [("skill", "skill-a", sk_a.fq)], tag="rolling")
    write_config(project_dir, bundles={"stack": f"{REGISTRY_HOST}/{bundle_repo}:rolling"})
    runner = grim_at(project_dir)

    runner.run("lock")
    runner.run("install")
    assert_dir_exists(project_dir / ".claude/skills/skill-a")
    assert_not_exists(project_dir / ".claude/skills/skill-b")

    # The bundle tag rolls forward to a version that ADDS skill-b.
    make_bundle(
        bundle_repo,
        [("skill", "skill-a", sk_a.fq), ("skill", "skill-b", sk_b.fq)],
        tag="rolling",
    )

    rows = runner.json("update")
    added = next(r for r in rows if r["name"] == "skill-b")
    assert added["action"] == "updated"
    assert added["old"] is None, "a freshly added member had no previous pin"
    assert added["new"] is not None, "the added member is pinned by the update"

    # skill-b is materialized to disk and recorded in the lock; skill-a stays.
    assert_dir_exists(project_dir / ".claude/skills/skill-b")
    assert_path_exists(project_dir / ".claude/skills/skill-b/SKILL.md")
    assert_dir_exists(project_dir / ".claude/skills/skill-a")
    lock = (project_dir / "grimoire.lock").read_text()
    assert "skill-a" in lock
    assert "skill-b" in lock, "the added member enters the lock"


def test_update_adds_and_removes_in_one_upgrade(
    grim_at, project_dir: Path, registry: str, unique_repo: str
) -> None:
    # A single bundle upgrade simultaneously DROPS one member and ADDS
    # another. The update must materialize the addition and prune the
    # removal in one pass.
    sk_a = _member_skill(unique_repo, "skill-a")
    sk_b = _member_skill(unique_repo, "skill-b")
    sk_c = _member_skill(unique_repo, "skill-c")
    bundle_repo = f"{unique_repo}/stack"
    # Start with skill-a + skill-b.
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
    assert_not_exists(project_dir / ".claude/skills/skill-c")

    # Roll forward: drop skill-b, add skill-c (skill-a unchanged).
    make_bundle(
        bundle_repo,
        [("skill", "skill-a", sk_a.fq), ("skill", "skill-c", sk_c.fq)],
        tag="rolling",
    )

    rows = runner.json("update")
    by_name = {r["name"]: r for r in rows}
    assert by_name["skill-c"]["action"] == "updated"
    assert by_name["skill-c"]["old"] is None, "skill-c is a fresh addition"
    assert by_name["skill-b"]["action"] == "removed"
    assert by_name["skill-b"]["new"] is None, "a pruned row has no new pin"

    # Disk + lock reflect both the addition and the removal; skill-a is intact.
    assert_dir_exists(project_dir / ".claude/skills/skill-a")
    assert_dir_exists(project_dir / ".claude/skills/skill-c")
    assert_not_exists(project_dir / ".claude/skills/skill-b")
    lock = (project_dir / "grimoire.lock").read_text()
    assert "skill-a" in lock
    assert "skill-c" in lock, "the added member enters the lock"
    assert "skill-b" not in lock, "the dropped member leaves the lock"


def test_update_keeps_modified_dropped_member_without_force(
    grim_at, project_dir: Path, registry: str, unique_repo: str
) -> None:
    # A locally modified member that the bundle later drops must be
    # preserved on a plain update and flagged, mirroring the install
    # integrity gate. The preservation is durable: the user's edit, the
    # still-declared sibling tree, and the sibling's lock entry all survive
    # untouched (force-gated deletion is covered by the follow-up test).
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

    # The kept-modified prune must not touch the still-declared sibling:
    # skill-a's tree, its index file, and its lock entry all remain.
    assert_dir_exists(project_dir / ".claude/skills/skill-a")
    assert_path_exists(project_dir / ".claude/skills/skill-a/SKILL.md")
    lock = (project_dir / "grimoire.lock").read_text()
    assert "skill-a" in lock, "the still-declared sibling stays locked"


def test_update_force_prunes_modified_dropped_member(
    grim_at, project_dir: Path, registry: str, unique_repo: str
) -> None:
    # --force overrides the integrity gate: a locally modified member the
    # bundle dropped is pruned, while the still-declared sibling is left
    # untouched. This is the force follow-up to
    # test_update_keeps_modified_dropped_member_without_force.
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

    # --force prunes the modified orphan despite the local edit.
    rows = runner.json("update", "--force")
    pruned = next(r for r in rows if r["name"] == "skill-b")
    assert pruned["action"] == "removed"
    assert_not_exists(project_dir / ".claude/skills/skill-b")

    # Forced prune of the orphan must not touch the still-declared sibling.
    assert_dir_exists(project_dir / ".claude/skills/skill-a")
    assert_path_exists(project_dir / ".claude/skills/skill-a/SKILL.md")


def test_remote_flag_is_removed(grim_at, project_dir: Path) -> None:
    # The access-mode collapse removed `--remote`; it must no longer parse.
    write_config(project_dir)
    runner = grim_at(project_dir)
    result = runner.run("--remote", "status", check=False)
    assert result.returncode != 0, "the removed --remote flag must be rejected"
