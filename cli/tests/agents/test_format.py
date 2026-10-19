"""Tests for fno.agents.format — registry-row serialization.

Covers cross-provider shape stability and the fields consumed by non-list
registry readers.
"""
from __future__ import annotations

from fno.agents.format import (
    serialize_entry,
)
from fno.agents.registry import AgentEntry


def _claude_entry(**overrides) -> AgentEntry:
    base = dict(
        name="worker-frontend",
        harness="claude",
        cwd="/Users/foo/code/proj",
        log_path="/Users/foo/.fno/agents/worker-frontend/output.jsonl",
        short_id="abc12345",
        created_at="2026-05-20T17:00:00Z",
        status="live",
        last_message_at="2026-05-20T17:30:12Z",
    )
    base.update(overrides)
    return AgentEntry(**base)


def _codex_entry(**overrides) -> AgentEntry:
    base = dict(
        name="worker-migration",
        harness="codex",
        cwd="/Users/foo/code/proj",
        log_path="/Users/foo/.fno/agents/worker-migration/output.jsonl",
        harness_session_id="codex-sess-xyz",
        created_at="2026-05-20T17:15:00Z",
        status="live",
        last_message_at="2026-05-20T17:15:43Z",
    )
    base.update(overrides)
    return AgentEntry(**base)


def test_serialize_entry_reaches_a_real_liveness_origin() -> None:
    """The shared rule gates on `pid`, and this serializer must supply it.

    The shared corpus cannot catch this. Every case there hands `pid` in by
    hand, so it proves the RULE and says nothing about whether the caller
    feeds it. This row is the one `fno agents list` serves, and without the
    pid it read `null` / `pid-absent` forever while the corpus stayed green.
    """
    entry = _claude_entry(pid=4321, pid_start_time="2026-05-20T17:05:00Z")

    row = serialize_entry(entry, live_status="Working")

    assert row["liveness_origin"] == "survivor"
    assert row["liveness_origin_basis"] is None
    # pid is rust-only on the wire (schemas/agents-list-row.json), so the
    # internal input must not leak into the serialized row.
    assert "pid" not in row
    assert "pid_start_time" not in row


def test_serialize_entry_surfaces_codex_session_id_as_session_id() -> None:
    """Codex agents expose their resume target via the unified session_id key.

    Previously codex_session_id was stored but never surfaced in
    `fno agents list`, so the resume UUID (the argument `codex resume`
    needs) was invisible to JSON consumers. session_id resolves to the
    provider-specific id.
    """
    row = serialize_entry(_codex_entry(), live_status=None)

    assert row["session_id"] == "codex-sess-xyz"
    # short_id stays claude-only for back-compat.
    assert row["short_id"] is None


def test_serialize_entry_shape_is_stable_across_providers() -> None:
    """AC3-HP — JSON shape stable across providers (same key set)."""
    claude_row = serialize_entry(_claude_entry(), live_status="Working")
    codex_row = serialize_entry(_codex_entry(), live_status=None)

    assert set(claude_row.keys()) == set(codex_row.keys())
    assert {
        "name",
        "harness",
        "observed_model",
        "short_id",
        "session_id",
        "cwd",
        "created_at",
        "last_message_at",
        "status",
        "live_status",
        "log_path",
    }.issubset(claude_row.keys())


def test_serialize_entry_provider_names_vendor_not_harness(_unused=None) -> None:
    """AC8: `provider` carries the stored vendor, never a harness.

    `provider: "claude"` on a zai-routed worker is what produced the wrong
    diagnosis this row shape was fixed for; the key was removed wholesale and
    the removal then hid the real v15+ vendor axis. The guard that survives is
    the one the original AC8 meant: the value under a vendor-named key must
    never be the harness.
    """
    row = serialize_entry(
        _claude_entry(provider="zai"), live_status=None
    )

    assert row["provider"] == "zai"
    assert row["provider"] != row["harness"]
    assert row["harness"] == "claude"
    assert "model" not in row
    assert "model_basis" not in row


# ---------------------------------------------------------------------------
# `address`: the form drain-self actually reads
#
# Before this key existed, every registry row on a live host carried
# `handle: null` and `short_id: null` and the table had no address column, so
# the only copyable identifier in a row was `name` -- and a name-lane durable
# write is the largest still-growing category of stranded mail on the bus.
# ---------------------------------------------------------------------------


def test_address_falls_back_to_the_transport_key_only_for_claude() -> None:
    """A claude row's ``short_id`` IS its first-eight, so it is an honest
    fallback when no full session id was recorded. A codex/opencode
    ``short_id`` is a daemon worker key, not an address, so guessing one there
    would advertise a key nothing drains."""
    claude = _claude_entry(harness_session_id=None, short_id="abc12345")
    assert serialize_entry(claude, live_status=None)["address"] == "abc12345"

    codex = _codex_entry(harness_session_id=None, short_id="abc12345")
    assert serialize_entry(codex, live_status=None)["address"] is None


# ---------------------------------------------------------------------------
# v23: the requested axis and the substitution marker on the row
# ---------------------------------------------------------------------------


def test_serialize_entry_carries_the_request_and_names_a_substitution() -> None:
    """A substituted row shows the verbatim request AND the marker naming both."""
    observed = {"kind": "observed", "model": "glm-5.3-flash", "samples": 31}

    row = serialize_entry(
        _claude_entry(requested_model="glm-5.3[1m]"),
        live_status=None,
        observed_model=observed,
    )
    assert row["requested_model"] == "glm-5.3[1m]"
    assert row["model_substituted"] == {
        "requested": "glm-5.3[1m]",
        "observed": "glm-5.3-flash",
    }
