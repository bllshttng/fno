"""The deliverables-1 ratio measurement (x-cbab, task 7).

Shipped as a TASK, not a note: a ``target_denominator`` event recorded at init +
a ``fno do target denominator-ratio`` command that reads it. The cheap
``--deliverables 1`` exit is the load-bearing bypass risk; this measures whether
it is reflexive (past ~80 percent, ``enumerated_scope`` needs widening).
"""
from __future__ import annotations

import json
from datetime import datetime, timedelta

from typer.testing import CliRunner


def _event(denominator: str, *, count=None, days_ago: int = 0) -> str:
    ts = (datetime.now() - timedelta(days=days_ago)).isoformat()
    data = {"denominator": denominator}
    if count is not None:
        data["count"] = count
    return json.dumps({"ts": ts, "type": "target_denominator", "source": "target", "data": data})


def test_ratio_command_flags_a_bypass(monkeypatch, tmp_path):
    """80% deliverables:1 -> the verdict names the bypass."""
    from fno.cli import app

    (tmp_path / ".fno").mkdir()
    events = [
        _event("plan"),
        _event("plan"),
        *[_event("deliverables", count=1) for _ in range(8)],  # 8 of 10 declared
    ]
    (tmp_path / ".fno" / "events.jsonl").write_text("\n".join(events) + "\n")
    monkeypatch.setattr("fno.paths.resolve_repo_root", lambda *a, **k: tmp_path)

    r = CliRunner().invoke(app, ["do", "target", "denominator-ratio", "--json"])
    assert r.exit_code == 0, r.output
    obj = json.loads(r.output)
    assert obj["deliverables_1_ratio_pct"] == 80.0
    assert obj["deliverables_1"] == 8
    assert obj["plan_backed"] == 2
    assert obj["verdict"].startswith("bypass")


def test_ratio_command_respects_the_window(monkeypatch, tmp_path):
    """Events older than --since-days are excluded (a stale spike must not read current)."""
    from fno.cli import app

    (tmp_path / ".fno").mkdir()
    events = [
        _event("deliverables", count=1, days_ago=40),  # outside the 28d window
        _event("plan"),
    ]
    (tmp_path / ".fno" / "events.jsonl").write_text("\n".join(events) + "\n")
    monkeypatch.setattr("fno.paths.resolve_repo_root", lambda *a, **k: tmp_path)

    obj = json.loads(
        CliRunner().invoke(app, ["do", "target", "denominator-ratio", "--json", "--since-days", "28"]).output
    )
    assert obj["plan_backed"] == 1
    assert obj["deliverables_declared"] == 0  # the 40-day-old one excluded
