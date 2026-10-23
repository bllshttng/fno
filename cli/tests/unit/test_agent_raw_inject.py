"""Constructor + schema tests for the agent_raw_inject provenance event.

The audit-floor event for an UNWRAPPED injection (no <fno_mail> marker survives
in the recipient transcript, so x-f26c's greppability property moves to the
ledger). Covers the fno.events schema-system builder; the transport emit sites
(mail-inject binary, mux pane send) are exercised in their own test modules.
"""
from __future__ import annotations

from fno.events import agent_raw_inject, validate


def test_agent_raw_inject_constructor_minimal():
    event = agent_raw_inject(
        target_session="ses-9",
        payload="/code-review <level> --comment --fix",
        harness="claude",
        lane="control.sock",
    )
    validate(event)  # the kind is schema-known and required fields are present
    assert event["type"] == "agent_raw_inject"
    # The schema declares sources: [daemon] and both transports emit as the
    # daemon; a "target" default would have written a source the schema does not
    # list (validate does not enforce per-type sources, so nothing caught it).
    assert event["source"] == "daemon"
    assert event["data"]["target_session"] == "ses-9"
    assert event["data"]["payload"] == "/code-review <level> --comment --fix"
    assert event["data"]["harness"] == "claude"
    assert event["data"]["lane"] == "control.sock"
    assert "sender" not in event["data"], "optional fields omitted when absent"


def test_raw_inject_records_the_dollar_command_token():
    """x-c976: a `$fno:` payload records its command; prose records none."""
    event = agent_raw_inject(
        target_session="ses-9",
        payload="$fno:review medium",
        harness="codex",
        lane="mux-pane",
    )
    assert event["data"]["verb"] == "$fno:review"
    prose = agent_raw_inject(
        target_session="ses-9",
        payload="fix the login bug",
        harness="codex",
        lane="mux-pane",
    )
    assert prose["data"]["verb"] is None
