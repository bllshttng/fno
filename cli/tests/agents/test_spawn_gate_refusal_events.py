"""Every spawn-gate refusal leaves a machine-readable trace.

Measured 2026-09-01: `agents.provider_limits.zai.lanes = 7` was binding on the
live fleet (7 of 7 rows, machine load 1.3 per CPU, far under the trigger), and
nothing recorded the refusals. A census of the global journal returned 4815
`claim_acquired` rows - the positive control that the file is read and written -
and zero rows of any kind naming a gate refusal. So an operator could not ask
why a node did not launch; the answer lived only in the stderr of a process that
had already exited.

Since x-6089 the gate's axes are decided in Rust and the refusals arrive as
data; the Python side still emits every one of them through `_refuse` when the
spawn enters Python. The receipts these tests feed `_refuse` are the same
shapes the command answers carry. Every assertion is on a POSITIVE marker: a
parsed JSON line whose `kind` is `spawn_gate_refused`. Never on a line count
alone, and never on the absence of an error - an absence has three
explanations and only one of them is the outcome.
"""
from __future__ import annotations

from pathlib import Path
from typing import Any

import pytest

from fno.agents import events as agent_events
from fno.agents import spawn_gate


@pytest.fixture
def journal(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Path:
    """Redirect the agents events journal into the test's own tmp dir.

    Belt and braces with the conftest per-module pin: the fixture also sets
    FNO_EVENTS_PATH so any non-patched emitter in this module lands here too.
    """
    target = tmp_path / "events.jsonl"
    monkeypatch.setenv("FNO_EVENTS_PATH", str(target))
    real_emit = agent_events.emit
    monkeypatch.setattr(
        agent_events,
        "emit",
        lambda kind, **data: real_emit(kind, path=target, **data),
    )
    return target


def _refusals(journal: Path) -> list[dict[str, Any]]:
    from tests._event_rows import event_rows

    return [r for r in event_rows(journal) if r.get("kind") == "spawn_gate_refused"]


def test_provider_cap_refusal_names_provider_cap_and_count(journal: Path) -> None:
    """AC1-HP. The refusal that was binding on the live fleet, made visible."""
    spawn_gate._CURRENT_SPAWN.set(("t-probe", "thread"))

    with pytest.raises(spawn_gate.GateRefused) as excinfo:
        spawn_gate._refuse(
            spawn_gate.EXIT_PROVIDER_CAP,
            {
                "status": "refused",
                "reason": "provider_cap",
                "provider": "zai",
                "cap": 7,
                "count": 7,
                "current_count": 7,
            },
        )

    assert excinfo.value.code == spawn_gate.EXIT_PROVIDER_CAP

    rows = _refusals(journal)
    assert len(rows) == 1, rows
    row = rows[0]
    assert row["reason"] == "provider_cap"
    assert row["provider"] == "zai"
    assert row["cap"] == 7
    assert row["count"] == 7
    assert row["name"] == "t-probe"
    assert row["substrate"] == "thread"
    assert row["gate"] == "python"
    assert row["exit_code"] == spawn_gate.EXIT_PROVIDER_CAP


def test_lead_share_refusal_emits_though_it_carries_no_receipt(journal: Path) -> None:
    """The lead share refuses with receipt=None, and still has to be answerable.

    This is the branch the seam exists for: four refusals never built a
    receipt, so a design that only forwarded receipts would have left them as
    silent as before. The command's event map carries the same fields today.
    """
    spawn_gate._CURRENT_SPAWN.set(("t-probe", "thread"))

    with pytest.raises(spawn_gate.GateRefused) as excinfo:
        spawn_gate._refuse(
            spawn_gate.EXIT_LEAD_SHARE,
            None,
            reason="lead_share",
            lead="leadA",
            held=4,
            share=4,
            max_live=8,
            leads=2,
        )

    assert excinfo.value.receipt is None, "stdout receipt shape must not change"

    rows = _refusals(journal)
    assert len(rows) == 1, rows
    assert rows[0]["reason"] == "lead_share"
    assert rows[0]["lead"] == "leadA"
    assert rows[0]["held"] == 4
    assert rows[0]["max_live"] == 8


def test_an_unwritable_journal_never_changes_the_refusal(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """AC2-ERR. Telemetry never changes a gate outcome."""

    def _boom(kind: str, **data: Any) -> None:
        raise OSError("read-only file system")

    monkeypatch.setattr(agent_events, "emit", _boom)

    with pytest.raises(spawn_gate.GateRefused) as excinfo:
        spawn_gate._refuse(
            spawn_gate.EXIT_PROVIDER_CAP,
            {
                "status": "refused",
                "reason": "provider_cap",
                "provider": "zai",
                "cap": 7,
                "count": 7,
                "current_count": 7,
            },
        )

    assert excinfo.value.code == spawn_gate.EXIT_PROVIDER_CAP
    assert excinfo.value.receipt is not None
    assert excinfo.value.receipt["reason"] == "provider_cap"


