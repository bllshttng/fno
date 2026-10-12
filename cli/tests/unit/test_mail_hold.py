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


def _msg(msg_id, sender, body, ts="2026-08-20T10:00:00Z"):
    return SimpleNamespace(id=msg_id, from_=sender, body=body, ts=ts)


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


def _expire(handle):
    """Age a live timed hold past its deadline without deleting its clock."""
    return _seed(
        hold_mod.Hold(
            handle=handle,
            until=datetime.now(timezone.utc) - timedelta(seconds=1),
            window_s=300,
        )
    )


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


# --- Task 1: the hold and its clock -----------------------------------------


def test_a_conversation_sourced_clock_renders_the_auto_suffix():
    # The conversation-sourced clock is the machine's own hold; the column
    # must not read it as a hold the user set on purpose.
    until = datetime.now(timezone.utc) + timedelta(minutes=5)
    window_s = 300
    clock_kind = "idle"
    ceiling = until + timedelta(seconds=window_s)
    auto = hold_mod.Hold(
        handle=HANDLE,
        until=until,
        window_s=window_s,
        clock_kind=clock_kind,
        ceiling=ceiling,
        source=hold_mod.CONVERSATION_SOURCE,
    )
    _seed(auto)
    assert hold_mod.read(HANDLE).source == hold_mod.CONVERSATION_SOURCE
    assert hold_mod.dnd_label(HANDLE).endswith(" (auto)")
    bare = hold_mod.Hold(
        handle=HANDLE,
        until=until,
        window_s=window_s,
        clock_kind=clock_kind,
        ceiling=ceiling,
    )
    _seed(bare)
    assert hold_mod.dnd_label(HANDLE) == hold_mod.remaining_label(HANDLE)
    assert not hold_mod.dnd_label(HANDLE).endswith(" (auto)")


# --- Task 2: auto-expire, on BOTH branches of the gate ----------------------


def test_a_flag_with_no_clock_is_not_a_hold_and_never_lapses():
    """A bus-only row with no clock predates busy mode, so it keeps its policy.

    Lifting it would silently revoke the no-paste guarantee of every row
    stamped by `fno agents register --delivery-policy bus-only`, which has no
    clock by construction. The hold-forever fear that argues for the opposite
    does not apply here: held mail is durable on the bus and surfaces at the
    next turn boundary, so a lost clock costs a stall, never a message.
    """
    assert hold_mod.lapsed(HANDLE) is False


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


def test_the_raw_door_parks_through_the_gate_and_relays_the_receipt(monkeypatch):
    """With ``park=True`` the raw door's gate call carries ``--park-on-hold``
    and the parked receipt rides back in place of the refusal; without it the
    same held row answers the plain refusal.
    """
    import json

    calls: list = []

    def _fake_run(argv, input="", capture_output=True, text=True, timeout=10, **_kw):
        calls.append((list(argv), input))
        if "--park-on-hold" in argv:
            out = {
                "verdict": "parked",
                "pass": None,
                "receipt": f"held: {input} runs on {HANDLE} when the hold ends",
                "until": None,
            }
        else:
            out = {"verdict": "hold", "pass": None, "receipt": None, "until": None}
        return SimpleNamespace(stdout=json.dumps(out) + "\n", returncode=0)

    monkeypatch.setattr(dispatch.subprocess, "run", _fake_run)
    monkeypatch.setattr("fno.rust_binary.resolve_installed_binary", lambda: "/bin/true")
    assert (
        dispatch._delivery_policy_refusal(_entry(), "/compact", park=True)
        == f"held: /compact runs on {HANDLE} when the hold ends"
    )
    assert any("--park-on-hold" in argv for argv, _ in calls)
    assert dispatch._delivery_policy_refusal(_entry(), "/compact") == dispatch.BUS_ONLY_POLICY
    monkeypatch.setattr(dispatch, "load_registry", lambda: [])
    assert dispatch._delivery_policy_refusal(HANDLE) is None


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


