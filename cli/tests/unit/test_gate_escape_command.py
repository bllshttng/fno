"""The manual `fno doctor event gate-escape` command + the rebase stale-base nudge (x-91b5).

Covers AC1-ERR (fail-closed enum), AC2-HP (manual tag counts), AC2-INV (dedup),
and AC1-UI (the rebase nudge is advisory-only - a note, never an event).
"""
from __future__ import annotations

from pathlib import Path

from tests._native_event_cli import EventRunner

cli = None
runner = EventRunner()


def _escapes(ev: Path, reason: str | None = None) -> list[dict]:
    from tests._event_rows import event_rows

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


def test_ac2_inv_dedup_same_key(tmp_path):
    """AC2-INV: three tags with the same explicit dedup key count once."""
    ev = tmp_path / "events.jsonl"
    for _ in range(3):
        runner.invoke(
            cli, ["gate-escape", "spawn-cap", "--dedup-key", "k1", "--events", str(ev)]
        )
    assert len(_escapes(ev, "spawn-cap")) == 1


def test_pr_and_dedup_key_together_rejected(tmp_path):
    """The command rejects --pr + --dedup-key with a clean error instead of
    silently dropping --dedup-key (gemini review on #241)."""
    ev = tmp_path / "events.jsonl"
    res = runner.invoke(
        cli,
        ["gate-escape", "flake", "--pr", "7", "--dedup-key", "k", "--events", str(ev)],
    )
    assert res.exit_code != 0
    assert "XOR" in res.output
    assert not ev.exists()


# The rebase nudge tests (AC1-UI) moved to
# crates/fno-agents/tests/pr_rebase_note.rs with the _rebase.py port to Rust.


def test_store_failure_records_a_counter_without_failing_host(tmp_path):
    journal = tmp_path / "events.jsonl"
    (tmp_path / "events.db").mkdir()
    result = runner.invoke(cli, ["gate-escape", "spawn-cap", "--dedup-key", "k", "--events", str(journal)])
    assert result.exit_code == 0
    assert "failed" in result.stderr
    import json
    failure = tmp_path / "gate_escape_emit_failures.jsonl"
    assert json.loads(failure.read_text().splitlines()[0])["reason"] == "spawn-cap"


def test_equals_spelling_preserves_values(tmp_path):
    ev = tmp_path / "events.jsonl"
    res = runner.invoke(cli, ["gate-escape", "flake", f"--events={ev}", "--detail=an=value", "--dedup-key=k"])
    assert res.exit_code == 0, res.output
    assert _escapes(ev)[0]["data"]["detail"] == "an=value"
