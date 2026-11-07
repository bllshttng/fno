"""Shell-level checks for the SessionStart report hook.

Decision D3 revised: every session reports, and the daemon mints a row for a
session no row holds. A session with no launch record is the user's own
(origin operator, an occupant toward the live cap, never refused, managed,
mailed or reaped). The hand-started register branch and its opt-in knob are
gone from this hook; the pane-worker restamp is the one Python leg left.
"""

from __future__ import annotations

import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
HOOK = ROOT / "hooks" / "register-session-start.sh"


def _agents_mock(bin_dir: Path) -> None:
    """A mock `fno-agents` recording the thin report's argv."""
    agents_bin = bin_dir / "fno-agents"
    agents_bin.write_text(
        '#!/usr/bin/env bash\nprintf \'%s\\n\' "$@" > "$AGENTS_CAPTURE"\n',
        encoding="utf-8",
    )
    agents_bin.chmod(0o755)


def _env(tmp_path: Path, extra: dict[str, str]) -> dict[str, str]:
    return {
        "PATH": f"{tmp_path / 'bin'}:/usr/bin:/bin",
        "HOME": str(tmp_path),
        # A hand-built env inherits no conftest pin, so it declares nothing.
        # HOME is already a sandbox here; say so, or the SessionStart chain
        # writes its stranded cache into the real checkout .fno.
        "FNO_TEST_HERMETIC": "1",
        "CLAUDE_PROJECT_DIR": str(tmp_path),
        "FNO_AGENTS_BIN": str(tmp_path / "bin" / "fno-agents"),
        "AGENTS_CAPTURE": str(tmp_path / "agents-argv"),
        **extra,
    }


def test_codex_disagreeing_ids_report_nothing(tmp_path: Path) -> None:
    """The resolvers degrade a same-family id disagreement to unresolved; a row
    minted under the table-first id is one this session can never resolve
    against, so the hook reports nothing instead."""
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    _agents_mock(bin_dir)
    subprocess.run(
        ["bash", str(HOOK)],
        check=True,
        env=_env(
            tmp_path,
            {
                "CODEX_PLUGIN_ROOT": str(ROOT),
                "CODEX_THREAD_ID": "thread-wins",
                "CODEX_SESSION_ID": "legacy-loses",
            },
        ),
    )
    assert not (tmp_path / "agents-argv").exists(), (
        "a disagreed id family must not report a row"
    )


def test_hand_started_session_reports_for_the_daemon_mint(tmp_path: Path) -> None:
    """Decision D3 revised: a hand-started session reports like a worker, with
    no --agent-self (there is no spawned row to correct) and no --origin-only
    (the daemon mints the operator row from the report). The hook runs no
    Python and reads no config: one bounded report call."""
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    _agents_mock(bin_dir)
    uv = bin_dir / "uv"
    uv.write_text(
        "#!/usr/bin/env bash\ntouch \"$UV_WAS_CALLED\"\n", encoding="utf-8"
    )
    uv.chmod(0o755)

    env = _env(
        tmp_path,
        {
            "CLAUDE_PLUGIN_ROOT": str(ROOT),
            "CLAUDE_CODE_SESSION_ID": "0718619e-2527-4bba-9cc0-5e493313240c",
            "UV_WAS_CALLED": str(tmp_path / "uv-was-called"),
        },
    )
    env["UV_CAPTURE"] = str(tmp_path / "uv-argv")
    subprocess.run(["bash", str(HOOK)], check=True, env=env)

    argv = (tmp_path / "agents-argv").read_text(encoding="utf-8").splitlines()
    assert argv[:2] == ["report", "--kind"]
    assert argv[2] == "session"
    assert "--origin-only" not in argv
    assert "--agent-self" not in argv
    assert argv[argv.index("--harness") + 1] == "claude"
    assert (
        argv[argv.index("--session-id") + 1] == "0718619e-2527-4bba-9cc0-5e493313240c"
    )
    assert not (tmp_path / "uv-was-called").exists(), "no Python leg on this lane"
