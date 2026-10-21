"""Control lane word ledger.

An ordinary send charges no rolling ledger: rule 7 is its only word gate, and
the acceptance test proves three same-pair sends inside one window all deliver
with no ledger file at all. The `control:` lane keeps its own 60-word rolling
ledger, and every control assertion here is a positive marker.
"""
from __future__ import annotations

import json
import time

import pytest
import typer

from fno import rust_binary
from fno.mail import budget


@pytest.fixture(autouse=True)
def isolated_bus(tmp_path, monkeypatch):
    monkeypatch.setenv("FNO_BUS_DIR", str(tmp_path / "bus"))
    monkeypatch.setenv("FNO_INBOX_ROOT", str(tmp_path / "inbox"))
    yield


def words(n: int) -> str:
    """A body of exactly ``n`` masked words, verified through Rule 7's counter."""
    text = " ".join(f"word{i}" for i in range(n))
    assert rust_binary.style_word_count(text) == n
    return text


def control_send(sender: str, recipient: str, n: int, msg_id: str, **keys):
    return budget.reserve_control(
        sender=sender,
        recipient=recipient,
        words=n,
        msg_id=msg_id,
        **keys,
    )


# --- AC1-HP: an ordinary pair window does not exist -------------------------

def test_three_79_word_sends_deliver_without_a_ledger(
    monkeypatch,
    tmp_path,
    capsys,
):
    """The flipped burst acceptance, all three lanes: three 79-word ordinary
    sends from one sender to one recipient inside one window all deliver, and
    no ``<bus_dir>/word-budget/`` file is ever written."""
    from fno import paths
    from fno.agents.discover import DiscoveredSession
    from fno.agents.registry import AgentEntry, write_registry
    from fno.mail import cli

    recipient = DiscoveredSession(
        session_id="11111111-2222-3333-4444-555566667777",
        short_id="11111111",
        handle="11111111",
        pid=0,
        cwd="/tmp",
        project=None,
        status=None,
        agent="codex",
        truth_state="working",
    )
    monkeypatch.setattr(
        "fno.agents.dispatch._mail_inject_codex",
        lambda _sid, body, **_kwargs: (body, False)[1],
    )
    body = words(79)
    for _ in range(3):
        # a durable demotion that cannot prove landing ends NOT LANDED, exit 14
        with pytest.raises(typer.Exit):
            cli._name_lane_send(body, from_name="sender", resolved=recipient)
        assert "queued (durable)" in capsys.readouterr().out

    registry = tmp_path / "agents.json"
    monkeypatch.setattr(paths, "agents_registry_path", lambda: registry)
    write_registry(
        [
            AgentEntry(
                name="worker",
                harness="claude",
                harness_session_id="aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee",
                short_id="aaaaaaaa",
                cwd=str(tmp_path),
                log_path="",
                status="idle",
            )
        ],
        registry,
    )
    monkeypatch.setattr(
        "fno.agents.dispatch._registered_family1_state",
        lambda _entry: "sleeping",
    )
    from fno.agents.dispatch import dispatch_send

    for _ in range(3):
        result = dispatch_send(
            "worker", body, provider=None, cwd=tmp_path, from_name="sender"
        )
        assert result.delivery == "durable"

    assert not (paths.bus_dir() / "word-budget").exists(), (
        "an ordinary send charges no rolling ledger"
    )


# --- control lane: operational control rides its own window ----------------

def test_is_control_reads_the_first_non_blank_line_only():
    assert budget.is_control("control: stop all spawns now")
    assert budget.is_control("CONTROL: stop all spawns now"), "one capital letter bypasses an exact-case check"
    assert budget.is_control("  \n control: lift the hold")
    assert not budget.is_control("we need a control: review of this")
    assert not budget.is_control("status report\ncontrol: mentioned below")
    assert not budget.is_control("")


