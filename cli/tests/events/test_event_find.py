"""Acceptance tests for the rotation-aware event journal query.

The native command resolves journals, folds rows and answers. The contract carries a coverage receipt, and a zero that coverage
cannot back exits 3.
"""
from __future__ import annotations

import json
from pathlib import Path

from tests._native_event_cli import EventRunner

event_cli = None


def _row(path: Path, row: dict) -> None:
    path.write_text(json.dumps(row) + "\n", encoding="utf-8")


def test_find_counts_rotated_rows(tmp_path: Path, monkeypatch) -> None:
    """A match in a retained segment is not a false zero."""
    live = tmp_path / "events.jsonl"
    rotated = live.with_name("events.jsonl.1")
    _row(live.with_name("events.jsonl.2"),
         {"ts": "2026-08-01T00:00:00Z", "type": "other", "data": {}})
    _row(rotated, {"ts": "2026-08-02T00:00:00Z", "kind": "operator_decision", "data": {}})
    _row(live, {"ts": "2026-08-03T00:00:00Z", "type": "other", "data": {}})

    result = EventRunner().invoke(event_cli, ["find", "operator_decision", "--json", "--events", str(live)])

    assert result.exit_code == 0, result.output
    payload = json.loads(result.output)
    assert payload["match_count"] == 1
    assert payload["file_count"] == 1
    assert payload["row_count"] == 3
    assert payload["fields_searched"] == ["type", "kind", "event"]


def test_find_zero_beyond_coverage_is_refused(tmp_path: Path, monkeypatch) -> None:
    """A zero reaching before the epoch is refused, never a confident zero."""
    live = tmp_path / "events.jsonl"
    _row(live, {"ts": "2026-08-03T00:00:00Z", "type": "other", "data": {}})

    result = EventRunner().invoke(event_cli, ["find", "operator_decision", "--since", "30d", "--events", str(live)])

    assert result.exit_code == 3, result.output
    assert "count refused" in result.output


def test_find_absent_store_refuses_the_zero(tmp_path: Path, monkeypatch) -> None:
    """A journal with no store proves nothing; its zero is refused."""
    missing = tmp_path / "events.jsonl"

    result = EventRunner().invoke(event_cli, ["find", "operator_decision", "--json", "--events", str(missing)])

    assert result.exit_code == 3, result.output
    payload = json.loads(result.output)
    assert payload["coverage"]["status"] == "unreadable"
    assert payload["file_count"] == 1
