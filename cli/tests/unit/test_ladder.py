"""Design-stage probe: the `design` rung of the derived lifecycle ladder."""
from __future__ import annotations

import pytest

from fno.graph.ladder import is_design_stage


DESIGN_FM = "---\nstatus: design\n---\n\n# Doc\n"


def _now() -> str:
    from datetime import datetime, timezone

    return datetime.now(timezone.utc).isoformat()


def _plan(tmp_path, body: str, name: str = "p.md") -> dict:
    """A node entry carrying an absolute plan_path (the simplest form)."""
    target = tmp_path / name
    target.write_text(body)
    return {"id": "x-test", "plan_path": str(target)}


def test_relative_plan_path_resolves_against_node_cwd(tmp_path):
    """The majority form on the live graph: repo-relative path + node `cwd`.

    Resolving against the calling process's cwd instead silently no-ops the
    gate for every foreign node - the daemon selects across projects.
    """
    (tmp_path / "plans").mkdir()
    (tmp_path / "plans" / "d.md").write_text(DESIGN_FM)
    entry = {"id": "x-test", "plan_path": "plans/d.md", "cwd": str(tmp_path)}
    assert is_design_stage(entry)


def test_relative_path_without_cwd_does_not_use_process_cwd(tmp_path, monkeypatch):
    """No `cwd` to resolve against: fail open rather than guess.

    The file deliberately EXISTS at that relative path in the process cwd - an
    earlier cut returned the bare relative path and would have design-gated an
    unrelated node off a coincidentally-matching local doc.
    """
    monkeypatch.chdir(tmp_path)
    (tmp_path / "d.md").write_text(DESIGN_FM)
    assert not is_design_stage({"id": "x-test", "plan_path": "d.md"})


def test_unparseable_plan_stays_armed(tmp_path):
    """Fail OPEN: only positive `status: design` evidence demotes a node."""
    assert not is_design_stage(_plan(tmp_path, "---\nstatus: [unclosed\n"))


def test_recompute_persists_the_design_rung(tmp_path):
    """The rung is persisted so every reader sees it, including the Rust mux."""
    from fno.graph.statuses import recompute_statuses

    design = tmp_path / "d.md"
    design.write_text(DESIGN_FM)
    blueprint = tmp_path / "b.md"
    blueprint.write_text("---\nstatus: ready\n---\n")
    entries = [
        {"id": "x-i", "plan_path": None},
        {"id": "x-d", "plan_path": str(design)},
        {"id": "x-r", "plan_path": str(blueprint)},
        {"id": "x-p", "plan_path": str(design), "locked_by": "w", "claimed_at": _now()},
    ]
    got = {e["id"]: e["status"] for e in recompute_statuses(entries)}
    assert got == {"x-i": "idea", "x-d": "design", "x-r": "ready", "x-p": "in_progress"}


def test_recompute_rolls_container_status_up_from_real_child_edges(tmp_path):
    from fno.graph.statuses import recompute_statuses

    plan = tmp_path / "child.md"
    plan.write_text("---\nstatus: ready\n---\n")
    entries = [
        {"id": "x-parent", "type": "feature", "plan_path": None},
        {"id": "x-done", "parent": "x-parent", "completed_at": "2026-08-20T00:00:00Z"},
        {"id": "x-review", "parent": "x-parent", "plan_path": str(plan), "pr_number": 7},
        {"id": "x-open", "parent": "x-parent", "plan_path": str(plan)},
    ]

    recompute_statuses(entries)
    by_id = {entry["id"]: entry for entry in entries}
    assert by_id["x-parent"]["status"] == "in_progress"

    by_id["x-review"]["completed_at"] = "2026-08-20T00:01:00Z"
    by_id["x-open"]["completed_at"] = "2026-08-20T00:02:00Z"
    recompute_statuses(entries)
    assert by_id["x-parent"]["status"] == "done"


def test_recompute_keeps_container_in_review_when_it_owns_an_open_pr(tmp_path):
    """All-done children must not close a parent that still has its own open PR."""
    from fno.graph.statuses import recompute_statuses

    entries = [
        {"id": "x-parent", "pr_number": 42},
        {"id": "x-kid", "parent": "x-parent", "completed_at": "2026-08-20T00:00:00Z"},
    ]
    recompute_statuses(entries)
    parent = entries[0]
    # done here would drop the row off the board with its PR still open, while
    # completed_at stays absent so every open-ness predicate still reads it open.
    assert parent["status"] == "in_review"
    assert parent.get("completed_at") is None


def _fm(path) -> str:
    import re

    m = re.search(r"^status:\s*(.+?)\s*$", path.read_text(), re.M)
    return m.group(1).strip().strip("'\"") if m else ""


