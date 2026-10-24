"""Tests for fno providers CLI surface (Phase 02).

Run: cd cli && uv run pytest src/fno/adapters/providers/test_cli.py -v
"""
from __future__ import annotations

import json
from pathlib import Path

import pytest
import tomli_w
import tomllib
from typer.testing import CliRunner

from fno.adapters.providers.cli import cli as providers_app


runner = CliRunner()


def _seed_state(path: Path, **blocks: dict) -> None:
    """Write a runtime-state file as the Rust writer leaves it."""
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(blocks), encoding="utf-8")


def _usage(probed_at: float, used_pct: float | None = None) -> dict:
    """One usage row; ``used_pct`` adds a 5h window resetting in an hour."""
    windows = [] if used_pct is None else [
        {"label": "5h", "used_pct": used_pct, "resets_at": probed_at + 3600}
    ]
    return {"windows": windows, "probed_at": probed_at, "source": "oauth-endpoint"}


# Wider help width for chip subcommands so options don't column-wrap
# (caught on PR #224: typer/rich line-wraps help in narrow CI envs and
# options vanish from captured stdout). Mirrors the project pattern.


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def _flatten(d: dict) -> dict:
    cfg = d.get("config")
    if not isinstance(cfg, dict):
        return d
    out = {k: v for k, v in d.items() if k != "config"}
    out.update(cfg)
    return out


def _strip_none(x):
    if isinstance(x, dict):
        return {k: _strip_none(v) for k, v in x.items() if v is not None}
    if isinstance(x, list):
        return [_strip_none(v) for v in x]
    return x


def _write_settings(path: Path, content: dict) -> None:
    """Write a flat config.toml at path (lifts any legacy config: wrapper)."""
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(tomli_w.dumps(_strip_none(_flatten(content))), encoding="utf-8")


def _seed_records(home: Path, *ids: str) -> None:
    """Global oauth_dir records under HOME, as `accounts add` writes them."""
    creds = home / ".claude"
    creds.mkdir(exist_ok=True)
    records = [
        {"id": pid, "name": pid, "harness": "claude", "auth": "oauth_dir", "credentials_source": str(creds)}
        for pid in ids
    ]
    _write_settings(home / ".fno" / "config.toml", {"accounts": {"records": records}})


def _invoke(args: list[str], cwd: Path, home: Path):
    """Invoke the CLI with environment isolation."""
    return runner.invoke(
        providers_app,
        args,
        env={
            "HOME": str(home),
            "PWD": str(cwd),
        },
        catch_exceptions=False,
    )


def _two_record_config(active: str = "claude-primary") -> dict:
    return {
        "config": {
            "providers": {
                "active": active,
                "records": [
                    {
                        "id": "claude-primary",
                        "name": "Claude Primary",
                        "harness": "claude",
                        "auth": "oauth_dir",
                        "credentials_source": str(Path.home() / ".claude"),
                        "priority": 10,
                    },
                    {
                        "id": "gemini-backup",
                        "name": "Gemini Backup",
                        "harness": "gemini",
                        "auth": "api_key",
                        "env": {"GEMINI_API_KEY": "test-key"},
                        "priority": 20,
                    },
                ],
            }
        }
    }


# ---------------------------------------------------------------------------
# AC02.3-CLI: Empty-state message is helpful
# ---------------------------------------------------------------------------

class TestListEmpty:
    def test_list_empty_state_message(self, tmp_path: Path):
        """AC02.3-CLI: accounts list on empty config prints helpful message; exit 0."""
        result = _invoke(["list"], cwd=tmp_path, home=tmp_path)
        assert result.exit_code == 0
        assert "No accounts configured" in result.output
        # The remedy it names must be a command that still exists (AC5 removed
        # `fno providers`), or the empty state teaches a command that exits 2.
        assert "fno config accounts add" in result.output

    def test_list_empty_state_exit_zero(self, tmp_path: Path):
        """AC02.3-CLI: exit code is 0 on empty state."""
        result = _invoke(["list"], cwd=tmp_path, home=tmp_path)
        assert result.exit_code == 0


# ---------------------------------------------------------------------------
# List with records
# ---------------------------------------------------------------------------

class TestListWithRecords:
    def test_list_shows_all_records(self, tmp_path: Path):
        """fno providers list shows both records."""
        settings_path = tmp_path / ".fno" / "config.toml"
        _write_settings(settings_path, _two_record_config())
        result = _invoke(["list"], cwd=tmp_path, home=tmp_path)
        assert result.exit_code == 0
        assert "claude-primary" in result.output
        assert "gemini-backup" in result.output

    def test_list_marks_active_with_asterisk(self, tmp_path: Path):
        """fno providers list marks the active record with *."""
        settings_path = tmp_path / ".fno" / "config.toml"
        _write_settings(settings_path, _two_record_config(active="claude-primary"))
        result = _invoke(["list"], cwd=tmp_path, home=tmp_path)
        assert result.exit_code == 0
        # The active record should have a * marker
        output_lines = result.output.splitlines()
        active_lines = [ln for ln in output_lines if "claude-primary" in ln]
        assert any("*" in line for line in active_lines), (
            f"Expected '*' next to active provider in: {active_lines}"
        )

    def test_list_inactive_has_no_asterisk(self, tmp_path: Path):
        """Non-active record is not marked with *."""
        settings_path = tmp_path / ".fno" / "config.toml"
        _write_settings(settings_path, _two_record_config(active="claude-primary"))
        result = _invoke(["list"], cwd=tmp_path, home=tmp_path)
        output_lines = result.output.splitlines()
        backup_lines = [ln for ln in output_lines if "gemini-backup" in ln]
        assert backup_lines, "gemini-backup should appear in output"
        assert not any("*" in line for line in backup_lines), (
            f"Expected no '*' next to inactive provider in: {backup_lines}"
        )


