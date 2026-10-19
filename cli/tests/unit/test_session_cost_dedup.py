#!/usr/bin/env python3
"""Unit tests for transcript dedup in fno.cost._session_cost (the former
scripts/metrics/session-cost.py).

Claude Code writes one transcript JSONL line per content block; every line
of the same API message repeats identical `message.usage`. Without dedup by
`(message.id, requestId)` the parser overstates tokens ~2.5-2.8x (verified
on a live transcript: 502 assistant lines -> 185 unique pairs).

Covers AC1-HP / AC1-ERR / AC1-UI / AC1-EDGE from the cost-accuracy plan.

Run: fno doctor test cli/tests/unit/test_session_cost_dedup.py
"""

import contextlib
import io
import json
import sys
import tempfile
from pathlib import Path

# Family goldens live under cli/tests (the cost family child owns them);
# fno resolves through the pinned worktree PYTHONPATH.
from fno.cost import _session_cost as session_cost  # noqa: E402

USAGE = {
    "input_tokens": 10,
    "output_tokens": 20,
    "cache_read_input_tokens": 1000,
    "cache_creation_input_tokens": 50,
}


import pytest  # noqa: E402


@pytest.fixture(autouse=True)
def _no_price_subprocess(monkeypatch):
    # The price leg is a subprocess; these tests pin the parse, not the
    # catalog. Resolve no binary so calculate_cost short-circuits unpriced
    # and no test shells a real fno-agents.
    monkeypatch.setattr(session_cost, "_fno_agents_binary", lambda: None)


def _assistant_line(
    msg_id: str,
    request_id: str | None,
    usage: dict | None = None,
    model: str = "claude-opus-4-8",
    ts: str = "2026-06-04T20:19:34.000Z",
) -> dict:
    message: dict = {"id": msg_id, "model": model, "usage": dict(usage or USAGE)}
    if msg_id is None:
        del message["id"]
    line = {
        "type": "assistant",
        "timestamp": ts,
        "message": message,
        "gitBranch": "main",
    }
    if request_id is not None:
        line["requestId"] = request_id
    return line


def _write_transcript(path: Path, lines: list) -> None:
    with open(path, "w") as f:
        for line in lines:
            f.write((line if isinstance(line, str) else json.dumps(line)) + "\n")


def _parse(lines: list, **kwargs):
    with tempfile.TemporaryDirectory() as td:
        p = Path(td) / "t.jsonl"
        _write_transcript(p, lines)
        return session_cost.parse_transcript(str(p), "test-session", **kwargs)


# --- AC1-HP: deduped, correctly priced cost ----------------------------------


def test_duplicate_lines_count_once():
    # Each API message spans 3 JSONL lines with byte-identical usage.
    lines = []
    for i in range(2):
        for _ in range(3):
            lines.append(_assistant_line(f"msg_{i}", f"req_{i}"))
    metrics = _parse(lines)
    assert metrics.input_tokens == 2 * USAGE["input_tokens"], metrics.input_tokens
    assert metrics.output_tokens == 2 * USAGE["output_tokens"]
    assert metrics.cache_read_tokens == 2 * USAGE["cache_read_input_tokens"]
    assert metrics.cache_create_tokens == 2 * USAGE["cache_creation_input_tokens"]
    assert metrics.assistant_messages == 2, metrics.assistant_messages


# --- AC1-ERR: missing dedup keys ----------------------------------------------


def test_lines_missing_request_id_count_as_is():
    lines = [
        _assistant_line("msg_0", None),  # no requestId: counted as-is
        _assistant_line("msg_0", None),  # again: counted again (no false dedup)
        _assistant_line("msg_1", "req_1"),
        _assistant_line("msg_1", "req_1"),  # full key: deduped
    ]
    metrics = _parse(lines)
    assert metrics.assistant_messages == 3, metrics.assistant_messages
    assert metrics.input_tokens == 3 * USAGE["input_tokens"]


# --- AC1-UI: output shape stability -------------------------------------------


def test_json_surfaces_unpriced_model():
    metrics = _parse([_assistant_line("msg_0", "req_0", model="claude-opus-next")])
    metrics.unpriced = True
    out = io.StringIO()
    with contextlib.redirect_stdout(out):
        session_cost.print_metrics(metrics, as_json=True)
    payload = json.loads(out.getvalue())
    assert payload["cost_usd"] is None
    assert payload["unpriced_model"] == "claude-opus-next"


# --- AC1-EDGE: multi-transcript dedup ------------------------------------------


def test_resumed_session_history_not_recounted():
    # Resumed sessions copy prior history lines (with usage) into the new
    # transcript file; a shared seen set across files must count each
    # unique message once.
    first = [_assistant_line("msg_0", "req_0"), _assistant_line("msg_1", "req_1")]
    second = [
        _assistant_line("msg_0", "req_0"),  # copied history
        _assistant_line("msg_1", "req_1"),  # copied history
        _assistant_line("msg_2", "req_2"),  # new work
    ]
    with tempfile.TemporaryDirectory() as td:
        p1, p2 = Path(td) / "s1.jsonl", Path(td) / "s2.jsonl"
        _write_transcript(p1, first)
        _write_transcript(p2, second)
        shared: set = set()
        m1 = session_cost.parse_transcript(str(p1), "s1", seen=shared)
        m2 = session_cost.parse_transcript(str(p2), "s2", seen=shared)
    combined = session_cost.merge_metrics([m1, m2])
    assert combined.input_tokens == 3 * USAGE["input_tokens"], combined.input_tokens
    assert combined.assistant_messages == 3


def _main() -> int:
    failures = 0
    for name, fn in sorted(globals().items()):
        if name.startswith("test_") and callable(fn):
            try:
                fn()
                print(f"  PASS {name}")
            except AssertionError as exc:
                failures += 1
                print(f"  FAIL {name}: {exc}")
    print(f"{'OK' if failures == 0 else 'FAILED'} ({failures} failures)")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(_main())
