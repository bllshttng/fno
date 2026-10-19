"""Tests for fno.agents.context — EventContext + build_context + caller_kind_from_env.

Task 1.1 from spec 2026-05-22-fno-agents-observability.md.

Locks in:
- request_id format invariant: 32 lowercase hex chars (UUIDv4, no dashes)
- caller_kind_from_env decision tree (env-only resolution; target-state wiring is Task 2.3)
- parse_target_session graceful degradation for stale/corrupt/missing files
"""
from __future__ import annotations

import re
from pathlib import Path

import pytest


# ---------------------------------------------------------------------------
# caller_kind_from_env — env-only decision tree (Task 1.1 scope)
# ---------------------------------------------------------------------------


def _clear_caller_env(monkeypatch) -> None:
    """Strip every env var the caller-kind tree reads."""
    for k in (
        "FNO_SPAWN_TRIGGER",
        "FNO_AGENT_SELF",
        "FNO_AGENT_HARNESS",
        "FNO_AGENT_SESSION",
        "MCP_CHANNEL_INBOUND_POKE",
        "CRON_JOB",
        "INVOCATION_ID",
    ):
        monkeypatch.delenv(k, raising=False)


def test_caller_kind_from_env_cron_job(monkeypatch) -> None:
    """CRON_JOB env => cron (priority 4)."""
    from fno.agents.context import caller_kind_from_env

    _clear_caller_env(monkeypatch)
    monkeypatch.setenv("CRON_JOB", "1")
    assert caller_kind_from_env() == "cron"


def test_caller_kind_priority_nested_beats_mcp(monkeypatch) -> None:
    """Nested-agent wins over MCP channel when both env vars are set."""
    from fno.agents.context import caller_kind_from_env

    _clear_caller_env(monkeypatch)
    monkeypatch.setenv("FNO_AGENT_SELF", "parent")
    monkeypatch.setenv("MCP_CHANNEL_INBOUND_POKE", "1")
    assert caller_kind_from_env() == "nested_agent"


def test_caller_kind_priority_mcp_beats_cron(monkeypatch) -> None:
    """MCP channel wins over cron when both env signals are set."""
    from fno.agents.context import caller_kind_from_env

    _clear_caller_env(monkeypatch)
    monkeypatch.setenv("MCP_CHANNEL_INBOUND_POKE", "1")
    monkeypatch.setenv("CRON_JOB", "1")
    assert caller_kind_from_env() == "mcp_channel"


# ---------------------------------------------------------------------------
# dispatcher: a machine dispatch no session asked for
# ---------------------------------------------------------------------------


def test_caller_kind_dispatcher_outranks_nested_agent(monkeypatch) -> None:
    """A worker merging its own PR runs the same ritual; the dispatcher chose
    the node, not the worker, so the trigger outranks FNO_AGENT_SELF."""
    from fno.agents.context import caller_kind_from_env

    _clear_caller_env(monkeypatch)
    monkeypatch.setenv("FNO_SPAWN_TRIGGER", "dispatch:ac")
    monkeypatch.setenv("FNO_AGENT_SELF", "worker-1")
    assert caller_kind_from_env() == "dispatcher"


def test_build_context_session_marker_alone_stays_human_cli(
    monkeypatch, tmp_path: Path
) -> None:
    """A harness session marker with no trigger is a session that asked:
    caller_kind stays human_cli."""
    from fno.agents.context import build_context

    _clear_caller_env(monkeypatch)
    monkeypatch.setenv("CLAUDE_CODE_SESSION_ID", "parent-session-1")
    monkeypatch.chdir(tmp_path)
    ctx = build_context(to_name="w", to_provider="claude")
    assert ctx.caller_kind == "human_cli"
    assert ctx.from_name == "fno"


# ---------------------------------------------------------------------------
# parse_target_session — stale / corrupt / missing degradation (AC5-ERR, AC5-EDGE)
# ---------------------------------------------------------------------------


