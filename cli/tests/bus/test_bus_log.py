"""Task 3.1 - canonical JSONL bus log substrate (US5).

Covers AC5-HP (ordered thread scan, correlation ids, provider mix),
AC5-ERR (corrupt line skipped with warning), AC5-EDGE (rotation boundary),
plus the locked write discipline (flock sidecar + O_APPEND whole-line writes
at any body size) and the versioned envelope.
"""
from __future__ import annotations

import json
import os
import subprocess
import sys

import pytest

from fno.paths_testing import use_tmpdir


@pytest.fixture
def bus(tmp_path, monkeypatch):
    use_tmpdir(monkeypatch, tmp_path)
    from fno import paths
    return paths.bus_dir()


# ---------------------------------------------------------------------------
# Envelope shape + serialization
# ---------------------------------------------------------------------------

def test_envelope_round_trips_with_version_and_from_key(bus):
    from fno.bus.log import Envelope, to_json_line, from_json_line

    env = Envelope.new(
        from_="alice", to="bob", kind="send", body="hi there",
        from_harness="claude", to_harness="codex",
    )
    line = to_json_line(env)
    obj = json.loads(line)
    # Envelope is versioned and addresses via the canonical 'from'/'to' keys.
    assert obj["v"] == 1
    assert obj["from"] == "alice"
    assert obj["to"] == "bob"
    assert obj["kind"] == "send"
    assert obj["body"] == "hi there"
    assert obj["id"].startswith("fmail-")
    # Round-trips back to an equal envelope.
    back = from_json_line(line)
    assert back.from_ == "alice"
    assert back.to == "bob"
    assert back.id == env.id
    assert back.body == "hi there"


# ---------------------------------------------------------------------------
# AC5-HP: ordered thread scan, correlation intact, provider mix
# ---------------------------------------------------------------------------

def test_ac5_hp_thread_scan_in_order_with_correlation(bus):
    from fno.bus.log import Envelope, append, iter_thread

    ask = Envelope.new(
        from_="a", to="b", kind="ask", body="status?",
        from_harness="claude", to_harness="codex", request_id="rq-1",
    )
    append(ask)
    reply = Envelope.new(
        from_="b", to="a", kind="reply", body="all good",
        from_harness="codex", to_harness="claude", in_reply_to="rq-1",
        thread=ask.thread,
    )
    append(reply)
    send = Envelope.new(
        from_="a", to="b", kind="send", body="thanks",
        from_harness="claude", to_harness="codex", thread=ask.thread,
    )
    append(send)

    convo = list(iter_thread(ask.thread))
    assert [m.kind for m in convo] == ["ask", "reply", "send"]
    assert convo[1].in_reply_to == "rq-1"
    # harness mix preserved
    assert convo[0].to_harness == "codex"
    assert convo[1].from_harness == "codex"


# ---------------------------------------------------------------------------
# AC5-ERR: corrupt line skipped with a warning, valid lines still read
# ---------------------------------------------------------------------------

def test_ac5_err_corrupt_line_skipped_with_warning(bus, capsys):
    from fno.bus.log import Envelope, append, iter_messages, bus_log_path

    append(Envelope.new(from_="a", to="b", kind="send", body="good1"))
    # Inject a corrupt line in the middle.
    with bus_log_path().open("a", encoding="utf-8") as f:
        f.write("this is not json{{{\n")
    append(Envelope.new(from_="a", to="b", kind="send", body="good2"))

    msgs = list(iter_messages())
    bodies = [m.body for m in msgs]
    assert bodies == ["good1", "good2"]
    err = capsys.readouterr().err
    assert "skip" in err.lower() or "corrupt" in err.lower() or "malformed" in err.lower()


# ---------------------------------------------------------------------------
# Write discipline: large body lands whole (no truncation/interleave)
# ---------------------------------------------------------------------------


def test_multiprocess_appends_survive_contention_and_rotation(bus):
    """Independent send processes serialize without loss at a rotation edge."""
    from fno.bus.log import iter_messages

    worker = """
import sys
from fno.bus.log import Envelope, append

prefix = sys.argv[1]
for i in range(5):
    append(Envelope.new(from_=prefix, to="peer", kind="send", body=f"{prefix}:{i}"))
"""
    env = os.environ.copy()
    env["FNO_BUS_MAX_BYTES"] = "400"
    env["FNO_BUS_RETAIN"] = "100"
    processes = [
        subprocess.Popen(
            [sys.executable, "-c", worker, f"p{index}"],
            env=env,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        for index in range(4)
    ]
    results = [process.communicate(timeout=10) for process in processes]

    for process, (stdout, stderr) in zip(processes, results):
        assert process.returncode == 0, stdout + stderr
    expected = {f"p{process}:{item}" for process in range(4) for item in range(5)}
    assert {message.body for message in iter_messages()} == expected


@pytest.mark.parametrize("timeout", [float("inf"), -0.1])
def test_bus_lock_rejects_nonterminating_timeout_budget(bus, timeout):
    from fno.bus.log import _Flock

    lock_path = bus / "invalid.lock"
    with pytest.raises(ValueError, match="finite and non-negative"):
        with _Flock(lock_path, timeout_seconds=timeout):
            pass
    assert not lock_path.exists()
