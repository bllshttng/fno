"""`fno backlog reopen` - the inverse of `done`, and its refusals.

The command is native (backlog/workflows.rs), so these scenarios drive the door
(the fno-agents binary against a sandboxed store), the way the golden
receipts do. The refusals are the point of the command, so most of this file is
about the cases it declines rather than the case it permits. A correction
command that permits everything is a hand-edit with a nicer name, and
hand-editing the graph is what the PreToolUse hook already forbids.

The wheel-level drive-audit receipt (`backlog_done_operator_initiated`)
moved into the binary with the command; a sandbox has no space dir for the
event journal, so that tail is not door-observable and has no python
monkeypatch surface left to pin.
"""
from __future__ import annotations
from tests.fixtures.graph_seed import seed_graph

import json
from pathlib import Path

import pytest

from tests.goldens._door import door, make_sandbox, warm, write_pr_stub
from fno.graph.store import read_graph_strict


def sandbox(tmp_path: Path, *entries: dict) -> Path:
    root = make_sandbox(tmp_path, list(entries))
    warm(root, entries[0]["id"] if entries else "ab-00000000")
    return root


def _write(root: Path, *entries: dict) -> None:
    seed_graph(root / "graph.json", json.dumps({"entries": list(entries)}))


def _read(root: Path) -> dict[str, dict]:
    return {e["id"]: e for e in read_graph_strict(root / "graph.json")}


def _node(nid: str, **over) -> dict:
    base = {
        "id": nid,
        "title": f"node {nid}",
        "slug": nid,
        "type": "feature",
        "status": "done",
        "completed_at": "2026-08-01T00:00:00+00:00",
        "domain": "code",
        "priority": "p2",
        "created_at": "2026-07-01T00:00:00+00:00",
    }
    base.update(over)
    return base


# -- the permitted case --


def test_a_node_closed_in_error_reopens(tmp_path):
    root = sandbox(tmp_path, _node("ab-11111111"))
    code, out, err = door(root, ["reopen", "ab-11111111", "--reason", "closed by mistake"])
    assert code == 0, err
    node = _read(root)["ab-11111111"]
    assert node["completed_at"] is None
    assert node["reopened_reason"] == "closed by mistake"
    assert node["reopened_at"]


# -- what it deliberately does not undo --


# -- refusals --


def test_a_merged_additional_pr_refuses_even_when_the_primary_is_not(tmp_path):
    """The gate has to see every ref, the way done's does: a node can close on
    a merged additional_prs entry while its primary sits closed and unmerged."""
    root = sandbox(
        tmp_path,
        _node(
            "ab-11111111",
            pr_number=41,
            pr_url="https://github.com/o/r/pull/41",
            additional_prs=[{"number": 42, "url": "https://github.com/o/r/pull/42"}],
        ),
    )
    write_pr_stub(root, {41: "CLOSED", 42: "MERGED"})
    code, out, err = door(root, ["reopen", "ab-11111111", "--reason", "x"], path_prepend=str(root / "stubbin"))
    assert code == 3, err
    assert _read(root)["ab-11111111"]["completed_at"] is not None


def test_a_forced_reopen_names_the_pr_that_produced_the_state(tmp_path):
    """The forced-reopen warning has to name the ref that actually merged,
    which is not necessarily the primary."""
    root = sandbox(
        tmp_path,
        _node(
            "ab-11111111",
            pr_number=41,
            pr_url="https://github.com/o/r/pull/41",
            additional_prs=[{"number": 42, "url": "https://github.com/o/r/pull/42"}],
        ),
    )
    write_pr_stub(root, {41: "CLOSED", 42: "MERGED"})
    code, out, err = door(root, ["reopen", "ab-11111111", "--reason", "x", "-F"], path_prepend=str(root / "stubbin"))
    assert code == 0, err
    assert "pull/42" in err


def test_a_gh_outage_leaves_the_node_done(tmp_path):
    """An unreachable gh is a missing answer, not a permitting one (exit 4,
    the retryable-outage slot)."""
    root = sandbox(tmp_path, _node("ab-11111111", pr_number=7, pr_url="https://github.com/o/r/pull/7"))
    write_pr_stub(root, {7: "OPEN"}, fail_stderr="gh: network unreachable")
    code, out, err = door(root, ["reopen", "ab-11111111", "--reason", "x"], path_prepend=str(root / "stubbin"))
    assert code == 4
    assert _read(root)["ab-11111111"]["completed_at"] is not None


def test_the_archived_refusal_survives_a_partial_id(tmp_path):
    """An exact compare here recreates the ambiguity the refusal exists to
    remove: `reopen ab-2222` must not report "not found" for a node sitting
    readable in the archive."""
    root = sandbox(tmp_path)
    (root / "graph-archive.json").write_text(json.dumps({"entries": [_node("ab-22222222")]}))
    code, out, err = door(root, ["reopen", "ab-2222", "--reason", "x"])
    assert code == 4, err
    assert "archived" in err


# -- the cascade --


def test_the_cascade_still_fires_for_a_partial_id(tmp_path):
    """The partial id resolves to the full id; the cascade walks full ids.
    Passing the argument straight through left an auto-closed epic done over
    a live child - a silent wrong answer, not an error."""
    root = sandbox(
        tmp_path,
        _node("ab-e0000000", type="epic", completion_note="auto-closed: all children complete"),
        _node("ab-c0000000", parent="ab-e0000000"),
    )
    code, out, err = door(root, ["reopen", "ab-c000", "--reason", "wrong"])
    assert code == 0, err
    nodes = _read(root)
    assert nodes["ab-c0000000"]["completed_at"] is None
    assert nodes["ab-e0000000"]["completed_at"] is None


