"""``fno agents lead faq add`` - write, refuse-with-no-exit, and scope defaults.

Invoked through ``agents_lead_app`` rather than the bare ``faq_app``: a Typer
app with exactly one command collapses to that command directly (no
subcommand word needed), so testing ``faq_app`` alone would silently swallow
the literal "add" the real ``fno agents lead faq add`` invocation sends.
``agents_lead_app`` carries sibling commands (init, done, cancel, ...) and
never collapses, matching the real CLI surface.
"""
from __future__ import annotations

from pathlib import Path

from typer.testing import CliRunner

from fno.lead.cli import agents_lead_app


def test_add_cmd_refuses_with_no_exit(monkeypatch, tmp_path):
    monkeypatch.setattr("fno.paths.lead_faqs_dir", lambda: tmp_path)
    runner = CliRunner()
    result = runner.invoke(
        agents_lead_app,
        ["faq", "add", "--question", "Q?", "--answer", "A", "--specimen", "S", "--scope", "x-6cac"],
    )
    assert result.exit_code == 2
    assert "no --exit" in result.output
    assert "workaround with no plan to stop needing it" in result.output
    assert list(tmp_path.iterdir()) == []


def test_add_cmd_refuses_with_no_scope_and_no_role(monkeypatch, tmp_path):
    import fno.agents.role as role

    monkeypatch.setattr(role, "current_role", lambda: None)
    monkeypatch.setattr("fno.paths.lead_faqs_dir", lambda: tmp_path)
    runner = CliRunner()
    result = runner.invoke(
        agents_lead_app,
        ["faq", "add", "--question", "Q?", "--answer", "A", "--specimen", "S", "--exit", "E"],
    )
    assert result.exit_code == 2
    assert "no role held and no --scope given" in result.output
    assert list(tmp_path.iterdir()) == []


def test_add_cmd_defaults_scope_from_current_role(monkeypatch, tmp_path):
    import fno.agents.role as role
    import fno.agents.self_stamp as self_stamp

    monkeypatch.setattr(
        role, "current_role", lambda: {"level": 2, "scope": "x-6cac", "grantor": "human"}
    )
    monkeypatch.setattr(self_stamp, "resolve_self_handle", lambda: "lead-fixture")
    monkeypatch.setattr(self_stamp, "resolve_self_session_id", lambda: "session-abc123")
    monkeypatch.setattr("fno.paths.lead_faqs_dir", lambda: tmp_path)

    runner = CliRunner()
    result = runner.invoke(
        agents_lead_app,
        ["faq", "add", "--question", "Q?", "--answer", "A", "--specimen", "S", "--exit", "E"],
    )
    assert result.exit_code == 0, result.output
    written = Path(result.output.strip())
    assert written.exists()
    text = written.read_text(encoding="utf-8")
    assert "scope: x-6cac" in text
    assert "lead: lead-fixture" in text
    assert "session: session-abc123" in text