def test_parse_target_session_returns_none_on_corrupt_yaml(
    tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    """AC5-EDGE: malformed frontmatter degrades to None + stderr WARN."""
    from fno.agents.context import parse_target_session

    (tmp_path / ".fno").mkdir()
    (tmp_path / ".fno" / "target-state.md").write_text(
        # Well-formed frontmatter brackets but the inner YAML is broken
        # — unbalanced bracket inside the value triggers yaml.YAMLError.
        "---\nstatus: IN_PROGRESS\nbroken: [unclosed list\n---\nbody\n"
    )
    result = parse_target_session(tmp_path)
    assert result is None
    err = capsys.readouterr().err
    assert "target-state" in err.lower() or "warn" in err.lower(), (
        f"expected stderr WARN on corrupt yaml; got: {err!r}"
    )


# ---------------------------------------------------------------------------
# build_context — factory wiring (Task 1.1: env-only path; target wiring in 2.3)
# ---------------------------------------------------------------------------


REQUEST_ID_RE = re.compile(r"^[a-f0-9]{32}$")


def test_build_context_request_id_matches_invariant(monkeypatch) -> None:
    """AC4-INVARIANT: request_id is 32 lowercase hex chars (UUIDv4, no dashes)."""
    from fno.agents.context import build_context

    _clear_caller_env(monkeypatch)
    ctx = build_context(to_name="r", to_provider="codex")
    assert REQUEST_ID_RE.match(ctx.request_id), (
        f"request_id {ctx.request_id!r} fails [a-f0-9]{{32}}"
    )


def test_build_context_nested_agent_from_env(monkeypatch) -> None:
    """FNO_AGENT_SELF/PROVIDER/SESSION populate from_* fields."""
    from fno.agents.context import build_context

    _clear_caller_env(monkeypatch)
    monkeypatch.setenv("FNO_AGENT_SELF", "parent-agent")
    monkeypatch.setenv("FNO_AGENT_HARNESS", "claude")
    monkeypatch.setenv("FNO_AGENT_SESSION", "session-xyz")
    ctx = build_context(to_name="child", to_provider="codex")
    assert ctx.caller_kind == "nested_agent"
    assert ctx.from_name == "parent-agent"
    assert ctx.from_provider == "claude"
    assert ctx.from_session_id == "session-xyz"


def test_build_context_dispatcher_from_env(monkeypatch, tmp_path: Path) -> None:
    """A dispatch:<source> trigger stamps caller_kind=dispatcher and the
    trigger value as from_name, with no session on the ask event."""
    from fno.agents.context import build_context

    _clear_caller_env(monkeypatch)
    monkeypatch.setenv("FNO_SPAWN_TRIGGER", "dispatch:ac")
    monkeypatch.chdir(tmp_path)
    ctx = build_context(to_name="w", to_provider="claude")
    assert ctx.caller_kind == "dispatcher"
    assert ctx.from_name == "dispatch:ac"
    assert ctx.from_provider is None
    assert ctx.from_session_id is None
    assert ctx.target_session_id is None


def test_build_context_from_name_override_in_human_cli(monkeypatch) -> None:
    """human_cli with --from-name flag stamps from_name = override value."""
    from fno.agents.context import build_context

    _clear_caller_env(monkeypatch)
    ctx = build_context(
        to_name="r", to_provider="codex", from_name_override="explicit-sender"
    )
    assert ctx.caller_kind == "human_cli"
    assert ctx.from_name == "explicit-sender"


def test_build_context_from_name_override_ignored_when_nested(monkeypatch) -> None:
    """Nested-agent identity (env) outranks human override."""
    from fno.agents.context import build_context

    _clear_caller_env(monkeypatch)
    monkeypatch.setenv("FNO_AGENT_SELF", "parent")
    ctx = build_context(
        to_name="r", to_provider="codex", from_name_override="ignored"
    )
    assert ctx.from_name == "parent"


# ---------------------------------------------------------------------------
# Task 2.3 — target-state.md wired into build_context (AC5-HP / AC5-FR /
# AC5-ERR-as-build-context / priority tree placement)
# ---------------------------------------------------------------------------


def _seed_target_state(cwd: Path, status: str = "IN_PROGRESS", sid: str = "rsid-abc") -> None:
    (cwd / ".fno").mkdir(exist_ok=True)
    (cwd / ".fno" / "target-state.md").write_text(
        f"---\nstatus: {status}\nsession_id: {sid}\n---\nbody\n"
    )


def test_build_context_target_session_when_live(
    monkeypatch, tmp_path: Path
) -> None:
    """AC5-HP: status=IN_PROGRESS stamps caller_kind=target_session + target_session_id."""
    from fno.agents.context import build_context

    _clear_caller_env(monkeypatch)
    _seed_target_state(tmp_path, status="IN_PROGRESS", sid="target-live-1")
    monkeypatch.chdir(tmp_path)

    ctx = build_context(to_name="r", to_provider="codex")
    assert ctx.caller_kind == "target_session"
    assert ctx.target_session_id == "target-live-1"
    assert ctx.from_name == "target"
    assert ctx.from_session_id == "target-live-1"


def test_build_context_stale_target_state_falls_back_to_human_cli(
    monkeypatch, tmp_path: Path
) -> None:
    """AC5-ERR via build_context: COMPLETE state => caller_kind=human_cli, target_session_id=None."""
    from fno.agents.context import build_context

    _clear_caller_env(monkeypatch)
    _seed_target_state(tmp_path, status="COMPLETE", sid="target-stale")
    monkeypatch.chdir(tmp_path)

    ctx = build_context(to_name="r", to_provider="codex")
    assert ctx.caller_kind == "human_cli"
    assert ctx.target_session_id is None
    assert ctx.from_name == "fno"


def test_build_context_nested_agent_beats_target_state(
    monkeypatch, tmp_path: Path
) -> None:
    """Priority tree #1 wins over #3: nested_agent overrides target_session."""
    from fno.agents.context import build_context

    _clear_caller_env(monkeypatch)
    monkeypatch.setenv("FNO_AGENT_SELF", "parent")
    _seed_target_state(tmp_path, status="IN_PROGRESS", sid="should-be-ignored")
    monkeypatch.chdir(tmp_path)

    ctx = build_context(to_name="r", to_provider="codex")
    assert ctx.caller_kind == "nested_agent"
    assert ctx.from_name == "parent"
    # target_session_id should be None because the env-attribution path
    # short-circuits before target-state is consulted.
    assert ctx.target_session_id is None


def test_build_context_target_state_beats_cron(
    monkeypatch, tmp_path: Path
) -> None:
    """Priority tree #3 wins over #4: target_session promotes over cron."""
    from fno.agents.context import build_context

    _clear_caller_env(monkeypatch)
    monkeypatch.setenv("CRON_JOB", "1")
    _seed_target_state(tmp_path, status="IN_PROGRESS", sid="target-over-cron")
    monkeypatch.chdir(tmp_path)

    ctx = build_context(to_name="r", to_provider="codex")
    assert ctx.caller_kind == "target_session"
    assert ctx.target_session_id == "target-over-cron"


def test_parse_target_session_handles_non_utf8(tmp_path: Path) -> None:
    """sigma-review H5: non-UTF8 bytes in target-state.md degrade silently.

    Pre-fix: read_text(encoding="utf-8") raised UnicodeDecodeError, which
    is NOT a subclass of OSError; the exception escaped parse_target_session
    and tore down every dispatch in that cwd. Post-fix: errors="replace"
    converts bad bytes to U+FFFD; the YAML parser then either ignores
    them (in comments / unused fields) or fails with YAMLError which is
    already handled.
    """
    from fno.agents.context import parse_target_session

    (tmp_path / ".fno").mkdir()
    state = tmp_path / ".fno" / "target-state.md"
    # Latin-1 byte 0x96 in a comment line; not valid UTF-8.
    state.write_bytes(
        b"---\nstatus: IN_PROGRESS\nsession_id: ok-sid\n# bad-byte: \x96 here\n---\n"
    )
    # Must NOT raise; should still extract the session_id (the bad byte
    # is in a comment line, replaced with U+FFFD, ignored by YAML).
    assert parse_target_session(tmp_path) == "ok-sid"
