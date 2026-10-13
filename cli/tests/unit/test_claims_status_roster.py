"""Roster parity tests for claim status, run across the port boundary.

The claim status leaf runs inside the fno-agents binary; the Python CLI
forwards argv and re-emits the streams. Patching Python roster readers
cannot reach the verdict, so every test here injects through the seams the
binary reads: a PATH stub of `claude` carries the roster payload, the
transcript root rides CLAUDE_DIR_OVERRIDE, and the graph seeds through
FNO_STATE_DIR.

The refused-ratio, undatable-transcript, dead-pid, graph-failure, and
stale-keeper arms lived on Python reader internals and are gone with the
readers. Their surviving outcomes stay pinned in tests/unit/test_claims_cli.py
and crates/fno-agents/tests/claim_status_join.rs.
"""

import json

from typer.testing import CliRunner

from fno.claims.cli import cli
from tests.fixtures.graph_seed import seed_graph
from .test_claims_cli import (  # noqa: F401
    _plant_transcript,
    _plant_worktree,
    _stub_row,
    stub_roster,
)

runner = CliRunner()


def test_the_degraded_human_line_carries_both_numbers(cwd_tmp, stub_roster):
    """One row resolves elsewhere, one names no node at all. An unresolved row
    that does not name this node is a fidelity note, not an occupant, so the
    verdict stays free - and both numbers survive so the operator can size the
    blind spot."""
    stub_roster([
        _stub_row("t-else", "working",
                  _plant_worktree(cwd_tmp, "else-node"), sid="sess-else"),
        _stub_row("t-lost", "working", str(cwd_tmp / "nowhere-lost"),
                  sid="sess-lost"),
    ])
    r = runner.invoke(cli, ["status", "node:ac1-node", "--json"])
    assert r.exit_code == 0
    info = json.loads(r.output)
    assert info["state"] == "free"
    assert info["roster_coverage"] == "degraded"
    assert info["roster_rows_scanned"] == 2
    assert info["roster_rows_unresolved"] == 1
    assert "basis" not in info
    r = runner.invoke(cli, ["status", "node:ac1-node"])
    assert "no row resolved to this node (2 scanned, 1 unresolved)" in r.output
    assert "roster coverage degraded" in r.output


def test_a_fresh_transcript_outranks_a_dead_pid(cwd_tmp, stub_roster, monkeypatch):
    """The roster row still carries the pid of a worker that died - a revived
    session keeps the stale pid until the next scan. The transcript moved
    seconds ago, which clears the falsifier, and the node reads staffed."""
    monkeypatch.setenv("FNO_STATE_DIR", str(cwd_tmp / "state"))
    seed_graph(
        cwd_tmp / "state" / "db" / "graph.json",
        [{"id": "ac1-node", "title": "t", "domain": "code", "project": "p",
          "sessions": [{"session_id": "sess-resumed", "phase": "execute",
                        "harness": "claude",
                        "started_at": "2026-10-10T00:00:00Z",
                        "ended_at": None}]}],
    )
    row = _stub_row("t-resumed", "working",
                    _plant_worktree(cwd_tmp, "ac1-node"), sid="sess-resumed")
    row.update({"pid": 999999, "pid_start_time": 12345})
    stub_roster([row])
    _plant_transcript(stub_roster.home, "sess-resumed")
    r = runner.invoke(cli, ["status", "node:ac1-node", "--json"])
    assert r.exit_code == 0
    info = json.loads(r.output)
    assert info["worked_by"] == ["t-resumed"]
    assert info["state"] == "unknown"
    r = runner.invoke(cli, ["status", "node:ac1-node"])
    assert "UNCLAIMED but a live worker is on this node: t-resumed" in r.output


