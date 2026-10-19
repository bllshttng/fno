"""Unit tests for ``fno.mcp.channel`` (envelope build / validate).

Wave 1.1 contract tests. The Wave 1.0 smoke script
(``cli/scripts/smoke/validate-mcp-channel.sh``) covers the structural
pin against the fixture; this file covers the round-trip and
error-path behavior the dispatcher relies on.
"""
from __future__ import annotations

import pytest

from fno.mcp.channel import (
    MCP_CHANNEL_METHOD,
    MCPChannelEnvelopeError,
    build_channel_notification,
    envelope_drift_diff,
    validate_envelope,
)


class TestBuildEnvelope:
    def test_minimal_envelope_has_pinned_shape(self) -> None:
        env = build_channel_notification(content="hello")
        assert env["jsonrpc"] == "2.0"
        assert env["method"] == MCP_CHANNEL_METHOD
        assert env["params"] == {"content": "hello", "meta": {}}

    def test_hyphen_or_non_identifier_meta_keys_are_rejected(self) -> None:
        bad_key = "chat-id"
        with pytest.raises(MCPChannelEnvelopeError) as exc:
            build_channel_notification(content="x", meta={bad_key: "v"})
        assert "meta keys must match" in str(exc.value)
        assert bad_key in str(exc.value)

    def test_non_string_meta_value_rejected(self) -> None:
        with pytest.raises(MCPChannelEnvelopeError):
            build_channel_notification(content="x", meta={"k": 123})  # type: ignore[arg-type]


class TestValidateEnvelope:

    def test_rejects_method_drift(self) -> None:
        env = build_channel_notification(content="hi")
        env["method"] = "notifications/some_other/channel"
        ok, reason = validate_envelope(env)
        assert ok is False
        assert reason is not None and reason.startswith("method_mismatch")

    def test_rejects_missing_content(self) -> None:
        ok, reason = validate_envelope(
            {"jsonrpc": "2.0", "method": MCP_CHANNEL_METHOD, "params": {"meta": {}}}
        )
        assert ok is False
        assert reason == "content_missing"

    def test_rejects_non_identifier_meta_key(self) -> None:
        ok, reason = validate_envelope(
            {
                "jsonrpc": "2.0",
                "method": MCP_CHANNEL_METHOD,
                "params": {"content": "x", "meta": {"chat-id": "abc"}},
            }
        )
        assert ok is False
        assert reason is not None and reason.startswith("meta_key_invalid")


class TestDriftDiff:

    def test_method_rename_surfaces_in_diff(self) -> None:
        a = build_channel_notification(content="x")
        b = dict(a)
        b["method"] = "notifications/new/channel"
        diff = envelope_drift_diff(a, b)
        assert diff["method_expected"] == MCP_CHANNEL_METHOD
        assert diff["method_received"] == "notifications/new/channel"
