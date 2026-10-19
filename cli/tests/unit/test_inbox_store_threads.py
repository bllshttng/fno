"""Unit tests for the thread-per-file inbox store (post-2026-05 layout)."""
from __future__ import annotations


import pytest


@pytest.fixture
def inbox_root(tmp_path, monkeypatch):
    monkeypatch.setenv("FNO_INBOX_ROOT", str(tmp_path))
    return tmp_path


def test_find_thread_by_msg_id_appended_reply(inbox_root):
    from fno.inbox.store import (
        write_new_thread, append_to_thread, find_thread_by_msg_id, Kind,
    )

    h = write_new_thread("alice", "bob", Kind.HEADS_UP.value, "root msg body")
    reply_id = append_to_thread(h.path, "alice", "reply text")
    found = find_thread_by_msg_id("alice", reply_id)
    assert found is not None and found.path == h.path


def test_inbox_dir_rejects_path_separator(inbox_root):
    from fno.inbox.store import inbox_dir_for

    with pytest.raises(ValueError):
        inbox_dir_for("../../etc/passwd")


def test_append_to_thread_resets_read_at(inbox_root):
    """A reply to a drained thread must resurface it for the next drain."""
    from fno.inbox.store import (
        write_new_thread, append_to_thread, mark_thread_read,
        read_thread, read_unread_threads, Kind,
    )

    h = write_new_thread("alice", "bob", Kind.HEADS_UP.value, "first message body")
    mark_thread_read(h.path)
    assert read_unread_threads("alice") == []

    append_to_thread(h.path, "carol", "follow-up reply body")

    re_read = read_thread(h.path)
    assert re_read is not None and re_read.is_unread
    unread = read_unread_threads("alice")
    assert len(unread) == 1 and unread[0].path == h.path


def test_concurrent_writers_do_not_clobber_filenames(inbox_root):
    """Two senders with the same body/date must NOT overwrite each other.

    The atomic O_CREAT|O_EXCL claim plus suffix bump guarantees both
    files exist on disk after a tight loop.
    """
    from fno.inbox.store import write_new_thread, Kind, inbox_dir_for

    body = "race to the same slug today"
    handles = [
        write_new_thread("alice", "bob", Kind.FYI.value, body)
        for _ in range(10)
    ]
    paths = {h.path for h in handles}
    assert len(paths) == 10
    files = sorted(inbox_dir_for("alice").glob("*.md"))
    assert len(files) == 10


def test_atomic_write_does_not_truncate_on_failure(inbox_root, monkeypatch):
    """A render-write failure mid-append leaves the prior render intact AND does
    not fail the send - the reply is durable on the bus (ab-cee91152 Move A).

    Post-flip contract: the markdown render is derived/best-effort. A render
    write failure is logged, never raised; the durable bus append already landed,
    so the reply survives and ``rebuild_render`` can regenerate the render.
    """
    from fno.inbox import store
    from fno.inbox.store import (
        write_new_thread, append_to_thread, read_thread, Kind,
    )
    from fno.bus.log import iter_thread

    h = write_new_thread("alice", "bob", Kind.FYI.value, "original body content")
    original_text = h.path.read_text()

    real_replace = store.os.replace

    def boom(*_args, **_kwargs):
        raise OSError("disk full")

    monkeypatch.setattr(store.os, "replace", boom)
    # The render update fails, but the append SUCCEEDS (best-effort render).
    new_id = append_to_thread(h.path, "carol", "doomed reply")
    assert new_id

    monkeypatch.setattr(store.os, "replace", real_replace)
    # The prior render file is byte-for-byte intact (atomic write never truncates).
    assert h.path.read_text() == original_text
    re_read = read_thread(h.path)
    assert re_read is not None and len(re_read.messages) == 1
    # The reply is durable on the canonical bus log despite the render failure.
    convo = list(iter_thread(h.thread_id))
    assert [m.id for m in convo] == [h.thread_id, new_id]
