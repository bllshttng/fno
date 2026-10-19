"""Tests for fno.setup.cli_hooks (Codex/Gemini SessionStart hook install).

Covers: fresh install, idempotent re-run, never-clobber of existing
hooks/settings, backup-before-write, Codex needs_trust, and the CLI surface.
"""
from __future__ import annotations

import json
import shlex

from typer.testing import CliRunner

from fno.setup.cli_hooks import (
    inspect_codex_hooks,
    install_claude_worktree_remove_hook,
    install_codex_hook,
)

CMD = "/opt/footnote/hooks/session-start.sh"


# --- Gemini -----------------------------------------------------------------


# --- Codex ------------------------------------------------------------------


def test_codex_preserves_existing_config_and_comments(tmp_path):
    config = tmp_path / "config.toml"
    original = (
        "# my codex config\n"
        'model = "gpt-5"\n\n'
        "[[hooks.Stop]]\n\n"
        "[[hooks.Stop.hooks]]\n"
        'type = "command"\n'
        'command = "my-stop.sh"\n'
    )
    config.write_text(original)
    res = install_codex_hook(CMD, config_path=config)
    assert res.changed and res.backup is not None and res.backup.exists()
    text = config.read_text()
    # Original content + comment survive verbatim.
    assert "# my codex config" in text
    assert 'model = "gpt-5"' in text
    assert "my-stop.sh" in text
    # New SessionStart hook added.
    import tomllib

    parsed = tomllib.loads(text)
    assert parsed["hooks"]["Stop"][0]["hooks"][0]["command"] == "my-stop.sh"
    ss_cmds = [h["command"] for g in parsed["hooks"]["SessionStart"] for h in g["hooks"]]
    assert any(CMD in c for c in ss_cmds)


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


def test_codex_duplicate_layers_warn_without_mutating_json(tmp_path):
    config = tmp_path / "config.toml"
    legacy = tmp_path / "hooks.json"
    _write_codex_toml(config, CMD)
    _write_codex_json(legacy, CMD)
    before = legacy.read_bytes()

    res = install_codex_hook(
        CMD, config_path=config, hooks_json_path=legacy
    )

    assert not res.changed and res.already_present
    assert res.note and "both Codex hook layers" in res.note
    assert str(config) in res.note and str(legacy) in res.note
    assert "TOML is preferred" in res.note
    assert legacy.read_bytes() == before
    assert not legacy.with_name("hooks.json.fno-bak").exists()


def test_codex_migration_preserves_foreign_events_and_exact_backup(tmp_path):
    config = tmp_path / "config.toml"
    legacy = tmp_path / "hooks.json"
    _write_codex_toml(config, CMD)
    original = (
        json.dumps(
            {
                "description": "Mixed legacy hooks",
                "hooks": {
                    "SessionStart": [
                        {"hooks": [{"type": "command", "command": CMD}]}
                    ],
                    "Stop": [
                        {
                            "hooks": [
                                {"type": "command", "command": "foreign-stop.sh"}
                            ]
                        }
                    ],
                },
            },
            indent=4,
        )
        + "\n"
    ).encode()
    legacy.write_bytes(original)

    res = install_codex_hook(
        CMD,
        config_path=config,
        hooks_json_path=legacy,
        migrate_legacy_hooks_json=True,
    )

    remaining = json.loads(legacy.read_text())
    assert "SessionStart" not in remaining["hooks"]
    assert remaining["hooks"]["Stop"][0]["hooks"][0]["command"] == "foreign-stop.sh"
    assert remaining["description"] == "Mixed legacy hooks"
    assert res.legacy_backup is not None
    assert res.legacy_backup.read_bytes() == original
    assert res.note and "manual consolidation" in res.note
    assert "foreign hooks were preserved" in res.note
    assert "foreign SessionStart hooks" not in res.note

    diagnostics = inspect_codex_hooks(config_path=config, hooks_json_path=legacy)
    assert diagnostics.state == "both"
    assert diagnostics.has_json_hooks
    assert diagnostics.json_foreign_commands == ("foreign-stop.sh",)