def test_gate_leaves_a_clockless_bus_only_row_refusing_on_both_branches(monkeypatch):
    """The clockless-hold guarantee, unchanged for every row busy mode
    never touched.

    The row keeps the production shape: its registry name is its label, not
    its handle, so the token branch must reach it through the address sweep
    (the canonical handle here), not through the name.
    """
    row = _entry(name="quill")
    monkeypatch.setattr(dispatch, "load_registry", lambda: [row])
    _stub_gate(monkeypatch, "hold")

    assert dispatch._delivery_policy_refusal(row) == dispatch.BUS_ONLY_POLICY
    assert dispatch._delivery_policy_refusal(HANDLE) == dispatch.BUS_ONLY_POLICY


# --- Task 5: the bounce -----------------------------------------------------


def test_bounce_reason_returns_the_gate_receipt(monkeypatch):
    """The receipt body lives in the Rust gate (C16); Python returns its
    ``receipt`` field verbatim, and its None too."""
    import json

    def _fake_run(argv, **_kw):
        out = json.dumps(
            {
                "verdict": "hold",
                "pass": None,
                "receipt": "held until about 21:04: worker is in do-not-disturb; "
                "delivers itself then",
                "until": None,
            }
        )
        return SimpleNamespace(stdout=out, returncode=0)

    monkeypatch.setattr(hold_mod.subprocess, "run", _fake_run)
    monkeypatch.setattr("fno.rust_binary.resolve_installed_binary", lambda: "/bin/true")

    reason = hold_mod.bounce_reason(HANDLE)

    assert reason is not None
    assert "do-not-disturb" in reason
    assert reason.startswith("held until about 21:04")


def test_bounce_reason_none_keeps_the_callers_text(monkeypatch):
    import json

    def _fake_run(argv, **_kw):
        out = json.dumps({"verdict": "hold", "pass": None, "receipt": None, "until": None})
        return SimpleNamespace(stdout=out, returncode=0)

    monkeypatch.setattr(hold_mod.subprocess, "run", _fake_run)
    monkeypatch.setattr("fno.rust_binary.resolve_installed_binary", lambda: "/bin/true")

    assert hold_mod.bounce_reason(HANDLE) is None


def test_bounce_reason_is_silent_for_a_permanent_policy_and_for_no_hold():
    # The permanent policy IS the absent clock beside the registry flag,
    # so the old arm_permanent write was already this test's starting state.
    assert hold_mod.bounce_reason(HANDLE) is None


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


def test_a_row_with_no_hold_renders_a_null_dnd():
    row = fmt.serialize_entry(_full_entry(delivery_policy=None), live_status=None)
    assert row["delivery_policy"] is None
    assert row["dnd"] is None


def test_hold_refuses_a_contaminated_env_rather_than_stamping_a_guessed_row(monkeypatch):
    """Precedence order is not ownership, so an inherited marker from a parent
    harness makes a precedence-only resolve answer with the PARENT session.

    `--to-self` already fails closed here. A hold must too, and the stakes are
    higher: a misaddressed send delivers one message to the wrong place, while
    a misaddressed hold stamps a delivery policy on another agent's row and
    arms a timer against their handle. This was live on the machine that wrote
    the test, where claude and codex markers were both present.

    The hold now shares the one owned-identity path rather than hand-rolling a
    >1-family check, so this drives the real seam: two families the process
    tree cannot decide between refuse, one family resolves.
    """
    import typer

    from fno.harness_identity import OwnedHarnessIdentity
    from fno.mail import cli as mail_cli

    monkeypatch.setattr(
        "fno.agents.self_stamp.resolve_self_identity",
        lambda *a, **k: OwnedHarnessIdentity(
            None,
            None,
            (
                ("CLAUDE_CODE_SESSION_ID", "claude", "x"),
                ("CODEX_THREAD_ID", "codex", "y"),
            ),
            "ambiguous",
        ),
    )

    with pytest.raises(typer.Exit) as caught:
        mail_cli._self_handle_or_exit()
    assert caught.value.exit_code == 3

    # One family present is a clean session and must still resolve.
    monkeypatch.setattr(
        "fno.agents.self_stamp.resolve_self_identity",
        lambda *a, **k: OwnedHarnessIdentity(
            f"{HANDLE}-full", "codex", (("CODEX_THREAD_ID", "codex", "y"),), "single"
        ),
    )
    resolved_handle, resolved_ident = mail_cli._self_handle_or_exit()
    assert resolved_handle == HANDLE
    # The identity travels back with the handle, so the caller writes the row
    # this function validated rather than re-resolving and getting another.
    assert resolved_ident.session_id == f"{HANDLE}-full"


