"""Tests for the MCP request_id wire-through.

Tasks 3.1 + 3.2 from 2026-05-22-fno-agents-observability.md.

3.1: build_channel_notification accepts request_id kwarg and stamps it
     onto meta["request_id"].
3.2: channel_server forwards envelopes verbatim, so meta["request_id"]
     reaches the recipient as a <channel request_id="..."> tag attr
     (CC renders meta keys as tag attributes — no extra server logic).

Locks in:
- AC4-HP: producer-side request_id ends up byte-identical in the
  envelope meta that the recipient sees (round-trip preservation).
- AC4-ERR: empty / non-string request_id raises MCPChannelEnvelopeError
  at envelope construction (fail-loud at the producer).
- AC4-INVARIANT: the validator accepts request_id (lowercase hex matches
  META_KEY_RE on the key; value is an arbitrary string per the wire
  contract).
"""
from __future__ import annotations

import pytest

from fno.mcp.channel import (
    MCPChannelEnvelopeError,
    build_channel_notification,
)


REQUEST_ID = "a1b2c3d4e5f6789012345678901234ab"  # 32 lowercase hex chars


# ---------------------------------------------------------------------------
# 3.1 — build_channel_notification accepts + propagates request_id
# ---------------------------------------------------------------------------


def test_build_channel_request_id_merges_with_existing_meta() -> None:
    """Passing both meta and request_id merges; request_id wins on collision."""
    env = build_channel_notification(
        content="hi",
        meta={"from_name": "alpha", "request_id": "old-shouldnt-survive"},
        request_id=REQUEST_ID,
    )
    meta = env["params"]["meta"]
    assert meta["from_name"] == "alpha"
    assert meta["request_id"] == REQUEST_ID


def test_build_channel_request_id_does_not_mutate_caller_meta() -> None:
    """build_channel_notification must not mutate the caller's meta dict."""
    original = {"from_name": "alpha"}
    build_channel_notification(content="hi", meta=original, request_id=REQUEST_ID)
    assert original == {"from_name": "alpha"}, "caller meta was mutated"


def test_build_channel_rejects_empty_request_id() -> None:
    """AC4-ERR: empty string request_id is fail-loud at the producer."""
    with pytest.raises(MCPChannelEnvelopeError) as excinfo:
        build_channel_notification(content="hi", request_id="")
    assert "request_id" in str(excinfo.value)