def test_cli_codex_malformed_config_exits_nonzero_without_success_output(
    tmp_path, monkeypatch
):
    import fno.paths as paths
    from fno.setup_cli import app

    fake_entry = tmp_path / "plugin" / "hooks" / "session-start.sh"
    fake_entry.parent.mkdir(parents=True)
    fake_entry.write_text("#!/usr/bin/env bash\n")
    monkeypatch.setattr(paths, "resolve_plugin_script", lambda rel: fake_entry)
    config = tmp_path / "config.toml"
    config.write_text("[[not valid")

    result = CliRunner().invoke(
        app,
        ["cli-hooks-codex", "--codex-config", str(config)],
    )

    assert result.exit_code == 1
    assert "malformed" in result.stderr
    assert "UNTRUSTED" not in result.output
    assert "Nothing to do" not in result.output


def test_codex_malformed_legacy_json_returns_failure_without_writing_toml(tmp_path):
    config = tmp_path / "config.toml"
    legacy = tmp_path / "hooks.json"
    legacy.write_text("{not json")

    result = install_codex_hook(
        CMD,
        config_path=config,
        hooks_json_path=legacy,
        migrate_legacy_hooks_json=True,
    )

    assert result.error and "malformed" in result.error
    assert not result.changed
    assert not result.needs_trust
    assert not config.exists()
    assert legacy.read_text() == "{not json"


# --- CLI surface ------------------------------------------------------------


def test_cli_cli_hooks_writes_both(tmp_path, monkeypatch):
    # Point the plugin-script resolver at a fake hook path.
    import fno.paths as paths

    fake_entry = tmp_path / "plugin" / "hooks" / "session-start.sh"
    fake_entry.parent.mkdir(parents=True)
    fake_entry.write_text("#!/usr/bin/env bash\n")
    monkeypatch.setattr(paths, "resolve_plugin_script", lambda rel: fake_entry)

    from fno.setup_cli import app

    gset = tmp_path / "g" / "settings.json"
    cconf = tmp_path / "c" / "config.toml"
    res = CliRunner().invoke(
        app,
        ["cli-hooks", "--no-claude", "--gemini-settings", str(gset),
         "--codex-config", str(cconf)],
    )
    assert res.exit_code == 0, res.output
    assert gset.exists() and cconf.exists()
    assert "UNTRUSTED" in res.output  # codex trust instruction surfaced
    hook = json.loads(gset.read_text())["hooks"]["SessionStart"][0]["hooks"][0]
    assert hook["command"] == f"env FNO_PLATFORM=gemini {fake_entry}"


def test_cli_cli_hooks_refuses_missing_plugin_hook(tmp_path, monkeypatch):
    import fno.paths as paths
    from fno.setup_cli import app

    missing_entry = tmp_path / "missing-plugin" / "hooks" / "session-start.sh"
    monkeypatch.setattr(paths, "resolve_plugin_script", lambda rel: missing_entry)
    gemini_settings = tmp_path / "gemini" / "settings.json"
    codex_config = tmp_path / "codex" / "config.toml"

    result = CliRunner().invoke(
        app,
        [
            "cli-hooks",
            "--gemini-settings",
            str(gemini_settings),
            "--codex-config",
            str(codex_config),
        ],
    )

    assert result.exit_code == 1
    assert "could not locate the installed footnote hooks dir" in result.output
    assert not gemini_settings.exists()
    assert not codex_config.exists()


# --- Claude WorktreeRemove --------------------------------------------------

WT_CMD = "/opt/footnote/hooks/worktree-remove.sh"


def test_claude_worktree_remove_preserves_other_hooks_and_settings(tmp_path):
    settings = tmp_path / "settings.json"
    settings.write_text(
        json.dumps(
            {
                "model": "opus",
                "hooks": {
                    "SessionStart": [
                        {"hooks": [{"type": "command", "command": "/my/own.sh"}]}
                    ],
                    "WorktreeRemove": [
                        {"hooks": [{"type": "command", "command": "/other/plugin.sh"}]}
                    ],
                },
            }
        )
    )
    res = install_claude_worktree_remove_hook(WT_CMD, settings_path=settings)
    assert res.changed and res.backup is not None

    data = json.loads(settings.read_text())
    assert data["model"] == "opus"
    assert data["hooks"]["SessionStart"][0]["hooks"][0]["command"] == "/my/own.sh"
    commands = [
        h["command"]
        for g in data["hooks"]["WorktreeRemove"]
        for h in g["hooks"]
    ]
    assert commands == ["/other/plugin.sh", f"bash {WT_CMD}"]


