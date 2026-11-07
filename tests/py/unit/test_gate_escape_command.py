"""The manual `fno doctor event gate-escape` command + the rebase stale-base nudge (x-91b5).

Covers AC1-ERR (fail-closed enum), AC2-HP (manual tag counts), AC2-INV (dedup),
and AC1-UI (the rebase nudge is advisory-only - a note, never an event).
"""
from __future__ import annotations

from pathlib import Path

from _native_event_cli import EventRunner

cli = None
runner = EventRunner()


def _escapes(ev: Path, reason: str | None = None) -> list[dict]:
    from _event_rows import event_rows

    out = []
    for e in event_rows(ev):
        if e.get("type") != "gate_escape":
            continue
        if reason is None or e["data"]["reason"] == reason:
            out.append(e)
    return out


def test_ac1_err_unknown_reason_fails_loud_no_event(tmp_path):
    """AC1-ERR: an unknown reason exits non-zero, names the valid reasons, and
    emits nothing."""
    ev = tmp_path / "events.jsonl"
    res = runner.invoke(cli, ["gate-escape", "flek", "--events", str(ev)])
    assert res.exit_code != 0
    assert "flek" in res.output and "allowed" in res.output
    assert not ev.exists()


def test_ac2_hp_manual_tag_counts(tmp_path):
    """AC2-HP: a manual wedge tag lands one gate_escape retro can rank."""
    ev = tmp_path / "events.jsonl"
    res = runner.invoke(
        cli,
        ["gate-escape", "wedge", "--node", "x-1234", "--detail", "freed suspect claim",
         "--events", str(ev)],
    )
    assert res.exit_code == 0
    escapes = _escapes(ev, "wedge")
    assert len(escapes) == 1
    assert escapes[0]["data"]["graph_node_id"] == "x-1234"
    assert escapes[0]["data"]["detail"] == "freed suspect claim"


# The rebase nudge tests (AC1-UI) moved to
# crates/fno-agents/tests/pr_rebase_note.rs with the _rebase.py port to Rust.
