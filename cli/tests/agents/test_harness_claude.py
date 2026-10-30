"""Tests for fno.agents.harnesses.claude — TDD Red phase for Task 1.2.

ACs (US1):
- AC1-HP: bg_create returns ProviderResult with parsed short_id from stdout
- Locked Decision 6: regex ``^backgrounded · ([0-9a-f]{8}) · `` extracts id
- AC1-FR parse failure: ProviderParseError carries first 200 chars of stdout
- AC1-FR subprocess non-zero: ProviderSubprocessError preserves verbatim stderr
- AC1-EDGE argv overflow: ``len_argv > 200KB`` routes via subprocess stdin
"""
from __future__ import annotations

from pathlib import Path
from unittest.mock import MagicMock




# ---------------------------------------------------------------------------
# bg_create — happy path with fake claude on PATH
# ---------------------------------------------------------------------------


# ---------------------------------------------------------------------------
# bg_create — argv overflow (AC1-EDGE 300KB → stdin pipe)
# ---------------------------------------------------------------------------


# ---------------------------------------------------------------------------
# bg_create and headless_create: argv and env via a stubbed subprocess
# ---------------------------------------------------------------------------


def test_headless_create_strips_incoherent_inherited_model_env(
    tmp_path: Path, monkeypatch, capsys
) -> None:
    """A daemon-carried foreign model var with no base URL is stripped from the
    child env with one stderr line naming it; the child falls back to the
    account's own default instead of dying on turn one."""
    from fno.agents.harnesses import claude as claude_mod

    monkeypatch.delenv("ANTHROPIC_BASE_URL", raising=False)
    monkeypatch.setenv("ANTHROPIC_DEFAULT_HAIKU_MODEL", "glm-4.5-air")
    captured: dict[str, object] = {}

    def fake_run(argv, **kwargs):  # type: ignore[no-untyped-def]
        captured["env"] = kwargs.get("env")
        captured["stderr_at_call"] = capsys.readouterr().err
        result = MagicMock()
        result.returncode = 0
        result.stdout = "ok"
        result.stderr = ""
        return result

    monkeypatch.setattr(claude_mod, "_subprocess_run", fake_run)
    cwd = tmp_path / "wd"
    cwd.mkdir()
    claude_mod.headless_create(message="hi", cwd=cwd)

    env = captured["env"]
    assert env is not None, "incoherent env must force an explicit child env"
    assert "ANTHROPIC_DEFAULT_HAIKU_MODEL" not in env
    assert "ANTHROPIC_DEFAULT_HAIKU_MODEL" in captured["stderr_at_call"]


def test_headless_create_scrubs_inherited_auth(tmp_path: Path, monkeypatch) -> None:
    """x-d012: an --account spawn scrubs inherited ANTHROPIC_API_KEY /
    CLAUDE_CODE_OAUTH_TOKEN so an ambient token can't override the account."""
    from fno.agents.harnesses import claude as claude_mod

    monkeypatch.setenv("ANTHROPIC_API_KEY", "sk-parent")
    monkeypatch.setenv("CLAUDE_CODE_OAUTH_TOKEN", "oauth-parent")
    captured: dict[str, object] = {}

    def fake_run(argv, **kwargs):  # type: ignore[no-untyped-def]
        captured["env"] = kwargs.get("env")
        result = MagicMock()
        result.returncode = 0
        result.stdout = "ok"
        result.stderr = ""
        return result

    monkeypatch.setattr(claude_mod, "_subprocess_run", fake_run)
    cwd = tmp_path / "wd"
    cwd.mkdir()
    claude_mod.headless_create(
        message="hi", cwd=cwd, account_env={"CLAUDE_CONFIG_DIR": "/x/.claude-alt"}
    )
    env = captured["env"]
    assert env["CLAUDE_CONFIG_DIR"] == "/x/.claude-alt"
    assert "ANTHROPIC_API_KEY" not in env
    assert "CLAUDE_CODE_OAUTH_TOKEN" not in env


