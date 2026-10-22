"""Tests for the live_status shellout in harnesses/claude.py.

Covers:
- ``claude_agents_json()`` — best-effort shellout, all-failure-modes return ({}, warnings).

The logs shellout this file once covered was ported to the Rust client
(run_logs in crates/fno-agents/src/client_commands.rs); its contracts are
frozen as goldens in crates/fno-agents/tests/golden/logs.

The actual ``claude`` binary is not invoked; ``subprocess.run`` is
monkeypatched via the module-level ``_subprocess_run`` indirection that
the existing surface already exposes (Locked Decision 6 from US1).
"""
from __future__ import annotations

import json
import subprocess
from types import SimpleNamespace

from fno.agents.harnesses import claude as claude_mod


def _fake_completed(stdout: str = "", stderr: str = "", returncode: int = 0):
    return SimpleNamespace(stdout=stdout, stderr=stderr, returncode=returncode)


# --- claude_agents_json() ---------------------------------------------------


def test_claude_agents_json_accepts_the_current_claude_row_shape(monkeypatch):
    """Regression: the parser read ``short_id``/``status``, but claude emits
    ``id``/``state``. Every row was dropped, so the live_status enrichment
    returned {} on EVERY call while this file's other tests stayed green on
    fabricated old-shape fixtures - a test suite pinning a shape production
    never produces.

    This payload is copied verbatim from real ``claude agents --json`` output
    (keys and lowercase state values as observed), so it fails if the parser
    stops handling what the binary actually emits."""
    payload = {
        "agents": [
            {
                "id": "907fc8c5",
                "sessionId": "907fc8c5-137e-44ff-95a0-f02b038ffefe",
                "name": "lead-role-agents",
                "state": "working",
                "kind": "background",
                "cwd": "/repo",
                "startedAt": 1786023667738,
            },
            {
                "id": "baf9409a",
                "sessionId": "baf9409a-2af5-444a-846c-059e8fa2f758",
                "name": "tgt-let-s-think-about-wh",
                "state": "blocked",
                "kind": "background",
            },
        ]
    }

    def _fake(argv, **kwargs):
        return _fake_completed(stdout=json.dumps(payload))

    monkeypatch.setattr(claude_mod, "_subprocess_run", _fake)
    result, warnings = claude_mod.claude_agents_json()

    assert result == {
        "907fc8c5": {"live_status": "Working"},
        "baf9409a": {"live_status": "Needs input"},
    }
    # No row is dropped and no vocabulary-drift warning fires: "working" and
    # "blocked" are real values the current binary emits, not drift. They are
    # normalized onto the output vocabulary rather than passed through raw, so
    # a consumer never has to know which binary produced the row.
    assert warnings == [], warnings


def test_claude_agents_json_prefers_state_over_status_on_live_rows(monkeypatch):
    """Regression: `state` outranks `status` when a row carries both.

    Payload captured verbatim from ``claude agents --json`` (2.1.220), which
    emits BOTH keys on the rows that have a live pid, in two different
    vocabularies whose values disagree: ``state: working`` next to
    ``status: idle``. Preferring ``status`` - the older spelling, present on a
    handful of rows where ``state`` is on ~94% of them - resolved an actively
    working session to Idle, which read.py then replaced with a stale
    truth-status reading. That is the render-Idle-while-working bug, arrived at
    after the id/state aliases had already landed.

    x-8bfb: the `working`/`idle` disagreement on row 2 is precedence working
    exactly as documented - both sides individually resolve to a KNOWN live
    status, `state` wins, and that is not drift - so it no longer warns
    (AC6-HP). The resolved value is unchanged (AC9-FR): still `Working`.
    """
    payload = [
        {
            "pid": 66325,
            "id": "7f7d7627",
            "cwd": "/repo/.claude/worktrees/x-af36",
            "kind": "background",
            "startedAt": 1786107208148,
            "sessionId": "7f7d7627-e39a-4f31-a4a5-836e6f174d50",
            "name": "target-x-af36-agents-list",
            "status": "busy",
            "state": "working",
        },
        {
            "pid": 66350,
            "id": "6500bad9",
            "cwd": "/repo",
            "kind": "background",
            "startedAt": 1786107224171,
            "sessionId": "6500bad9-7fe4-4303-acfe-16270c12333c",
            "name": "lead-x-b917-wave16",
            "status": "idle",
            "state": "working",
        },
        {
            "pid": 69787,
            "cwd": "/repo",
            "kind": "interactive",
            "startedAt": 1786107225801,
            "sessionId": "00e91b4c-04c0-41a4-8ab8-6183461a5994",
            "name": "regready-ccld-pipeline-50",
        },
    ]

    def _fake(argv, **kwargs):  # noqa: ARG001
        return _fake_completed(stdout=json.dumps(payload))

    monkeypatch.setattr(claude_mod, "_subprocess_run", _fake)
    result, warnings = claude_mod.claude_agents_json()

    assert result == {
        "7f7d7627": {"live_status": "Working"},
        "6500bad9": {"live_status": "Working"},
    }
    # busy/working mean the same thing, so normalizing before the comparison
    # keeps that row silent; idle/working disagree on the wire but both sides
    # are individually recognized, so `state`'s precedence answers it quietly
    # too (x-8bfb AC6-HP) - no warning of either shape.
    assert warnings == [], warnings


