"""The enveloped pane lane: wrap by default, ``raw`` opt-out, prompt refusal.

Node x-3a64. A pane drive types at a worker's prompt, and unwrapped it is
indistinguishable from the operator typing. These tests pin the three decisions
that changed: what a default send puts on the wire, that ``raw=True`` is
byte-identical to the old behavior, and that a pane showing an option prompt is
refused rather than typed into.

Deliberately NO module-level "idle pane" fixture (the two legacy pane modules
have one so their transport assertions still run). Here the gate is the subject.
"""
from __future__ import annotations

from types import SimpleNamespace


import fno.agents.dispatch as dispatch


def _entry():
    return SimpleNamespace(
        mux={"session": "main", "pane_id": 7},
        harness="claude",
        harness_session_id="worker-session",
        session_id=None,
        cwd="/w",
        status="live",
    )


def _runner(calls, *, screen="", returncode=0):
    """A fake ``subprocess.run`` covering the pane commands this lane issues.

    The ``mail-envelope`` render is NOT faked: the renderer lives behind the
    Rust binary, and a fake here would turn the envelope assertions below into
    tautologies. Pass that one through to the real subprocess.
    """
    real_run = dispatch.subprocess.run

    def run(argv, **kwargs):
        calls.append({"argv": list(argv), "input": kwargs.get("input")})
        if "mail-envelope" in argv:
            return real_run(argv, **kwargs)
        if argv[1:4] == ["mux", "pane", "read"]:
            return SimpleNamespace(returncode=0, stdout=screen, stderr="")
        return SimpleNamespace(returncode=returncode, stdout="", stderr="")

    return run


def _detector(monkeypatch, asked, verdict=None):
    """Stub the manifest engine and record the harness it was asked about.

    Patched at :func:`fno.agents.mux_spawn._evaluate_manifest_screen` rather
    than through ``subprocess.run``, because that function resolves an INSTALLED
    ``fno-agents`` binary and returns ``{"error": "manifest-eval binary
    unavailable"}`` without ever shelling out when there is none. CI has none, so
    a subprocess-level stub silently never ran there and every gate assertion
    read the unavailable branch instead of the one it named.

    ``verdict=None`` is the default no-match. Pass ``{"error": ...}`` to model a
    detector that could not run at all, which is a different answer from "no
    prompt showing".
    """
    answer = verdict if verdict is not None else {"matched": False}

    def _eval(harness, screen, runner=None, **_kwargs):
        asked.append({"harness": harness, "screen": screen})
        return answer

    monkeypatch.setattr("fno.agents.mux_spawn._evaluate_manifest_screen", _eval)
    return asked


def _pasted(calls):
    for call in calls:
        if "--stdin" in call["argv"]:
            return call["input"] or ""
    return ""


def _commands(calls):
    return [
        c["argv"][3] for c in calls if c["argv"][1:3] == ["mux", "pane"] and len(c["argv"]) > 3
    ]


def test_default_send_wraps_the_body_in_an_fno_mail_envelope(monkeypatch):
    """The paste opens with the delivered attribution and carries the body.

    This is the whole node in one assertion: a worker reading the paste can tell
    it came from a peer, whichever transport typed it. Mail never sends as bare
    `fno`, so the lane names its sender.

    Promoted topology, because x-2dfa gates the trailer on the role: the
    subject here is that the PANE transport carries whatever the single
    renderer produced, not which topology produces a trailer (that pair is
    asserted in ``test_mail_origin.py``).
    """
    calls: list[dict] = []
    monkeypatch.setattr(dispatch.subprocess, "run", _runner(calls))
    monkeypatch.setattr(dispatch.time, "sleep", lambda *_a: None)
    _detector(monkeypatch, [])

    assert (
        dispatch._mux_pane_send(_entry(), "status?", guarded=False, sender="lead")
        is True
    )

    paste = _pasted(calls)
    assert paste.startswith("`@lead · ")
    assert "status?" in paste
    # The wrapped attribution itself is the point; the retired peer-mail
    # footer no longer renders.
    assert "peer mail" not in paste


def test_read_receipt_identity_gate_matches_on_uuid_not_the_name(monkeypatch, capsys):
    """Identity is the session uuid, never the name.

    A receipt whose uuid differs from the addressed row refuses before typing
    (the old pinned refusal); a receipt whose uuid MATCHES but whose pane
    label lags a rename delivers.
    """
    calls: list[dict] = []
    entry = _entry()
    entry.name = "worker"
    monkeypatch.setattr(
        "fno.mail.pane_transport._pane_entry",
        lambda _session, _pane: entry,
    )
    _detector(monkeypatch, [])

    real_run = dispatch.subprocess.run

    def run(argv, **kwargs):
        calls.append({"argv": list(argv), "input": kwargs.get("input")})
        if "mail-envelope" in argv:
            return real_run(argv, **kwargs)
        if argv[1:4] == ["mux", "pane", "read"]:
            return SimpleNamespace(
                returncode=0,
                stdout=receipt,
                stderr="",
            )
        if argv[1:4] == ["mux", "pane", "ls"]:
            return SimpleNamespace(
                returncode=0,
                stdout=pane_ls,
                stderr="",
            )
        return SimpleNamespace(returncode=0, stdout="", stderr="")

    # Both differ: refuse before typing.
    receipt = (
        '{"pane_id":7,"text":"$ ","pane_name":"caller",'
        '"registry_fno_id":"caller-session"}'
    )
    pane_ls = '[{"pane_id":7,"name":"worker","fno_id":"worker-session"}]'
    calls.clear()
    monkeypatch.setattr(dispatch.subprocess, "run", run)
    monkeypatch.setattr(dispatch.time, "sleep", lambda *_a: None)

    assert dispatch._mux_pane_send(entry, "status?", guarded=False) is False
    assert not any(call["argv"][1:4] == ["mux", "pane", "send"] for call in calls)
    assert "identity mismatch" in capsys.readouterr().err

    # Only the name differs: the renamed worker (bob, ex-kestrel-successor)
    # still receives, because the uuid names the same live session.
    receipt = (
        '{"pane_id":7,"text":"$ ","pane_name":"kestrel-successor",'
        '"registry_fno_id":"worker-session"}'
    )
    pane_ls = '[{"pane_id":7,"name":"kestrel-successor","fno_id":"worker-session"}]'
    calls.clear()
    capsys.readouterr()

    assert (
        dispatch._mux_pane_send(entry, "status?", guarded=False, sender="lead") is True
    )
    assert any(call["argv"][1:4] == ["mux", "pane", "send"] for call in calls)


