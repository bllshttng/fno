"""The failure streak `fno.graph.failure` derives from walker events."""

from __future__ import annotations


from fno.graph import failure as f


def _fail(nid: str) -> dict:
    return {
        "type": "node_failed",
        "data": {"session_id": "s", "unit_id": nid, "iteration": 1, "exit_code": 1},
    }


def _close(nid: str, close: str) -> dict:
    return {"type": "node_closed", "data": {"unit_id": nid, "close": close}}


def _undefer(nid: str) -> dict:
    # Flat agents-emitter envelope (fno.agents.events.emit shape).
    return {"kind": "node_undeferred", "unit_id": nid}


def test_streak_counts_failures_since_the_last_reset():
    x = "ab-x"
    cases = [
        ([], 0),
        ([_fail(x), _fail(x), _fail(x)], 3),
        # A parked close is a failure.
        ([_fail(x), _close(x, "parked")], 2),
        # Another node's failures never count.
        ([_fail(x), _fail("ab-y"), _fail(x)], 2),
        # A success close resets; a later failure counts from the boundary.
        ([_fail(x), _fail(x), _close(x, "closed")], 0),
        ([_fail(x), _fail(x), _close(x, "closed"), _fail(x)], 1),
        # An undefer is a reset boundary.
        ([_fail(x), _fail(x), _fail(x), _undefer(x), _fail(x)], 1),
        # A dispatch refusal neither counts nor resets.
        ([_fail(x), _close(x, "refused"), _fail(x)], 2),
    ]
    for events, want in cases:
        assert f.consecutive_failures(x, events) == want, events


def test_read_events_skips_malformed_lines_and_spans_rotation(tmp_path):
    from fno.events.store_client import emit_envelope, native_rows

    assert f.read_events(tmp_path / "nope.jsonl") == []

    live = {
        "ts": "2026-07-24T03:00:00Z",
        "source": "loop",
        "type": "node_failed",
        "data": {"session_id": "s", "unit_id": "ab-x", "iteration": 1, "exit_code": 1},
    }
    # A truncated or non-JSON line is skipped; valid lines still read.
    log = tmp_path / "a" / "events.jsonl"
    log.parent.mkdir()
    log.write_text(
        '{"ts":"2026-07-24T01:00:00Z","source":"test","type":"node_failed","data":{"unit_id":"ab-x"}}\n'
        '{"ts":"2026-07-24T02:00:00Z","source":"test","type":"node_failed","data":{"unit_id":"ab-x"\n'
        "not json at all\n"
    )
    emit_envelope(live, log)
    native_rows(log)
    events = f.read_events(log)
    assert len(events) == 2
    assert f.consecutive_failures("ab-x", events) == 2

    # A rotation boundary cannot reset a consecutive failure streak.
    active = tmp_path / "b" / "events.jsonl"
    active.parent.mkdir()
    (active.parent / "events.jsonl.1").write_text(
        '{"ts":"2026-07-24T01:00:00Z","source":"test","type":"node_failed","data":{"unit_id":"ab-x"}}\n'
        '{"ts":"2026-07-24T02:00:00Z","source":"test","type":"node_failed","data":{"unit_id":"ab-x"}}\n',
        encoding="utf-8",
    )
    emit_envelope(live, active)
    native_rows(active)
    assert f.consecutive_failures("ab-x", f.read_events(active)) == 3
