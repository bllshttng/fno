"""The postcompact reinject hook's per-harness wire schema.

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


@pytest.mark.parametrize(
    ("platform", "hook_input"),
    [
        pytest.param("claude", '{"source":"compact"}', id="claude-compact"),
        pytest.param("codex", "{}", id="codex"),
    ],
)
def test_postcompact_producer_uses_each_harness_wire_schema(
    tmp_path: Path,
    platform: str,
    hook_input: str,
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
    # Claude reinjects via SessionStart(source=compact); Codex via PostCompact.
    hook_env = {
        **os.environ,
        "FNO_PLATFORM": platform,
    }
    if platform == "codex":
        hook_env["PLUGIN_ROOT"] = str(plugin)
        hook_env["CLAUDE_PLUGIN_ROOT"] = str(tmp_path / "foreign-claude-plugin")
    else:
        hook_env["CLAUDE_PLUGIN_ROOT"] = str(plugin)
    result = subprocess.run(
        [str(ROOT / "hooks" / "target-postcompact-reinject.sh")],
        cwd=tmp_path,
        env=hook_env,
        input=hook_input,
        text=True,
        capture_output=True,
        check=False,
    )

    assert result.returncode == 0, result.stderr
    payload = json.loads(result.stdout)
    if platform == "codex":
        assert set(payload) == {"systemMessage"}
        assert "Keep the target oriented" in payload["systemMessage"]
    else:
        assert set(payload) == {"hookSpecificOutput"}
        assert payload["hookSpecificOutput"]["hookEventName"] == "SessionStart"
        assert (
            "Keep the target oriented"
            in payload["hookSpecificOutput"]["additionalContext"]
        )
