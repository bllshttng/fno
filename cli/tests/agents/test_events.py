"""Tests for fno.agents.events.

Locks down: emit() flattens data fields beside ts/kind, promotes a fair-usage
failure to a rate-limit event, never raises on a filesystem error, and the
identity-resolution record carries every marker and rejection.
"""
from __future__ import annotations

from pathlib import Path


from fno.paths_testing import use_tmpdir
from tests._event_rows import event_rows


def test_emit_includes_kind_and_data_fields(tmp_path: Path, monkeypatch) -> None:
    """emit() flattens kwargs into the top-level JSON object alongside ts/kind."""
    use_tmpdir(monkeypatch, tmp_path)
    from fno.agents.events import emit

    events_path = tmp_path / ".fno" / "events.jsonl"
    emit(
        "agent_ask_done",
        name="bar",
        provider="codex",
        short_id="7c5dcf5d",
        duration_ms=1234,
        reply_chars=456,
        path=events_path,
    )
    parsed = event_rows(events_path)[0]
    assert parsed["kind"] == "agent_ask_done"
    assert parsed["name"] == "bar"
    assert parsed["provider"] == "codex"
    assert parsed["short_id"] == "7c5dcf5d"
    assert parsed["duration_ms"] == 1234


def test_emit_promotes_fair_usage_error_to_rate_limited_event(
    tmp_path: Path, monkeypatch
) -> None:
    use_tmpdir(monkeypatch, tmp_path)
    from fno.agents.events import emit

    events_path = tmp_path / ".fno" / "events.jsonl"
    emit(
        "agent_ask_failed",
        provider="zai",
        account="readyrule",
        error="HTTP 429 Fair Usage code 1313",
        path=events_path,
    )
    records = event_rows(events_path)
    assert records[-1]["kind"] == "provider_rate_limited"
    assert records[-1]["provider"] == "zai"
    assert records[-1]["account"] == "readyrule"


def test_emit_swallows_oserror_and_warns(
    tmp_path: Path, monkeypatch, capsys
) -> None:
    """emit() is best-effort — an OSError from the filesystem is logged, not raised."""
    use_tmpdir(monkeypatch, tmp_path)
    from fno.agents.events import emit

    # Point at an unwritable target (read-only parent).
    parent = tmp_path / "readonly"
    parent.mkdir()
    parent.chmod(0o500)
    target = parent / "events.jsonl"

    try:
        # Must not raise — telemetry failures cannot break primary ops.
        emit("agent_ask_started", path=target)
    finally:
        parent.chmod(0o700)  # let pytest clean up

    captured = capsys.readouterr()
    assert "warning" in captured.err.lower()
    assert "agent_ask_started" in captured.err