def test_claude_worktree_remove_malformed_left_unchanged(tmp_path):
    settings = tmp_path / "settings.json"
    settings.write_text("{not json")
    res = install_claude_worktree_remove_hook(WT_CMD, settings_path=settings)
    assert not res.changed and not res.already_present
    assert "malformed" in (res.note or "")
    assert res.error == res.note  # the caller must see this as a refusal
    assert settings.read_text() == "{not json"


def test_claude_worktree_remove_repairs_a_dead_path(tmp_path):
    """An entry whose script no longer exists is the stranding bug wearing a
    different hat, so re-running must repair it rather than report success."""
    settings = tmp_path / "settings.json"
    dead = tmp_path / "archived-worktree" / "hooks" / "worktree-remove.sh"
    install_claude_worktree_remove_hook(str(dead), settings_path=settings)

    live = tmp_path / "canonical" / "hooks" / "worktree-remove.sh"
    live.parent.mkdir(parents=True)
    live.write_text("#!/usr/bin/env bash\n")
    res = install_claude_worktree_remove_hook(str(live), settings_path=settings)

    assert res.changed and not res.already_present and res.error is None
    data = json.loads(settings.read_text())
    groups = data["hooks"]["WorktreeRemove"]
    assert len(groups) == 1  # repaired in place, not appended alongside
    assert groups[0]["hooks"][0]["command"] == f"bash {live}"


def test_claude_worktree_remove_quotes_a_spacey_path(tmp_path):
    settings = tmp_path / "settings.json"
    spacey = tmp_path / "My Plugins" / "hooks" / "worktree-remove.sh"
    spacey.parent.mkdir(parents=True)
    spacey.write_text("#!/usr/bin/env bash\n")

    install_claude_worktree_remove_hook(str(spacey), settings_path=settings)
    command = json.loads(settings.read_text())["hooks"]["WorktreeRemove"][0]["hooks"][0][
        "command"
    ]
    # Unquoted, bash would resolve this to ".../My" and exit 127.
    assert shlex.split(command) == ["bash", str(spacey)]
    # And the quoted form must still read back as already-wired.
    assert install_claude_worktree_remove_hook(
        str(spacey), settings_path=settings
    ).already_present


def test_claude_repair_only_never_wires_an_unwired_hook(tmp_path):
    """Wiring a GLOBAL hook stays an explicit opt-in; repair only keeps a hook
    the user already asked for alive."""
    settings = tmp_path / "settings.json"
    settings.write_text(json.dumps({"model": "opus"}))
    res = install_claude_worktree_remove_hook(
        WT_CMD, settings_path=settings, repair_only=True
    )
    assert not res.changed and not res.already_present
    assert json.loads(settings.read_text()) == {"model": "opus"}


def test_claude_explicit_install_repoints_a_live_but_stale_version(tmp_path):
    """An upgrade that leaves the old version on disk must not make an explicit
    install a silent no-op pointing at the stale one."""
    old = tmp_path / "fno" / "0.3.0" / "hooks" / "worktree-remove.sh"
    old.parent.mkdir(parents=True)
    old.write_text("#!/usr/bin/env bash\n")
    new = tmp_path / "fno" / "0.3.1" / "hooks" / "worktree-remove.sh"
    new.parent.mkdir(parents=True)
    new.write_text("#!/usr/bin/env bash\n")

    settings = tmp_path / "settings.json"
    install_claude_worktree_remove_hook(str(old), settings_path=settings)
    res = install_claude_worktree_remove_hook(str(new), settings_path=settings)

    assert res.changed
    groups = json.loads(settings.read_text())["hooks"]["WorktreeRemove"]
    assert len(groups) == 1
    assert groups[0]["hooks"][0]["command"] == f"bash {new}"


def test_claude_repair_only_leaves_a_live_other_root_alone(tmp_path):
    """The automatic heal must not ping-pong global settings between a dev
    checkout and a marketplace install on alternating sessions."""
    a = tmp_path / "a" / "hooks" / "worktree-remove.sh"
    a.parent.mkdir(parents=True)
    a.write_text("#!/usr/bin/env bash\n")
    b = tmp_path / "b" / "hooks" / "worktree-remove.sh"
    b.parent.mkdir(parents=True)
    b.write_text("#!/usr/bin/env bash\n")

    settings = tmp_path / "settings.json"
    install_claude_worktree_remove_hook(str(a), settings_path=settings)
    before = settings.read_text()
    res = install_claude_worktree_remove_hook(
        str(b), settings_path=settings, repair_only=True
    )
    assert res.already_present and not res.changed
    assert settings.read_text() == before