# ---------------------------------------------------------------------------
# The usage=<age> column (distinct from cred-snapshot=<age>, the
# credential blob age) and the DISARMED footer.
# ---------------------------------------------------------------------------

class TestListUsageColumn:
    def test_never_probed_shows_usage_never(self, tmp_path: Path):
        settings_path = tmp_path / ".fno" / "config.toml"
        _write_settings(settings_path, _two_record_config())
        result = _invoke(["list"], cwd=tmp_path, home=tmp_path)
        assert result.exit_code == 0
        for line in result.output.splitlines():
            if "claude-primary" in line:
                assert "usage=never" in line

    def test_stale_snapshot_reports_its_age_with_ttl(self, tmp_path: Path):
        import time

        settings_path = tmp_path / ".fno" / "config.toml"
        _write_settings(settings_path, _two_record_config())
        state_path = tmp_path / "provider-runtime-state.json"
        _seed_state(state_path, usage={"claude-primary": _usage(time.time() - 900)})

        result = runner.invoke(
            providers_app, ["list"],
            env={"HOME": str(tmp_path), "PWD": str(tmp_path),
                 "FNO_RUNTIME_STATE_PATH": str(state_path)},
            catch_exceptions=False,
        )
        assert result.exit_code == 0
        line = next(ln for ln in result.output.splitlines() if "claude-primary" in ln)
        assert "usage=15m (STALE, ttl=5m)" in line

    def test_configured_ttl_renders_in_stale_label(self, tmp_path: Path):
        import time

        settings_path = tmp_path / ".fno" / "config.toml"
        config = _two_record_config()
        config["config"]["providers"]["quota"] = {"probe_ttl_seconds": 900}
        _write_settings(settings_path, config)
        state_path = tmp_path / "provider-runtime-state.json"
        _seed_state(state_path, usage={"claude-primary": _usage(time.time() - 3600)})

        result = runner.invoke(
            providers_app, ["list"],
            env={"HOME": str(tmp_path), "PWD": str(tmp_path),
                 "FNO_RUNTIME_STATE_PATH": str(state_path)},
            catch_exceptions=False,
        )
        assert result.exit_code == 0
        line = next(ln for ln in result.output.splitlines() if "claude-primary" in ln)
        assert "ttl=15m" in line

    def test_json_rows_carry_usage_fields(self, tmp_path: Path):
        settings_path = tmp_path / ".fno" / "config.toml"
        _write_settings(settings_path, _two_record_config())
        result = _invoke(["list", "--json"], cwd=tmp_path, home=tmp_path)
        assert result.exit_code == 0
        import json as _json

        rows = _json.loads(result.output)
        row = next(r for r in rows if r["id"] == "claude-primary")
        assert row["usage_age_s"] is None
        assert row["usage_stale"] is False
        assert row["usage_ttl_seconds"] == 300

    def test_lock_line_names_the_lock_and_the_credential_snapshot(self, tmp_path: Path):
        """The misread that filed the node: `headroom=exhausted` and
        `snapshot=15d` rendered as one sentence, so the credential blob age
        read as the verdict's provenance. The headroom now names its own
        source, and the credential column can no longer be read as it."""
        import json as _json
        import time

        settings_path = tmp_path / ".fno" / "config.toml"
        config = _two_record_config()
        # only a managed record prints the credential-snapshot column
        config["config"]["providers"]["records"] = [
            {"id": "makers", "name": "Makers", "harness": "claude",
             "auth": "managed", "priority": 100},
        ]
        config["config"]["providers"]["active"] = "makers"
        _write_settings(settings_path, config)
        state_path = tmp_path / "provider-runtime-state.json"
        now = time.time()
        state_path.write_text(_json.dumps({
            "schema_version": 2,
            "provider_health": {"makers": {
                "provider_id": "makers", "backoff_level": 1,
                "rate_limited_until": now + 3600, "last_error_at": now - 47 * 60,
                "model_locks": {}}},
        }), encoding="utf-8")

        result = runner.invoke(
            providers_app, ["list"],
            env={"HOME": str(tmp_path), "PWD": str(tmp_path),
                 "FNO_RUNTIME_STATE_PATH": str(state_path)},
            catch_exceptions=False,
        )
        assert result.exit_code == 0
        line = next(ln for ln in result.output.splitlines() if "makers" in ln)
        assert "headroom=exhausted(lock" in line
        assert "cred-snapshot=" in line
        assert "headroom=exhausted  snapshot=" not in line


