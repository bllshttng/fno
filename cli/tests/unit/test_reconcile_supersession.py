"""Supersession verdicts and PR merge-state queries over the surviving reconcile defs.

Split out of the retired test_pr_closure_producer.py: the closure producer
module is gone (superseded by the fno-agents binary verbs), but the
reconcile-side defs these tests cover live on in fno.graph._reconcile.
"""
from __future__ import annotations

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


def test_leading_dot_slash_is_still_stripped_on_both_sides():
    from fno.graph._reconcile import verify_pending_supersessions

    entries = [_pending("x-old", "x-new", ["./cli/a.py"])]
    receipts = verify_pending_supersessions(
        entries, successor="x-new", changed_files=["cli/a.py"], evidence_pr=1
    )
    assert receipts == []
    assert entries[0]["supersession"]["verified_at"]


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


def test_complete_rest_file_list_is_not_marked_truncated():
    from fno.graph._reconcile import query_pr_merge_state

    state = query_pr_merge_state(
        5,
        info_reader=lambda number, *, repo=None, cwd=None: ({
            "pr": 5,
            "state": "MERGED",
            "url": "u",
            "merged_at": "t",
            "merge_sha": "sha",
        }, ""),
        files_reader=lambda number, *, repo=None, cwd=None: ([f"f{i}.py" for i in range(100)], ""),
    )
    assert state.files_truncated is False


def test_query_composes_injected_rest_readers_and_complete_files():
    from fno.graph._reconcile import query_pr_merge_state

    calls: list[tuple[str, int, str | None]] = []

    def info_reader(number, *, repo=None, cwd=None):
        calls.append(("info", int(number), repo))
        return ({
            "pr": int(number),
            "url": "https://github.com/o/r/pull/5",
            "state": "MERGED",
            "head_sha": "head",
            "head_ref": "feature/test",
            "base_ref": "main",
            "mergeable": "UNKNOWN",
            "merged_at": "2026-08-25T12:00:00Z",
            "merge_sha": "merge",
        }, "")

    def files_reader(number, *, repo=None, cwd=None):
        calls.append(("files", int(number), repo))
        return (["cli/a.py", "cli/b.py"], "")

    state = query_pr_merge_state(
        5,
        repo="o/r",
        info_reader=info_reader,
        files_reader=files_reader,
        include_files=True,
    )

    assert state.state == "MERGED"
    assert state.url == "https://github.com/o/r/pull/5"
    assert state.merged_at == "2026-08-25T12:00:00Z"
    assert state.merge_sha == "merge"
    assert state.changed_files == ["cli/a.py", "cli/b.py"]
    assert state.files_truncated is False
    assert calls == [("info", 5, "o/r"), ("files", 5, "o/r")]


def test_query_skips_file_evidence_for_merge_only_callers():
    from fno.graph._reconcile import query_pr_merge_state

    def files_reader(*args, **kwargs):
        raise AssertionError("merge-only query must not read changed files")

    state = query_pr_merge_state(
        5,
        repo="o/r",
        info_reader=lambda number, *, repo=None, cwd=None: ({
            "pr": 5,
            "state": "MERGED",
            "url": "u",
            "merged_at": "t",
            "merge_sha": "sha",
        }, ""),
        files_reader=files_reader,
    )

    assert state.state == "MERGED"
    assert state.changed_files == []


def test_query_source_contains_no_graphql_view_read():
    import inspect

    from fno.graph._reconcile import query_pr_merge_state

    assert "gh pr view" not in inspect.getsource(query_pr_merge_state)


def test_query_runner_guard_sees_only_routed_rest_argv():
    from fno.graph._reconcile import query_pr_merge_state
    from fno.pr._proc import Result

    calls: list[list[str]] = []
    pull = {
        "number": 5,
        "html_url": "https://github.com/o/r/pull/5",
        "state": "closed",
        "merged": True,
        "merged_at": "2026-08-25T12:00:00Z",
        "merge_commit_sha": "merge",
        "head": {"sha": "head", "ref": "feature/test"},
        "base": {"ref": "main"},
    }

    def runner(cmd, **kwargs):
        calls.append(list(cmd))
        assert cmd[:2] == ["gh", "api"], f"unrouted PR read: {cmd}"
        if "/files?" in cmd[-1]:
            return Result(0, '[{"filename":"cli/a.py"}]', "")
        return Result(0, json.dumps(pull), "")

    state = query_pr_merge_state(5, repo="o/r", runner=runner, include_files=True)

    assert state.state == "MERGED"
    assert state.changed_files == ["cli/a.py"]
    assert all(cmd[:3] != ["gh", "pr", "view"] for cmd in calls)


