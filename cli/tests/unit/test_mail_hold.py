"""Busy mode: the hold, its clock, the release, and what each one proves (x-481e).

Every assertion here is a POSITIVE marker. None of them assert that nothing was
injected during a hold, because a working hold and a dead bus produce the same
absence and a test that cannot separate them is not an instrument.
"""

from __future__ import annotations

from datetime import datetime, timedelta, timezone
from types import SimpleNamespace

import pytest

from fno.agents import dispatch, format as fmt
from fno.harness_identity import session_identity_key
from fno.mail import hold as hold_mod

HANDLE = "abcd1234"


@pytest.fixture(autouse=True)
def _isolated_state(tmp_path, monkeypatch):
    """Point the clock directory at a tmp state root, not the real ~/.fno.

    FNO_HOME aims the Rust writer (arm/clear shell the mail-hold verb) at
    the same root the Python readers use; without it the arm writes
    the machine's state while read() checks the tmp one.
    """
    monkeypatch.setattr("fno.paths.state_dir", lambda: tmp_path)
    monkeypatch.setenv("FNO_HOME", str(tmp_path))
    monkeypatch.setenv("FNO_STATE_DIR", str(tmp_path))
    return tmp_path


def _entry(**over):
    base = dict(
        name=HANDLE,
        short_id="",
        harness_session_id=f"{HANDLE}-full",
        delivery_policy="bus-only",
    )
    base.update(over)
    return SimpleNamespace(**base)


def _seed(hold: hold_mod.Hold) -> hold_mod.Hold:
    """Write a clock file directly: the transport only arms and clears, and
    several tests need states it cannot express (a lapsed clock, a
    conversation-sourced one). Same bytes the old writer wrote."""
    import json

    fields = {
        "until": hold.until.strftime("%Y-%m-%dT%H:%M:%SZ") if hold.until else None,
        "window_s": hold.window_s,
        "clock_kind": hold.clock_kind,
        "ceiling": hold.ceiling.strftime("%Y-%m-%dT%H:%M:%SZ") if hold.ceiling else None,
    }
    if hold.source:
        fields["source"] = hold.source
    path = hold_mod.hold_path(hold.handle)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(fields) + "\n", encoding="utf-8")
    return hold


def _stub_gate(monkeypatch, verdict: str):
    """Stub the Rust gate subprocess to answer ``verdict``; returns the calls."""
    import json

    calls: list = []

    def _fake_run(argv, input="", capture_output=True, text=True, timeout=10, **_kw):
        calls.append((argv, input))
        # The Rust gate's control pass, mirrored: a body whose first line
        # opens with the directive delivers with pass=control.
        effective = verdict
        if input.strip().lower().startswith("control:"):
            effective = "deliver"
        out = json.dumps({"verdict": effective, "pass": None, "receipt": None, "until": None})
        return SimpleNamespace(stdout=out + "\n", returncode=0)

    monkeypatch.setattr(dispatch.subprocess, "run", _fake_run)
    monkeypatch.setattr("fno.rust_binary.resolve_installed_binary", lambda: "/bin/true")
    return calls


# --- Task 2: auto-expire, on BOTH branches of the gate ----------------------


def test_a_live_hold_does_not_lapse_and_an_expired_one_does():
    _seed(
        hold_mod.Hold(
            handle=HANDLE,
            until=datetime.now(timezone.utc) + timedelta(minutes=5),
            window_s=300,
        )
    )
    assert hold_mod.lapsed(HANDLE) is False

    _seed(
        hold_mod.Hold(
            handle=HANDLE,
            until=datetime.now(timezone.utc) - timedelta(seconds=1),
            window_s=300,
        )
    )
    assert hold_mod.lapsed(HANDLE) is True


def test_a_corrupt_clock_reads_as_no_clock_and_keeps_holding():
    """An unreadable clock is not evidence the hold ended, so the flag stands.

    The hold then lifts on `fno agents mail hold --off`. A stall, and a
    bounded one.
    """
    _seed(
        hold_mod.Hold(
            handle=HANDLE,
            until=datetime.now(timezone.utc) + timedelta(minutes=5),
            window_s=300,
        )
    )
    hold_mod.hold_path(HANDLE).write_text("{not json", encoding="utf-8")
    assert hold_mod.read(HANDLE) is None
    assert hold_mod.lapsed(HANDLE) is False


