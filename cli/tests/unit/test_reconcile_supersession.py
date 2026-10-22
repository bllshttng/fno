"""Supersession verdicts and PR merge-state queries over the surviving reconcile defs.

Split out of the retired test_pr_closure_producer.py: the closure producer
module is gone (superseded by the fno-agents binary commands), but the
reconcile-side defs these tests cover live on in fno.graph._reconcile.
"""
from __future__ import annotations

import json

import pytest

from fno.graph.store import read_graph_strict
from tests.fixtures.graph_seed import seed_graph


def _pending(node_id: str, successor: str, surfaces: list[str]) -> dict:
    return {
        "id": node_id,
        "superseded_by": successor,
        "supersession": {
            "successor": successor,
            "cause": "c",
            "surfaces": surfaces,
            "verified_at": None,
        },
    }


def test_dotfile_surface_does_not_match_a_dot_stripped_lookalike():
    """lstrip('./') ate the leading dot, so `github/ci.yml` matched `.github/ci.yml`."""
    from fno.graph._reconcile import verify_pending_supersessions

    entries = [_pending("x-old", "x-new", ["github/ci.yml"])]
    receipts = verify_pending_supersessions(
        entries, successor="x-new", changed_files=[".github/ci.yml"], evidence_pr=1
    )
    assert [r["kind"] for r in receipts] == ["supersession_unverified"]
    assert entries[0]["supersession"]["verified_at"] is None


def test_truncated_evidence_blames_the_truncation_not_the_surface():
    """A miss against a short file list is an absence with two explanations."""
    from fno.graph._reconcile import verify_pending_supersessions

    entries = [_pending("x-old", "x-new", ["cli/untouched.py"])]
    receipts = verify_pending_supersessions(
        entries,
        successor="x-new",
        changed_files=["cli/a.py"],
        evidence_pr=1,
        evidence_complete=False,
    )
    assert [r["kind"] for r in receipts] == ["supersession_evidence_truncated"]
    assert entries[0]["supersession"]["verified_at"] is None


def test_query_rest_timeout_is_retryable_availability_failure(monkeypatch):
    from fno.rust_binary import CommandUnavailable

    from fno.graph._reconcile import ReconcileError, query_pr_merge_state

    def _door(command, payload, timeout=None):
        raise CommandUnavailable(
            "fno-agents authorized-merge status-pr-info failed: "
            "timed out after 30.0s"
        )

    monkeypatch.setattr("fno.rust_binary.command_call", _door)
    with pytest.raises(ReconcileError) as raised:
        query_pr_merge_state(5, repo="o/r")

    assert raised.value.kind == "availability"
    assert raised.value.retryable is True
    assert "retry" in raised.value.remedy_for(pr_number=5, repo="o/r").lower()


def test_query_file_cap_is_non_retryable_incomplete_evidence():
    from fno.graph._reconcile import ReconcileError, query_pr_merge_state

    with pytest.raises(ReconcileError) as raised:
        query_pr_merge_state(
            5,
            repo="o/r",
            info_reader=lambda number, *, repo=None, cwd=None: ({
                "pr": 5,
                "state": "MERGED",
                "url": "u",
                "merged_at": "t",
                "merge_sha": "sha",
            }, ""),
            files_reader=lambda number, *, repo=None, cwd=None: (
                None,
                "gh api pull files reached GitHub's 3,000-file cap without a short page",
            ),
            include_files=True,
        )

    assert raised.value.kind == "evidence_incomplete"
    assert raised.value.retryable is False
    assert "force --reason" in raised.value.remedy_for(pr_number=5, repo="o/r")


def test_query_rejects_missing_required_reader_metadata():
    from fno.graph._reconcile import ReconcileError, query_pr_merge_state

    with pytest.raises(ReconcileError) as raised:
        query_pr_merge_state(
            5,
            repo="o/r",
            info_reader=lambda number, *, repo=None, cwd=None: ({"url": "u"}, ""),
            files_reader=lambda number, *, repo=None, cwd=None: ([], ""),
        )

    assert raised.value.kind == "malformed"
    assert raised.value.retryable is False


def _native_supersede(graph, *args: str):
    """The supersede leaf answers natively; drive the dev binary over the
    conftest-sandboxed store (the graph path is frozen at import time)."""
    import os as _os
    import subprocess as _sp

    import pytest as _pytest

    from fno.rust_binary import find_dev_binary, resolve_binary

    binary = find_dev_binary() or resolve_binary()
    if binary is None:
        _pytest.skip("no fno-agents dev build (cargo build -p fno-agents)")
    # The child's cwd is the store dir; a caller that never seeded still needs
    # it to exist, or the exec fails ENOENT against the cwd itself.
    graph.parent.mkdir(parents=True, exist_ok=True)
    proc = _sp.run(
        [str(binary), "backlog", "supersede", *args],
        capture_output=True,
        text=True,
        timeout=60,
        env={
            "PATH": _os.environ["PATH"],
            "HOME": str(graph.parent),
            "FNO_STATE_DIR": str(graph.parent),
            "FNO_TRACKER_BACKEND": "graph",
        },
        cwd=str(graph.parent),
    )
    return proc.returncode, proc.stdout + proc.stderr


def test_supersede_keeps_the_human_reason(tmp_path, monkeypatch):
    """--reason is accepted and documented, so it must land somewhere."""
    import json

    # The graph path is frozen at import time (see conftest), so point at the
    # already-sandboxed location rather than a fresh env var.
    from fno.graph._constants import GRAPH_JSON as graph

    graph.parent.mkdir(parents=True, exist_ok=True)
    seed_graph(graph, json.dumps({"entries": [
        {"id": "x-1a2b", "title": "new"},
        {"id": "x-9f0c", "title": "old"},
    ]}))

    code, out = _native_supersede(graph,
        "x-1a2b", "--replaces", "x-9f0c",
        "--cause", "owned the parser", "--surface", "cli/p.py",
        "--reason", "folded into the rewrite", "--covers", "the rewrite owns the parser",
    )
    assert code == 0, out
    rows = {e["id"]: e for e in read_graph_strict(graph)}
    assert rows["x-9f0c"]["supersession"]["reason"] == (
        "folded into the rewrite; covers: the rewrite owns the parser"
    )


def test_supersede_without_surface_names_a_runnable_example():
    from fno.graph._constants import GRAPH_JSON as graph

    code, out = _native_supersede(graph,
        "x-1a2b", "--replaces", "x-9f0c", "--cause", "c",
    )
    assert code == 1
    assert "--surface" in out
    assert "fno backlog supersede" in out


def test_a_closed_successor_still_owes_its_predecessor_a_verdict():
    """The close path is not the only way a successor reaches done."""
    from fno.graph._reconcile import successors_owing_verification

    entries = [
        {"id": "x-9f0c", "superseded_by": "x-1a2b",
         "supersession": {"successor": "x-1a2b", "surfaces": ["cli/p.py"], "verified_at": None}},
        {"id": "x-1a2b", "completed_at": "2026-08-20T00:00:00Z", "pr_number": 7},
    ]
    assert list(successors_owing_verification(entries)) == ["x-1a2b"]
