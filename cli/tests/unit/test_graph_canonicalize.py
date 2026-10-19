"""Unit tests for graph entry canonicalization: status-forward key order +
derived ``children`` summary index.

Canonicalization runs inside ``commit_rows_via_store`` after ``recompute_statuses``
so every write produces a consistent, status-near-top key order and a fresh
inverse-of-``parent`` ``children`` index. The index entries are compact
summaries ``{id, title, project, status}`` -- enough to scan a node's children
without a second lookup, light enough not to denormalize the flat store.
"""
from __future__ import annotations

from pathlib import Path

import pytest

from fno.rust_binary import find_dev_binary
from fno.graph.store import (
    canonical_field_order,
    canonicalize_entries,
    commit_rows_via_store,
    read_graph_strict,
    _read_json,
)
from tests.fixtures.graph_seed import seed_graph

# Since the store port every test here rides the keeper, so the module needs
# the compiled runtime and skips whole where the smoke harness deleted the
# worker binary (the parity-test convention). The skip keeps the import-time
# canonical_field_order() read below from ever running without a worker.
requires_rust = pytest.mark.dev_build

pytestmark = requires_rust

# The canonical key order, read from the ported store: one source of truth,
# never a re-typed copy that drifts from the on-disk shape. Guarded so a
# worker-less import (the smoke shard) stays a clean module skip instead of
# a collection error.
CANONICAL_FIELD_ORDER = canonical_field_order() if find_dev_binary() else []


def _make_graph(tmp_path: Path, entries: list[dict]) -> Path:
    p = tmp_path / "graph.json"
    seed_graph(p, {"entries": entries})
    return p


def _noop(entries):
    return entries


# -- key ordering --


def test_status_is_second_key_after_id(tmp_path):
    """After a mutation, every written entry has ``status`` as its 2nd key."""
    path = _make_graph(tmp_path, [{"id": "ab-aaaa0001", "title": "T"}])
    commit_rows_via_store(path, _noop)
    raw = _read_json(path)  # raw read preserves on-disk key order
    keys = list(raw[0].keys())
    assert keys[0] == "id"
    assert keys[1] == "status"


def test_extra_keys_preserved_at_end(tmp_path):
    """Legacy/unknown keys (e.g. ``points``) survive canonicalization, appended
    after the canonical keys -- never dropped."""
    path = _make_graph(
        tmp_path, [{"id": "ab-aaaa0003", "title": "T", "points": 5}]
    )
    commit_rows_via_store(path, _noop)
    raw = _read_json(path)
    assert raw[0]["points"] == 5
    # appended after the canonical block
    keys = list(raw[0].keys())
    assert keys.index("points") > keys.index("status")


# -- children index --


def test_legacy_graph_lock_timestamp_migrates_once_without_nested_rename(tmp_path):
    """AC6-HP/AC6-ERR: migrate only the top-level graph lock timestamp."""
    timestamp = "2026-08-23T12:34:56+00:00"
    path = _make_graph(
        tmp_path,
        [
            {
                "id": "ab-legacy-lock",
                "title": "Legacy lock",
                "status": "in_progress",
                "locked_by": "worker-1",
                "session_id": "worker-1",
                "claimed_at": timestamp,
                "sessions": [
                    {
                        "phase": "do",
                        "harness": "claude",
                        "session_id": "worker-1",
                        "claimed_at": timestamp,
                    }
                ],
            }
        ],
    )

    commit_rows_via_store(path, _noop)

    # The stored row through the store read: the legacy top-level key itself
    # is retired by the publish, and the lock family now projects from the
    # claim store, so the unbacked legacy lock reads unheld. The nested
    # session copy is a different record and survives untouched.
    raw = read_graph_strict(path)[0]
    assert raw["locked_at"] is None
    assert raw.get("claimed_at") is None
    assert raw["sessions"][0]["claimed_at"] == timestamp


# -- canonicalize_entries unit (no I/O) --


def test_canonicalize_entries_pure():
    entries = [
        {"id": "ab-p", "title": "P", "status": "idea"},
        {"id": "ab-c", "title": "C", "parent": "ab-p", "status": "idea"},
    ]
    out = canonicalize_entries(entries)
    by_id = {e["id"]: e for e in out}
    assert by_id["ab-p"]["children"] == [
        {"id": "ab-c", "title": "C", "project": None, "status": "idea"}
    ]
    assert list(by_id["ab-c"].keys())[:2] == ["id", "status"]
