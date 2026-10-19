"""The pi rpc lane: strict JSONL framing, held-open stdin, and honest receipts.

Three traps are pinned here, and each has already cost somebody a wrong answer
on some harness:

1. **A generic line reader is not protocol-compliant.** pi's rpc framing makes
   LF the only record delimiter. Node's ``readline`` also splits on U+2028 and
   U+2029, which are legal inside a JSON string, and Python's text-mode line
   iteration has the same defect. A splitter that breaks there corrupts a
   record whose CONTENT happens to carry one.
2. **rpc mode exits on stdin EOF, mid-turn, with status 0.** A prompt fed from
   a file yielded five events and stopped at the user's own ``message_end``;
   the assistant never spoke and the exit code still read success. So the
   driver settles on the POSITIVE ``agent_settled`` event and refuses to treat
   a stream that merely ended as a turn that completed.
3. **``success: true`` is a receipt about ACCEPTANCE.** Failures after
   acceptance arrive through the event stream, never as a second response for
   the same request id, so a receipt claiming the agent acted is a receipt that
   lies.
"""
from __future__ import annotations

import json
import sys
import threading
from pathlib import Path

import pytest

_TEST_DIR = Path(__file__).resolve().parent
_SRC = _TEST_DIR.parents[1] / "src"
if str(_SRC) not in sys.path:
    sys.path.insert(0, str(_SRC))

from fno.agents.harnesses.pi import (  # noqa: E402
    SETTLED_EVENT,
    PiRpcSession,
    iter_jsonl,
    receipt_for_response,
)


def test_AC4_EDGE_the_receipt_says_accepted_and_never_acted_on():
    accepted = receipt_for_response(
        {"type": "response", "command": "steer", "success": True}
    )
    assert "accepted" in accepted
    assert "not yet acted on" in accepted
    refused = receipt_for_response(
        {"type": "response", "command": "steer", "success": False, "error": "no run"}
    )
    assert "REFUSED" in refused and "no run" in refused


def test_AC3_HP_framing_splits_on_LF_only():
    """A U+2028 inside a JSON string is CONTENT, not a record boundary."""
    payload = {"type": "message_end", "message": "line one line two"}
    stream = iter([json.dumps(payload).encode("utf-8") + b"\n"])
    events = list(iter_jsonl(stream))
    assert events == [payload], events


class _FakeStdin:
    def __init__(self) -> None:
        self.written: list[bytes] = []
        self.closed = False

    def write(self, data: bytes) -> None:
        assert not self.closed, "stdin was closed mid-turn; rpc mode exits on EOF"
        self.written.append(data)

    def flush(self) -> None:
        pass

    def close(self) -> None:
        self.closed = True


class _FakeStdout:
    def __init__(self, chunks: list[bytes]) -> None:
        self._chunks = list(chunks)

    def read1(self, _size: int) -> bytes:
        return self._chunks.pop(0) if self._chunks else b""

    def close(self) -> None:
        # A real pipe has one, and `close()` closes the read ends to stop
        # leaking two descriptors per session.
        pass


class _FakeProc:
    def __init__(self, chunks: list[bytes]) -> None:
        self.stdin = _FakeStdin()
        self.stdout = _FakeStdout(chunks)
        # None on purpose: these tests inject the proc after start(), so no
        # drain thread runs and there is no pipe to fill.
        self.stderr = None

    def wait(self, timeout: float | None = None) -> int:
        return 0

    def kill(self) -> None:
        pass


def _session(chunks: list[bytes]) -> PiRpcSession:
    session = PiRpcSession("s-1", "/repo")
    session.proc = _FakeProc(chunks)  # type: ignore[assignment]
    return session


def test_AC3_EDGE_a_stream_that_merely_ends_is_not_a_completed_turn():
    """The EOF trap: five events, no assistant reply, exit 0, and it is NOT done.

    Asserting the absence of an error here would report the exact failure this
    harness produces silently.
    """
    session = _session(
        [
            b'{"type": "agent_start"}\n',
            b'{"type": "message_start", "message": {"role": "user"}}\n',
            b'{"type": "message_end", "message": {"role": "user"}}\n',
        ]
    )
    with pytest.raises(RuntimeError, match="without 'agent_settled'"):
        session.run_turn("hello")


def test_the_framing_buffer_survives_a_turn_boundary():
    """Turn two must not resume mid-record.

    A 64KB read routinely returns ``agent_settled`` plus the first bytes of the
    next record. A per-turn generator drops that tail, so turn two starts on a
    fragment, ``json.loads`` rejects it, and the skip arm swallows it silently -
    losing events, and hanging the turn outright when the discarded tail held
    its own settle marker. This feeds exactly that shape: one chunk carrying
    turn one's settle AND the start of turn two.
    """
    session = _session(
        [
            b'{"type": "agent_settled"}\n{"type": "agent_start"}\n',
            b'{"type": "agent_settled"}\n',
        ]
    )
    first = session.run_turn("one")
    assert [e["type"] for e in first] == [SETTLED_EVENT]
    second = session.run_turn("two")
    assert [e["type"] for e in second] == ["agent_start", SETTLED_EVENT], second


class _BlockingStdout:
    """A pi that has stopped talking and has NOT exited.

    ``read1`` blocks until the watchdog kills the child, exactly as a real
    pipe does: no bytes, and no EOF either. This is the absence a stream end
    cannot express, so a reader with no bound never leaves this call.
    """

    def __init__(self, released: threading.Event) -> None:
        self._released = released

    def read1(self, _size: int) -> bytes:
        self._released.wait(timeout=10)
        return b""


class _HangingProc:
    def __init__(self) -> None:
        self.stdin = _FakeStdin()
        self.released = threading.Event()
        self.stdout = _BlockingStdout(self.released)
        self.stderr = None
        self.killed = False

    def poll(self) -> int | None:
        return None if not self.killed else -9

    def kill(self) -> None:
        self.killed = True
        self.released.set()

    def wait(self, timeout: float | None = None) -> int:
        return -9


def test_a_turn_that_hangs_without_exiting_is_bounded_and_says_so():
    """The other absence: a pi that stops emitting and does not exit ends no
    stream, so the EOF raise can never fire and the reader blocks forever.

    Asserts the POSITIVE marker the timeout produces - the child was killed and
    the message names the bound - rather than merely that the call returned.
    """
    session = PiRpcSession("s-1", "/repo")
    proc = _HangingProc()
    session.proc = proc  # type: ignore[assignment]
    with pytest.raises(RuntimeError) as excinfo:
        session.run_turn("hello", timeout_s=0.2)
    assert proc.killed, "the watchdog must kill the child to break the blocked read"
    assert "0.2s" in str(excinfo.value)
    assert SETTLED_EVENT in str(excinfo.value)

