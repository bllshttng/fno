"""Converge the inbox store onto the canonical bus log.

The bus log is the system of record: every store write appends one envelope to
the global log. The mail read and ack commands are native; their goldens
live under crates/fno-agents/tests/golden/mail_unread and mail_ack.
"""
from __future__ import annotations

import fcntl
import os
import subprocess
import sys

import pytest


@pytest.fixture
def inbox_and_bus(tmp_path, monkeypatch):
    """Co-isolate the bus log (FNO_INBOX_ROOT) under tmp."""
    monkeypatch.setenv("FNO_INBOX_ROOT", str(tmp_path))
    return tmp_path


# ---------------------------------------------------------------------------
# Every store write lands a canonical envelope in the bus log
# ---------------------------------------------------------------------------

def test_write_new_thread_appends_to_bus_log(inbox_and_bus):
    from fno.inbox.store import write_new_thread, Kind
    from fno.bus.log import iter_messages

    msg_id = write_new_thread("alice", "bob", Kind.HEADS_UP.value, "region column work")

    msgs = list(iter_messages())
    assert len(msgs) == 1
    env = msgs[0]
    assert env.id == msg_id
    assert env.from_ == "bob"
    assert env.to == "alice"
    assert env.kind == "heads-up"
    assert env.body == "region column work"


def test_ac1_err_contended_bus_lock_times_out_without_record(
    inbox_and_bus,
):
    """A stalled canonical writer cannot hold a durable send forever."""
    from fno.bus import log as bus_log

    lock_path = bus_log._lock_path()
    lock_path.parent.mkdir(parents=True, exist_ok=True)
    holder = os.open(lock_path, os.O_CREAT | os.O_RDWR, 0o600)
    fcntl.flock(holder, fcntl.LOCK_EX)
    script = """
import os
from fno.bus import log as bus_log
from fno.inbox.store import Kind, write_new_thread

os.environ["FNO_BUS_LOCK_TIMEOUT_SECS"] = "0.05"
try:
    write_new_thread("alice", "bob", Kind.SEND.value, "must not disappear")
except Exception as exc:
    print(type(exc).__name__)
    print(str(exc))
else:
    raise SystemExit("contended write unexpectedly succeeded")
"""
    try:
        proc = subprocess.run(
            [sys.executable, "-c", script],
            env=os.environ.copy(),
            capture_output=True,
            text=True,
            timeout=3.0,
            check=False,
        )
    except subprocess.TimeoutExpired:
        pytest.fail("durable write remained blocked on messages.jsonl.lock")
    finally:
        fcntl.flock(holder, fcntl.LOCK_UN)
        os.close(holder)

    assert proc.returncode == 0, proc.stderr
    assert "BusLockTimeout" in proc.stdout
    assert "no durable envelope was written" in proc.stdout
    assert not bus_log.bus_log_path().exists()