def test_read_receipt_without_text_refuses_before_typing(monkeypatch, capsys):
    calls: list[dict] = []
    entry = _entry()
    entry.name = "worker"
    monkeypatch.setattr(dispatch.subprocess, "run", _runner(calls))
    monkeypatch.setattr(dispatch.time, "sleep", lambda *_a: None)

    def run(argv, **kwargs):
        calls.append({"argv": list(argv), "input": kwargs.get("input")})
        if argv[1:4] == ["mux", "pane", "ls"]:
            return SimpleNamespace(
                returncode=0,
                stdout='[{"pane_id":7,"name":"worker","fno_id":"worker-session"}]',
                stderr="",
            )
        if argv[1:4] == ["mux", "pane", "read"]:
            return SimpleNamespace(
                returncode=0,
                stdout='{"pane_id":7,"pane_name":"worker",'
                '"registry_fno_id":"worker-session"}',
                stderr="",
            )
        return SimpleNamespace(returncode=0, stdout="", stderr="")

    monkeypatch.setattr(dispatch.subprocess, "run", run)
    _detector(monkeypatch, [])

    failure: list[str] = []
    assert (
        dispatch._mux_pane_send(
            entry, "status?", guarded=False, failure_out=failure
        )
        is False
    )
    assert failure == ["pre-submit"]
    assert not any(call["argv"][1:4] == ["mux", "pane", "send"] for call in calls)
    assert "text" in capsys.readouterr().err


def test_an_unrunnable_detector_refuses_rather_than_typing_blind(monkeypatch, capsys):
    """An instrument that never ran is not an idle pane.

    The absence of a detected prompt has two explanations and a send built on
    one cannot tell them apart, so the unmeasurable case refuses.
    """
    calls: list[dict] = []
    monkeypatch.setattr(dispatch.subprocess, "run", _runner(calls))
    monkeypatch.setattr(dispatch.time, "sleep", lambda *_a: None)
    _detector(monkeypatch, [], {"matched": False, "error": "manifest-eval unavailable"})

    assert dispatch._mux_pane_send(_entry(), "hello", guarded=False) is False
    assert "send" not in _commands(calls)
    err = capsys.readouterr().err
    assert "manifest-eval unavailable" in err
    assert "refusing to type blind" in err


def test_a_blocked_pane_with_no_answer_grammar_is_still_refused(monkeypatch, capsys):
    """`answerable` is a SUBSET of blocked, and the wrong half to gate on.

    `evaluate_answerable` returns nothing for a matched blocked rule carrying no
    answer grammar, or one whose options failed to parse: a codex auth wall, a
    trust prompt whose menu did not render as a list. Those are the panes most
    in need of the refusal, and gating on `answerable` alone sent every one of
    them a paste and a CR. Spawn readiness already tests `state == "blocked"`,
    and the transport doc states that same contract.
    """
    calls: list[dict] = []
    monkeypatch.setattr(dispatch.subprocess, "run", _runner(calls))
    monkeypatch.setattr(dispatch.time, "sleep", lambda *_a: None)
    _detector(
        monkeypatch,
        [],
        {"matched": True, "state": "blocked", "rule_id": "auth_wall"},
    )

    assert dispatch._mux_pane_send(_entry(), "hello", guarded=False) is False
    assert "send" not in _commands(calls)
    err = capsys.readouterr().err
    assert "auth_wall" in err
    assert "blocking prompt" in err


def test_a_non_mail_payload_is_verbatim_but_still_gated(monkeypatch, capsys):
    """Verbatim and ungated are DIFFERENT requests, and only one was asked for.

    `prepare` does two jobs: it wraps, and it refuses a pane showing a prompt.
    Skipping it wholesale to keep a ritual or a digest byte-exact also skipped
    the gate. On a codex auth wall the CR then takes the wall's default, the
    payload is discarded, and the bytes-written verdict still reads True, so a
    hold release advances the cursor and retires every held message unread.

    Only a keystroke ANSWERING a prompt wants the gate off.
    """
    calls: list[dict] = []
    monkeypatch.setattr(dispatch.subprocess, "run", _runner(calls))
    monkeypatch.setattr(dispatch.time, "sleep", lambda *_a: None)
    _detector(
        monkeypatch, [], {"matched": True, "state": "blocked", "rule_id": "auth_wall"}
    )
    monkeypatch.setattr(
        dispatch, "_delivery_policy_refusal", lambda *_a: None, raising=False
    )

    assert dispatch._deliver_live(_entry(), "run the ritual", "fno") is False
    assert "send" not in _commands(calls), "a blocked pane must receive nothing"
    assert "auth_wall" in capsys.readouterr().err


