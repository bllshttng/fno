"""US6: terminal ownership classification rides the bus envelope meta.

The bus JSONL is the system of record the dead-letter sweep reads (LD11), so a
durable write's ``owner``/``ttl_at`` must land in the envelope ``meta``, and a
caller that omits them must write a thread byte-identical to before.
"""
from __future__ import annotations


from fno.inbox.store import (
    DurableOwner,
    classify_durable_owner,
    write_new_thread,
)
from fno.paths_testing import use_tmpdir


def _bus():
    from fno.bus.log import iter_messages

    return list(iter_messages())


def test_classify_matrix():
    c = classify_durable_owner
    assert c(param_forced=True, recipient_live=True, recipient_resumable=True) is DurableOwner.INBOX_DRAIN
    assert c(param_forced=False, recipient_live=True, recipient_resumable=False) is DurableOwner.LIVE_DRAIN
    assert c(param_forced=False, recipient_live=False, recipient_resumable=True) is DurableOwner.WAKE_DAEMON
    assert c(param_forced=False, recipient_live=False, recipient_resumable=False) is DurableOwner.DEAD_LETTER


def test_write_new_thread_stamps_owner_and_ttl_on_bus(tmp_path, monkeypatch):
    use_tmpdir(monkeypatch, tmp_path)
    write_new_thread("claude-cafe0001", "web", "send", "hello", owner=DurableOwner.WAKE_DAEMON.value)
    msgs = _bus()
    assert len(msgs) == 1
    assert msgs[0].meta.get("owner") == "wake-daemon"
    assert msgs[0].meta.get("ttl_at")  # derived from owner class + created ts
