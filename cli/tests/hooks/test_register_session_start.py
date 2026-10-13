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
SHARED_HOOK = ROOT / "hooks" / "session-start.sh"


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


def _run_hook(hook: Path, env: dict[str, str], stdin: str = "") -> None:
    subprocess.run(
        ["bash", str(hook)],
        check=True,
        cwd=env["CLAUDE_PROJECT_DIR"],
        env=env,
        input=stdin,
        text=True,
        stdout=subprocess.DEVNULL,
    )


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


def test_codex_same_value_reports_the_thread_once(tmp_path: Path) -> None:
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
                "CODEX_THREAD_ID": "same-id",
                "CODEX_SESSION_ID": "same-id",
            },
        ),
    )
    argv = (tmp_path / "agents-argv").read_text(encoding="utf-8").splitlines()
    assert argv[:2] == ["report", "--kind"]
    assert argv[2] == "session"
    assert "--origin-only" not in argv
    assert argv[argv.index("--harness") + 1] == "codex"
    assert argv[argv.index("--session-id") + 1] == "same-id"


def test_shared_codex_session_start_reports_the_thread_once(tmp_path: Path) -> None:
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    _agents_mock(bin_dir)
    _run_hook(
        SHARED_HOOK,
        _env(
            tmp_path,
            {
                "FNO_PLATFORM": "codex",
                "CODEX_THREAD_ID": "shared-thread",
            },
        ),
        stdin="{}",
    )
    argv = (tmp_path / "agents-argv").read_text(encoding="utf-8").splitlines()
    assert argv.count("--kind") == 1
    assert argv[argv.index("--harness") + 1] == "codex"
    assert argv[argv.index("--session-id") + 1] == "shared-thread"


def test_shared_session_start_does_not_duplicate_claude_registration(
    tmp_path: Path,
) -> None:
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    capture = tmp_path / "uv-argv"
    # The observer's uv call is gone, so the capture may stay empty; it is
    # pre-touched so the absence asserts below read a file, not a missing one.
    capture.touch()
    uv = bin_dir / "uv"
    uv.write_text(
        "#!/usr/bin/env bash\nprintf '%s\\n' \"$@\" >> \"$UV_CAPTURE\"\n",
        encoding="utf-8",
    )
    uv.chmod(0o755)
    env = {
        "PATH": f"{bin_dir}:/usr/bin:/bin",
        "HOME": str(tmp_path),
        # A hand-built env inherits no conftest pin, so it declares nothing.
        # HOME is already a sandbox here; say so, or the SessionStart chain
        # writes its stranded cache into the real checkout .fno.
        "FNO_TEST_HERMETIC": "1",
        "FNO_PLATFORM": "claude",
        "CLAUDE_PROJECT_DIR": str(tmp_path),
        "CLAUDE_PLUGIN_ROOT": str(ROOT),
        "CLAUDE_SESSION_ID": "claude-direct-hook-owns-reporting",
        "UV_CAPTURE": str(capture),
    }

    _run_hook(SHARED_HOOK, env, stdin="{}")

    argv = capture.read_text(encoding="utf-8").splitlines()
    assert not any(item.endswith("context_observation.py") for item in argv)
    assert "agents" not in argv
    assert "register" not in argv


def test_spawned_worker_restamps_without_consulting_any_knob(tmp_path: Path) -> None:
    """A footnote-spawned worker (FNO_AGENT_SELF) takes the restamp path.

    The daemon's session-report ingest holds the reported id on every lane;
    the bounded Python restamp supplements it on the pane lane only
    (FNO_AGENT_ROW_PENDING), the path that heals the row's mux ref and opens
    the parked pending graph row. `--agent-self` must reach both the thin
    report and the entry point (registration keys on the re-mintable session
    id and would append a second row instead of correcting the first). No
    opt-in knob exists anymore: the hook consults no config read.
    """
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    capture = tmp_path / "uv-argv"
    agents_capture = tmp_path / "agents-argv"
    knob_read = tmp_path / "knob-read"
    uv = bin_dir / "uv"
    uv.write_text(
        "#!/usr/bin/env bash\nprintf '%s\\n' \"$@\" > \"$UV_CAPTURE\"\n", encoding="utf-8"
    )
    uv.chmod(0o755)
    _agents_mock(bin_dir)

    env = _env(
        tmp_path,
        {
            "CLAUDE_PLUGIN_ROOT": str(ROOT),
            "CLAUDE_CODE_SESSION_ID": "08054b1d-a907-47ab-a3d2-4a1e7a87eb4e",
            "FNO_AGENT_SELF": "target-x-f0c2",
            # Pane substrate: the only worker lane that still runs the Python
            # restamp beside the thin report.
            "FNO_AGENT_ROW_PENDING": "target-x-f0c2",
            "UV_CAPTURE": str(capture),
            "KNOB_READ": str(knob_read),
        },
    )
    subprocess.run(["bash", str(HOOK)], check=True, env=env)

    report = agents_capture.read_text(encoding="utf-8").splitlines()
    assert report[report.index("--agent-self") + 1] == "target-x-f0c2"
    argv = capture.read_text(encoding="utf-8").splitlines()
    assert argv[argv.index("--agent-self") + 1] == "target-x-f0c2"
    assert argv[argv.index("--harness") + 1] == "claude"
    assert (
        argv[argv.index("--session-id") + 1] == "08054b1d-a907-47ab-a3d2-4a1e7a87eb4e"
    )
    assert not knob_read.exists(), "no opt-in knob exists for any lane anymore"


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
