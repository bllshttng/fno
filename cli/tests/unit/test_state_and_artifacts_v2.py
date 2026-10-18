"""Phase 07: `fno do state show --v2` honors v2 layout.

Covers:
- `fno do state show --v2` reads `.fno/v2/target-state.md` when present
- `fno do state show --v2` falls back to v1 with a stderr note when v2 absent
- `fno do state show` without --v2 is unchanged

Note: gate artifact tests (artifact_path, check_gate_safe) removed in
Task 3.2 (control-plane collapse ab-d0337fbc) along with fno.gates.
"""
from __future__ import annotations

import os
import subprocess
from pathlib import Path

import yaml


SESSION_ID = "state-v2-session-001"


def _write_state(base: Path, session_id: str = SESSION_ID, status: str = "IN_PROGRESS") -> Path:
    base.mkdir(parents=True, exist_ok=True)
    data = {
        "status": status,
        "session_id": session_id,
        "current_phase": "blueprint",
    }
    path = base / "target-state.md"
    path.write_text("---\n" + yaml.dump(data, default_flow_style=False) + "---\n")
    return path


def _run_fno(args: list[str], cwd: Path) -> subprocess.CompletedProcess:
    cli_dir = Path(__file__).parent.parent.parent
    return subprocess.run(
        ["uv", "run", "fno-py", *args],
        cwd=cli_dir,
        capture_output=True,
        text=True,
        env={**os.environ, "FNO_REPO_ROOT": str(cwd)},
    )


# ---- AC1-FALLBACK: --v2 with no v2 file falls back to v1 -------------

def test_state_show_v2_falls_back_to_v1_with_stderr_note(tmp_path):
    _write_state(tmp_path / ".fno", session_id="v1-fallback")
    # v2 dir does NOT exist
    assert not (tmp_path / ".fno" / "v2").exists()

    completed = _run_fno(["state", "show", "--v2"], cwd=tmp_path)

    assert completed.returncode == 0
    assert "v1-fallback" in completed.stdout
    assert "v2 not found" in completed.stderr.lower()


# ---- AC1-DEFAULT: no --v2 keeps v1-only behavior ---------------------
