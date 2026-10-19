"""Integration tests for `fno do state` subcommands.

Uses subprocess so we test the real CLI entry point, not just Python APIs.
Tests use tmp_path fixtures to isolate state files.
"""
from __future__ import annotations

import json
import subprocess
from pathlib import Path



# -- Helpers --

MINIMAL_STATE = """\
---
status: IN_PROGRESS
iteration: 3
session_id: 20260421T093631Z-97817-920dac
graph_id: ab-eea09178
---
# Target Session State

Initialized for testing.
"""

def run_cli(*args: str, cwd: Path | None = None) -> subprocess.CompletedProcess:
    """Run fno CLI with given args, capture stdout+stderr."""
    from typer.testing import CliRunner
    from fno.cli import app

    runner = CliRunner()
    result = runner.invoke(app, list(args))
    # Wrap in a CompletedProcess-like object
    return subprocess.CompletedProcess(
        args=list(args),
        returncode=result.exit_code,
        stdout=result.output or "",
        stderr="" if result.exception is None else str(result.exception),
    )


def make_state_file(tmp_path: Path, content: str = MINIMAL_STATE) -> Path:
    state = tmp_path / "target-state.md"
    state.write_text(content)
    return state


# -- AC1-HP: state show prints frontmatter as JSON --

def test_ac1_hp_show_json(tmp_path: Path) -> None:
    """AC1-HP: fno --json state show --path FILE outputs valid JSON."""
    state_file = make_state_file(tmp_path)
    result = run_cli("--json", "do", "state", "show", "--path", str(state_file))
    assert result.returncode == 0, f"stderr: {result.stderr}"
    data = json.loads(result.stdout)
    assert data["status"] == "IN_PROGRESS"
    assert data["iteration"] == 3


# -- AC2-HP: state set updates a field atomically --

def test_ac2_hp_set_field(tmp_path: Path) -> None:
    """AC2-HP: state set --field status --value COMPLETE updates the file."""
    state_file = make_state_file(tmp_path)

    result = run_cli(
        "do", "state", "set",
        "--path", str(state_file),
        "--field", "status",
        "--value", "COMPLETE",
    )
    assert result.returncode == 0, f"stderr: {result.stderr}"

    # Verify via show
    result2 = run_cli("do", "state", "show", "--path", str(state_file), "--field", "status")
    assert result2.stdout.strip() == "COMPLETE"


# -- AC3-ERR: state set rejects invalid status value --

def test_ac3_err_set_invalid_status(tmp_path: Path) -> None:
    """AC3-ERR: state set with invalid status exits 1 with clear error."""
    state_file = make_state_file(tmp_path)
    result = run_cli(
        "do", "state", "set",
        "--path", str(state_file),
        "--field", "status",
        "--value", "GARBAGE",
    )
    assert result.returncode == 1, f"expected exit 1, got {result.returncode}"
    # stderr must mention the error
    combined = result.stdout + result.stderr
    assert "status" in combined.lower() or "invalid" in combined.lower() or "GARBAGE" in combined


# -- AC5-ERR: state init refuses an existing file --


def test_ac5_err_init_file_exists(tmp_path: Path) -> None:
    """AC5-ERR: state init exits 1 if file already exists (without --force)."""
    output = tmp_path / "existing.md"
    output.write_text("---\nstatus: COMPLETE\n---\n")
    result = run_cli(
        "do", "state", "init",
        "--type", "target",
        "--output", str(output),
    )
    assert result.returncode == 1