def test_query_rest_timeout_is_retryable_availability_failure():
    import subprocess

    from fno.graph._reconcile import ReconcileError, query_pr_merge_state

    def runner(cmd, **kwargs):
        raise subprocess.TimeoutExpired(cmd, 30)

    with pytest.raises(ReconcileError) as raised:
        query_pr_merge_state(5, repo="o/r", runner=runner)

    assert raised.value.kind == "availability"
    assert raised.value.retryable is True
    assert "retry" in raised.value.remedy_for(pr_number=5, repo="o/r").lower()


def test_query_missing_gh_is_retryable_availability_failure():
    from fno.graph._reconcile import ReconcileError, query_pr_merge_state
    from fno.pr._proc import ToolMissing

    def runner(cmd, **kwargs):
        raise ToolMissing("gh")

    with pytest.raises(ReconcileError) as raised:
        query_pr_merge_state(5, repo="o/r", runner=runner)

    assert raised.value.kind == "availability"
    assert raised.value.retryable is True


def test_the_bucket_probe_is_bounded_below_the_read_it_explains():
    """The probe only DECORATES a failure the caller already holds, so it may
    never cost what the read itself cost.

    This assertion used to read ``[30.0]`` against a caller that also declared
    30.0, and those two spellings produce the same number: the forwarded
    budget and ``_bucket_remaining``'s own default were indistinguishable. The
    bug hiding behind that ambiguity was real. A caller declaring 3s got a 30s
    probe, because the helper's explicit default wins over the runner's
    substitution, so one ``read_pr_state`` call could spend 33 seconds against
    a declared 3.

    The caller here declares 3.0 precisely so neither old explanation can
    produce the observed number. Residual, recorded rather than hidden: a
    caller declaring less than the ceiling still pays the ceiling, which is
    bounded and small where 30 was neither.
    """
    import json

    from fno.graph._reconcile import ReconcileError, query_pr_merge_state
    from fno.pr import _rest
    from fno.pr._proc import Result

    probe_timeouts: list[float | None] = []

    def runner(cmd, **kwargs):
        if cmd[-1] == "rate_limit":
            probe_timeouts.append(kwargs.get("timeout"))
            return Result(0, json.dumps({"resources": {"core": {"remaining": 0, "reset": 1}}}), "")
        return Result(1, "", "API rate limit exceeded")

    with pytest.raises(ReconcileError) as raised:
        query_pr_merge_state(5, repo="o/r", runner=runner, timeout_s=3.0)

    assert raised.value.kind == "availability"
    assert probe_timeouts == [_rest._REASON_DIAGNOSTIC_TIMEOUT_S]
    assert probe_timeouts[0] < 30.0


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


def test_query_default_rest_timeout_stays_retryable(monkeypatch):
    import subprocess

    from fno.graph._reconcile import ReconcileError, query_pr_merge_state
    from fno.pr import _rest

    def timed_out(*args, **kwargs):
        raise subprocess.TimeoutExpired(args[0], kwargs.get("timeout", 30))

    monkeypatch.setattr(_rest, "run", timed_out)

    with pytest.raises(ReconcileError) as raised:
        query_pr_merge_state(5, repo="o/r")

    assert raised.value.kind == "availability"
    assert raised.value.retryable is True


def test_query_unknown_rest_state_is_a_malformed_failure():
    from fno.graph._reconcile import ReconcileError, query_pr_merge_state

    with pytest.raises(ReconcileError) as raised:
        query_pr_merge_state(
            5,
            repo="o/r",
            info_reader=lambda number, *, repo=None, cwd=None: ({
                "pr": 5,
                "state": "UNKNOWN",
                "url": "u",
            }, ""),
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
        "--reason", "folded into the rewrite",
    )
    assert code == 0, out
    rows = {e["id"]: e for e in read_graph_strict(graph)}
    assert rows["x-9f0c"]["supersession"]["reason"] == "folded into the rewrite"


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


def test_an_open_successor_is_not_owed_yet():
    """Its ordinary close still lies ahead, and that path already verifies."""
    from fno.graph._reconcile import successors_owing_verification

    entries = [
        {"id": "x-9f0c", "superseded_by": "x-1a2b",
         "supersession": {"successor": "x-1a2b", "surfaces": ["cli/p.py"], "verified_at": None}},
        {"id": "x-1a2b", "pr_number": 7},
    ]
    assert successors_owing_verification(entries) == {}


def test_an_already_verified_predecessor_is_not_owed():
    from fno.graph._reconcile import successors_owing_verification

    entries = [
        {"id": "x-9f0c", "superseded_by": "x-1a2b",
         "supersession": {"successor": "x-1a2b", "surfaces": [], "verified_at": "t"}},
        {"id": "x-1a2b", "completed_at": "t", "pr_number": 7},
    ]
    assert successors_owing_verification(entries) == {}