def _project_is_noop(tmp_path, monkeypatch, node) -> bool:
    """Project `node` through the keeper client over a seeded temp graph.

    True when the round trip left the doc's status line alone. The keeper
    fills graph defaults (priority, tags, ...) into the bare test row, so the
    mirror fields may legitimately change; the ladder guards only status.
    """
    import fno.graph.store as gs
    from fno.plan._project import project_graph_nodes
    from tests.fixtures.graph_seed import seed_graph

    g = tmp_path / "graph.json"
    # The writer's client reads the store, so the seed must materialize the
    # db, not just the frozen json export.
    seed_graph(g, {"entries": [node]})
    monkeypatch.setattr(gs, "GRAPH_JSON", g)
    from pathlib import Path

    plan = Path(node["plan_path"])
    before = _fm(plan)
    project_graph_nodes([node], [node["id"]])
    return _fm(plan) == before


def test_stale_graph_design_never_regresses_a_blueprinted_doc(tmp_path, monkeypatch):
    """`plan sync` must not undo a fresh `/blueprint`.

    `/blueprint` rewrites the doc design -> ready without touching the graph,
    and `read_graph` does not recompute, so the persisted `status` can still
    say `design` when the sweep runs. Repainting from that stale value would
    stamp `design` back onto the doc and un-blueprint it. The forward-only rule
    in the status projection is what prevents it.
    """
    plan = tmp_path / "p.md"
    plan.write_text("---\nstatus: ready\ntitle: T\n---\n\n# T\n\nbody\n")
    stale = {"id": "x-a", "plan_path": str(plan), "status": "design"}

    assert _project_is_noop(tmp_path, monkeypatch, stale)
    assert _fm(plan) == "ready"  # blueprint survives


def test_starvation_receipt_names_design_not_quarantined(tmp_path, monkeypatch):
    """A design-stage node is a lifecycle rung, not starvation.

    Reporting it as the generic `quarantined` would read as a stuck node and
    send an operator hunting for a problem that isn't there.
    """
    from datetime import datetime, timezone

    from fno.graph.cli import _starvation_receipts

    plan = tmp_path / "d.md"
    plan.write_text("---\nstatus: design\n---\n")
    node = {
        "id": "x-aaaa",
        "status": "ready",
        "plan_path": str(plan),
        "created_at": datetime.now(timezone.utc).isoformat(),
    }
    _seed_graph(tmp_path, monkeypatch, [node])
    out = _starvation_receipts(
        [node], None, True, None, set(), datetime.now(timezone.utc), 21
    )
    assert out == [("x-aaaa", "design")]


# ---------------------------------------------------------------------------
# plan_rung + the two named policies
# ---------------------------------------------------------------------------


def _undecodable(tmp_path, name: str = "bin.md"):
    target = tmp_path / name
    target.write_bytes(b"\xff\xfe\x00\x80not utf-8 at all")
    return {"id": "x-test", "plan_path": str(target)}


@pytest.mark.parametrize(
    "status,expected",
    [
        ("design", "DESIGN"),
        ("stub", "IDEA"),  # retired spelling, still on disk in old scaffolds
        ("shipped", "IN_REVIEW"),  # retired spelling
        ("archived", "SUPERSEDED"),  # retired spelling
    ],
)
def test_every_vocabulary_word_maps_to_its_rung(tmp_path, status, expected):
    from fno.graph.ladder import Rung, plan_rung

    entry = _plan(tmp_path, f"---\nstatus: {status}\n---\n")
    assert plan_rung(entry) is getattr(Rung, expected)


def test_no_plan_path_is_NONE_not_IDEA(tmp_path):
    """Distinct on purpose: only NONE means there is nothing on disk to fill."""
    from fno.graph.ladder import Rung, plan_rung

    assert plan_rung({"id": "x-test"}) is Rung.NONE
    assert plan_rung({"id": "x-test", "plan_path": ""}) is Rung.NONE
    assert plan_rung(_plan(tmp_path, "---\nstatus: idea\n---\n")) is Rung.IDEA


def test_AC4_ERR_unreadable_is_distinguished_from_status_less(tmp_path):
    """The collapse `plan_rung` must not inherit.

    `_read_plan_frontmatter` returns {} for missing, unreadable, malformed AND
    status-less alike. Those last two must route to opposite failure policies,
    so a resolver built on it would be unable to tell them apart at all.
    """
    from fno.graph.ladder import Rung, plan_rung

    status_less = _plan(tmp_path, "---\ntitle: a plan\n---\n\n# Doc\n", name="q.md")
    undecodable = _undecodable(tmp_path)

    assert plan_rung(status_less) is not plan_rung(undecodable)
    assert plan_rung(status_less) is Rung.READY
    assert plan_rung(undecodable) is Rung.UNREADABLE


