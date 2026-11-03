"""Tests for the work/maintenance classifier in scripts/diagnostics/dsplit.py.

dsplit feeds the slowness gate's daemon-child axis, and the gate reads the
maintenance figure. A misclassification moves fleet loop work inside the
gate's budget, so the class rule is pinned here: nearest direct-child
ancestor decides, and the per-session worker binary roots fleet work.
"""
from __future__ import annotations

import importlib.util
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[3]
SCRIPT = REPO_ROOT / "scripts" / "diagnostics" / "dsplit.py"


def _load():
    spec = importlib.util.spec_from_file_location("dsplit", SCRIPT)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


ds = _load()


def _rows(pairs):
    return {pid: (ppid, 0.0, cmd) for pid, ppid, cmd in pairs}


def test_worker_subtree_reads_work_and_daemon_chores_read_maintenance():
    rows = _rows([
        (2, 1, "/Users/bb16/.cargo/bin/fno-agents-worker --stream --short-id w1 -- claude"),
        (3, 2, "claude --print x"),
        (4, 3, "rg pattern"),
        (5, 1, "/Users/bb16/.cargo/bin/fno-agents backlog reconcile --json"),
        (6, 5, "/Users/bb16/.cargo/bin/fno backlog render-views"),
        (7, 1, "gh pr checks 12"),
        (8, 1, "claude agents --json --all"),
    ])
    members, classes = ds.adopt(rows, {1}, {}, 1)
    assert members == {1, 2, 3, 4, 5, 6, 7, 8}
    assert classes == {2: True, 3: True, 4: True,
                       5: False, 6: False, 7: False, 8: False}


def test_plain_fno_agents_child_is_never_work():
    # The prefix match keys on the worker binary; the plain CLI stays upkeep.
    rows = _rows([(2, 1, "/Users/bb16/.cargo/bin/fno-agents backlog decisions n --json")])
    _, classes = ds.adopt(rows, {1}, {}, 1)
    assert classes == {2: False}


def test_is_work_reads_the_executable_and_ignores_the_arguments():
    assert ds.is_work("/opt/homebrew/bin/fno-agents-worker --stream --short-id w1")
    assert ds.is_work("fno-agents-worker")
    assert not ds.is_work("fno-agents backlog reconcile --json")
    assert not ds.is_work("")
    assert not ds.is_work("   ")


def test_label_names_the_command_kind_with_its_arguments():
    assert ds.label("/Users/bb16/.cargo/bin/fno-agents backlog reconcile --json") == \
        "fno-agents backlog reconcile"
    assert ds.label("gh pr checks 12 --json name,bucket") == "gh pr checks"
    assert ds.label("") == "?"
    assert len(ds.label("x " + "a" * 100)) <= 48
