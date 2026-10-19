"""The Python writer for `control_plane_tick` rows (x-1b88)."""
from __future__ import annotations


from fno.control_plane import emit_tick


def test_emit_tick_writes_a_schema_valid_row(tmp_path, monkeypatch):
    journal = tmp_path / "events.jsonl"
    monkeypatch.setenv("FNO_EVENTS_PATH", str(journal))

    ok = emit_tick(
        "watchdog",
        scheduler="launchd:sh.fno.pr-watcher",
        interval_s=600,
        acted=2,
        detail="wake=1 report=1",
    )

    assert ok is True
    from tests._event_rows import event_rows

    rows = event_rows(journal)
    assert len(rows) == 1
    row = rows[0]
    assert row["type"] == "control_plane_tick"
    assert row["data"] == {
        "arm": "watchdog",
        "scheduler": "launchd:sh.fno.pr-watcher",
        "acted": 2,
        "interval_s": 600,
        "detail": "wake=1 report=1",
    }


def test_emit_tick_never_raises_on_a_dead_journal(tmp_path):
    # A path whose parent is a FILE: every write fails; the arm it observes
    # must not break.
    blocker = tmp_path / "blocker"
    blocker.write_text("not a dir")
    ok = emit_tick(
        "stop_hook",
        scheduler="hook:target-stop-hook",
        interval_s=0,
        events_path=blocker / "events.jsonl",
    )
    assert ok is False
