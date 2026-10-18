"""`fno do pr merge` reconcile and node-resolution coverage.

The ship-phase provenance stamp that gave this file its name moved to
`fno backlog update --pr-number` (the PR-link choke point every shipped node
passes through). What remains here is the coverage the deletion nearly took
with it: the codex P1/P2 cross-repo collision guards on `_find_pr_node_id`
(unused by `_reconcile_merged_pr_node` since x-59a6, but kept as a tested
utility), and the repo-scoped delegation / unsuppressed-failure behavior of
`_reconcile_merged_pr_node` and `_on_confirmed_merge`. Those two now
delegate entirely to `backlog reconcile --pr-number/--repo` (x-59a6) rather
than resolving and stamping one node by hand, so a PR naming several nodes
closes all of them, not just the one this process happens to find first;
the plural binding itself is tested in test_pr_closure.py and
test_backlog_reconcile.py. These functions still run on every confirmed
merge, so losing the tests here would let a future refactor regress the
repo-scoping-is-mandatory guarantee they pin.
"""

from __future__ import annotations
from tests.fixtures.graph_seed import seed_graph

import json
from pathlib import Path


def _make_graph(tmp_path: Path, entries: list[dict]) -> Path:
    """Rows the way the store writes them: the typed api drops a row the
    model cannot parse, so seeds carry the stamped fields."""
    complete = []
    for e in entries:
        row = {"type": "feature", "priority": "p2", "status": "idea", **e}
        row.setdefault("title", e.get("id", "node"))
        row.setdefault("slug", e.get("id", "node"))
        complete.append(row)
    g = tmp_path / "graph.json"
    seed_graph(g, json.dumps({"entries": complete}, indent=2) + "\n")
    return g


def _patch(monkeypatch, g: Path) -> None:
    import fno.graph._constants as gc
    import fno.graph.store as gs
    import fno.paths as paths

    monkeypatch.setattr(gc, "GRAPH_JSON", g)
    monkeypatch.setattr(gc, "GRAPH_MD", g.parent / "graph.md")
    monkeypatch.setattr(gs, "GRAPH_JSON", g)
    monkeypatch.setattr(paths, "graph_json", lambda: g)


def _clear_env(monkeypatch):
    for v in ("CODEX_THREAD_ID", "CLAUDE_CODE_SESSION_ID", "CODEX_SESSION_ID", "GEMINI_SESSION_ID"):
        monkeypatch.delenv(v, raising=False)


# --- fno do pr merge closes its own node (baked-in reconcile, no memory) ---------

_FOOT = "https://github.com/bllshttng/footnote/pull"


def _stub_run(calls, *, ok=True, stderr=""):
    """A run() stub that records argv and returns a Result-like object."""

    class _R:
        def __init__(self):
            self.ok = ok
            self.stdout = ""
            self.stderr = stderr

    def _run(argv, cwd=None, **kwargs):
        calls.append(argv)
        return _R()

    return _run


def _fake_gh_url(url: str):
    class _R:
        ok = True
        stdout = url
        stderr = ""

    return lambda args, cwd: _R()


def test_find_pr_node_id_refuses_bare_number_without_url():
    # No url to scope against -> a bare-number match is cross-repo-unsafe -> refuse.
    import fno.pr._merge as M

    entries = [{"id": "ab-x", "pr_number": 5}]
    assert M._find_pr_node_id(entries, 5, "") is None


def test_find_pr_node_id_refuses_cross_repo_number_collision():
    # Two repos, same PR number; our url is footnote -> return footnote's node,
    # never the abilities node (codex P1: a bare-number match closed unrelated work).
    import fno.pr._merge as M

    entries = [
        {
            "id": "ab-abil",
            "pr_number": 5,
            "pr_url": "https://github.com/bllshttng/abilities/pull/5",
        },
        {"id": "ab-foot", "pr_number": 5, "pr_url": f"{_FOOT}/5"},
    ]
    assert M._find_pr_node_id(entries, 5, f"{_FOOT}/5") == "ab-foot"