def test_an_epic_closed_on_its_own_evidence_is_left_done_and_named(tmp_path):
    """Silently reopening it would discard a judgment this command never made."""
    root = sandbox(
        tmp_path,
        _node("ab-e0000000", type="epic", completion_note="closed by operator after review"),
        _node("ab-c0000000", parent="ab-e0000000"),
    )
    code, out, err = door(root, ["reopen", "ab-c0000000", "--reason", "wrong"])
    assert code == 0, err
    nodes = _read(root)
    assert nodes["ab-c0000000"]["completed_at"] is None
    assert nodes["ab-e0000000"]["completed_at"] is not None
    assert "ab-e0000000" in out
    assert "own evidence" in out


def test_a_close_reopen_close_cycle_re_annotates_the_epic(tmp_path):
    """The regression the completion_note clear exists to prevent.

    If reopen left prose in completion_note, the close cascade would skip its
    `auto-closed:` write on the second close (it only writes when the field is
    empty), and a second reopen would leave the epic done under a live child.
    """
    from fno.graph.cli import _apply_completion_fields, _cascade_close_parents

    root = sandbox(
        tmp_path,
        _node("ab-e0000000", type="epic", completion_note="auto-closed: all children complete"),
        _node("ab-c0000000", parent="ab-e0000000"),
    )
    code, out, err = door(root, ["reopen", "ab-c0000000", "--reason", "wrong"])
    assert code == 0, err

    # Re-close the child the way `done` does, then let the cascade run.
    live = list(read_graph_strict(root / "graph.json"))
    child = next(e for e in live if e["id"] == "ab-c0000000")
    _apply_completion_fields(child)
    _cascade_close_parents(live, "ab-c0000000")
    epic = next(e for e in live if e["id"] == "ab-e0000000")
    assert epic["completed_at"] is not None
    assert str(epic["completion_note"]).startswith("auto-closed:")


# -- the plan doc, projected for real --


def test_the_plan_doc_comes_off_terminal_done(tmp_path):
    """The P1 the stubbed fixture hid: clearing completed_at in the graph
    while the plan stays stamped `done` leaves dispatch refusing the node.
    The projector is forward-only, so reopen forces the plan off terminal the
    way unsupersede does."""
    root = sandbox(tmp_path, _node("ab-11111111"))
    plan = tmp_path / "plan.md"
    plan.write_text("---\nstatus: done\ndone_at: 2026-08-01T00:00:00Z\n---\n\n# a plan\n")
    _write(root, _node("ab-11111111", plan_path=str(plan)))

    code, out, err = door(root, ["reopen", "ab-11111111", "--reason", "wrong"])
    assert code == 0, err
    assert "status: done" not in plan.read_text()


# -- the event --


# -- update projects difficulty (restored from the x-dd1f branch; main moved
# its own difficulty coverage to test_graph_status.py) --

@pytest.mark.real_plan_projection
@pytest.mark.parametrize(
    ("doc_seed", "node_over", "args", "expect_value"),
    [
        pytest.param(
            None, {}, ("difficulty=high",), "high",
            id="set-writes-the-doc",
        ),
        pytest.param(
            "difficulty: medium", {"difficulty": None}, ("priority=p1",), "medium",
            id="persisted-null-band-survives-a-priority-edit",
        ),
    ],
)
def test_update_mirrored_keys_project_to_the_plan_doc(
    tmp_path, doc_seed, node_over, args, expect_value
):
    """`difficulty=` is a mirrored key, so an edit must trigger the projection
    like `priority=` does; a persisted difficulty: null must not delete a band
    the doc authored since."""
    front = "---\nstatus: ready\ncreated: 2026-05-05\n"
    if doc_seed:
        front += f"{doc_seed}\n"
    plan = tmp_path / "plan.md"
    plan.write_text(front + "---\n\n# a plan\n")
    root = sandbox(
        tmp_path,
        _node("ab-11111111", status="ready", completed_at=None, plan_path=str(plan), **node_over),
    )

    code, out = _native_update(root, "ab-11111111", *args)
    assert code == 0, out
    assert f"difficulty: {expect_value}" in plan.read_text()


def _native_update(root: Path, *args: str):
    """The update leaf answers natively; drive the dev binary over the sandbox
    the fixture seeded (in-process monkeypatches cannot reach a subprocess)."""
    import os as _os
    import subprocess as _sp

    from fno.rust_binary import find_dev_binary, resolve_binary

    binary = find_dev_binary() or resolve_binary()
    if binary is None:
        pytest.skip("no fno-agents dev build (cargo build -p fno-agents)")
    proc = _sp.run(
        [str(binary), "backlog", "update", *args],
        capture_output=True,
        text=True,
        timeout=60,
        env={
            "PATH": _os.environ["PATH"],
            "HOME": str(root),
            "FNO_CONFIG": str(root / "config.toml"),
            "FNO_GLOBAL_SETTINGS_PATH": "/dev/null",
            "FNO_TRACKER_BACKEND": "graph",
            "FNO_CLAIMS_ROOT": str(root / "claims"),
        },
        cwd=str(root),
    )
    return proc.returncode, proc.stdout + proc.stderr
