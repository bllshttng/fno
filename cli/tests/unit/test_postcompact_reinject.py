"""The postcompact reinject hook's wire schema on the harness that reads it raw.

Moved from test_context_audit.py (whose module it never imported): the hook
test rides with the hook family now, not with the census it sat beside.
"""

import json
import os
import shutil
import subprocess
from pathlib import Path

import pytest


ROOT = Path(__file__).resolve().parents[3]
pytestmark = pytest.mark.xdist_group("context-audit-processes")


def test_codex_postcompact_reads_its_own_plugin_root_and_emits_a_system_message(
    tmp_path: Path,
) -> None:
    plugin = tmp_path / "plugin"
    guard = plugin / "scripts" / "lib" / "target-guard.sh"
    guard.parent.mkdir(parents=True)
    guard.write_text(
        "target_is_active() { return 0; }\n"
        "target_state_field() {\n"
        "  sed -n \"s/^$1: *//p\" \"$2\" | head -1 | tr -d '\"'\n"
        "}\n",
        encoding="utf-8",
    )
    # The hook sources its carrier from the plugin's lib dir; a fake plugin that
    # ships the guard but not the carrier would silence the hook (by design).
    shutil.copy(
        ROOT / "scripts" / "lib" / "postcompact-carrier.sh",
        plugin / "scripts" / "lib" / "postcompact-carrier.sh",
    )
    state = tmp_path / ".fno" / "target-state.md"
    state.parent.mkdir()
    state.write_text(
        "session_id: wire-session\n"
        'input: "Keep the target oriented"\n'
        "plan_path: null\n"
        "graph_node_id: x-2e3c\n",
        encoding="utf-8",
    )
    # A foreign CLAUDE_PLUGIN_ROOT must lose to Codex's own PLUGIN_ROOT.
    hook_env = {
        **os.environ,
        "FNO_PLATFORM": "codex",
        "PLUGIN_ROOT": str(plugin),
        "CLAUDE_PLUGIN_ROOT": str(tmp_path / "foreign-claude-plugin"),
    }
    result = subprocess.run(
        [str(ROOT / "hooks" / "target-postcompact-reinject.sh")],
        cwd=tmp_path,
        env=hook_env,
        input="{}",
        text=True,
        capture_output=True,
        check=False,
    )

    assert result.returncode == 0, result.stderr
    payload = json.loads(result.stdout)
    assert set(payload) == {"systemMessage"}
    assert "Keep the target oriented" in payload["systemMessage"]
