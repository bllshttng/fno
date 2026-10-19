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

import pytest

from tests.agents._fake_claude import configure_fake, install_fake_claude


# ---------------------------------------------------------------------------
# bg_create — happy path with fake claude on PATH
# ---------------------------------------------------------------------------


def test_bg_create_happy_path(tmp_path: Path, monkeypatch) -> None:
    """bg_create invokes claude --bg, parses short_id, returns ProviderResult."""
    from fno.agents.harnesses.base import ProviderResult
    from fno.agents.harnesses.claude import bg_create

    bin_dir = tmp_path / "bin"
    install_fake_claude(bin_dir)
    monkeypatch.setenv("PATH", str(bin_dir))
    configure_fake(monkeypatch)

    cwd = tmp_path / "workdir"
    cwd.mkdir()

    result = bg_create(
        name="frontend-worker",
        message="implement Login.tsx",
        cwd=cwd,
        timeout=10,
    )

    assert isinstance(result, ProviderResult)
    assert result.exit_code == 0
    assert result.session_id_out == "7c5dcf5d"
    assert "backgrounded" in result.stdout
    assert result.duration_ms >= 0


def test_bg_create_subprocess_non_zero(tmp_path: Path, monkeypatch) -> None:
    """Non-zero subprocess exit raises ProviderSubprocessError with verbatim stderr."""
    from fno.agents.harnesses.claude import (
        ProviderSubprocessError,
        bg_create,
    )

    bin_dir = tmp_path / "bin"
    install_fake_claude(bin_dir)
    monkeypatch.setenv("PATH", str(bin_dir))
    configure_fake(
        monkeypatch,
        exit_code=1,
        stderr="Error: not authenticated. Run claude /login\n",
    )

    cwd = tmp_path / "workdir"
    cwd.mkdir()

    with pytest.raises(ProviderSubprocessError) as exc_info:
        bg_create(name="x", message="hi", cwd=cwd, timeout=10)

    err = exc_info.value
    assert err.exit_code == 1
    assert "not authenticated" in err.stderr


def test_bg_create_parse_failure(tmp_path: Path, monkeypatch) -> None:
    """Subprocess succeeds but unparseable stdout raises ProviderParseError."""
    from fno.agents.harnesses.claude import ProviderParseError, bg_create

    bin_dir = tmp_path / "bin"
    install_fake_claude(bin_dir)
    monkeypatch.setenv("PATH", str(bin_dir))
    configure_fake(monkeypatch, stdout="Session created: foo-bar\n")

    cwd = tmp_path / "workdir"
    cwd.mkdir()

    with pytest.raises(ProviderParseError) as exc_info:
        bg_create(name="x", message="hi", cwd=cwd, timeout=10)

    assert "Session created: foo-bar" in exc_info.value.stdout_head


# ---------------------------------------------------------------------------
# bg_create — argv overflow (AC1-EDGE 300KB → stdin pipe)
# ---------------------------------------------------------------------------


def test_bg_create_argv_overflow_routes_via_stdin(
    tmp_path: Path, monkeypatch
) -> None:
    """Messages above 200KB are piped via subprocess.run(input=msg)."""
    from fno.agents.harnesses.claude import bg_create

    bin_dir = tmp_path / "bin"
    install_fake_claude(bin_dir)
    monkeypatch.setenv("PATH", str(bin_dir))

    stdin_dump = tmp_path / "stdin.bin"
    configure_fake(monkeypatch, stdin_dump=str(stdin_dump))

    cwd = tmp_path / "workdir"
    cwd.mkdir()

    # 300KB message — clearly above the 200KB safety threshold.
    big_msg = "X" * (300 * 1024)
    result = bg_create(
        name="big",
        message=big_msg,
        cwd=cwd,
        timeout=15,
    )

    assert result.exit_code == 0
    assert result.session_id_out == "7c5dcf5d"
    # The fake captured stdin to disk — verify the full 300KB arrived.
    received = stdin_dump.read_text(encoding="utf-8")
    assert len(received) == len(big_msg)
    assert received == big_msg


# ---------------------------------------------------------------------------
# bg_create and headless_create: argv and env via a stubbed subprocess
# ---------------------------------------------------------------------------