class TestListDisarmedFooter:
    def test_footer_names_both_flags_when_neither_is_set(self, tmp_path: Path):
        """Two decisions, two flags, and `observe` is recommended first because
        it is the reversible half."""
        settings_path = tmp_path / ".fno" / "config.toml"
        _write_settings(settings_path, _two_record_config())
        result = _invoke(["list"], cwd=tmp_path, home=tmp_path)
        assert result.exit_code == 0
        assert "quota observation is OFF" in result.output
        assert "accounts.quota.observe" in result.output
        assert "accounts.quota.defer_dispatch" in result.output

    def test_footer_says_observing_but_disarmed_when_only_observe_is_set(
        self, tmp_path: Path
    ):
        """The posture that was unavailable: a working meter that holds nothing."""
        settings_path = tmp_path / ".fno" / "config.toml"
        config = _two_record_config()
        config["config"]["providers"]["quota"] = {"observe": True}
        _write_settings(settings_path, config)
        result = _invoke(["list"], cwd=tmp_path, home=tmp_path)
        assert result.exit_code == 0
        assert "observation is ON and rotation is DISARMED" in result.output
        assert "quota observation is OFF" not in result.output

    def test_footer_absent_when_defer_dispatch_true(self, tmp_path: Path):
        settings_path = tmp_path / ".fno" / "config.toml"
        config = _two_record_config()
        config["config"]["providers"]["quota"] = {"defer_dispatch": True}
        _write_settings(settings_path, config)
        result = _invoke(["list"], cwd=tmp_path, home=tmp_path)
        assert result.exit_code == 0
        assert "DISARMED" not in result.output
        assert "quota observation is OFF" not in result.output

    def test_footer_absent_on_empty_config(self, tmp_path: Path):
        """The disarmed footer is a `list` addendum, not a fresh-install nag;
        the empty-state branch returns before the footer check."""
        result = _invoke(["list"], cwd=tmp_path, home=tmp_path)
        assert result.exit_code == 0
        assert "DISARMED" not in result.output
        assert "quota observation is OFF" not in result.output

    def test_footer_absent_from_json_output(self, tmp_path: Path):
        settings_path = tmp_path / ".fno" / "config.toml"
        _write_settings(settings_path, _two_record_config())
        result = _invoke(["list", "--json"], cwd=tmp_path, home=tmp_path)
        assert result.exit_code == 0
        assert "DISARMED" not in result.output


# ---------------------------------------------------------------------------
# Use command
# ---------------------------------------------------------------------------

class TestUse:
    def test_use_updates_active(self, tmp_path: Path):
        """AC02.1-HP: fno providers use <id> updates config.providers.active."""
        from fno.adapters.providers.loader import load_providers

        _seed_records(tmp_path, "my-provider")
        result = _invoke(["use", "my-provider", "--scope", "global"], cwd=tmp_path, home=tmp_path)
        assert result.exit_code == 0

        config = load_providers(repo_root=tmp_path)
        assert config.active == "my-provider"

    def test_use_nonexistent_exits_nonzero(self, tmp_path: Path):
        """fno providers use nonexistent exits non-zero."""
        result = _invoke(["use", "nonexistent"], cwd=tmp_path, home=tmp_path)
        assert result.exit_code != 0

    def test_use_refuses_config_dir_account(self, tmp_path: Path):
        """A config-dir account bills through spawn --account, never the slot."""
        cfg = tmp_path / "claude-alt"
        cfg.mkdir()
        (cfg / ".credentials.json").write_text("{}")
        record = {"id": "readyrule", "name": "readyrule", "harness": "claude",
                  "auth": "managed", "config_dir": str(cfg)}
        _write_settings(tmp_path / ".fno" / "config.toml", {"accounts": {"records": [record]}})
        result = _invoke(["use", "readyrule", "--scope", "global"], cwd=tmp_path, home=tmp_path)
        assert result.exit_code == 1
        assert "spawn-only" in result.output and "--account readyrule" in result.output


# ---------------------------------------------------------------------------
# Helper
# ---------------------------------------------------------------------------

def result_has_active(output: str, provider_id: str) -> bool:
    """Return True if any output line contains both provider_id and '*'."""
    for line in output.splitlines():
        if provider_id in line and "*" in line:
            return True
    return False


# ---------------------------------------------------------------------------
# CG4: fno providers combos {add, list, remove, test, use}
# Plan B (Spec 4). AC4.1-4.5.
# ---------------------------------------------------------------------------

@pytest.fixture
def combos_cli_env(tmp_path: Path, monkeypatch):
    """Pre-seed two providers + isolate runtime_state so combos cli tests are independent."""
    settings = tmp_path / ".fno" / "config.toml"
    _write_settings(settings, _two_record_config())
    monkeypatch.setenv(
        "FNO_RUNTIME_STATE_PATH",
        str(tmp_path / "runtime-state.json"),
    )
    return tmp_path


class TestCombosAdd:
    def test_add_writes_combo_to_settings(self, combos_cli_env: Path):
        """AC4.1-HP: add writes a fresh combo block."""
        result = _invoke(
            [
                "combos", "add", "my-stack",
                "--strategy", "round_robin",
                "--sticky", "3",
                "--providers", "claude-primary,gemini-backup",
                "--scope", "project",
            ],
            cwd=combos_cli_env,
            home=combos_cli_env,
        )
        assert result.exit_code == 0, result.output
        data = tomllib.loads(
            (combos_cli_env / ".fno" / "config.toml").read_text()
        )
        combos = data["accounts"]["combos"]
        assert "my-stack" in combos
        assert combos["my-stack"]["strategy"] == "round_robin"
        assert combos["my-stack"]["sticky_limit"] == 3
        assert combos["my-stack"]["providers"] == ["claude-primary", "gemini-backup"]

    def test_add_unknown_provider_id_fails_without_mutation(
        self, combos_cli_env: Path
    ):
        """AC4.2-ERR: unknown provider id rejected; settings.yaml unchanged."""
        before = (combos_cli_env / ".fno" / "config.toml").read_text()
        result = _invoke(
            [
                "combos", "add", "bad",
                "--providers", "claude-primary,does-not-exist,gemini-backup",
            ],
            cwd=combos_cli_env,
            home=combos_cli_env,
        )
        assert result.exit_code != 0
        assert "does-not-exist" in result.output
        after = (combos_cli_env / ".fno" / "config.toml").read_text()
        assert before == after

    def test_add_duplicate_name_fails(self, combos_cli_env: Path):
        """Duplicate combo name is rejected (must remove first)."""
        ok = _invoke(
            ["combos", "add", "dup", "--providers", "claude-primary"],
            cwd=combos_cli_env, home=combos_cli_env,
        )
        assert ok.exit_code == 0, ok.output
        dup = _invoke(
            ["combos", "add", "dup", "--providers", "gemini-backup"],
            cwd=combos_cli_env, home=combos_cli_env,
        )
        assert dup.exit_code != 0
        assert "already exists" in dup.output


