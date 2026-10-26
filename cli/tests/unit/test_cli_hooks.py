"""Tests for the Codex hook reader in fno.setup.cli_hooks.

The installers moved to the Rust front door; crates/fno/src/cli_hooks_tests.rs
covers them. This file keeps the reader `fno doctor` still calls.
"""
from __future__ import annotations

import json

from fno.setup.cli_hooks import inspect_codex_hooks

CMD = "/opt/footnote/hooks/session-start.sh"


def _write_codex_toml(path, command):
    path.write_text(
        "[[hooks.SessionStart]]\n\n"
        "[[hooks.SessionStart.hooks]]\n"
        'type = "command"\n'
        f"command = {json.dumps(command)}\n"
    )


def _write_codex_json(path, *commands):
    path.write_text(
        json.dumps(
            {
                "hooks": {
                    "SessionStart": [
                        {
                            "hooks": [
                                {"type": "command", "command": command}
                                for command in commands
                            ]
                        }
                    ]
                }
            }
        )
    )


def test_codex_inspector_classifies_owned_and_foreign_commands(tmp_path):
    config = tmp_path / "config.toml"
    legacy = tmp_path / "hooks.json"
    wrapper_only = "env FNO_PLATFORM=codex custom-session-command"
    other_plugin = "/opt/other-plugin/hooks/session-start.sh"
    foreign = "bash '/Users/bb16/.codex/sample-agent-state.sh' session"
    _write_codex_toml(config, CMD)
    _write_codex_json(legacy, wrapper_only, other_plugin, foreign)

    diagnostics = inspect_codex_hooks(
        config_path=config, hooks_json_path=legacy
    )

    assert diagnostics.toml_footnote_commands == (CMD,)
    assert diagnostics.json_footnote_commands == ()
    assert diagnostics.json_foreign_commands == (
        wrapper_only,
        other_plugin,
        foreign,
    )
