"""Move A (G1, ab-cee91152) - flip the inbox store canon to jsonl-first.

This suite pins the contract:

  - AC1-ERR: a bus-log append failure FAILS the send (the log is the durable
    write the caller depends on), surfacing the error - never a silent loss.
"""
from __future__ import annotations

import pytest

from fno.paths_testing import use_tmpdir


@pytest.fixture
def inbox_and_bus(tmp_path, monkeypatch):
    """Co-isolate the bus log (FNO_INBOX_ROOT) under tmp."""
    monkeypatch.setenv("FNO_INBOX_ROOT", str(tmp_path))
    use_tmpdir(monkeypatch, tmp_path)
    return tmp_path


# ---------------------------------------------------------------------------
# AC1-ERR: a bus-log append failure fails the send (durable-first)
# ---------------------------------------------------------------------------

def test_send_fails_when_bus_append_fails(inbox_and_bus, monkeypatch):
    from fno.inbox import store
    from fno.bus.log import iter_messages

    def boom(_env):
        raise OSError("disk full")

    # The store imports append lazily inside the helper; patch the source.
    monkeypatch.setattr("fno.bus.log.append", boom)

    with pytest.raises(OSError):
        store.write_new_thread("alice", "bob", store.Kind.HEADS_UP.value, "region work")

    assert list(iter_messages()) == []