class TestCombosList:
    def test_list_empty_when_no_combos(self, combos_cli_env: Path):
        result = _invoke(
            ["combos", "list"], cwd=combos_cli_env, home=combos_cli_env,
        )
        assert result.exit_code == 0
        assert "No combos configured" in result.output

    def test_list_after_add_shows_combo(self, combos_cli_env: Path):
        """AC4.3-UI: list shows name, strategy, sticky, members."""
        _invoke(
            [
                "combos", "add", "my-stack",
                "--strategy", "round_robin",
                "--sticky", "2",
                "--providers", "claude-primary,gemini-backup",
            ],
            cwd=combos_cli_env, home=combos_cli_env,
        )
        result = _invoke(
            ["combos", "list"], cwd=combos_cli_env, home=combos_cli_env,
        )
        assert result.exit_code == 0
        assert "my-stack" in result.output
        assert "round_robin" in result.output
        assert "claude-primary" in result.output

    def test_list_json_returns_structured_data(self, combos_cli_env: Path):
        """--json returns the same data as JSON."""
        import json as json_mod
        _invoke(
            [
                "combos", "add", "my-stack",
                "--providers", "claude-primary,gemini-backup",
            ],
            cwd=combos_cli_env, home=combos_cli_env,
        )
        result = _invoke(
            ["combos", "list", "--json"], cwd=combos_cli_env, home=combos_cli_env,
        )
        assert result.exit_code == 0
        rows = json_mod.loads(result.output)
        assert len(rows) == 1
        assert rows[0]["name"] == "my-stack"
        assert rows[0]["members"] == ["claude-primary", "gemini-backup"]


class TestCombosRemove:
    def test_remove_clears_active_combo_and_warns(self, combos_cli_env: Path):
        """AC4.4-EDGE: remove clears active_combo when matched."""
        _invoke(
            ["combos", "add", "my-stack", "--providers", "claude-primary"],
            cwd=combos_cli_env, home=combos_cli_env,
        )
        _invoke(
            ["combos", "use", "my-stack"],
            cwd=combos_cli_env, home=combos_cli_env,
        )
        result = _invoke(
            ["combos", "remove", "my-stack"],
            cwd=combos_cli_env, home=combos_cli_env,
        )
        assert result.exit_code == 0
        assert "active_combo cleared" in result.output
        data = tomllib.loads(
            (combos_cli_env / ".fno" / "config.toml").read_text()
        )
        assert data["accounts"].get("active_combo") is None
        assert "my-stack" not in data["accounts"].get("combos", {})

    def test_remove_unknown_combo_fails(self, combos_cli_env: Path):
        result = _invoke(
            ["combos", "remove", "ghost"],
            cwd=combos_cli_env, home=combos_cli_env,
        )
        assert result.exit_code != 0
        assert "ghost" in result.output


class TestCombosTest:
    def test_test_reports_per_member_health(self, combos_cli_env: Path):
        """AC4.5-FR: test surfaces a-in-cooldown-b-ok-c-ok shape + verdict."""
        import time

        _invoke(
            [
                "combos", "add", "my-stack",
                "--providers", "claude-primary,gemini-backup",
            ],
            cwd=combos_cli_env, home=combos_cli_env,
        )
        # Cooldown 'claude-primary' so the verdict becomes partial_cooldown.
        now = time.time()
        _seed_state(combos_cli_env / "runtime-state.json", provider_health={"claude-primary": {
            "backoff_level": 0, "rate_limited_until": now + 60, "last_error_at": now,
        }})

        result = _invoke(
            ["combos", "test", "my-stack"],
            cwd=combos_cli_env, home=combos_cli_env,
        )
        assert result.exit_code == 0
        assert "claude-primary" in result.output
        assert "in_cooldown" in result.output
        assert "gemini-backup" in result.output
        assert "verdict: partial_cooldown" in result.output


class TestCombosUse:
    def test_use_sets_active_combo(self, combos_cli_env: Path):
        _invoke(
            ["combos", "add", "my-stack", "--providers", "claude-primary"],
            cwd=combos_cli_env, home=combos_cli_env,
        )
        result = _invoke(
            ["combos", "use", "my-stack"],
            cwd=combos_cli_env, home=combos_cli_env,
        )
        assert result.exit_code == 0
        data = tomllib.loads(
            (combos_cli_env / ".fno" / "config.toml").read_text()
        )
        assert data["accounts"]["active_combo"] == "my-stack"

    def test_use_unknown_combo_fails(self, combos_cli_env: Path):
        result = _invoke(
            ["combos", "use", "ghost"],
            cwd=combos_cli_env, home=combos_cli_env,
        )
        assert result.exit_code != 0
        assert "ghost" in result.output


# ---------------------------------------------------------------------------
# providers list -J emitter (Connections UI read plane)
# ---------------------------------------------------------------------------

