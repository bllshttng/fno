"""``fno mail send --force``: receipt vocabulary and the pane provenance row.

Node x-3a64. ``--force`` changes only the TRANSPORT. Before it existed a
live-miss made the sender switch commands, and switching commands is what lost the
envelope, the message id, the reply handle, and the outbox row.

The receipt is the part that must not drift. `typed` is not `delivered`: bytes
written to a PTY is not delivery and is certainly not action, since a full
payload can arrive, render, and be discarded while the return selects a prompt's
default.
"""
from __future__ import annotations

import json
from types import SimpleNamespace

import pytest
from typer.testing import CliRunner

from fno.mail.cli import mail_app
from fno.paths_testing import use_tmpdir

runner = CliRunner()


@pytest.fixture
def _tmp_state(tmp_path, monkeypatch):
    use_tmpdir(monkeypatch, tmp_path)
    return tmp_path


def _entry():
    return SimpleNamespace(
        name="worker",
        mux={"session": "main", "pane_id": 45},
        harness="claude",
        harness_session_id="0199aaaa-1111-7000-8000-aaaaaaaaaaaa",
        session_id="0199aaaa-1111-7000-8000-aaaaaaaaaaaa",
        cwd="/w",
        status="live",
    )


def _install(monkeypatch, *, typed=True, refusal=None):
    """Resolve the recipient to a mux row and stub the pane write.

    ``_mux_pane_send`` is stubbed rather than driven through subprocess, because
    what is under test here is the RECEIPT and the outbox row, not the paste.
    The paste itself is covered in ``test_dispatch_mux_send.py``.
    """
    import fno.agents.dispatch as dispatch
    import fno.agents.discover as discover_mod
    import fno.agents.registry as registry

    entry = _entry()
    monkeypatch.setattr(
        discover_mod,
        "resolve_or_suggest",
        lambda _token, **_k: (None, []),
    )
    monkeypatch.setattr(
        registry,
        "resolve_agent",
        lambda _token, **_k: SimpleNamespace(entry=entry),
    )

    sent: list[tuple] = []

    def _pane_send(e, text, **kwargs):
        sent.append((e, text, kwargs))
        return typed

    monkeypatch.setattr(dispatch, "_mux_pane_send", _pane_send)
    return entry, sent


@pytest.mark.parametrize(
    "argv",
    [
        ["send", "node:x-1234", "ship it", "--force"],
        ["send", "--to-project", "footnote", "ship it", "--force"],
        ["send", "worker", "/fno:review", "--raw", "--force"],
        ["send", "--to-project", "footnote", "heads up", "--kind", "heads-up", "--force"],
    ],
    ids=["job-address", "project", "raw", "kind"],
)
def test_force_refuses_the_lanes_that_have_no_pane(_tmp_state, monkeypatch, argv):
    """A dropped transport flag is worse than a refused one.

    None of these lanes names one pane, and each ENDS the command itself. So a
    guard placed after any of them leaves the flag dropped there, which is how
    the first version of this guard missed `--raw` and `--kind`: it sat below
    both. The parametrize is the point, not the individual cases.
    """
    _entry_row, sent = _install(monkeypatch)

    result = runner.invoke(mail_app, argv)

    assert result.exit_code == 2
    assert "--force" in (result.stderr or "")
    assert not sent
    assert "queued (durable)" not in result.output