def test_reconcile_merged_pr_node_delegates_to_pr_number_reconcile(tmp_path, monkeypatch):
    # x-59a6: this call site no longer resolves a node or stamps pr_number
    # itself - it delegates entirely to `backlog reconcile --pr-number`,
    # which binds every node the PR's exact Backlog-Closure trailer names
    # (not just the one this process happens to find first) before running
    # the existing close scan. Repo scoping is explicit, resolved from the
    # PR's own url, never left to a cwd guess.
    url = f"{_FOOT}/777"
    g = _make_graph(tmp_path, [{"id": "ab-recon001", "title": "t", "pr_url": url}])
    _patch(monkeypatch, g)
    import fno.pr._merge as M

    monkeypatch.setattr(M, "_gh", _fake_gh_url(url))
    calls = []
    monkeypatch.setattr(M, "run", _stub_run(calls))
    M._reconcile_merged_pr_node(777, cwd=str(tmp_path))
    assert len(calls) == 1
    assert calls[0][-7:] == [
        "backlog",
        "reconcile",
        "--pr-number",
        "777",
        "--repo",
        "bllshttng/footnote",
        "--json",
    ]


def test_reconcile_backfills_pr_number_for_a_url_only_node(tmp_path, monkeypatch):
    # x-59a6 review fix: delegating to `backlog reconcile --pr-number` dropped
    # the pr_url-only backfill `_find_pr_node_id` used to provide. A node
    # stamped with pr_url but no pr_number (partial stamp, or an
    # off-convention branch name) must still get pr_number backfilled here,
    # since the delegated forward scan requires an int pr_number to find it.
    url = f"{_FOOT}/777"
    g = _make_graph(tmp_path, [{"id": "ab-recon001", "title": "t", "pr_url": url}])
    _patch(monkeypatch, g)
    import fno.pr._merge as M

    monkeypatch.setattr(M, "_gh", _fake_gh_url(url))
    monkeypatch.setattr(M, "run", _stub_run([]))
    M._reconcile_merged_pr_node(777, cwd=str(tmp_path))
    from fno.graph.store import read_graph_strict

    node = next(e for e in read_graph_strict(g) if e["id"] == "ab-recon001")
    assert node["pr_number"] == 777
    assert node["pr_url"] == url


def test_reconcile_does_not_clobber_existing_primary(tmp_path, monkeypatch):
    # The merged PR matches an additional_prs entry on a node that already has a
    # DIFFERENT primary; the primary number/url must be preserved (codex P2).
    url = f"{_FOOT}/777"
    g = _make_graph(
        tmp_path,
        [
            {
                "id": "ab-multi01",
                "title": "t",
                "pr_number": 100,
                "pr_url": f"{_FOOT}/100",
                "additional_prs": [{"number": 777, "url": url}],
            }
        ],
    )
    _patch(monkeypatch, g)
    import fno.pr._merge as M

    monkeypatch.setattr(M, "_gh", _fake_gh_url(url))
    calls = []
    monkeypatch.setattr(M, "run", _stub_run(calls))
    M._reconcile_merged_pr_node(777, cwd=str(tmp_path))
    from fno.graph.store import read_graph_strict

    node = next(e for e in read_graph_strict(g) if e["id"] == "ab-multi01")
    assert node["pr_number"] == 100  # primary untouched
    assert node["pr_url"] == f"{_FOOT}/100"  # url pair intact
    assert len(calls) == 1  # still closed, scoped to the node


def test_reconcile_surfaces_subprocess_failure(tmp_path, monkeypatch, capsys):
    # A non-zero reconcile must NOT be swallowed - that leaves the node open under a
    # green merge, the exact gap this closes (codex P2).
    url = f"{_FOOT}/321"
    g = _make_graph(tmp_path, [{"id": "ab-fail001", "title": "t", "pr_url": url}])
    _patch(monkeypatch, g)
    import fno.pr._merge as M

    monkeypatch.setattr(M, "_gh", _fake_gh_url(url))
    monkeypatch.setattr(M, "run", _stub_run([], ok=False, stderr="gh query unavailable"))
    M._reconcile_merged_pr_node(321, cwd=str(tmp_path))
    err = capsys.readouterr().err
    assert "reconcile for PR #321" in err and "failed" in err


def test_on_confirmed_merge_syncs_status_and_closes_node(tmp_path, monkeypatch):
    url = f"{_FOOT}/556"
    g = _make_graph(tmp_path, [{"id": "ab-conf001", "title": "t", "pr_number": 556, "pr_url": url}])
    _patch(monkeypatch, g)
    monkeypatch.setattr(
        "fno.graph._reconcile.resolve_current_repo_slug",
        lambda cwd: "bllshttng/footnote",
    )
    _clear_env(monkeypatch)
    import fno.pr._merge as M

    monkeypatch.setattr(M, "_gh", _fake_gh_url(url))
    calls = []
    monkeypatch.setattr(M, "run", _stub_run(calls))
    M._on_confirmed_merge(556, str(tmp_path))
    from fno.graph.store import read_graph_strict

    node = next(e for e in read_graph_strict(g) if e["id"] == "ab-conf001")
    assert node.get("merge_status") == "merged"
    assert calls
    assert calls[0][-7:] == [
        "backlog",
        "reconcile",
        "--pr-number",
        "556",
        "--repo",
        "bllshttng/footnote",
        "--json",
    ]


