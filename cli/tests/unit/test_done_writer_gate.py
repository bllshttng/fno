"""Regression guards for the done=merged invariant.

A backlog node may only close on MERGED evidence. Historically the finalize
ledger append shelled an ungated ``update --completed`` leg and closed nodes
at ship time, 2h before their PR merged. These tests pin the writers shut.
"""

from __future__ import annotations
from tests.fixtures.graph_seed import seed_graph

import json
from pathlib import Path

import pytest
from fno.graph.store import read_graph_strict


# ---------------------------------------------------------------------------
# AC1-HP: a ledger append never closes a graph node
# ---------------------------------------------------------------------------


def _seed_graph(home: Path, *, plan_path: str) -> Path:
    """Write a graph fixture whose node is the ledger entry's join target."""
    graph = home / ".fno" / "graph.json"
    graph.parent.mkdir(parents=True, exist_ok=True)
    seed_graph(graph, json.dumps(
            {
                "entries": [
                    {
                        "id": "x-fixture",
                        "slug": "x-fixture",
                        "title": "x-fixture",
                        "type": "feature",
                        "priority": "p2",
                        "plan_path": plan_path,
                        "pr_number": 505,
                        "status": "in_review",
                    }
                ]
            },
            indent=2,
        ))
    return graph


def test_register_entry_leaves_graph_byte_identical(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """A finalize ledger append must not change the graph store.

    Two assertions, because either alone is weak. Byte-identity catches an
    in-process writer but is only as red as the subprocess it fails to spawn;
    the shell-out assertion catches the actual incident shape (the deleted leg
    shelled ``roadmap-tasks.py update --completed``) deterministically, without
    depending on whether that subprocess would have succeeded here.

    The deleted writer fired only when the entry carried plan_path AND
    pr_number, so the entry below is exactly its trigger shape.
    """
    from fno.cost import _register

    monkeypatch.setenv("HOME", str(tmp_path))
    monkeypatch.setattr(Path, "home", classmethod(lambda cls: tmp_path))

    plan_path = str(tmp_path / "plan.md")
    graph = _seed_graph(tmp_path, plan_path=plan_path)
    before = read_graph_strict(graph)

    ledger = tmp_path / "ledger.json"
    monkeypatch.setattr(_register._paths, "ledger_json", lambda: ledger)

    shelled: list[list[str]] = []
    real_run = _register.subprocess.run

    def _spy(cmd, *a, **kw):
        shelled.append([str(c) for c in cmd] if isinstance(cmd, (list, tuple)) else [str(cmd)])
        return real_run(cmd, *a, **kw)

    monkeypatch.setattr(_register.subprocess, "run", _spy)

    _register.register_entry(
        {
            "type": "execution",
            "plan_path": plan_path,
            "pr_number": 505,
            "pr_url": "https://github.com/o/r/pull/505",
            "graph_node_id": "x-e4bc",
            "session_id": "sess-1",
            "root_path": str(tmp_path),
        }
    )

    assert ledger.exists(), "the ledger append itself must still land"
    assert read_graph_strict(graph) == before, "ledger append must not mutate graph store rows"

    graph_writes = [
        c for c in shelled if any("roadmap-tasks" in p or "--completed" in p for p in c)
    ]
    assert not graph_writes, f"ledger append shelled a graph close: {graph_writes}"


# ---------------------------------------------------------------------------
# AC2-HP / AC3-ERR / AC5-EDGE: `fno done` gates like `backlog done`
# ---------------------------------------------------------------------------


@pytest.fixture
def done_graph(tmp_path, monkeypatch) -> Path:
    """A graph routed to the done command's code path, with an empty ledger."""
    g = tmp_path / "graph.json"
    ledger = tmp_path / "ledger.json"
    ledger.write_text('{"entries": []}\n')
    import fno.graph._constants as gc
    import fno.graph.store as gs

    monkeypatch.setattr(gc, "GRAPH_JSON", g)
    monkeypatch.setattr(gc, "GRAPH_MD", tmp_path / "graph.md")
    monkeypatch.setattr(gc, "LEDGER_JSON", ledger)
    monkeypatch.setattr(gs, "GRAPH_JSON", g)
    monkeypatch.delenv("CLAUDECODE_SESSION_ID", raising=False)
    return g


def _seed(g: Path, entry: dict) -> None:
    row = {"type": "feature", "priority": "p2", "status": "idea", **entry}
    # The store derives done from completed_at on every write.
    if row["status"] == "done" and not row.get("completed_at"):
        row["completed_at"] = "2026-09-01T00:00:00Z"
    row.setdefault("title", entry.get("id", "node"))
    row.setdefault("slug", entry.get("id", "node"))
    seed_graph(g, json.dumps({"entries": [row]}, indent=2) + "\n")


def _node(g: Path, node_id: str) -> dict:
    return next(e for e in read_graph_strict(g) if e["id"] == node_id)


def _stub_gh(monkeypatch, state: str | None, *, calls: list | None = None):
    """Point `fno done`'s gh seam at a fixed state, or raise for an outage."""
    import fno.done.cli as done_cli
    from fno.graph._reconcile import PrMergeState, ReconcileError

    def _q(pr_number, **kwargs):
        if calls is not None:
            calls.append(pr_number)
        if state is None:
            raise ReconcileError("gh: not authenticated")
        return PrMergeState(
            number=pr_number, state=state, url=None,
            merged_at="2026-01-01T00:00:00Z" if state == "MERGED" else None,
        )

    # x-6233: the fold merged the two done commands; the canonical close
    # queries through graph.cli's seam, the delegated surface through
    # done.cli's. Patch both so either path sees the fixed state.
    import fno.graph.cli as graph_cli

    monkeypatch.setattr(done_cli, "_gh_query", _q)
    monkeypatch.setattr(graph_cli, "_done_gh_query", _q)


def test_ac3_err_gh_failure_fails_closed(done_graph, monkeypatch):
    """AC3-ERR: an authentication failure refuses with its repair command."""
    from typer.testing import CliRunner
    from fno.cli import app

    _seed(done_graph, {"id": "ab-out001", "title": "Outage node", "domain": "code"})
    _stub_gh(monkeypatch, None)

    r = CliRunner().invoke(app, ["done", "ab-out001", "--pr", "42"])

    assert r.exit_code == 3
    combined = r.output + (r.stderr or "")
    assert "gh auth login" in combined
    assert "retryable once gh is available again" not in combined
    entry = _node(done_graph, "ab-out001")
    assert entry.get("completed_at") is None
    assert entry.get("merge_status") is None


def test_compat_done_routing_refusal_is_not_retryable(tmp_path):
    """The close's routing refusal names its remedy and is not retryable."""
    from tests.goldens._door import door, make_sandbox, seed_node, warm, write_pr_stub

    root = make_sandbox(tmp_path, [seed_node("ab-routec01", pr_number=1140, pr_url="https://github.com/o/r/pull/1140")])
    warm(root, "ab-routec01")
    stubbin = write_pr_stub(
        root,
        fail_stderr="[fno GraphQL reserve] use `fno do pr info 1140`; unconditional route refusal",
    )
    code, out, err = door(root, ["done", "ab-routec01"], path_prepend=str(stubbin))
    assert code == 3, out + err
    assert "fno do pr info 1140 --repo o/r" in err
    assert "retryable once gh is available again" not in err
    entry = _node(root / "graph.json", "ab-routec01")
    assert entry.get("completed_at") is None


def test_merged_pr_closes_and_records_resolved_merge_status(done_graph, monkeypatch):
    """merge_status is written from the gh-resolved state, never a literal."""
    from typer.testing import CliRunner
    from fno.cli import app

    _seed(done_graph, {"id": "ab-mrg001", "title": "Merged node", "domain": "code"})
    _stub_gh(monkeypatch, "MERGED")

    r = CliRunner().invoke(app, ["done", "ab-mrg001", "--pr", "42"])

    assert r.exit_code == 0
    entry = _node(done_graph, "ab-mrg001")
    assert entry.get("completed_at") is not None
    assert entry.get("merge_status") == "merged"


def test_ac5_edge_non_pr_close_is_ungated(done_graph, monkeypatch):
    """AC5-EDGE: a node closing on --note has no PR, so the gate never applies."""
    from typer.testing import CliRunner
    from fno.cli import app

    _seed(done_graph, {"id": "ab-doc001", "title": "Docs node", "domain": "docs"})
    calls: list = []
    _stub_gh(monkeypatch, None, calls=calls)  # would raise if consulted

    r = CliRunner().invoke(app, ["done", "ab-doc001", "--note", "shipped brief"])

    assert r.exit_code == 0
    assert calls == [], "a PR-less close must not query gh"
    entry = _node(done_graph, "ab-doc001")
    assert entry.get("completed_at") is not None
    assert entry.get("completion_note") == "shipped brief"


# ---------------------------------------------------------------------------
# US5 / AC1-HP + AC4-FR: sweep every inventoried writer against one fixture
# ---------------------------------------------------------------------------


def test_us5_no_writer_closes_a_node_whose_pr_is_open(done_graph, tmp_path, monkeypatch):
    """Every close path refuses an OPEN PR, so none can regress alone.

    The inventory that produced this fix listed six writers; W1/W2 were already
    merge-gated, W5 is deleted, and W3/W4 are fixed here. Sweeping them against
    one fixture is what keeps a future seventh writer from quietly rejoining the
    list - the node must read in_review afterward no matter who tried. The bare
    close is native now, so W1 drives the door over its own sandbox.
    """
    from typer.testing import CliRunner
    from fno.cli import app
    from fno.graph._reconcile import PrMergeState
    import fno.graph.cli as graph_cli
    from tests.goldens._door import door, make_sandbox, seed_node, warm, write_pr_stub

    _seed(done_graph, {
        "id": "ab-sweep01",
        "title": "Open PR sweep node",
        "domain": "code",
        "status": "in_review",
        "pr_number": 42,
        "pr_url": "https://github.com/o/r/pull/42",
    })
    _stub_gh(monkeypatch, "OPEN")
    monkeypatch.setattr(
        graph_cli,
        "_done_gh_query",
        lambda n, **kw: PrMergeState(number=n, state="OPEN", url=None, merged_at=None),
    )

    runner = CliRunner()
    # W4 `fno done` reports awaiting merge on the rich surface.
    assert runner.invoke(app, ["done", "ab-sweep01", "--pr", "42"]).exit_code == 5
    # W1 `backlog done` is the native close: the same refusal through the door.
    root = make_sandbox(tmp_path, [seed_node("ab-sweep01", "in_review", pr_number=42, pr_url="https://github.com/o/r/pull/42")])
    warm(root, "ab-sweep01")
    stubbin = write_pr_stub(root, {42: "OPEN"})
    code, out, err = door(root, ["done", "ab-sweep01"], path_prepend=str(stubbin))
    assert code == 5, out + err
    # W3 `backlog update --completed`: the flag is gone, so it cannot be reached.
    assert runner.invoke(
        app, ["backlog", "update", "ab-sweep01", "--completed"]
    ).exit_code != 0

    entry = _node(done_graph, "ab-sweep01")
    assert entry.get("completed_at") is None
    assert entry.get("status") == "in_review"
    assert entry.get("merge_status") is None


def test_exit_code_contract_is_owned_by_one_table():
    """The Rust loop keys on 5 for AwaitingMerge, so these numbers are frozen.

    Both done commands raise `evidence.exit_code` rather than re-deriving the
    mapping, which is what keeps them from drifting apart.
    """
    from fno.graph._reconcile import MergeEvidence

    assert MergeEvidence(outcome="merged").exit_code == 0
    assert MergeEvidence(outcome="awaiting_merge").exit_code == 5
    assert MergeEvidence(outcome="outage").exit_code == 4
    assert MergeEvidence(outcome="refused").exit_code == 3


def _stub_no_git(monkeypatch):
    """git/gh subprocesses all fail, so PR auto-detect resolves to nothing."""
    import fno.done.cli as done_cli

    class _Res:
        stdout = ""
        stderr = ""
        returncode = 1

    real_run = done_cli.subprocess.run

    def fake_run(*a, **kw):
        cmd = a[0] if a else kw.get("cmd")
        if cmd and {"doctor", "event"} <= {str(p) for p in cmd}:
            # Event emission rides the same seam; reach the real binary.
            return real_run(*a, **kw)
        return _Res()

    monkeypatch.setattr(done_cli.subprocess, "run", fake_run)


def test_note_close_cannot_bypass_the_gate_on_a_node_with_an_open_pr(
    done_graph, monkeypatch
):
    """--note suppresses auto-detect, so it must not also suppress the gate."""
    from typer.testing import CliRunner
    from fno.cli import app

    _seed(done_graph, {
        "id": "ab-note001", "title": "Note close", "domain": "code",
        "pr_number": 42, "pr_url": "https://github.com/o/r/pull/42",
    })
    _stub_no_git(monkeypatch)
    _stub_gh(monkeypatch, "OPEN")

    r = CliRunner().invoke(app, ["done", "ab-note001", "--note", "shipped"])

    assert r.exit_code == 5
    assert _node(done_graph, "ab-note001").get("completed_at") is None


def test_explicit_pr_is_scoped_to_the_nodes_repo_not_the_callers(
    done_graph, monkeypatch
):
    """An explicit --pr is evidenced in the NODE's repo, wherever it is run.

    Multi-repo graphs are supported, so closing a node from a different
    checkout must not let a same-numbered PR in the caller's repo authorize it.
    """
    from typer.testing import CliRunner
    from fno.cli import app
    import fno.done.cli as done_cli
    from fno.graph._reconcile import PrMergeState

    _seed(done_graph, {
        "id": "ab-repo001", "title": "Node in repo-a", "domain": "code",
        "pr_number": 7, "pr_url": "https://github.com/o/repo-a/pull/7",
    })
    # The caller is standing in repo-b; gh would resolve that slug.
    monkeypatch.setattr(done_cli, "pr_url_for_repo",
                        lambda n, cwd=None: f"https://github.com/o/repo-b/pull/{n}")
    seen: list = []

    def _q(pr_number, **kwargs):
        seen.append((pr_number, kwargs.get("repo")))
        return PrMergeState(number=pr_number, state="OPEN", url=None, merged_at=None)

    # x-6233: the fold merged the two done commands; the canonical close
    # queries through graph.cli's seam, the delegated surface through
    # done.cli's. Patch both so either path sees the fixed state.
    import fno.graph.cli as graph_cli

    monkeypatch.setattr(done_cli, "_gh_query", _q)
    monkeypatch.setattr(graph_cli, "_done_gh_query", _q)

    CliRunner().invoke(app, ["done", "ab-repo001", "--pr", "99"])

    assert seen == [(99, "o/repo-a")], "explicit --pr must resolve in the node's repo"


def test_explicit_pr_replaces_a_stale_merged_ref_as_the_evidence(
    done_graph, monkeypatch
):
    """A merged old PR must not authorize a close whose replacement is open.

    Closing with --pr names the PR being closed on; treating the node's older
    merged ref as evidence would stamp merge_status: merged while writing the
    new, still-open PR number.
    """
    from typer.testing import CliRunner
    from fno.cli import app
    import fno.done.cli as done_cli
    from fno.graph._reconcile import PrMergeState

    _seed(done_graph, {
        "id": "ab-repl001", "title": "Replacement PR", "domain": "code",
        "pr_number": 100, "pr_url": "https://github.com/o/r/pull/100",
        "merge_status": "merged",
    })
    monkeypatch.setattr(
        done_cli, "_gh_query",
        lambda n, **kw: PrMergeState(
            number=n,
            state="MERGED" if n == 100 else "OPEN",
            url=None,
            merged_at="2026-01-01T00:00:00Z" if n == 100 else None,
        ),
    )

    r = CliRunner().invoke(app, ["done", "ab-repl001", "--pr", "101"])

    assert r.exit_code == 5
    assert _node(done_graph, "ab-repl001").get("completed_at") is None
