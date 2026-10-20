"""x-6346 - `--to-lead <scope>` resolves the role at SEND time.

Succession moves the role row; it does not move the mail handle a peer learned
while that handle was promoted. Resolving the ROLE off the registry at send time
is what removes the stale handle, and a forwarding pointer written at departure
would only be the same recorded identity one hop over.

Covers AC1 (one live holder resolves to it), AC2 (no live holder refuses and
queues nothing; a successor resolves to the successor), the split-role refusal,
and the delivery-time recipient stamp that AC3 asks for.
"""
from __future__ import annotations

import pytest
from typer.testing import CliRunner

from fno.paths_testing import use_tmpdir


@pytest.fixture
def env(tmp_path, monkeypatch):
    use_tmpdir(monkeypatch, tmp_path)
    monkeypatch.setenv("FNO_INBOX_ROOT", str(tmp_path / "agents"))
    return tmp_path


def _register(name, *, scope=None, level=None, status="live", session=None):
    from fno.agents.registry import AgentEntry, load_registry, write_registry

    try:
        existing = list(load_registry())
    except Exception:
        existing = []
    existing.append(
        AgentEntry(
            name=name,
            harness="claude",
            cwd="/tmp",
            log_path=f"/tmp/{name}.log",
            short_id=f"id-{name}",
            status=status,
            harness_session_id=session or f"session-{name}",
            role_level=level,
            role_scope=scope,
        )
    )
    write_registry(existing)


# ---------------------------------------------------------------------------
# Resolver
# ---------------------------------------------------------------------------


def test_a_set_role_answers_to_each_member_epic(env):
    from fno.agents.role import resolve_to_lead

    _register("lead-set", scope="x-119e,x-4d9b", level=2)

    assert resolve_to_lead("x-4d9b") == ["lead-set"]
    assert resolve_to_lead("x-119e") == ["lead-set"]
    assert resolve_to_lead("x-4d9b,x-119e") == ["lead-set"]
    assert resolve_to_lead("x-aaaa") == []
    assert resolve_to_lead("x-4d9b,x-aaaa") == []


def test_a_portfolio_role_does_not_answer_for_one_of_its_projects(env, monkeypatch):
    """Projects go through the real resolver, never a stub."""
    from fno.agents.role import resolve_to_lead
    from fno.projects import resolve as proj_resolve

    cfg = env / "config.toml"
    cfg.write_text(
        '[work.workspaces.ws1]\n'
        'projects = [{ name = "alpha" }, { name = "beta" }]\n',
        encoding="utf-8",
    )
    monkeypatch.setattr(proj_resolve, "SETTINGS_PATH", cfg)
    proj_resolve._clear_cache()
    try:
        _register("lead-portfolio", scope="alpha,beta", level=0)
        _register("lead-alpha", scope="alpha", level=1)

        assert resolve_to_lead("alpha") == ["lead-alpha"]
        assert resolve_to_lead("alpha,beta") == ["lead-portfolio"]
    finally:
        proj_resolve._clear_cache()


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------


def test_cli_to_lead_refuses_when_the_role_is_vacant(env):
    from fno.mail.cli import mail_app

    _register("former-lead")
    res = CliRunner().invoke(mail_app, ["send", "--to-lead", "fno", "ping"])

    assert res.exit_code == 16
    assert "no live row holds this role" in res.output


def test_cli_to_lead_refuses_a_split_role_naming_both(env):
    from fno.mail.cli import mail_app

    _register("lead-a", scope="fno", level=1)
    _register("lead-b", scope="fno", level=1)
    res = CliRunner().invoke(mail_app, ["send", "--to-lead", "fno", "ping"])

    assert res.exit_code == 17
    assert "lead-a" in res.output and "lead-b" in res.output


def test_cli_to_lead_refuses_a_second_addressing_mode(env):
    from fno.mail.cli import mail_app

    res = CliRunner().invoke(
        mail_app, ["send", "--to-lead", "fno", "--to-project", "p", "ping"]
    )
    assert res.exit_code == 2
    assert "mutually exclusive" in res.output


def test_cli_to_lead_hands_the_holder_to_the_name_lane(env):
    """AC1: the resolved holder, not a handle a peer remembered, is what the
    ordinary name lane is asked to deliver to.

    The send body lives in the rust leaf, so the handoff is pinned through its
    stdout receipt: the `to` field names the resolved holder, and the test
    host's only deliverable outcome is the durable demotion.
    """
    import json as _json

    from fno.mail.cli import mail_app

    _register("lead-fno-g5", scope="fno", level=1)
    res = CliRunner().invoke(mail_app, ["send", "--to-lead", "fno", "ping"])

    assert res.exit_code == 14, res.output
    receipt = _json.loads(res.stdout.strip().splitlines()[0])
    assert receipt["to"] == "lead-fno-g5"
    assert "queued (durable)" in receipt["status"]