def test_headless_receipt_emitted_before_blocking_subprocess(
    tmp_path: Path, monkeypatch, capsys
) -> None:
    """The headless spawn receipt is flushed to stderr BEFORE the synchronous
    blocking claude -p runs, and names the passed worktree cwd + transcript
    locator. Without an up-front receipt a long one-shot looks dead to a
    watcher (the twin-spawn failure mode this closes)."""
    import json

    from fno.agents.harnesses import claude as claude_mod
    from fno.provenance.resolver import _slug

    captured: dict[str, object] = {}

    def fake_run(argv, **kwargs):  # type: ignore[no-untyped-def]
        # Snapshot stderr AT the moment the blocking subprocess is invoked:
        # the receipt must already be flushed here, not written after.
        captured["stderr_at_call"] = capsys.readouterr().err
        result = MagicMock()
        result.returncode = 0
        result.stdout = "ok"
        result.stderr = ""
        return result

    monkeypatch.setattr(claude_mod, "_subprocess_run", fake_run)
    cwd = tmp_path / "wd"
    cwd.mkdir()
    claude_mod.headless_create(message="hi", cwd=cwd, name="wk-zzz")

    snap = captured.get("stderr_at_call")
    assert snap, "no receipt was flushed before the blocking subprocess call"
    rec = json.loads(snap.strip().splitlines()[-1])
    assert rec["substrate"] == "headless"
    assert rec["name"] == "wk-zzz"
    assert rec["cwd"] == str(cwd)
    assert rec["lifecycle"] == "ephemeral"
    assert rec["roster"] == "unbound"
    # transcript locator slug derived from the effective cwd (canonical encoding)
    assert _slug(str(cwd)) in rec["transcript_dir"]
    # promises nothing that does not exist yet at this synchronous boundary
    assert "session_id" not in rec
    assert "pid" not in rec


def test_headless_receipt_goes_to_stderr_not_stdout(
    tmp_path: Path, monkeypatch, capsys
) -> None:
    """The receipt goes to stderr so stdout stays the clean reply stream. A
    structured consumer (pr_watch under --output-format json) does json.loads on
    the full headless stdout; a receipt line there would corrupt the envelope."""
    import json

    from fno.agents.harnesses import claude as claude_mod

    envelope = '{"is_error":false,"result":"ok"}'

    def fake_run(argv, **kwargs):  # type: ignore[no-untyped-def]
        result = MagicMock()
        result.returncode = 0
        result.stdout = envelope
        result.stderr = ""
        return result

    monkeypatch.setattr(claude_mod, "_subprocess_run", fake_run)
    cwd = tmp_path / "wd"
    cwd.mkdir()
    res = claude_mod.headless_create(
        message="hi", cwd=cwd, name="wk", output_format="json"
    )
    captured = capsys.readouterr()
    # the reply returned to the caller is the clean envelope, unmodified
    assert json.loads(res.stdout) == json.loads(envelope)
    # the receipt landed on stderr, never on stdout
    rec = json.loads(captured.err.strip().splitlines()[-1])
    assert rec["substrate"] == "headless"
    assert "headless" not in captured.out


def test_headless_receipt_transcript_dir_follows_account_config_dir(
    tmp_path: Path, monkeypatch, capsys
) -> None:
    """An --account spawn relocates claude's config tree via CLAUDE_CONFIG_DIR;
    the transcript locator must point under that root, not ~/.claude/projects
    where the transcript would never appear (which would recreate the
    false-dead failure this receipt exists to prevent)."""
    import json

    from fno.agents.harnesses import claude as claude_mod

    captured: dict[str, object] = {}

    def fake_run(argv, **kwargs):  # type: ignore[no-untyped-def]
        captured["stderr_at_call"] = capsys.readouterr().err
        result = MagicMock()
        result.returncode = 0
        result.stdout = "ok"
        result.stderr = ""
        return result

    monkeypatch.setattr(claude_mod, "_subprocess_run", fake_run)
    cwd = tmp_path / "wd"
    cwd.mkdir()
    claude_mod.headless_create(
        message="hi",
        cwd=cwd,
        name="wk",
        account_env={"CLAUDE_CONFIG_DIR": "/x/.claude-alt"},
    )

    rec = json.loads(captured["stderr_at_call"].strip().splitlines()[-1])
    assert rec["transcript_dir"].startswith("/x/.claude-alt/projects/")