def test_gate_maps_a_stubbed_hold_verdict_to_the_refusal(monkeypatch):
    """The gate body lives in Rust (mail_hold.rs ``gate``); Python maps the
    verdict. The entry branch returns before ``load_registry``; the body rides
    the gate so the control pass can see it.
    """
    calls = _stub_gate(monkeypatch, "hold")
    assert dispatch._delivery_policy_refusal(_entry()) == dispatch.BUS_ONLY_POLICY
    assert dispatch._delivery_policy_refusal(_entry(), "control: stop") is None
    assert calls[-1][1] == "control: stop"


def test_gate_maps_deliver_to_none_and_fails_closed_on_a_broken_gate(monkeypatch):
    """A failed or unreadable gate never lifts a hold it could not read: a
    stamped row fails closed to BUS_ONLY_POLICY, while an unstamped token
    keeps failing open toward live delivery.
    """
    monkeypatch.setattr(dispatch, "load_registry", lambda: [_entry()])
    _stub_gate(monkeypatch, "deliver")
    assert dispatch._delivery_policy_refusal(_entry()) is None
    assert dispatch._delivery_policy_refusal(HANDLE) is None

    def _boom(*_a, **_kw):
        raise OSError("gate down")

    monkeypatch.setattr(dispatch.subprocess, "run", _boom)
    assert dispatch._delivery_policy_refusal(_entry()) == dispatch.BUS_ONLY_POLICY


def test_gate_fails_closed_when_the_binary_is_missing(monkeypatch):
    """No gate binary on a stamped row: the refusal stands (fail closed)."""
    monkeypatch.setattr("fno.rust_binary.resolve_installed_binary", lambda: None)
    assert dispatch._delivery_policy_refusal(_entry()) == dispatch.BUS_ONLY_POLICY
    monkeypatch.setattr(dispatch, "load_registry", lambda: [])
    assert dispatch._delivery_policy_refusal(HANDLE) is None


def test_two_same_window_codex_rows_never_share_one_clock():
    """Clocks key the full session identity key.

    Codex UUIDv7 ids opened in one 65.536-second window share their first
    eight, so two same-window sessions used to share one clock file: one
    release deleted the shared clock while only the first matching row's
    policy cleared, leaving the sibling stamped bus-only with no clock, which
    never lapses.
    """
    sid_a = "0198a3f2-77e3-7000-8000-000000000001"
    sid_b = "0198a3f2-77e3-7000-8000-000000000002"
    row_a = SimpleNamespace(
        name="alpha", short_id="", harness_session_id=sid_a, delivery_policy="bus-only"
    )
    row_b = SimpleNamespace(
        name="beta", short_id="", harness_session_id=sid_b, delivery_policy="bus-only"
    )

    _seed(
        hold_mod.Hold(
            handle=session_identity_key(sid_a),
            until=datetime.now(timezone.utc) + timedelta(minutes=5),
            window_s=300,
        )
    )

    assert hold_mod.read_any(row_a) is not None
    assert hold_mod.read_any(row_b) is None
    # The sibling's clock cannot satisfy its row either way: B is stamped
    # with no clock of its own, and that must read as refusing, not as held
    # on A's clock.
    assert dispatch._delivery_policy_refusal(row_b) == dispatch.BUS_ONLY_POLICY


# --- Task 6: the render -----------------------------------------------------


def test_serialized_row_carries_the_policy_and_the_remaining_hold():
    _seed(
        hold_mod.Hold(
            handle=HANDLE,
            until=datetime.now(timezone.utc) + timedelta(minutes=5),
            window_s=300,
        )
    )
    row = fmt.serialize_entry(_full_entry(), live_status=None)

    assert row["delivery_policy"] == "bus-only"
    assert row["dnd"].endswith("m")


def _full_entry(**over):
    from fno.agents.registry import AgentEntry

    fields = dict(
        name=HANDLE,
        harness="claude",
        cwd="/tmp",
        harness_session_id=f"{HANDLE}-full",
        log_path="",
        delivery_policy="bus-only",
    )
    fields.update(over)
    return AgentEntry(**fields)
