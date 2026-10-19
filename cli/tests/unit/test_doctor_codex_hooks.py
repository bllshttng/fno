"""Focused tests for the advisory ``fno doctor --codex-hooks`` mode."""
from __future__ import annotations

import json
from pathlib import Path

from typer.testing import CliRunner

from fno.cli import app

runner = CliRunner()
HOOK = "/opt/footnote/hooks/session-start.sh"


def _write_toml(config: Path, *, trusted: bool = True) -> str:
    state_key = f"{config.absolute()}:session_start:0:0"
    trust = (
        f"\n[hooks.state.{json.dumps(state_key)}]\ntrusted_hash = \"sha256:test\"\n"
        if trusted
        else ""
    )
    config.write_text(
        "[[hooks.SessionStart]]\n\n"
        "[[hooks.SessionStart.hooks]]\n"
        'type = "command"\n'
        f"command = {json.dumps(f'env FNO_PLATFORM=codex {HOOK}')}\n"
        f"{trust}"
    )
    return state_key


def _write_json(path: Path, *commands: str) -> None:
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


def test_toml_footnote_hook_with_recorded_hash_is_unverified(tmp_path, monkeypatch) -> None:
    monkeypatch.setenv("CODEX_HOME", str(tmp_path))
    config = tmp_path / "config.toml"
    state_key = _write_toml(config)

    result = runner.invoke(app, ["doctor", "--codex-hooks"])

    assert result.exit_code == 0
    assert "fno doctor: codex hooks: warn" in result.stdout
    assert "preferred=config.toml" in result.stdout
    assert "footnote SessionStart=wired" in result.stdout
    assert f"trust state recorded-unverified: {state_key}" in result.stdout
    assert "trusted_hash was not locally verified" in result.stdout


def test_both_layers_with_foreign_json_warns_and_preserves_paths(
    tmp_path, monkeypatch
) -> None:
    monkeypatch.setenv("CODEX_HOME", str(tmp_path))
    config = tmp_path / "config.toml"
    legacy = tmp_path / "hooks.json"
    foreign = "bash '/Users/test/.codex/sample-agent-state.sh' session"
    _write_toml(config)
    _write_json(legacy, foreign)

    result = runner.invoke(app, ["doctor", "--codex-hooks"])

    assert result.exit_code == 0
    assert "fno doctor: codex hooks: warn" in result.stdout
    assert f"loading hooks from both {legacy} and {config}" in result.stdout
    assert f"foreign legacy JSON hook preserved: {foreign}" in result.stdout
    assert f"manually consolidate it into {config}" in result.stdout
    assert "--migrate-legacy-hooks-json" not in result.stdout


def test_malformed_layer_reports_error_but_exits_zero(tmp_path, monkeypatch) -> None:
    monkeypatch.setenv("CODEX_HOME", str(tmp_path))
    bad_path = tmp_path / "config.toml"
    bad_path.write_text("[[not valid")

    result = runner.invoke(app, ["doctor", "--codex-hooks"])

    assert result.exit_code == 0
    assert "fno doctor: codex hooks: error" in result.stdout
    assert "parse error" in result.stdout
    assert str(bad_path) in result.stdout


def test_neither_layer_warns_with_setup_action(tmp_path, monkeypatch) -> None:
    monkeypatch.setenv("CODEX_HOME", str(tmp_path))

    result = runner.invoke(app, ["doctor", "--codex-hooks"])

    assert result.exit_code == 0
    assert "fno doctor: codex hooks: warn" in result.stdout
    assert "layers=neither" in result.stdout
    assert "`fno config setup cli-hooks-codex`" in result.stdout
