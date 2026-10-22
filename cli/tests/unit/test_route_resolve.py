"""The route_resolve transport: payload shape, verbatim passthrough, and the
degrade-open stance. The band math, the grid fold, and the precedence chain
live in Rust (crates/fno-agents/src/route_slot.rs, route_gather.rs); the
characterization goldens in crates/fno-agents/tests/fixtures/route_gather/ pin
what the command gathers. What stays here is the contract no Rust test can pin:
the one payload the transport sends and the answers it hands back verbatim.
"""
from __future__ import annotations

import pytest

from fno import route_resolve as rr


@pytest.fixture
def slot_answer(monkeypatch):
    """Stub fno.route_slot_client.route_slot_call; returns the calls list."""

    calls: list = []

    def install(answer=None, *, unavailable=None):
        def fake(payload, *a, **kw):
            calls.append(payload)
            if unavailable is not None:
                from fno.route_slot_client import RouteSlotUnavailable

                raise RouteSlotUnavailable(str(unavailable))
            return dict(answer or {})

        monkeypatch.setattr("fno.route_slot_client.route_slot_call", fake)
        return calls

    return install


def test_resolve_slot_sends_explicit_inputs_only(slot_answer):
    """The config-derived payload keys (declared rows, slot table, policy)
    are the command's to gather: the transport sends the explicit inputs and
    nothing else."""
    calls = slot_answer({"candidate": {"name": "flash-x"}, "chain": [], "verdict": "armed"})
    node = {"difficulty": "high", "priority": "p1", "plan_path": "/p.md", "extra": "dropped"}
    meta: dict = {}
    rr.resolve_slot(
        "target", node, {"claude": "ok"},
        substrate="thread", permission_mode="yolo", constrain_harness="codex",
        role="review", model_occupied=True, explicit_lane=True,
        work_command="pr", explicit_model_value="m1", explicit_route_value="r",
        explicit_vendor_value="v", meta=meta,
    )
    payload = calls[0]
    assert "meta" not in payload
    assert payload["rung_base"] == "agents.profiles.target"
    assert payload["node"] == {
        "difficulty": "high", "priority": "p1", "plan_path": "/p.md",
        "model": "", "provider": "", "effort": "",
    }
    assert payload["capacity"] == {"claude": "ok"}
    assert payload["substrate"] == "thread"
    assert payload["constrain_harness"] == "codex"
    assert payload["model_occupied"] is True
    assert payload["work_command"] == "pr"
    assert "declared_rows" not in payload and "lanes" not in payload
    assert "settings" not in payload and "inventory" not in payload


def test_resolve_slot_returns_the_answer_verbatim(slot_answer):
    candidate = {"name": "flash-x", "harness": "claude", "model": "glm"}
    chain = ["slot=agents.profiles.target lanes[0] flash-x", "slot note kept"]
    meta: dict = {}
    slot_answer({
        "candidate": candidate, "chain": chain, "verdict": "armed",
        "refusal_terminal": {"class": "strict", "text": "no"},
        "exhausted_payload": {"reason": "slot_exhausted"},
        "fingerprint": "abc123",
    })
    out_candidate, out_chain, verdict = rr.resolve_slot("target", None, None, meta=meta)
    assert out_candidate == candidate
    assert out_chain == chain, "chain lines ride verbatim"
    assert verdict == "armed"
    assert meta["refusal"] == {"class": "strict", "text": "no"}
    assert meta["exhausted"] == {"reason": "slot_exhausted"}
    assert meta["fingerprint"] == "abc123"


def test_resolve_slot_unavailable_names_strict_routing(slot_answer, monkeypatch):
    """Under enforce_inventory the fault is a named strict-routing refusal,
    not a silent open spawn."""
    monkeypatch.setattr(
        rr, "_routing_enforced", lambda: True
    )
    meta: dict = {}
    slot_answer(unavailable="no binary")
    rr.resolve_slot("target", None, None, meta=meta)
    assert meta["refusal"]["text"].endswith("(strict routing: config routing.enforce_inventory)")


def test_dispatch_model_pins_answer_without_the_command(slot_answer):
    """A pin is superuser authority: it answers in-process, no command round
    trip - the old seam's contract, kept on the transport."""
    calls = slot_answer({"model": "grid-x", "source": "x", "chain": []})
    assert rr.resolve_dispatch_model(explicit="pin-e") == ("pin-e", "explicit", ["explicit"])
    assert rr.resolve_dispatch_model(task_model="pin-t") == ("pin-t", "task-pin", ["task-pin"])
    assert calls == [], "a pin never consults the command"
