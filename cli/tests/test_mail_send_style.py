"""The style gate on ``fno agents mail send``.

A subprocess ``fno agents mail send`` with a violating body exits 1 and
names the rule, before any delivery. The escape (``--style-exception``) and
the kill switch (``FNO_STYLE_ENFORCE=0``) let a violating body through.
The structural cap-style pairing guard moved with the gate into the native
leaf (crates/fno-agents mail_cli send::vet_body and its tests).
"""
from __future__ import annotations

import os
import subprocess
import sys
from pathlib import Path

REPO_CLI = Path(__file__).resolve().parents[1]

STYLE_REFUSED = "message blocked by the style rules"


def _run(args: list[str], env_extra: dict[str, str], tmp_path: Path):
    """Run ``fno agents mail send`` in a sandboxed state dir; return the CompletedProcess."""
    state = tmp_path / ".fno"
    (state / "agents").mkdir(parents=True, exist_ok=True)
    settings = state / "settings.yaml"
    settings.write_text(f"schema_version: 1\nconfig:\n  state_dir: {state}/\n")
    (state / ".path-migration-done").touch()
    env = {
        "PATH": "/usr/bin:/bin",
        "HOME": str(tmp_path),
        "FNO_CONFIG": str(settings),
        "PYTHONPATH": str(REPO_CLI / "src"),
        **env_extra,
    }
    # The style gate rides the fno-agents binary; a sandboxed subprocess keeps
    # the caller's pin so CI's smoke-provided binary stays resolvable.
    for key in ("FNO_AGENTS_FRONT", "FNO_AGENTS_BIN"):
        value = os.environ.get(key)
        if value:
            env[key] = value
    return subprocess.run(
        [sys.executable, "-m", "fno.cli", "mail", "send", *args],
        capture_output=True,
        text=True,
        env=env,
        cwd=str(tmp_path),
        timeout=120,
    )


OVER_CAP = " ".join(["w"] * 81)
def test_violating_body_is_refused_before_delivery(tmp_path):
    proc = _run(["worker", OVER_CAP], {}, tmp_path)
    assert proc.returncode == 1, proc.stderr
    assert STYLE_REFUSED in proc.stderr, proc.stderr
    # The refusal prints both word counts, so the sender can act on it.
    assert "81 words" in proc.stderr and "80 words" in proc.stderr, proc.stderr


def test_control_lane_bypasses_the_style_gate(tmp_path):
    # AC7. A stop, a resume, or a scope change must land whatever the prose
    # rules say; the control lane is what keeps the bus unwedgeable. The body
    # carries a semicolon and "thanks" to prove the bypass is real, not an
    # absence.
    proc = _run(["worker", "control: stop work; report where you are. thanks."], {}, tmp_path)
    # The handle 'worker' does not resolve in the sandbox and the send dies
    # after the gate, so the marker is the refusal's absence on a body that
    # carries both a semicolon and "thanks".
    assert STYLE_REFUSED not in proc.stderr, proc.stderr


def test_style_exception_flag_bypasses_the_gate(tmp_path):
    proc = _run(
        ["worker", OVER_CAP, "--style-exception", "legacy one-off"],
        {},
        tmp_path,
    )
    assert STYLE_REFUSED not in proc.stderr, proc.stderr
