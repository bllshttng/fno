"""Unit tests for the plan-less ``new`` filing word.

`new` rides the native create door (the scoped-create spelling of add), so
these tests exec the dev binary over a fixture store via run_native_create;
they skip where no dev build exists, like the other native-door suites.
"""
from __future__ import annotations
from tests.fixtures.graph_seed import seed_graph
from tests.conftest import run_native_create

import json
from pathlib import Path

import pytest


@pytest.fixture
def tmp_graph(tmp_path, monkeypatch) -> Path:
    g = tmp_path / "graph.json"
    seed_graph(g, '{"entries": []}\n')
    return g


def _seed(g: Path, entries: list[dict]) -> None:
    """Rows the way the store writes them: the typed api drops a row the
    model cannot parse, so seeds carry the stamped fields."""
    complete = []
    for e in entries:
        row = {"type": "feature", "priority": "p2", "status": "idea", **e}
        row.setdefault("title", e.get("id", "node"))
        row.setdefault("slug", e.get("id", "node"))
        if row["status"] == "done" and not row.get("completed_at"):
            row["completed_at"] = "2026-09-01T00:00:00Z"
        complete.append(row)
    seed_graph(g, json.dumps({"entries": complete}, indent=2) + "\n")


def _read(g: Path) -> list[dict]:
    from fno.graph.store import read_graph_strict

    return read_graph_strict(g)


def _new_id(result) -> str:
    """The create door's stdout receipt carries the id in its JSON body."""
    return json.loads(result.stdout)["id"]


# -- new --


def test_scenario4_hp_new_creates_entry(tmp_graph):
    """Scenario 4 (HP): fno new writes a new ab- entry with defaults."""
    result = run_native_create(
        tmp_graph, "new", "Research: Q3 budgets", "--domain", "research",
    )
    assert result.exit_code == 0, result.stderr
    new_id = _new_id(result)
    entries = _read(tmp_graph)
    assert len(entries) == 1
    e = entries[0]
    assert e["id"] == new_id
    assert e["title"] == "Research: Q3 budgets"
    assert e["domain"] == "research"
    # `fno new` creates plan-less nodes, so they derive to idea (not ready).
    # The `ready` state requires a plan_path.
    assert e["status"] == "idea"
    assert e["source"] == "fno-new"


def test_new_default_domain_is_code(tmp_graph):
    """Without --domain, new entries default to code."""
    result = run_native_create(tmp_graph, "new", "Some code task")
    assert result.exit_code == 0, result.stderr
    e = _read(tmp_graph)[0]
    assert e["domain"] == "code"


def test_new_sets_project_and_priority(tmp_graph):
    """--project and --priority flow through."""
    result = run_native_create(
        tmp_graph, "new", "Urgent thing", "--project", "acme", "--priority", "p1",
    )
    assert result.exit_code == 0, result.stderr
    e = _read(tmp_graph)[0]
    assert e["project"] == "acme"
    assert e["priority"] == "p1"


def test_scenario5_edge_new_fuzzy_domain_warns(tmp_graph):
    """Scenario 5 (EDGE): fuzzy domain match asks for --force-domain."""
    _seed(tmp_graph, [
        {"id": "ab-seed0001", "title": "seed", "status": "done",
         "domain": "research", "project": "p"},
    ])
    result = run_native_create(tmp_graph, "new", "New task", "--domain", "res")
    assert result.exit_code == 2, result.stdout + result.stderr
    combined = result.stdout + result.stderr
    assert "research" in combined
    assert "force-domain" in combined.lower()
    # No entry written
    entries = _read(tmp_graph)
    assert len(entries) == 1  # only the seed
    assert entries[0]["id"] == "ab-seed0001"


def test_scenario6_edge_new_force_domain_bypasses(tmp_graph):
    """Scenario 6 (EDGE): --force-domain bypasses the suggestion."""
    _seed(tmp_graph, [
        {"id": "ab-seed0001", "title": "seed", "status": "done",
         "domain": "research", "project": "p"},
    ])
    result = run_native_create(
        tmp_graph, "new", "New task", "--domain", "res", "--force-domain",
    )
    assert result.exit_code == 0, result.stderr
    # New entry has domain="res" verbatim (not auto-corrected to research)
    entries = _read(tmp_graph)
    new_entries = [e for e in entries if e["id"] != "ab-seed0001"]
    assert len(new_entries) == 1
    assert new_entries[0]["domain"] == "res"


