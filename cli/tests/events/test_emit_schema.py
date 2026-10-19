"""Tests for Task 1.2: --emit-schema on Python events module.

Verifies that `python -m fno.events --emit-schema` prints valid JSON
to stdout describing the unified envelope shape and writes nothing to
stderr.
"""
from __future__ import annotations

import json
import subprocess
import sys
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[3]


def _run_emit_schema() -> subprocess.CompletedProcess:
    return subprocess.run(
        [sys.executable, "-m", "fno.events", "--emit-schema"],
        capture_output=True,
        text=True,
        cwd=REPO_ROOT,
        timeout=30,
    )


# ---------------------------------------------------------------------------
# AC2-HP: --emit-schema prints valid JSON to stdout, exit 0
# ---------------------------------------------------------------------------


def test_emit_schema_envelope_has_required_fields() -> None:
    """Envelope schema must list required fields [ts, type, source, data]."""
    result = _run_emit_schema()
    data = json.loads(result.stdout)
    envelope = data["envelope"]
    required = set(envelope.get("required", []))
    assert {"ts", "type", "source", "data"} == required


def test_emit_schema_excludes_rust_only_loop_events() -> None:
    """A loop/daemon kind belongs only to the Rust producer registry."""
    result = _run_emit_schema()
    data = json.loads(result.stdout)
    assert "node_failed" not in data["event_types"]


def test_emit_schema_no_stderr_on_success() -> None:
    """--emit-schema must produce no stderr output on success (stdout is clean JSON)."""
    result = _run_emit_schema()
    assert result.returncode == 0
    # stdout must parse cleanly as JSON (no mixed prose)
    json.loads(result.stdout)
    # stderr must be empty on the success path
    assert result.stderr == "", (
        f"--emit-schema wrote unexpected stderr:\n{result.stderr[:500]}"
    )