def test_control_body_over_the_cap_refuses_with_the_lane_marker(monkeypatch, capsys):
    """AC1-ERR: one 61-word control body refuses naming the lane; two 40-word
    control bodies refuse on the second."""
    from fno.agents.discover import DiscoveredSession
    from fno.mail import cli

    recipient = DiscoveredSession(
        session_id="11111111-2222-3333-4444-555566667777",
        short_id="11111111",
        handle="11111111",
        pid=0,
        cwd="/tmp",
        project=None,
        status=None,
        agent="codex",
        truth_state="working",
    )
    monkeypatch.setattr(
        "fno.agents.dispatch._mail_inject_codex",
        lambda _sid, _body, **_kwargs: False,
    )
    with pytest.raises(typer.Exit) as raised:
        cli._name_lane_send("control: " + words(60), from_name="sender", resolved=recipient)
    assert raised.value.exit_code == 1
    assert "refused: control word budget" in capsys.readouterr().err

    with pytest.raises(typer.Exit) as raised_mid:
        cli._name_lane_send("control: " + words(39), from_name="sender", resolved=recipient)
    assert raised_mid.value.exit_code == 14
    capsys.readouterr()
    with pytest.raises(typer.Exit) as raised2:
        cli._name_lane_send("control: " + words(39), from_name="sender", resolved=recipient)
    assert raised2.value.exit_code == 1
    refusal = capsys.readouterr().err
    assert "refused: control word budget" in refusal
    assert "running=40 current=40 projected=80 cap=60 window=10m" in refusal


def test_control_window_resets_on_an_inbound_reply():
    budget.reserve_control(sender="a", recipient="b", words=55, msg_id="msg-ctl-1")
    reset_id = _inbound("a", "b", "msg-in-ctl")
    second = budget.reserve_control(sender="a", recipient="b", words=55, msg_id="msg-ctl-2")
    assert second.running_before == 0
    assert second.reset_by == reset_id


@pytest.mark.parametrize("kind", ["withdraw"])
def test_non_authored_reverse_rows_do_not_reset_the_control_window(kind):
    control_send("a", "b", 55, f"ctl-out-{kind}")
    _inbound("a", "b", f"ctl-in-{kind}", kind=kind)
    with pytest.raises(budget.BudgetRefused) as raised:
        control_send("a", "b", 55, f"ctl-after-{kind}")
    assert raised.value.running == 55


# --- the control ledger ------------------------------------------------------

def _inbound(
    sender: str,
    recipient: str,
    msg_id: str,
    *,
    kind: str = "send",
) -> str:
    """Write one bus envelope FROM the recipient TO the sender."""
    from fno.bus.log import Envelope, append

    append(
        Envelope.new(
            id=msg_id,
            from_=recipient,
            to=sender,
            kind=kind,
            body="ok",
            word_count=1,
        )
    )
    return msg_id


def test_concurrent_sends_serialize_on_the_pair_ledger():
    import threading

    results: list = []

    def attempt(msg_id: str) -> None:
        try:
            results.append(("ok", control_send("a", "b", 50, msg_id)))
        except budget.BudgetRefused as exc:
            results.append(("refused", exc))

    threads = [threading.Thread(target=attempt, args=(f"msg-c{i}",)) for i in range(2)]
    for t in threads:
        t.start()
    for t in threads:
        t.join()

    kinds = sorted(k for k, _ in results)
    assert kinds == ["ok", "refused"]
    refusal = next(v for k, v in results if k == "refused")
    assert refusal.marker() == (
        "running=50 current=50 projected=100 cap=60 window=10m"
    )
    stored = json.loads(budget._ledger_path("control:a -> b").read_text())
    assert len(stored["entries"]) == 1, "exactly one live reservation for the pair"


# --- window expiry and release --------------------------------------------

def test_entries_older_than_the_window_are_pruned():
    res = control_send("a", "b", 55, "msg-old")
    path = budget._ledger_path(res.pair)
    stored = json.loads(path.read_text())
    stored["entries"][0]["ts"] = time.time() - budget.WINDOW_SECONDS - 1
    path.write_text(json.dumps(stored))
    fresh = control_send("a", "b", 55, "msg-new")
    assert fresh.running_before == 0


# --- fail closed ----------------------------------------------------------

def test_a_malformed_active_ledger_refuses_rather_than_resetting():
    res = control_send("a", "b", 55, "msg-one")
    path = budget._ledger_path(res.pair)
    path.write_text("{ not json")
    with pytest.raises(budget.BudgetUnavailable) as exc:
        control_send("a", "b", 1, "msg-two")
    assert "control:a -> b" in str(exc.value)
    assert str(path) in str(exc.value), "the refusal names the recovery path"


def test_the_enforcing_count_refuses_when_the_door_fails(monkeypatch):
    # A zero would read as unlimited traffic under the rolling cap, so the
    # enforcing count raises, never answers 0.
    def _dead(*_a, **_kw):
        return ("fno-agents binary not found", None)

    monkeypatch.setattr(rust_binary, "call_binary_json", _dead)
    with pytest.raises(budget.BudgetCountUnavailable) as exc:
        rust_binary.style_word_count_checked("control: hold")
    assert "binary not found" in str(exc.value)
