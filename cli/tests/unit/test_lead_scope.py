"""Coverage for the lead board's surviving Python surfaces."""

from __future__ import annotations

import pytest
from typer.testing import CliRunner

from fno.lead.scope import compile_scope_ids, scope_undelivered


def test_project_scope_compiles_to_the_project_union():
    entries = [
        {"id": "f-1", "project": "fdata"},
        {"id": "e-1", "project": "etl"},
    ]
    assert compile_scope_ids("fdata", entries, resolve=lambda _: (1, "fdata")) == {"f-1"}
    assert compile_scope_ids(
        "fdata,etl", entries, resolve=lambda _: (0, "fdata,etl")
    ) == {"f-1", "e-1"}


# --- the compile resolves against the rows it holds -----------------


@pytest.fixture
def _no_projects(tmp_path, monkeypatch):
    """No projects configured: the native resolve door refuses every name.
    Non-project spawn-overlay shapes exec the real binary (the ladder door
    rides the same env pin)."""
    from fno.rust_binary import find_dev_binary, resolve_binary

    real = find_dev_binary() or resolve_binary()
    fallback = f'exec "{real}" "$@"\n' if real else "exit 1\n"
    stub = tmp_path / "agents-stub"
    stub.write_text(
        "#!/bin/sh\n"
        'if [ "$2" = "project" ]; then\n'
        "  exit 1\n"
        "fi\n" + fallback,
        encoding="utf-8",
    )
    stub.chmod(0o755)
    monkeypatch.setenv("FNO_AGENTS_BIN", str(stub))


def test_an_epic_set_compiles_without_a_second_graph_read(_no_projects, monkeypatch):
    # The compile holds the entries; resolve_role must not pay a
    # whole-graph parse to relearn what by_id already knows.
    from fno.agents import role as role_mod

    entries = [
        {"id": "x-a", "type": "epic", "project": "fno"},
        {"id": "x-a1", "parent": "x-a", "project": "fno"},
        {"id": "x-b", "type": "epic", "project": "fno"},
        {"id": "x-b1", "parent": "x-b", "project": "fno"},
    ]

    def _refuse():
        raise AssertionError("a held-row compile must not re-read the graph")

    monkeypatch.setattr(role_mod, "_graph_index", _refuse)
    ids = compile_scope_ids("x-a,x-b", entries)
    assert ids == {"x-a", "x-a1", "x-b", "x-b1"}


def test_a_non_epic_member_still_refuses_through_the_held_rows(_no_projects):
    # The held-row resolver enforces the same epic-only rule.
    entries = [
        {"id": "x-a", "type": "epic", "project": "fno"},
        {"id": "x-n", "type": "feature", "project": "fno"},
    ]
    with pytest.raises(ValueError, match="not an epic"):
        compile_scope_ids("x-a,x-n", entries)


# --- the drain read (the term goal's own count) -------------------------------


def test_scope_undelivered_counts_only_nodes_not_done_or_superseded():
    entries = [
        {"id": "x-root", "type": "epic"},
        {
            "id": "x-done",
            "parent": "x-root",
            "status": "done",
            "completed_at": "2026-09-06T00:00:00Z",
        },
        {"id": "x-sup", "parent": "x-root", "superseded_by": "x-done"},
        {"id": "x-driven", "parent": "x-root", "status": "in_review", "pr": 1497},
        {"id": "x-blocked", "parent": "x-root", "status": "blocked"},
        {"id": "x-deferred", "parent": "x-root", "status": "deferred"},
        {"id": "x-outside", "status": "in_progress"},
    ]
    assert scope_undelivered("x-root", entries, resolver=lambda _: (2, "x-root")) == 4


def test_scope_undelivered_raises_on_an_uncompilable_scope():
    # A reader that would END a term on zero must see the failure, not read
    # it as drained; the wake caller catches this itself.
    entries = [{"id": "x-root", "type": "feature"}]
    with pytest.raises(ValueError):
        scope_undelivered("x-root", entries, resolver=lambda _: (2, "x-root"))


def test_drain_refuses_graph_scope_reads_under_an_external_tracker(
    tmp_path, monkeypatch
):
    from fno.lead.cli import agents_lead_app
    from fno.paths_testing import use_tmpdir

    use_tmpdir(monkeypatch, tmp_path)
    monkeypatch.setenv("FNO_TRACKER_BACKEND", "github")

    result = CliRunner().invoke(agents_lead_app, ["drain", "x-root"])

    assert result.exit_code == 1, result.output
    assert "unavailable under an external tracker backend" in result.output


# --- the human board renderer ------------------------------------------------


def _queue(**overrides):
    q = {
        "name": "undispatched",
        "source": "fno backlog undispatched --json",
        "status": "ok",
        "error": "",
        "count": 4,
        "actionable": True,
        "note": "one worker per node",
        "verb": "/fno:target",
    }
    q.update(overrides)
    return q


def test_render_names_an_over_budget_queue_not_a_count(capsys):
    from fno.lead.cli import _render

    q = _queue(
        status="over_budget",
        error="fno backlog undispatched --json: killed at its 4.6s slice of the board budget; the source did not fail",
        rows=[],
    )
    board = {"actionable": 0, "warnings": [], "queues": [q]}
    _render(board, 25)
    out = capsys.readouterr().out
    assert "NOT READ" in out
    assert "killed at its 4.6s slice" in out
    assert "None" not in out