class TestListJson:
    def test_list_json_empty_is_array(self, tmp_path: Path):
        """AC1-EDGE groundwork: empty config emits [] on -J (parseable, no prose)."""
        import json as json_mod

        result = _invoke(["list", "-J"], cwd=tmp_path, home=tmp_path)
        assert result.exit_code == 0
        assert json_mod.loads(result.output) == []

    def test_list_json_rows_carry_ui_fields(self, tmp_path: Path):
        """AC1-HP: -J emits one row per record with id/cli/auth/priority/active/headroom."""
        import json as json_mod

        settings_path = tmp_path / ".fno" / "config.toml"
        _write_settings(settings_path, _two_record_config(active="claude-primary"))
        result = _invoke(["list", "-J"], cwd=tmp_path, home=tmp_path)
        assert result.exit_code == 0, result.output
        rows = json_mod.loads(result.output)
        by_id = {r["id"]: r for r in rows}
        assert set(by_id) == {"claude-primary", "gemini-backup"}
        cp = by_id["claude-primary"]
        assert cp["harness"] == "claude"
        assert cp["auth"] == "oauth_dir"
        assert cp["priority"] == 10
        assert cp["active"] is True
        assert "headroom" in cp        # 'unknown' allowed; the key must exist
        assert "cred-snapshot" in cp   # None for non-managed; key present
        assert by_id["gemini-backup"]["active"] is False


class TestCombosListActiveField:
    def test_combos_list_json_marks_active(self, combos_cli_env: Path):
        """AC1-HP: combos list -J rows gain an 'active' bool from settings.active_combo."""
        import json as json_mod

        _invoke(
            ["combos", "add", "alpha", "--providers", "claude-primary"],
            cwd=combos_cli_env, home=combos_cli_env,
        )
        _invoke(
            ["combos", "add", "beta", "--providers", "gemini-backup"],
            cwd=combos_cli_env, home=combos_cli_env,
        )
        _invoke(
            ["combos", "use", "beta"],
            cwd=combos_cli_env, home=combos_cli_env,
        )
        result = _invoke(
            ["combos", "list", "-J"], cwd=combos_cli_env, home=combos_cli_env,
        )
        assert result.exit_code == 0, result.output
        rows = json_mod.loads(result.output)
        active_map = {r["name"]: r["active"] for r in rows}
        assert active_map == {"alpha": False, "beta": True}
        # Existing fields unchanged.
        beta = next(r for r in rows if r["name"] == "beta")
        assert beta["members"] == ["gemini-backup"]
        assert "strategy" in beta and "sticky_limit" in beta


# ---------------------------------------------------------------------------
# atomic `combos update` command (kills the remove+add hazard)
# ---------------------------------------------------------------------------

class TestCombosUpdate:
    def test_update_replaces_members_atomically(self, combos_cli_env: Path):
        """AC4-HP groundwork: exactly one update call commits the new order."""
        _invoke(
            [
                "combos", "add", "main",
                "--providers", "claude-primary,gemini-backup",
            ],
            cwd=combos_cli_env, home=combos_cli_env,
        )
        result = _invoke(
            [
                "combos", "update", "main",
                "--providers", "gemini-backup,claude-primary",
            ],
            cwd=combos_cli_env, home=combos_cli_env,
        )
        assert result.exit_code == 0, result.output
        data = tomllib.loads(
            (combos_cli_env / ".fno" / "config.toml").read_text()
        )
        assert data["accounts"]["combos"]["main"]["providers"] == [
            "gemini-backup", "claude-primary",
        ]

    def test_update_unknown_member_rejected_without_mutation(
        self, combos_cli_env: Path
    ):
        """AC2-EDGE groundwork: unknown member id rejected; config unchanged."""
        _invoke(
            ["combos", "add", "main", "--providers", "claude-primary"],
            cwd=combos_cli_env, home=combos_cli_env,
        )
        before = (combos_cli_env / ".fno" / "config.toml").read_text()
        result = _invoke(
            ["combos", "update", "main", "--providers", "claude-primary,ghost"],
            cwd=combos_cli_env, home=combos_cli_env,
        )
        assert result.exit_code != 0
        assert "ghost" in result.output
        after = (combos_cli_env / ".fno" / "config.toml").read_text()
        assert before == after

    def test_update_unknown_combo_fails(self, combos_cli_env: Path):
        """Updating a combo that does not exist is a refusal, not a create."""
        result = _invoke(
            ["combos", "update", "ghost", "--providers", "claude-primary"],
            cwd=combos_cli_env, home=combos_cli_env,
        )
        assert result.exit_code != 0
        assert "ghost" in result.output

    def test_update_preserves_strategy_when_omitted(self, combos_cli_env: Path):
        """A pure reorder must NOT silently rewrite round_robin -> fallback."""
        _invoke(
            [
                "combos", "add", "main",
                "--strategy", "round_robin",
                "--sticky", "3",
                "--providers", "claude-primary,gemini-backup",
            ],
            cwd=combos_cli_env, home=combos_cli_env,
        )
        # Reorder only: no --strategy/--sticky.
        result = _invoke(
            [
                "combos", "update", "main",
                "--providers", "gemini-backup,claude-primary",
            ],
            cwd=combos_cli_env, home=combos_cli_env,
        )
        assert result.exit_code == 0, result.output
        combo = tomllib.loads(
            (combos_cli_env / ".fno" / "config.toml").read_text()
        )["accounts"]["combos"]["main"]
        assert combo["strategy"] == "round_robin"  # preserved
        assert combo["sticky_limit"] == 3           # preserved
        assert combo["providers"] == ["gemini-backup", "claude-primary"]

    def test_update_can_change_strategy(self, combos_cli_env: Path):
        """--strategy on update replaces the stored strategy."""
        _invoke(
            [
                "combos", "add", "main",
                "--strategy", "fallback",
                "--providers", "claude-primary,gemini-backup",
            ],
            cwd=combos_cli_env, home=combos_cli_env,
        )
        result = _invoke(
            [
                "combos", "update", "main",
                "--strategy", "round_robin",
                "--sticky", "2",
                "--providers", "claude-primary,gemini-backup",
            ],
            cwd=combos_cli_env, home=combos_cli_env,
        )
        assert result.exit_code == 0, result.output
        data = tomllib.loads(
            (combos_cli_env / ".fno" / "config.toml").read_text()
        )
        combo = data["accounts"]["combos"]["main"]
        assert combo["strategy"] == "round_robin"
        assert combo["sticky_limit"] == 2

    def test_update_resets_round_robin_cursor(self, combos_cli_env: Path):
        """Reordering members invalidates the stored cursor (hash change)."""
        import time

        from fno.adapters.providers.rotation import compute_providers_hash
        from fno.adapters.providers.runtime_state import read_cursor

        _invoke(
            [
                "combos", "add", "main",
                "--strategy", "round_robin",
                "--providers", "claude-primary,gemini-backup",
            ],
            cwd=combos_cli_env, home=combos_cli_env,
        )
        old_hash = compute_providers_hash(("claude-primary", "gemini-backup"))
        _seed_state(combos_cli_env / "runtime-state.json", combo_cursors={"main": {
            "cursor_index": 0, "consecutive_use_count": 1,
            "providers_hash": old_hash, "last_rotated_at": time.time(),
        }})
        assert read_cursor("main", old_hash) is not None

        _invoke(
            [
                "combos", "update", "main",
                "--providers", "gemini-backup,claude-primary",
            ],
            cwd=combos_cli_env, home=combos_cli_env,
        )
        new_hash = compute_providers_hash(("gemini-backup", "claude-primary"))
        assert read_cursor("main", new_hash) is None