def test_an_absent_status_is_not_the_same_defect_as_a_stub_status(tmp_path):
    """Silence stays READY; only a pre-design WORD demotes.

    `status: stub` read as `ready` because it was in no vocabulary - that is the
    bug. A doc with no `status:` at all is the older, legitimate shape (most of
    a mature vault), and `fno backlog intake` on one must still yield a workable
    node. Demoting silence too would empty the board to fix a bug it never had.
    """
    from fno.graph.ladder import Rung, is_dispatchable, plan_rung

    silent = _plan(tmp_path, "---\ntitle: An older plan\n---\n\n# Body\n", "s.md")
    stubbed = _plan(tmp_path, "---\nstatus: stub\n---\n", "t.md")

    assert plan_rung(silent) is Rung.READY
    assert is_dispatchable(silent) is True
    assert plan_rung(stubbed) is Rung.IDEA
    assert is_dispatchable(stubbed) is False


@pytest.mark.parametrize(
    "body",
    [
        "---\nstatus: design\n",  # frontmatter opened, never closed
        "---\nstatus: brand_new_word\n---\n",  # newer vocabulary, or corrupt
    ],
)
def test_uncertain_documents_park(tmp_path, body):
    from fno.graph.ladder import Rung, is_dispatchable, plan_rung

    entry = _plan(tmp_path, body)
    assert plan_rung(entry) is Rung.UNREADABLE
    assert is_dispatchable(entry) is False


def test_missing_file_is_unreadable_and_not_dispatchable(tmp_path):
    """A missing plan file reads UNREADABLE, except on an in_review node:
    the plan did its job once the PR opened, so a vanished file reads as the
    node's own rung and stays dispatchable. Any other status keeps failing
    closed."""
    from fno.graph.ladder import Rung, is_dispatchable, plan_rung

    entry = {"id": "x-test", "plan_path": str(tmp_path / "gone.md")}
    assert plan_rung(entry) is Rung.UNREADABLE
    assert is_dispatchable(entry) is False

    gone = {"id": "x-gone", "plan_path": str(tmp_path / "gone.md"), "status": "in_review"}
    building = {"id": "x-bld", "plan_path": str(tmp_path / "gone.md"), "status": "in_progress"}
    assert plan_rung(gone) is Rung.IN_REVIEW
    assert is_dispatchable(gone) is True
    assert plan_rung(building) is Rung.UNREADABLE
    assert is_dispatchable(building) is False


def test_is_cold_dispatchable_is_the_autonomous_drain_gate(tmp_path):
    """Admit status 'idea' AND rung NONE only.

    A linked-but-undesigned decompose stub (Rung.IDEA) stays excluded, and the
    status conjunct drops blocked / in_progress / ready plan-less nodes.
    """
    from fno.graph.ladder import is_cold_dispatchable

    # Plan-less idea -> admitted.
    assert is_cold_dispatchable({"id": "a", "status": "idea"}) is True
    # A linked undesigned doc (Rung.IDEA) is NOT admitted.
    stub = _plan(tmp_path, "---\nstatus: idea\n---\n\n# Child\n")
    stub["status"] = "idea"
    assert is_cold_dispatchable(stub) is False
    # The status conjunct excludes non-idea plan-less nodes.
    assert is_cold_dispatchable({"id": "b", "status": "blocked"}) is False
    assert is_cold_dispatchable({"id": "c", "status": "in_progress"}) is False
    assert is_cold_dispatchable({"id": "d", "status": "ready"}) is False


def test_plan_rung_never_raises_on_a_malformed_entry():
    from fno.graph.ladder import Rung, plan_rung

    for entry in (None, "a string", 42, [], {"plan_path": 7}):
        assert plan_rung(entry) in set(Rung)


def _seed_graph(tmp_path, monkeypatch, rows: list[dict]) -> None:
    """Persist the in-memory rows so the binary verdict sees the same graph."""
    import json as _json

    from tests.fixtures.graph_seed import seed_graph

    complete = []
    for e in rows:
        row = {"type": "feature", "priority": "p2", "status": "ready", **e}
        row.setdefault("title", e.get("id", "node"))
        row.setdefault("slug", e.get("id", "node"))
        complete.append(row)
    graph = tmp_path / "graph.json"
    seed_graph(graph, _json.dumps({"entries": complete}))
    monkeypatch.setattr("fno.paths.graph_json", lambda: graph)