def test_on_confirmed_merge_defer_close_syncs_status_and_skips_reconcile(tmp_path, monkeypatch):
    # The watcher's durable-grant merge cannot hold the 79-179s whole-graph
    # reconcile inside its phase slice: defer_close still syncs merge_status
    # but fires no reconcile child, and the queued owners (the sweep ritual's
    # reconcile leg and the daemon's merge_close arm) close the node instead.
    url = f"{_FOOT}/556"
    g = _make_graph(tmp_path, [{"id": "ab-defer01", "title": "t", "pr_number": 556, "pr_url": url}])
    _patch(monkeypatch, g)
    monkeypatch.setattr(
        "fno.graph._reconcile.resolve_current_repo_slug",
        lambda cwd: "bllshttng/footnote",
    )
    _clear_env(monkeypatch)
    import fno.pr._merge as M

    monkeypatch.setattr(M, "_gh", _fake_gh_url(url))
    calls = []
    monkeypatch.setattr(M, "run", _stub_run(calls))
    closed = M._on_confirmed_merge(556, str(tmp_path), defer_close=True)
    from fno.graph.store import read_graph_strict

    node = next(e for e in read_graph_strict(g) if e["id"] == "ab-defer01")
    assert node.get("merge_status") == "merged"
    assert calls == []
    assert closed == []


def test_reconcile_merged_pr_node_closes_via_seam_under_external(tmp_path, monkeypatch):
    """Under external selection the reconcile verb refuses, so the merge close
    must terminate through the shared seam: the primary-link backfill lands in
    the SIDECAR (the local graph is never written), exactly one tracker.close,
    and no reconcile subprocess fires."""
    from fno.tracker.types import NodeNotFound, TrackerCandidate, TrackerState

    url = f"{_FOOT}/777"
    g = _make_graph(tmp_path, [{"id": "ab-recon001", "title": "t", "pr_url": url}])
    _patch(monkeypatch, g)
    import fno.pr._merge as M

    monkeypatch.setattr(M, "_gh", _fake_gh_url(url))
    calls = []
    monkeypatch.setattr(M, "run", _stub_run(calls))

    class _T:
        name = "fake-external"

        def __init__(self):
            self.close_calls = []

        def read(self, id):
            if id != "ab-recon001":
                raise NodeNotFound(id)
            return TrackerCandidate(
                id=id,
                title="t",
                state=TrackerState.open,
                parent=None,
                blocked_by=[],
            )

        def list_open(self):
            return []

        def close(self, id):
            self.close_calls.append(id)

    tracker = _T()
    monkeypatch.setattr("fno.tracker.get_tracker", lambda *a, **k: tracker)
    sc_dir = tmp_path / "sidecars"
    sc_dir.mkdir()
    (sc_dir / "ab-recon001.json").write_text(json.dumps({"id": "ab-recon001", "pr_url": url}))
    import fno.tracker.sidecar as sidecar_store

    monkeypatch.setattr(sidecar_store, "sidecar_path", lambda i: sc_dir / f"{i}.json")
    monkeypatch.setattr("fno.paths.graph_json", lambda: g)
    monkeypatch.setenv("FNO_TRACKER_BACKEND", "github")
    monkeypatch.setenv("FNO_CLAIMS_ROOT", str(tmp_path / "claims"))
    import fno.graph._constants as gc

    monkeypatch.setattr(gc, "LEDGER_JSON", tmp_path / "absent-ledger.json")

    from fno.graph._reconcile import PrMergeState

    monkeypatch.setattr(
        "fno.graph.cli._done_gh_query",
        lambda pr, **kw: PrMergeState(
            number=777,
            state="MERGED",
            url=url,
            merged_at="2026-08-17T00:00:00Z",
        ),
    )

    M._reconcile_merged_pr_node(777, cwd=str(tmp_path))
    assert tracker.close_calls == ["ab-recon001"]
    assert calls == []  # the refused reconcile subprocess never fired
    # The backfill went to the sidecar, not the graph.
    from fno.graph.store import read_graph_strict

    node = next(e for e in read_graph_strict(g) if e["id"] == "ab-recon001")
    assert node.get("pr_number") != 777
    sc = json.loads((sc_dir / "ab-recon001.json").read_text())
    assert sc["pr_number"] == 777