# ---------------------------------------------------------------------------
# AC5-CON: routing-active and slot-active resolve through ONE function
# ---------------------------------------------------------------------------


def _managed_pair_config(active: str) -> dict:
    return {
        "config": {
            "providers": {
                "active": active,
                "records": [
                    {"id": "readyrule", "name": "readyrule", "harness": "claude",
                     "auth": "managed", "priority": 10},
                    {"id": "makers", "name": "makers", "harness": "claude",
                     "auth": "managed", "priority": 20},
                ],
            }
        }
    }


class TestEffectiveActive:
    """`config.providers.active` and the slot occupant can disagree.

    Live on the machine that motivated this: config said `readyrule` while the
    shared slot held `makers`, so the display path marked one account and the
    dispatch path evaluated the other's headroom.
    """

    @pytest.fixture()
    def diverged(self, tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Path:
        _write_settings(tmp_path / ".fno" / "config.toml", _managed_pair_config("readyrule"))
        monkeypatch.setenv("HOME", str(tmp_path))
        monkeypatch.setenv("PWD", str(tmp_path))
        monkeypatch.setenv("FNO_STATE_DIR", str(tmp_path / ".fno"))
        from fno.adapters.providers import managed

        managed.stamp_active_slot("claude", "makers")
        return tmp_path

    def test_resolver_returns_the_slot_occupant(self, diverged: Path) -> None:
        from fno.adapters.providers.loader import effective_active

        assert effective_active(repo_root=diverged) == "makers"

    def test_list_marks_the_slot_occupant_not_the_config_pointer(
        self, diverged: Path
    ) -> None:
        result = _invoke(["list"], cwd=diverged, home=diverged)
        assert result.exit_code == 0, result.output
        starred = [ln for ln in result.output.splitlines() if ln.lstrip().startswith("*")]
        assert len(starred) == 1
        assert "makers" in starred[0]

    def test_non_managed_active_still_reads_routing_active(
        self, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        # A record whose credential does NOT come from the shared slot keeps
        # routing-active as its authority.
        _write_settings(tmp_path / ".fno" / "config.toml", _two_record_config())
        monkeypatch.setenv("HOME", str(tmp_path))
        monkeypatch.setenv("PWD", str(tmp_path))
        monkeypatch.setenv("FNO_STATE_DIR", str(tmp_path / ".fno"))
        from fno.adapters.providers.loader import effective_active

        assert effective_active(repo_root=tmp_path) == "claude-primary"

    def test_managed_active_with_no_stamp_is_nobody(
        self, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        # No stamp means nothing is known to occupy the slot. Guessing
        # routing-active here is how a dispatch ends up billing an account whose
        # credential is not actually loaded.
        _write_settings(tmp_path / ".fno" / "config.toml", _managed_pair_config("readyrule"))
        monkeypatch.setenv("HOME", str(tmp_path))
        monkeypatch.setenv("PWD", str(tmp_path))
        monkeypatch.setenv("FNO_STATE_DIR", str(tmp_path / ".fno"))
        from fno.adapters.providers.loader import effective_active

        assert effective_active(repo_root=tmp_path) is None


# ---------------------------------------------------------------------------
# fno providers pick (launch-time headroom picking)
# ---------------------------------------------------------------------------


def _config_dir_pair(tmp_path: Path, active: str = "readyrule") -> dict:
    for name in ("claude-alt", "claude-main"):
        d = tmp_path / name
        d.mkdir(exist_ok=True)
        (d / ".credentials.json").write_text("{}")
    return {
        "config": {
            "providers": {
                "active": active,
                "records": [
                    {"id": "readyrule", "name": "readyrule", "harness": "claude",
                     "auth": "managed", "config_dir": str(tmp_path / "claude-alt")},
                    {"id": "makers", "name": "makers", "harness": "claude",
                     "auth": "managed", "config_dir": str(tmp_path / "claude-main")},
                ],
            }
        }
    }


# ---------------------------------------------------------------------------
# window: the usage env the projection reads
# ---------------------------------------------------------------------------


def _usage_env(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Path:
    _write_settings(tmp_path / ".fno" / "config.toml", _config_dir_pair(tmp_path))
    monkeypatch.setenv("HOME", str(tmp_path))
    monkeypatch.setenv("PWD", str(tmp_path))
    monkeypatch.setenv("FNO_STATE_DIR", str(tmp_path / ".fno"))
    monkeypatch.setenv("FNO_RUNTIME_STATE_PATH", str(tmp_path / "runtime-state.json"))
    return tmp_path


class TestWindowCommand:
    """AC3 at the CLI boundary: a projection, and an honest unknown."""

    def test_ac3_edge_a_probeless_record_prints_unknown_and_keeps_the_key(
        self, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        # done_probe 2 greps for "closes_in_s". The key must be present even
        # when the answer is unknown, or a shapeless response passes the gate.
        import json as _json

        _usage_env(tmp_path, monkeypatch)
        result = _invoke(["window", "--json"], cwd=tmp_path, home=tmp_path)
        assert result.exit_code == 0, result.output
        payload = _json.loads(result.output.strip())
        assert payload
        for row in payload.values():
            assert "closes_in_s" in row
            assert row["closes_in_s"] is None
            assert row["headroom"] == "unknown"

    def test_ac3_hp_a_recorded_open_prints_seconds_to_close(
        self, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        import json as _json
        import time as _time

        import fno.adapters.providers.runtime_state as rs

        _usage_env(tmp_path, monkeypatch)
        monkeypatch.setattr(
            rs, "_record_window_seconds", lambda _pid: 300 * 60, raising=True
        )
        _seed_state(tmp_path / "runtime-state.json", windows_opened={
            "readyrule": {"window": {"opened_at": _time.time() - 2 * 3600, "warned": False}},
        })

        result = _invoke(["window", "--json"], cwd=tmp_path, home=tmp_path)
        assert result.exit_code == 0, result.output
        row = _json.loads(result.output.strip())["readyrule"]
        assert row["closes_in_s"] == pytest.approx(3 * 3600, abs=5)
        assert row["closes_at"] is not None
        # The projection must not have taught headroom a percentage.
        assert row["headroom"] == "unknown"

    def test_the_human_row_says_unknown_rather_than_a_number(
        self, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        _usage_env(tmp_path, monkeypatch)
        result = _invoke(["window"], cwd=tmp_path, home=tmp_path)
        assert result.exit_code == 0, result.output
        assert "unknown" in result.output


class TestListIdentityColumn:
    """AC1/AC2: the accounts list answers who each row is really
    signed in as, from the same binding owner usage and doctor read. The
    stimulus was a live drift: list said `* makers headroom=ok` in the same
    minute doctor reported the slot serving jason@readyrule.com."""

    @staticmethod
    def _pair_env(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Path:
        _write_settings(
            tmp_path / ".fno" / "config.toml", _managed_pair_config("makers")
        )
        monkeypatch.setenv("HOME", str(tmp_path))
        monkeypatch.setenv("PWD", str(tmp_path))
        monkeypatch.setenv("FNO_STATE_DIR", str(tmp_path / ".fno"))
        return tmp_path

    @staticmethod
    def _bind(record_id: str, tmp_path: Path) -> None:
        from fno.adapters.providers import managed

        managed.write_record_principal(
            record_id,
            {"account_uuid": f"acct-{record_id}", "organization_uuid": "org-1"},
            tmp_path / ".fno" / "providers",
        )

    def test_mismatch_row_names_who_the_slot_really_serves(
        self, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        """AC1-HP: stamp says makers, slot credential serves readyrule."""
        from fno.adapters.providers import managed

        self._pair_env(tmp_path, monkeypatch)
        root = tmp_path / ".fno" / "providers"
        managed.stamp_active_slot("claude", "makers", root)
        self._bind("readyrule", tmp_path)
        self._bind("makers", tmp_path)
        monkeypatch.setattr(managed, "canonical_slot_blobs", lambda cli: ["{}"])
        monkeypatch.setattr(
            managed, "slot_principal",
            lambda blob: (
                {"account_uuid": "acct-readyrule", "organization_uuid": "org-1",
                 "email": "other@example.com"},
                None,
            ),
        )

        result = _invoke(["list"], cwd=tmp_path, home=tmp_path)

        assert result.exit_code == 0, result.output
        makers_row = next(ln for ln in result.output.splitlines() if " makers " in ln)
        readyrule_row = next(
            ln for ln in result.output.splitlines() if " readyrule " in ln
        )
        assert "!serves readyrule" in makers_row
        # identity is the last column, so the modal can take the row's tail verbatim.
        assert makers_row.endswith("identity=!serves readyrule")
        assert "!serves" not in readyrule_row
        assert "identity=readyrule" in readyrule_row
        # AC1-HP names doctor as the agreeing witness in the same store.
        from fno.adapters.providers.cli import _doctor_findings

        assert "slot-identity-drift" in {f["problem"] for f in _doctor_findings()}
        # Once the stamp names readyrule, makers is simply off the slot: the
        # slot serving someone else is expected, so its cell is no fault.
        managed.stamp_active_slot("claude", "readyrule", root)
        result = _invoke(["list"], cwd=tmp_path, home=tmp_path)
        makers_row = next(ln for ln in result.output.splitlines() if " makers " in ln)
        assert makers_row.endswith("identity=off-slot"), result.output

    def test_unprovable_identity_is_named_and_still_exits_zero(
        self, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        """AC1-ERR: an unproven row renders ?<reason>, never an account name."""
        from fno.adapters.providers import managed

        self._pair_env(tmp_path, monkeypatch)
        self._bind("readyrule", tmp_path)
        self._bind("makers", tmp_path)
        monkeypatch.setattr(managed, "canonical_slot_blobs", lambda cli: [])
        result = _invoke(["list"], cwd=tmp_path, home=tmp_path)
        assert result.exit_code == 0, result.output
        assert "?no-slot-credential" in result.output
        assert "!serves" not in result.output

    def test_a_raised_identity_read_renders_no_observation(
        self, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        """A read that raises is a failed observation, not a proven account."""
        import fno.adapters.providers.binding as binding

        self._pair_env(tmp_path, monkeypatch)
        monkeypatch.setattr(
            binding, "resolve_account_binding",
            lambda *a, **k: (_ for _ in ()).throw(RuntimeError("keychain down")),
        )
        result = _invoke(["list"], cwd=tmp_path, home=tmp_path)
        assert result.exit_code == 0, result.output
        assert "?no-observation" in result.output
        assert "!serves" not in result.output

    def test_two_scoped_roots_on_one_principal_flag_shared_identity(
        self, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        """AC1-EDGE: different credential roots observing one principal."""
        from fno.adapters.providers import managed

        dir_a = tmp_path / "cfg-a"
        dir_b = tmp_path / "cfg-b"
        _write_settings(
            tmp_path / ".fno" / "config.toml",
            {"config": {"providers": {"active": "scoped-a", "records": [
                {"id": "scoped-a", "name": "scoped-a", "harness": "claude",
                 "auth": "oauth_dir", "credentials_source": str(dir_a),
                 "priority": 10},
                {"id": "scoped-b", "name": "scoped-b", "harness": "claude",
                 "auth": "oauth_dir", "credentials_source": str(dir_b),
                 "priority": 20},
            ]}}},
        )
        monkeypatch.setenv("HOME", str(tmp_path))
        monkeypatch.setenv("PWD", str(tmp_path))
        monkeypatch.setenv("FNO_STATE_DIR", str(tmp_path / ".fno"))
        for rid in ("scoped-a", "scoped-b"):
            self._bind(rid, tmp_path)
        monkeypatch.setattr(managed, "slot_blobs", lambda harness, root: ["{}"])
        monkeypatch.setattr(
            managed, "slot_principal",
            lambda blob: (
                {"account_uuid": "acct-shared", "organization_uuid": "org-1"},
                None,
            ),
        )

        result = _invoke(["list"], cwd=tmp_path, home=tmp_path)

        assert result.exit_code == 0, result.output
        assert result.output.count("!shared-identity") == 2

    def test_a_managed_pair_on_one_slot_never_flags_shared_identity(
        self, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        """AC1-EDGE negative: one slot is one root, so no flag either row."""
        from fno.adapters.providers import managed

        self._pair_env(tmp_path, monkeypatch)
        self._bind("readyrule", tmp_path)
        self._bind("makers", tmp_path)
        monkeypatch.setattr(managed, "canonical_slot_blobs", lambda cli: ["{}"])
        monkeypatch.setattr(
            managed, "slot_principal",
            lambda blob: (
                {"account_uuid": "acct-shared", "organization_uuid": "org-1"},
                None,
            ),
        )

        result = _invoke(["list"], cwd=tmp_path, home=tmp_path)

        assert result.exit_code == 0, result.output
        assert "shared-identity" not in result.output

    def test_doctor_findings_ride_the_row_but_stored_expiry_does_not(
        self, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        """Per-record doctor findings ride the row as ` !<problem>`; a stored copy's past expiresAt is not one."""
        import json as _json

        self._pair_env(tmp_path, monkeypatch)
        blob = _json.dumps({
            "claudeAiOauth": {"accessToken": "t", "expiresAt": 1783352000000}
        })
        root = tmp_path / ".fno" / "providers"
        for rid in ("readyrule", "makers"):
            (root / rid).mkdir(parents=True)
            (root / rid / "blob").write_text(blob)
        result = _invoke(["list"], cwd=tmp_path, home=tmp_path)
        assert result.exit_code == 0, result.output
        assert result.output.count("!duplicate-credential") == 2
        assert "!expired-credential" not in result.output

    def test_json_without_the_flag_stays_byte_compatible_and_offline(
        self, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        """AC2-HP: no flag, same keys, and no binding work at all."""
        import json as _json

        import fno.adapters.providers.binding as binding

        self._pair_env(tmp_path, monkeypatch)
        monkeypatch.setattr(
            binding, "resolve_account_binding",
            lambda *a, **k: pytest.fail("bare list -J must not resolve identity"),
        )
        result = _invoke(["list", "-J"], cwd=tmp_path, home=tmp_path)
        assert result.exit_code == 0, result.output
        rows = _json.loads(result.output)
        assert {r["id"] for r in rows} == {"readyrule", "makers"}
        assert all(set(r) == {
            "id", "name", "harness", "auth", "priority", "active", "headroom",
            "cred-snapshot", "usage_age_s", "usage_ttl_seconds", "usage_stale",
        } for r in rows)