def test_new_exact_domain_match_no_warning(tmp_graph):
    """Exact domain match (confidence=exact) does NOT trigger warning."""
    _seed(tmp_graph, [
        {"id": "ab-seed0001", "title": "seed", "status": "done",
         "domain": "research", "project": "p"},
    ])
    result = run_native_create(tmp_graph, "new", "New task", "--domain", "research")
    assert result.exit_code == 0, result.stderr


def test_new_unfamiliar_domain_passes_through(tmp_graph):
    """Truly new domain (confidence=new, no prefix collision) passes through."""
    _seed(tmp_graph, [
        {"id": "ab-seed0001", "title": "seed", "status": "done",
         "domain": "code", "project": "p"},
    ])
    result = run_native_create(tmp_graph, "new", "New task", "--domain", "trading")
    assert result.exit_code == 0, result.stderr
    entries = _read(tmp_graph)
    new_entries = [e for e in entries if e["id"] != "ab-seed0001"]
    assert new_entries[0]["domain"] == "trading"


def test_new_id_has_correct_prefix_and_length(tmp_graph):
    """Generated id matches the ab-xxxxxxxx pattern."""
    result = run_native_create(tmp_graph, "new", "T")
    assert result.exit_code == 0, result.stderr
    e = _read(tmp_graph)[0]
    assert e["id"].startswith("ab-")
    assert len(e["id"]) == 11  # ab- + 8 hex chars


def test_new_sets_created_at_iso8601(tmp_graph):
    """created_at is ISO 8601."""
    result = run_native_create(tmp_graph, "new", "T")
    assert result.exit_code == 0, result.stderr
    e = _read(tmp_graph)[0]
    assert e["created_at"]
    assert "T" in e["created_at"]


# -- Task 1.2: --source-* flags on fno new --


def test_ac1_hp_new_with_source_flags_populates_provenance(tmp_graph):
    """AC1-HP: fno new --source-kind from_inbox creates entry with all four source fields."""
    result = run_native_create(
        tmp_graph, "new", "Add region filter",
        "--project", "acme-web",
        "--source-kind", "from_inbox",
        "--source-project", "example-pipeline",
        "--source-inbox-msg", "msg-a4f1",
    )
    assert result.exit_code == 0, result.stderr
    entries = _read(tmp_graph)
    assert len(entries) == 1
    e = entries[0]
    assert e["source_kind"] == "from_inbox"
    assert e["source_project"] == "example-pipeline"
    assert e["source_inbox_msg"] == "msg-a4f1"
    # source_session_id not provided, should be None
    assert e.get("source_session_id") is None


def test_ac1_hp_new_with_source_session_id(tmp_graph):
    """AC1-HP: --source-session-id is also stored on the entry."""
    result = run_native_create(
        tmp_graph, "new", "Session sourced task",
        "--source-kind", "from_supervisor",
        "--source-session-id", "sess-xyz123",
    )
    assert result.exit_code == 0, result.stderr
    e = _read(tmp_graph)[0]
    assert e["source_kind"] == "from_supervisor"
    assert e["source_session_id"] == "sess-xyz123"


def test_ac2_err_new_invalid_source_kind_rejected(tmp_graph):
    """AC2-ERR: --source-kind invalid_value exits non-zero, no graph mutation."""
    result = run_native_create(tmp_graph, "new", "x", "--source-kind", "invalid_value")
    assert result.exit_code != 0, result.stdout
    # No entries written
    entries = _read(tmp_graph)
    assert len(entries) == 0


def test_new_source_kind_defaults_to_organic(tmp_graph):
    """Without --source-kind, new entries default to source_kind=organic."""
    result = run_native_create(tmp_graph, "new", "Organic task")
    assert result.exit_code == 0, result.stderr
    e = _read(tmp_graph)[0]
    assert e["source_kind"] == "organic"
    assert e.get("source_project") is None
    assert e.get("source_session_id") is None
    assert e.get("source_inbox_msg") is None
