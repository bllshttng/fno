"""Converge the inbox store onto the canonical bus log.

The bus log is the system of record; the per-recipient markdown thread file is
its render (written on every mutation). Every store write mirrors an envelope
into the global log. The mail read and ack verbs are native; their goldens
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
    """Co-isolate the md store (FNO_INBOX_ROOT) and the bus log under tmp."""
    monkeypatch.setenv("FNO_INBOX_ROOT", str(tmp_path))
    return tmp_path


# ---------------------------------------------------------------------------
# Dual-write: every store write lands a canonical envelope in the bus log
# ---------------------------------------------------------------------------

def test_write_new_thread_mirrors_to_bus_log(inbox_and_bus):
    from fno.inbox.store import write_new_thread, Kind
    from fno.bus.log import iter_messages

    h = write_new_thread("alice", "bob", Kind.HEADS_UP.value, "region column work")

    msgs = list(iter_messages())
    assert len(msgs) == 1
    env = msgs[0]
    assert env.id == h.thread_id
    assert env.from_ == "bob"
    assert env.to == "alice"
    assert env.kind == "heads-up"
    assert env.body == "region column work"
    # The render path is carried so drain can preserve provenance back to the md.
    assert env.meta.get("render_path", "").endswith(".md")


def test_ac1_err_contended_bus_lock_times_out_without_record_or_render(
    inbox_and_bus,
):
    """A stalled canonical writer cannot hold a durable send forever."""
    from fno.bus import log as bus_log
    from fno.inbox.store import inbox_dir_for

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
    inbox = inbox_dir_for("alice")
    assert not list(inbox.glob("*.md"))


def test_append_to_thread_mirrors_reply_envelope(inbox_and_bus):
    from fno.inbox.store import write_new_thread, append_to_thread, Kind
    from fno.bus.log import iter_messages, iter_thread

    h = write_new_thread("alice", "bob", Kind.HEADS_UP.value, "root message body")
    new_id = append_to_thread(h.path, "alice", "a reply from alice")

    convo = list(iter_thread(h.thread_id))
    assert [m.id for m in convo] == [h.thread_id, new_id]
    # Reply correlates to the thread root and stays addressed to the thread owner.
    reply = convo[1]
    assert reply.in_reply_to == h.thread_id
    assert reply.to == "alice"
    assert reply.from_ == "alice"
    assert reply.kind == "heads-up"  # appended messages inherit the thread kind
    assert len(list(iter_messages())) == 2
