"""Resolution semantics over classified lineage.

One row's ids serve different jobs. Delivery (mail, inject, pane send) always
consumes the row's CURRENT address, even when the caller named a predecessor.
Exact-id resume keeps the historical id a caller actually spelled, except a
claude session under the Python runtime, which refuses (exit 13) and names
the spawn door instead of reopening it unpinned. A token that could name
either of two live workers refuses and names both full ids, never picking a
winner.
"""
from __future__ import annotations

from dataclasses import dataclass
from typing import Optional


A = "e6f78b98-e594-47ed-ad81-84f8a78b8bb7"
B = "08054b1d-a907-47ab-a3d2-4a1e7a87eb4e"
C = "cafef00d-a907-47ab-a3d2-4a1e7a87eb4e"


@dataclass
class _Row:
    """Duck-typed AgentEntry with just what the resolver reads."""

    name: str
    harness: str = "claude"
    cwd: str = "/proj"
    log_path: str = ""
    short_id: str = ""
    provider: Optional[str] = None
    effort: Optional[str] = None
    session_id: Optional[str] = None
    harness_session_id: Optional[str] = None
    related_session_id: Optional[str] = None
    aliases: list = None  # type: ignore[assignment]
    fno_id: Optional[str] = None
    predecessor_session_ids: list = None  # type: ignore[assignment]
    forked_from_session_id: Optional[str] = None

    def __post_init__(self):
        if self.aliases is None:
            self.aliases = []
        if self.predecessor_session_ids is None:
            self.predecessor_session_ids = []

    def __getattr__(self, item):
        # Duck-typed fake: any registry field this file does not care about
        # reads as None, exactly like the getattr-based consumers see.
        if item.startswith("__"):
            raise AttributeError(item)
        return None


def _successor_row() -> _Row:
    """A row after succession: B current, A retained in the chain."""
    return _Row(
        name="worker",
        harness_session_id=B,
        short_id=B.split("-", 1)[0],
        fno_id="thread-a",
        predecessor_session_ids=[A],
    )


# ---------------------------------------------------------------------------
# AC6-HP: addressing follows B; the match records which id was named
# ---------------------------------------------------------------------------


# ---------------------------------------------------------------------------
# AC6-ERR: a live branch never lets a token pick one worker
# ---------------------------------------------------------------------------


# ---------------------------------------------------------------------------
# AC6-HP resume half: an exact predecessor id is the resume target
# ---------------------------------------------------------------------------


class _CodexSuccessorRow(_Row):
    """A codex-shaped succession row (its id reads through the
    harness_session_id fallback beside claude's short_id exception)."""


# ---------------------------------------------------------------------------
# AC7: the list row states both identity axes
# ---------------------------------------------------------------------------


def test_serialize_entry_states_thread_and_current_beside_each_other() -> None:
    """AC7-HP/AC7-ERR: after succession the row reports current_session_id B
    POSITIVELY, with the stable thread id and the predecessor chain beside it.
    A renderer sourcing current identity from fno_id, pane metadata, or the
    first predecessor fails this assertion instead of passing silently."""
    from fno.agents.format import serialize_entry

    entry = _Row(
        name="worker",
        harness_session_id=B,
        short_id=B.split("-", 1)[0],
        fno_id="thread-a",
        predecessor_session_ids=[A],
    )
    row = serialize_entry(entry, live_status=None)  # type: ignore[arg-type]

    assert row["current_session_id"] == B, "current identity is B, never A"
    assert row["harness_session_id"] == B
    assert row["thread_id"] == "thread-a", "the thread key stays visible"
    assert row["predecessor_session_ids"] == [A]
    assert row["forked_from_session_id"] is None, "succession is not a fork edge"