def test_bg_create_uses_rust_birth_exec_fence(tmp_path: Path, monkeypatch) -> None:
    from fno import rust_binary
    from fno.agents.harnesses import claude as claude_mod

    monkeypatch.setattr(rust_binary, "resolve_binary", lambda: None)
    captured: dict[str, object] = {}

    def fake_run(argv, **kwargs):  # type: ignore[no-untyped-def]
        captured["argv"] = argv
        result = MagicMock()
        result.returncode = 0
        result.stdout = "backgrounded · 7c5dcf5d · demo\n"
        result.stderr = ""
        return result

    monkeypatch.setattr(claude_mod, "_subprocess_run", fake_run)
    claude_mod.bg_create(name="demo", message="hi", cwd=tmp_path, timeout=5)

    assert captured["argv"][:3] == ["fno-agents", "claude-birth-exec", "--"]
    assert captured["argv"][3] == "claude"


def test_bg_create_stale_rust_binary_suggests_update(tmp_path: Path, monkeypatch) -> None:
    from fno.agents.harnesses import claude as claude_mod

    result = MagicMock(returncode=2, stdout="", stderr="unknown verb: claude-birth-exec")
    monkeypatch.setattr(claude_mod, "_subprocess_run", lambda *args, **kwargs: result)

    with pytest.raises(claude_mod.ProviderSubprocessError) as exc:
        claude_mod.bg_create(name="demo", message="hi", cwd=tmp_path, timeout=5)

    assert exc.value.exit_code == 2
    assert "fno doctor update" in exc.value.stderr


def test_bg_create_floors_incoherent_model_env_via_settings_file(
    tmp_path: Path, monkeypatch
) -> None:
    """A `claude --bg` session's serving process is forked by the claude
    daemon with the DAEMON's own env (x-6de8), so scrubbing this process's
    spawn_env alone is decorative on that lane. Without a route/account
    settings file the poisoned-inherited-env fix must still reach the real
    worker, so bg_create floats a --settings file flooring just the
    offending model vars."""
    from fno.paths_testing import use_tmpdir

    use_tmpdir(monkeypatch, tmp_path)
    from fno.agents.harnesses import claude as claude_mod

    # Pin every model var: the exact-scope assertion below must not see tier
    # vars leaked in by the ambient (possibly routed or poisoned) test shell.
    monkeypatch.delenv("ANTHROPIC_BASE_URL", raising=False)
    monkeypatch.delenv("ANTHROPIC_MODEL", raising=False)
    for alias in ("OPUS", "SONNET", "HAIKU", "FABLE"):
        monkeypatch.delenv(f"ANTHROPIC_DEFAULT_{alias}_MODEL", raising=False)
    monkeypatch.setenv("ANTHROPIC_DEFAULT_HAIKU_MODEL", "glm-4.5-air")
    captured: dict[str, object] = {}

    def fake_run(argv, **kwargs):  # type: ignore[no-untyped-def]
        captured["argv"] = argv
        result = MagicMock()
        result.returncode = 0
        result.stdout = "backgrounded · 7c5dcf5d · demo\n"
        result.stderr = ""
        return result

    monkeypatch.setattr(claude_mod, "_subprocess_run", fake_run)
    cwd = tmp_path / "workdir"
    cwd.mkdir()
    claude_mod.bg_create(name="demo", message="hi", cwd=cwd, timeout=5)

    argv = captured["argv"]
    assert "--settings" in argv, "poisoned model env must float a settings file"
    settings_path = Path(argv[argv.index("--settings") + 1])
    import json

    payload = json.loads(settings_path.read_text())
    assert payload["env"]["ANTHROPIC_DEFAULT_HAIKU_MODEL"] == ""
    # Only the offending var is floored, not the whole SCRUB_AUTH_VARS set -
    # a coherent ANTHROPIC_API_KEY (unset here, but the contract is scope) is
    # not implicated by a poisoned model tier alone.
    assert set(payload["env"]) == {"ANTHROPIC_DEFAULT_HAIKU_MODEL"}


