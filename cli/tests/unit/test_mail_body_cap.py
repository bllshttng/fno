"""Body brevity cap for authored relay mail.

Mail is re-read every turn, so the cap targets the verbose tail (duplicate
node/doc content mailed in bulk), not the median. Fail-open: a disabled tier or
an override never blocks coordination.
"""
from __future__ import annotations

import pytest
from typer.testing import CliRunner

from fno.cli import app
from fno.mail import cli as mail_cli
from fno.paths_testing import use_tmpdir


@pytest.fixture
def runner():
    return CliRunner()


@pytest.fixture
def mailbox(tmp_path, monkeypatch):
    monkeypatch.delenv("FNO_BUS_DIR", raising=False)
    monkeypatch.setenv("FNO_CLAUDE_DAEMON_DIR", str(tmp_path / "daemon-empty"))
    monkeypatch.setenv("FNO_INBOX_ROOT", str(tmp_path))
    use_tmpdir(monkeypatch, tmp_path)
    return tmp_path


def test_enforce_warns_but_does_not_block(capsys, monkeypatch):
    monkeypatch.setattr(mail_cli, "_BODY_WARN_BYTES", 10)
    monkeypatch.setattr(mail_cli, "_BODY_REFUSE_BYTES", 1000)
    mail_cli._enforce_body_cap("x" * 50)
    err = capsys.readouterr().err
    assert "brevity guide" in err


def test_cli_send_refuses_oversized_body(runner, mailbox, monkeypatch):
    monkeypatch.setattr(mail_cli, "_BODY_WARN_BYTES", 5)
    monkeypatch.setattr(mail_cli, "_BODY_REFUSE_BYTES", 10)
    body = "x" * 200
    res = runner.invoke(
        app,
        ["mail", "send", "--to-project", "web", "--kind", "fyi",
         "--from-name", "etl", "--body", body],
    )
    assert res.exit_code == 1
    assert "put the detail in a node or doc" in (res.output + (res.stderr or ""))