def test_an_unreadable_clock_renders_a_question_mark_not_an_empty_cell(monkeypatch):
    """An instrument that declines to answer must not answer anyway.

    None renders as "-", the same cell a row with no hold gets, so a failed
    read would report mail flowing to a session whose flag says it is held.
    """
    def _boom(_entry):
        raise RuntimeError("clock unreadable")

    monkeypatch.setattr(hold_mod, "dnd_label", _boom)

    assert fmt._dnd_label(_full_entry()) == "?"


def test_a_bus_only_row_with_no_clock_reads_held_not_blank():
    """The pre-busy-mode row: flag set by hand, no clock, mail genuinely held.

    A blank cell here would be the column lying about the one row it exists to
    describe, and every row stamped before this file existed is this shape.
    """
    row = fmt.serialize_entry(_full_entry(), live_status=None)

    assert row["delivery_policy"] == "bus-only"
    assert row["dnd"] == "held"


def test_the_dnd_column_and_the_delivery_gate_never_disagree(monkeypatch):
    """Whatever the column says, the gate must agree mail is or is not moving.

    The stub answers from the SAME Python clock state the column reads, so the
    parity contract itself is what runs; the gate's own clock logic is tested
    in Rust.
    """
    import json

    real_run = hold_mod.subprocess.run

    def _fake_run(argv, **_kw):
        if "--session" not in argv:
            # A mail-hold read/write the port routed through the door: serve
            # the real clock state (the seeds write it, the column and the
            # gate mirror both read it back) instead of answering "no clock"
            # for every case, which would agree vacuously.
            return real_run(argv, capture_output=True, text=True, timeout=10)
        token = argv[argv.index("--session") + 1]
        clock = hold_mod.read_any(token)
        # Mirror the Rust gate: only a LAPSED timed clock delivers; no clock,
        # a permanent (until: null) clock, and a live clock all hold.
        lapsed = (
            clock is not None
            and clock.until is not None
            and clock.until <= hold_mod._now()
        )
        verdict = "deliver" if lapsed else "hold"
        out = json.dumps({"verdict": verdict, "pass": None, "receipt": None, "until": None})
        return SimpleNamespace(stdout=out, returncode=0)

    monkeypatch.setattr(dispatch.subprocess, "run", _fake_run)
    monkeypatch.setattr("fno.rust_binary.resolve_installed_binary", lambda: "/bin/true")
    # The entry branch passes the harness session id to the gate, so the
    # clocks here sit under that same key; the column's addresses() sweep
    # finds it either way.
    full = _entry().harness_session_id
    # The clockful states seed directly: inside this test every subprocess
    # ride is the stub, so a transport arm would no-op and each case would
    # degenerate to "no clock" while its name still said otherwise.
    cases = [
        ("no clock", lambda: None),
        (
            "permanent",
            lambda: _seed(hold_mod.Hold(handle=full, until=None, window_s=None)),
        ),
        (
            "live timed",
            lambda: _seed(
                hold_mod.Hold(
                    handle=full,
                    until=datetime.now(timezone.utc) + timedelta(minutes=5),
                    window_s=300,
                    clock_kind="idle",
                    ceiling=datetime.now(timezone.utc) + timedelta(minutes=10),
                )
            ),
        ),
        ("lapsed timed", lambda: _expire(full)),
    ]
    for name, arrange in cases:
        hold_mod.hold_path(HANDLE).unlink(missing_ok=True)
        hold_mod.hold_path(full).unlink(missing_ok=True)
        arrange()
        held_per_column = fmt.serialize_entry(_full_entry(), live_status=None)["dnd"]
        held_per_gate = (
            dispatch._delivery_policy_refusal(_entry()) == dispatch.BUS_ONLY_POLICY
        )
        assert (held_per_column is not None) is held_per_gate, name


def test_a_lapsed_hold_renders_no_dnd_because_mail_flows_again():
    _seed(
        hold_mod.Hold(
            handle=HANDLE,
            until=datetime.now(timezone.utc) - timedelta(seconds=1),
            window_s=60,
        )
    )
    row = fmt.serialize_entry(_full_entry(), live_status=None)
    assert row["dnd"] is None


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