def test_bg_create_sets_stop_hook_block_cap(tmp_path: Path, monkeypatch) -> None:
    """bg spawn_env carries the raised Stop-hook block cap, honoring override (x-1680)."""
    from fno.agents.harnesses import claude as claude_mod

    captured: dict[str, object] = {}

    def fake_run(argv, **kwargs):  # type: ignore[no-untyped-def]
        captured["env"] = kwargs.get("env")
        result = MagicMock()
        result.returncode = 0
        result.stdout = "backgrounded · 7c5dcf5d · demo\n"
        result.stderr = ""
        return result

    monkeypatch.setattr(claude_mod, "_subprocess_run", fake_run)
    monkeypatch.delenv("CLAUDE_CODE_STOP_HOOK_BLOCK_CAP", raising=False)

    cwd = tmp_path / "workdir"
    cwd.mkdir()
    claude_mod.bg_create(name="demo", message="hi", cwd=cwd, timeout=5)
    assert captured["env"]["CLAUDE_CODE_STOP_HOOK_BLOCK_CAP"] == "50"

    # An explicit operator value wins over the fno default.
    monkeypatch.setenv("CLAUDE_CODE_STOP_HOOK_BLOCK_CAP", "99")
    claude_mod.bg_create(name="demo2", message="hi", cwd=cwd, timeout=5)
    assert captured["env"]["CLAUDE_CODE_STOP_HOOK_BLOCK_CAP"] == "99"


def test_build_argv_resume_session() -> None:
    """US4 bg-thread revival: a resume_session_id inserts ``--resume <uuid>`` so a
    replacement bg supervisor continues the dead session's transcript under the
    new account's env. Unset is byte-identical to today. This flag is spawn-only
    (the Rust ask-hop build_argv never resumes), so cross-runtime parity is scoped
    to the model/permission/effort flags, not this one."""
    from fno.agents.harnesses.claude import _build_argv

    argv = _build_argv("a", "hi", False, resume_session_id="U-123")
    assert argv == [
        "claude", "--bg", "--name", "a", "--resume", "U-123", "--", "hi",
    ]
    assert "-p" not in argv, "a hosted bg revival stays interactive"
    # stdin path (large message): message omitted, resume still present.
    assert _build_argv("a", "hi", True, resume_session_id="U-123") == [
        "claude", "--bg", "--name", "a", "--resume", "U-123",
    ]
    # Unset/empty == today (byte-identical, no flag).
    assert _build_argv("a", "hi", False, resume_session_id=None) == [
        "claude", "--bg", "--name", "a", "--", "hi",
    ]
    assert _build_argv("a", "hi", False, resume_session_id="") == _build_argv(
        "a", "hi", False, resume_session_id=None
    )
    # Composes with a model pin (resume after model, before message).
    assert _build_argv("a", "hi", False, "fable", resume_session_id="U-9") == [
        "claude", "--bg", "--name", "a", "--model", "fable", "--resume", "U-9", "--", "hi",
    ]


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


def test_headless_create_routed_scrubs_ambient_creds(tmp_path: Path, monkeypatch) -> None:
    """x-6de8 codex P1: a routed headless spawn must scrub ambient
    ANTHROPIC_API_KEY / CLAUDE_CODE_OAUTH_TOKEN (claude prefers an env credential
    over the --settings file), else it authenticates with the primary account
    instead of the routed provider. Mirrors bg_create's scrub."""
    from fno.agents.harnesses import claude as claude_mod

    monkeypatch.setenv("ANTHROPIC_API_KEY", "sk-parent")
    monkeypatch.setenv("CLAUDE_CODE_OAUTH_TOKEN", "oauth-parent")
    captured: dict[str, object] = {}

    def fake_run(argv, **kwargs):  # type: ignore[no-untyped-def]
        captured["env"] = kwargs.get("env")
        captured["argv"] = argv
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
        route_env={
            "ANTHROPIC_BASE_URL": "https://api.z.ai/api/anthropic",
            "ANTHROPIC_AUTH_TOKEN": "zk-routed",
            # Every tier: headless_create crosses resolve_spawn_route, which
            # refuses a foreign endpoint without the full model set.
            "ANTHROPIC_MODEL": "glm-5.2",
            "ANTHROPIC_DEFAULT_OPUS_MODEL": "glm-5.2",
            "ANTHROPIC_DEFAULT_SONNET_MODEL": "glm-5.2",
            "ANTHROPIC_DEFAULT_HAIKU_MODEL": "glm-4.7",
            "ANTHROPIC_DEFAULT_FABLE_MODEL": "glm-5.2",
        },
    )
    env = captured["env"]
    assert env is not None, "route_env must build a scrubbed spawn env"
    assert "ANTHROPIC_API_KEY" not in env
    assert "CLAUDE_CODE_OAUTH_TOKEN" not in env
    assert env["ANTHROPIC_AUTH_TOKEN"] == "zk-routed"
    assert env["ANTHROPIC_BASE_URL"] == "https://api.z.ai/api/anthropic"
    assert "--settings" in captured["argv"]


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

