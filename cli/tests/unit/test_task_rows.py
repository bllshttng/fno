"""Tests for `fno backlog task` - the native claim-gated task grain.

The verb answers from the binary (the claim IS the transition), so these
exec it over a fixture store with the plan file the node binds to; they
skip where this checkout has no dev build.
"""
from __future__ import annotations
from tests._native_door import run_native
from tests.fixtures.graph_seed import seed_graph

import json
from pathlib import Path

import pytest

PLAN_BODY = """# Plan

## Execution Strategy

```yaml
tasks:
  - id: 1.1
  - id: 1.2
```
"""


@pytest.fixture
def task_fixture(tmp_path, monkeypatch) -> tuple[Path, str]:
    monkeypatch.setenv("HOME", str(tmp_path))
    monkeypatch.setenv("FNO_STATE_DIR", str(tmp_path))
    monkeypatch.setenv("FNO_CLAIMS_ROOT", str(tmp_path / "claims"))
    plan = tmp_path / "plan.md"
    plan.write_text(PLAN_BODY, encoding="utf-8")
    seed_graph(
        tmp_path / "graph.json",
        json.dumps({"entries": [{
            "id": "x-ta5k0001",
            "type": "feature",
            "priority": "p2",
            "status": "in_progress",
            "title": "Grained",
            "slug": "grained",
            "plan_path": str(plan),
        }]})
        + "\n",
    )
    return tmp_path, "x-ta5k0001"


def test_in_progress_takes_the_claim_and_writes_the_row(task_fixture):
    tmp_path, node = task_fixture
    code, out, err = run_native(
        "backlog", "task", "update", node, "1.1", "--status", "in_progress",
        "--owner", "worker-a",
    )
    assert code == 0, out + err
    assert "in_progress holder=worker-a" in out
    code, out, err = run_native("backlog", "task", "list", node, "--json")
    assert code == 0, out + err
    rows = {r["id"]: r for r in json.loads(out)["tasks"]}
    assert rows["1.1"]["status"] == "in_progress"
    assert rows["1.1"]["owner"] == "worker-a"
    assert rows["1.2"]["status"] == "pending"
    assert rows["1.2"]["owner"] is None


def test_a_second_holder_is_refused(task_fixture):
    _tmp_path, node = task_fixture
    code, out, err = run_native(
        "backlog", "task", "update", node, "1.1", "--status", "in_progress",
        "--owner", "worker-a",
    )
    assert code == 0, out + err
    code, out, err = run_native(
        "backlog", "task", "update", node, "1.1", "--status", "in_progress",
        "--owner", "worker-b",
    )
    assert code == 3, out + err
    assert "another holder owns" in (out + err)


def test_done_writes_the_row_and_releases_the_claim(task_fixture):
    _tmp_path, node = task_fixture
    run_native(
        "backlog", "task", "update", node, "1.1", "--status", "in_progress",
        "--owner", "worker-a",
    )
    code, out, err = run_native(
        "backlog", "task", "update", node, "1.1", "--status", "done",
        "--owner", "worker-a",
    )
    assert code == 0, out + err
    code, out, err = run_native("backlog", "task", "list", node, "--json")
    rows = {r["id"]: r for r in json.loads(out)["tasks"]}
    assert rows["1.1"]["status"] == "done"
    # The released claim frees the task for the next worker.
    code, out, err = run_native(
        "backlog", "task", "update", node, "1.1", "--status", "in_progress",
        "--owner", "worker-b",
    )
    assert code != 0, "re-opening shipped work must refuse"


def test_list_materializes_pending_rows(task_fixture):
    _tmp_path, node = task_fixture
    code, out, err = run_native("backlog", "task", "list", node)
    assert code == 0, out + err
    lines = out.strip().splitlines()
    assert lines[0].startswith("1.1\tpending\t-")
    assert lines[1].startswith("1.2\tpending\t-")
