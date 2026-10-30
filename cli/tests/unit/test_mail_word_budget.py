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

# --- control lane: operational control rides its own window ----------------

def test_is_control_reads_the_first_non_blank_line_only():
    assert budget.is_control("control: stop all spawns now")
    assert budget.is_control("CONTROL: stop all spawns now"), "one capital letter bypasses an exact-case check"
    assert budget.is_control("  \n control: lift the hold")
    assert not budget.is_control("we need a control: review of this")
    assert not budget.is_control("status report\ncontrol: mentioned below")
    assert not budget.is_control("")


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
    from tests.fixtures.bus_rows import append_row

    append_row(
        {
            "id": msg_id,
            "from": recipient,
            "to": sender,
            "kind": kind,
            "body": "ok",
            "word_count": 1,
        }
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
