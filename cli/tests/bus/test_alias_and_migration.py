"""`fno agents mail send` over the one bus writer.

Post-cutover (ab-cee91152): `fno agents mail send` writes through the SAME bus writer
(one log line, no md-store divergence) and the existing drain finds it.
"""
from __future__ import annotations


import pytest
from typer.testing import CliRunner

from fno.mail.cli import mail_app


@pytest.fixture
def inbox_and_bus(tmp_path, monkeypatch):
    """Co-isolate the md store (FNO_INBOX_ROOT) and the bus log under tmp."""
    monkeypatch.setenv("FNO_INBOX_ROOT", str(tmp_path))
    # `fno agents mail send` resolves the sender via settings.yaml; pin it.
    monkeypatch.chdir(tmp_path)
    (tmp_path / ".fno").mkdir(exist_ok=True)
    (tmp_path / ".fno" / "settings.yaml").write_text(
        "project: sender-proj\n", encoding="utf-8"
    )
    return tmp_path


@pytest.fixture
def runner() -> CliRunner:
    return CliRunner()


# ---------------------------------------------------------------------------
# G4: the sender path (`fno agents mail send`, the alias's replacement) routes
# through the one bus writer (no divergence) and the drain still finds it.
# ---------------------------------------------------------------------------

def test_g4_agents_send_writes_one_bus_line(inbox_and_bus, runner):
    from fno.bus.log import iter_messages

    res = runner.invoke(
        mail_app,
        ["send", "--to-project", "acme", "--kind", "fyi", "--body", "build is green"],
    )
    assert res.exit_code == 0, res.output

    # Exactly ONE bus line, addressed to the recipient (no md-store divergence).
    msgs = list(iter_messages())
    assert len(msgs) == 1
    assert msgs[0].to == "acme"
    assert msgs[0].kind == "fyi"
    assert msgs[0].body == "build is green"