def test_claude_agents_json_total_row_drop_warns_once_about_drift(monkeypatch):
    """A schema change dropping EVERY row shipped unnoticed once: 42 quiet
    per-row warnings and an empty map that reads exactly like "no agents
    running". A total drop is a different claim from a partial one and says so.
    """
    payload = [
        {"agentId": "907fc8c5", "kind": "background", "state": "working"}
        for _ in range(3)
    ]

    def _fake(argv, **kwargs):  # noqa: ARG001
        return _fake_completed(stdout=json.dumps(payload))

    monkeypatch.setattr(claude_mod, "_subprocess_run", _fake)
    result, warnings = claude_mod.claude_agents_json()

    assert result == {}
    assert any("0 of 3 agent rows parsed" in w for w in warnings), warnings
    # The tail must name the axis that actually failed. Every row resolving a
    # short id lands in out_map whatever its status held, so an empty map means
    # short-id resolution failed - blaming live_status would send a drift
    # investigation to the one axis never reached. Asserting only the count is
    # what let the wrong wording ship.
    drop = next(w for w in warnings if "0 of 3 agent rows parsed" in w)
    assert "no usable short id on any of them" in drop, drop


def test_claude_agents_json_all_interactive_rows_is_not_drift(monkeypatch):
    """Peer-review finding: an operator with no agents running is not drift.

    claude lists the operator's own interactive sessions in the same array and
    those carry no id by design, so counting them toward the total-drop claim
    cries schema drift at a perfectly healthy machine - the false alarm that
    teaches people to ignore the warning this exists to make loud.
    """
    payload = [
        {"pid": 69787, "kind": "interactive", "name": "repo-50"},
        {"pid": 75516, "kind": "interactive", "name": "repo-74"},
    ]

    def _fake(argv, **kwargs):  # noqa: ARG001
        return _fake_completed(stdout=json.dumps(payload))

    monkeypatch.setattr(claude_mod, "_subprocess_run", _fake)
    result, warnings = claude_mod.claude_agents_json()

    assert result == {}
    # Silent, not merely un-warned about drift: a per-row "no usable short id"
    # for each interactive session is the same false alarm one level down.
    assert warnings == [], warnings


def test_claude_agents_json_conflicting_aliases_warn_rather_than_pick_silently(monkeypatch):
    """Two aliases both valid but DIFFERENT: there is no way to tell which the
    producer meant, so the pick is reported. Silently choosing one is the
    receipt-cannot-express-the-truth shape; the row still resolves so a
    conflict never costs the enrichment."""
    payload = {"agents": [{"short_id": "aaaaaaaa", "id": "bbbbbbbb", "state": "working"}]}

    def _fake(argv, **kwargs):
        return _fake_completed(stdout=json.dumps(payload))

    monkeypatch.setattr(claude_mod, "_subprocess_run", _fake)
    result, warnings = claude_mod.claude_agents_json()

    assert result == {"aaaaaaaa": {"live_status": "Working"}}
    assert len(warnings) == 1, warnings
    assert "conflicting values across aliases" in warnings[0]
    assert "aaaaaaaa" in warnings[0] and "bbbbbbbb" in warnings[0]


def test_claude_agents_json_unhashable_status_does_not_raise(monkeypatch):
    """A status alias is accepted as any non-None JSON value, so a dict or list
    one reaches the conflict check and the vocabulary check. Comparing aliases
    through a set, or hashing one for the sentinel lookup, would raise TypeError
    out of a best-effort probe that read.py deliberately does not wrap, crashing
    `fno agents ls` on drifted output.

    Row 1 carries the dict alongside a valid ``state``, so the good value still
    wins; row 2 carries only the dict, so it is what reaches the output."""
    payload = {
        "agents": [
            {"id": "907fc8c5", "status": {"phase": "run"}, "state": "working"},
            {"id": "baf9409a", "status": {"phase": "run"}},
        ]
    }

    def _fake(argv, **kwargs):
        return _fake_completed(stdout=json.dumps(payload))

    monkeypatch.setattr(claude_mod, "_subprocess_run", _fake)
    result, warnings = claude_mod.claude_agents_json()

    assert result == {
        "907fc8c5": {"live_status": "Working"},
        "baf9409a": {"live_status": {"phase": "run"}},
    }
    assert any("conflicting values across aliases" in w for w in warnings), warnings
    assert any("unrecognized status=" in w for w in warnings), warnings


def test_claude_agents_json_timeout_returns_empty_with_warning(monkeypatch):
    def _fake(argv, **kwargs):  # noqa: ARG001
        raise subprocess.TimeoutExpired(cmd=argv, timeout=kwargs.get("timeout", 3.0))

    monkeypatch.setattr(claude_mod, "_subprocess_run", _fake)

    result, warnings = claude_mod.claude_agents_json(timeout=3.0)

    assert result == {}
    assert any("timed out" in w.lower() for w in warnings)


# --- claude_agents_json live_status sentinel validation --------------------


def test_claude_agents_json_unrecognized_live_status_warns_passes_through(monkeypatch):
    """Sentinel drift warning — unknown status surfaces a WARN, value passes through."""
    payload = {
        "agents": [
            {"short_id": "abc12345", "status": "Reflecting"},
        ]
    }

    def _fake(argv, **kwargs):  # noqa: ARG001
        return _fake_completed(stdout=json.dumps(payload))

    monkeypatch.setattr(claude_mod, "_subprocess_run", _fake)

    result, warnings = claude_mod.claude_agents_json()

    assert result == {"abc12345": {"live_status": "Reflecting"}}
    assert any("Reflecting" in w for w in warnings)
    assert any("unrecognized" in w.lower() or "expected" in w.lower() for w in warnings)