# --- the merge mints its own cleanup request (the machine's reap order) ----------


def _write_manifest(tmp_path: Path) -> Path:
    state_dir = tmp_path / ".fno"
    state_dir.mkdir(exist_ok=True)
    manifest = state_dir / "target-state.md"
    manifest.write_text("---\nsession_id: sess-abc\nharness: claude\n---\n")
    return manifest


def _patch_events_log(monkeypatch, tmp_path: Path) -> Path:
    import fno.agents.events as E

    log = tmp_path / "agents-events.jsonl"
    monkeypatch.setattr(E, "daemon_lifecycle_log", lambda: log)
    return log


def _stub_gh_merged(monkeypatch, module, branch: str = "feature/x-07dc", url: str = ""):
    class _R:
        ok = True
        stderr = ""
        stdout = ""

    def _gh(args, cwd):
        r = _R()
        fields = next((a for a in args if a.startswith("state,headRefName")), "")
        if "--json" in args and fields:
            r.stdout = json.dumps(
                {
                    "state": "MERGED",
                    "headRefName": branch,
                    "url": url,
                    "mergedAt": "2026-09-07T15:00:00Z",
                }
            )
        return r

    monkeypatch.setattr(module, "_gh", _gh)


def _stub_git_root(monkeypatch, module, root: Path):
    class _R:
        ok = True
        stderr = ""
        stdout = str(root)

    monkeypatch.setattr(module, "_git", lambda args, cwd: _R())


def _requested_rows(log: Path) -> list[dict]:
    from tests._event_rows import event_rows

    return [row for row in event_rows(log) if row.get("type") == "merge_cleanup_requested"]


def _stub_mint_verb(monkeypatch):
    """Stub the verb_call seam both mint sites ride: record payloads, answer
    the receipt with the fold-key formula the native mint implements. The
    envelope itself is pinned by the Rust goldens."""
    import hashlib

    import fno.rust_binary as rust_binary

    seen = []

    def fake_verb_call(verb, payload, *args, **kwargs):
        seen.append((tuple(verb), payload))
        identity = "|".join(
            [str(payload.get("project")), str(payload.get("pr")), payload.get("branch") or ""]
        )
        return {"request_id": "merge-cleanup-" + hashlib.sha256(identity.encode()).hexdigest()[:20]}

    monkeypatch.setattr(rust_binary, "verb_call", fake_verb_call)
    return seen


def test_post_merge_followups_mints_cleanup_request(tmp_path, monkeypatch):
    # AC1-HP: a confirmed merge with no ritual run mints the request itself,
    # carrying merged_at, the bound node ids, and the session identity. The
    # envelope fields (candidates, sorted ids, ts-vs-merged_at) are pinned by
    # the Rust goldens; this side pins what the merge sends.
    import fno.pr._merge as M

    log = _patch_events_log(monkeypatch, tmp_path)
    _stub_gh_merged(monkeypatch, M)
    _stub_git_root(monkeypatch, M, tmp_path)
    M._REPO_ROOT_CACHE[str(tmp_path)] = str(tmp_path)
    _write_manifest(tmp_path)
    # A linked worktree's `.git` is a FILE; this fixture needs the merge
    # cleanup to see cwd as one (x-7b9c inlined the predicate in _merge.py).
    (tmp_path / ".git").write_text("gitdir: /elsewhere/worktrees/t/.git\n")
    seen = _stub_mint_verb(monkeypatch)

    M._run_post_merge_followups(9, "squash", str(tmp_path), bound_node_ids=["x-07dc"])

    assert len(seen) == 1
    verb, payload = seen[0]
    assert verb == ("pr-watch", "merge-cleanup-mint")
    assert payload["pr"] == 9
    assert payload["branch"] == "feature/x-07dc"
    assert payload["node_ids"] == ["x-07dc"]
    assert payload["session_id"] == "sess-abc"
    assert payload["harness"] == "claude"
    assert payload["merged_at"] and payload["merged_at"].endswith("Z")
    assert _requested_rows(log) == []
