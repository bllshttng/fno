"""Tests for worktree overlap recording and recurrence reporting.

Every test injects a temp ``journal`` path so nothing touches the real
machine-global ``~/.fno/events.jsonl``. The fold is exercised both through the
recording verb (real append + re-read) and directly with hand-written events.
"""
from __future__ import annotations

import json
from datetime import datetime, timezone
from pathlib import Path

import pytest

from fno.events import worktree_overlap_observed
from fno.worktree_cli.overlaps import (
    overlaps_report,
)

NOW = datetime(2026, 8, 3, 12, 0, 0, tzinfo=timezone.utc)
PAYLOAD = (
    '{"observer_session_id":"obs-1","peer_session_ids":["peer-a"],'
    '"worktree_git_dir":"/r/.git/worktrees/wt1",'
    '"repository_common_dir":"/r/.git","live_window_seconds":120}'
)


def _overlap_event(
    *,
    observer: str,
    peers: list[str],
    worktree: str,
    ts: datetime,
    repo: str = "/r/.git",
) -> dict:
    """A valid overlap event with a controlled envelope ts (for window tests)."""
    event = worktree_overlap_observed(
        observer_session_id=observer,
        peer_session_ids=peers,
        repository_key=repo,
        worktree_key=worktree,
    )
    event["ts"] = ts.isoformat().replace("+00:00", "Z")
    return event


def test_malformed_line_makes_report_partial_and_exits_nonzero(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setattr("fno.events.store_client.native_rows", lambda *a, **k: None)
    journal = tmp_path / "events.jsonl"
    journal.write_text(
        json.dumps(_overlap_event(observer="o1", peers=["a"], worktree="/r/.git/wt1", ts=NOW))
        + "\nnot-valid-json\n",
        encoding="utf-8",
    )
    report, code = overlaps_report(journal=journal, now=NOW)
    assert report["state"] == "partial"
    assert code == 1, "partial evidence must not read as zero"
    assert report["coverage"]["malformed_lines"] == 1
    assert report["distinct_observations"] == 1, "the valid line still counts"


# -- carrier contract: the real Typer command accepts the carrier's invocation --
#
# The unit tests above call the Python functions directly; the hook test uses a
# fno shim. Neither exercises real flag parsing, so a missing --stdin option on
# the command would ship green while the carrier's invocation failed every time.
# This pins the carrier's exact argv against the real command.


def test_overlap_record_cli_accepts_carrier_invocation(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    from typer.testing import CliRunner

    monkeypatch.setenv("FNO_HOME", str(tmp_path / "fno"))
    runner = CliRunner()
    from fno.worktree_cli.cli import app

    result = runner.invoke(
        app,
        ["overlap-record", "--stdin", "--since", "28"],
        input=PAYLOAD,
    )
    assert result.exit_code == 0, result.output
    parsed = json.loads(result.stdout.strip())
    assert parsed["recorded"] is True
    assert parsed["fold"]["distinct_observations"] == 1
    # The store beside the journal now holds one durable row; the raw file
    # may stay absent (the commit is the write boundary). The shared sandbox
    # journal also carries rows from other tests, so filter by type.
    from tests._event_rows import event_rows
    from fno.paths import global_events_json

    journal = global_events_json()
    rows = [
        r for r in event_rows(journal)
        if r.get("type") == "worktree_overlap_observed"
    ]
    assert rows, f"no worktree_overlap_observed row in {journal}"
