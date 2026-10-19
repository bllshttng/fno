"""E2E PreToolUse hook blocks direct edits to graph.json.

Pipes a synthesized PreToolUse payload into hooks/write-gate.sh (the relay to
the native gate) and checks that Edit/Write of `~/.fno/graph.json` returns
decision="block" with a reason that points at `fno backlog`. The approve
paths and the Bash families are covered by the native gate's unit tests and
tests/hooks/test_graph_write_protect.sh.
"""
from __future__ import annotations

import json
import os
import subprocess
from pathlib import Path

import pytest


_REPO_ROOT = Path(__file__).parent.parent.parent.parent
_HOOK_SCRIPT = _REPO_ROOT / "hooks" / "write-gate.sh"


def _invoke_hook(payload: dict) -> dict:
    """Pipe the JSON payload to the hook script's stdin and parse stdout."""
    assert _HOOK_SCRIPT.exists(), f"hook script missing: {_HOOK_SCRIPT}"
    proc = subprocess.run(
        ["bash", str(_HOOK_SCRIPT)],
        input=json.dumps(payload),
        capture_output=True,
        text=True,
        timeout=10,
    )
    assert proc.returncode == 0, f"hook exited {proc.returncode}: {proc.stderr}"
    return json.loads(proc.stdout)


# ---------------------------------------------------------------------------
# Block path: Edit/Write on real graph.json
# ---------------------------------------------------------------------------


@pytest.mark.parametrize("tool", ["Edit", "Write"])
def test_hook_blocks_edit_or_write_to_graph_json(tool):
    """AC5-EDGE-graph: Edit/Write to ~/.fno/graph.json must be blocked."""
    home = os.path.expanduser("~")
    payload = {
        "tool_name": tool,
        "tool_input": {
            "file_path": f"{home}/.fno/graph.json",
            "new_string": "tampered",
        },
    }
    result = _invoke_hook(payload)
    assert result["decision"] == "block", (
        f"expected block on {tool} of graph.json, got {result}"
    )
    assert "fno backlog" in result.get("reason", ""), (
        f"block reason must redirect to `fno backlog`, got {result}"
    )