def test_a_stale_working_word_never_reads_live(cwd_tmp, stub_roster, monkeypatch):
    """The row still says working, but its last transcript line is three hours
    old. A word is not a measurement: reachability reads the transcript age,
    and past the stall threshold the row cannot raise the unstaffed-node
    alarm."""
    monkeypatch.setenv("FNO_STATE_DIR", str(cwd_tmp / "state"))
    seed_graph(
        cwd_tmp / "state" / "db" / "graph.json",
        [{"id": "ac1-node", "title": "t", "domain": "code", "project": "p",
          "sessions": [{"session_id": "sess-stale", "phase": "execute",
                        "harness": "claude",
                        "started_at": "2026-10-10T00:00:00Z",
                        "ended_at": None}]}],
    )
    stub_roster([
        _stub_row("t-stale", "working",
                  _plant_worktree(cwd_tmp, "ac1-node"), sid="sess-stale")
    ])
    _plant_transcript(stub_roster.home, "sess-stale", age_s=3 * 3600)
    r = runner.invoke(cli, ["status", "node:ac1-node", "--json"])
    assert r.exit_code == 0
    info = json.loads(r.output)
    assert "worked_by" not in info
    r = runner.invoke(cli, ["status", "node:ac1-node"])
    assert "UNCLAIMED but a live worker" not in r.output


def test_a_closed_session_frees_the_claim_status_verdict(cwd_tmp, stub_roster, monkeypatch):
    """The roster still lists the blueprint worker, but the graph closed that
    session an hour after it started. The closed-worker join retires the row
    before classification, so a finished session never holds the verdict
    hostage."""
    monkeypatch.setenv("FNO_STATE_DIR", str(cwd_tmp / "state"))
    seed_graph(
        cwd_tmp / "state" / "db" / "graph.json",
        [{"id": "ac1-node", "title": "t", "domain": "code", "project": "p",
          "sessions": [{"session_id": "bp-x", "phase": "blueprint",
                        "harness": "claude",
                        "started_at": "2026-09-11T15:00:00Z",
                        "ended_at": "2026-09-11T16:00:54Z"}]}],
    )
    stub_roster([
        _stub_row("bp-x", "done",
                  _plant_worktree(cwd_tmp, "ac1-node"), sid="bp-x")
    ])
    r = runner.invoke(cli, ["status", "node:ac1-node", "--json"])
    assert r.exit_code == 0
    info = json.loads(r.output)
    assert info["state"] == "free"
    assert "worked_by" not in info
    assert info["roster_closed_workers"] == ["bp-x"]
    r = runner.invoke(cli, ["status", "node:ac1-node"])
    assert "1 finished session(s) resolved to it: bp-x" in r.output


def test_a_closed_worker_is_named_over_an_unresolved_roster(cwd_tmp, stub_roster, monkeypatch):
    """Thirty-seven rows name nothing resolvable and one closed blueprint
    session does. The verdict stays free, the finished session is named, and
    the unresolved count carries the blind spot instead of faking coverage."""
    monkeypatch.setenv("FNO_STATE_DIR", str(cwd_tmp / "state"))
    seed_graph(
        cwd_tmp / "state" / "db" / "graph.json",
        [{"id": "ac1-node", "title": "t", "domain": "code", "project": "p",
          "sessions": [{"session_id": "bp-x", "phase": "blueprint",
                        "harness": "claude",
                        "started_at": "2026-09-11T15:00:00Z",
                        "ended_at": "2026-09-11T16:00:54Z"}]}],
    )
    stub_roster(
        [_stub_row("bp-x", "done",
                   _plant_worktree(cwd_tmp, "ac1-node"), sid="bp-x")]
        + [_stub_row("t-lost-%d" % i, "working",
                     str(cwd_tmp / "nowhere-%d" % i), sid="sess-lost-%d" % i)
           for i in range(37)]
    )
    r = runner.invoke(cli, ["status", "node:ac1-node", "--json"])
    assert r.exit_code == 0
    info = json.loads(r.output)
    assert info["state"] == "free"
    assert info["roster_closed_workers"] == ["bp-x"]
    assert info["roster_rows_unresolved"] == 37
    assert info["roster_workers"] == []
    r = runner.invoke(cli, ["status", "node:ac1-node"])
    assert "1 finished session(s) resolved to it: bp-x" in r.output
    assert "no row resolved to this node (38 scanned, 37 unresolved)" in r.output
