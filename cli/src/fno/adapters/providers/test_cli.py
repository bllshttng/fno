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
# Test command
# ---------------------------------------------------------------------------

class TestTestCommand:
    def test_test_valid_record_returns_zero(self, tmp_path: Path):
        """fno providers test <id> returns 0 when binary on PATH + credentials_source exists."""
        import shutil
        # Only run if 'claude' binary is actually available, otherwise skip
        if not shutil.which("claude"):
            pytest.skip("claude binary not on PATH in this environment")

        creds = tmp_path / ".claude"
        creds.mkdir()
        settings_path = tmp_path / ".fno" / "config.toml"
        _write_settings(settings_path, {
            "config": {
                "providers": {
                    "active": None,
                    "records": [
                        {
                            "id": "claude-test",
                            "name": "Claude Test",
                            "harness": "claude",
                            "auth": "oauth_dir",
                            "credentials_source": str(creds),
                            "priority": 10,
                        }
                    ],
                }
            }
        })
        result = _invoke(["test", "claude-test"], cwd=tmp_path, home=tmp_path)
        assert result.exit_code == 0

    def test_test_nonexistent_cli_binary_exits_nonzero(self, tmp_path: Path):
        """fno providers test exits non-zero when CLI binary is not on PATH."""
        creds = tmp_path / ".claude"
        creds.mkdir()
        settings_path = tmp_path / ".fno" / "config.toml"
        _write_settings(settings_path, {
            "config": {
                "providers": {
                    "active": None,
                    "records": [
                        {
                            "id": "hermes-test",
                            "name": "Hermes Test",
                            "harness": "hermes",
                            "auth": "oauth_dir",
                            "credentials_source": str(creds),
                            "priority": 10,
                        }
                    ],
                }
            }
        })
        result = _invoke(["test", "hermes-test"], cwd=tmp_path, home=tmp_path)
        assert result.exit_code != 0

    def test_test_nonexistent_id_exits_nonzero(self, tmp_path: Path):
        """fno providers test nonexistent exits non-zero."""
        result = _invoke(["test", "no-such-provider"], cwd=tmp_path, home=tmp_path)
        assert result.exit_code != 0

    def test_test_missing_credentials_source_exits_nonzero(self, tmp_path: Path):
        """fno providers test exits non-zero when credentials_source path doesn't exist."""
        settings_path = tmp_path / ".fno" / "config.toml"
        _write_settings(settings_path, {
            "config": {
                "providers": {
                    "active": None,
                    "records": [
                        {
                            "id": "claude-missing-creds",
                            "name": "Claude Missing Creds",
                            "harness": "claude",
                            "auth": "oauth_dir",
                            "credentials_source": str(tmp_path / "nonexistent-creds"),
                            "priority": 10,
                        }
                    ],
                }
            }
        })
        result = _invoke(["test", "claude-missing-creds"], cwd=tmp_path, home=tmp_path)
        assert result.exit_code != 0


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
# Smoke test passes dispatch_env vars (Gemini Code Assist MEDIUM PR #199)
# ---------------------------------------------------------------------------

class TestSmokeDispatchEnv:
    """fno providers test --smoke must pass dispatch_env() env vars to the subprocess."""

    def test_smoke_passes_dispatch_env_vars_for_api_key_provider(
        self, tmp_path: Path, monkeypatch
    ):
        """fno providers test --smoke must inject dispatch_env() vars (e.g. GEMINI_API_KEY)
        into the subprocess env, not just inherit the parent process env."""
        import subprocess as subprocess_module

        # Stage a fake api_key provider (api_key needs no filesystem staging).
        settings_path = tmp_path / ".fno" / "config.toml"
        _write_settings(
            settings_path,
            {
                "config": {
                    "providers": {
                        "active": None,
                        "records": [
                            {
                                "id": "gemini-smoke-test",
                                "name": "Gemini Smoke Test",
                                "harness": "gemini",
                                "auth": "api_key",
                                "env": {"GEMINI_API_KEY": "test-smoke-key"},
                            }
                        ],
                    }
                }
            },
        )

        # Ensure `gemini` binary resolves on PATH by pointing PATH at a fake bin dir.
        fake_bin = tmp_path / "bin"
        fake_bin.mkdir()
        fake_gemini = fake_bin / "gemini"
        fake_gemini.write_text("#!/bin/sh\nexit 0\n")
        fake_gemini.chmod(0o755)
        monkeypatch.setenv("PATH", str(fake_bin))

        # Capture (cmd, env) for each subprocess.run. The smoke arm makes
        # internal `git` calls (repo-root resolution) before the CLI invocation,
        # so key off the actual `gemini --help` run rather than the first call.
        captured: list[tuple[list, dict]] = []

        def capturing_run(cmd, **kwargs):
            captured.append((list(cmd), dict(kwargs.get("env") or {})))
            import subprocess
            return subprocess.CompletedProcess(cmd, returncode=0, stdout="", stderr="")

        # Patch subprocess.run at the module level; cli.py's smoke arm uses
        # the module-level subprocess import, so this intercepts the call.
        monkeypatch.setattr(subprocess_module, "run", capturing_run)

        result = _invoke(
            ["test", "gemini-smoke-test", "--smoke"],
            cwd=tmp_path,
            home=tmp_path,
        )

        assert result.exit_code == 0, f"Expected exit 0, got {result.exit_code}: {result.output}"
        smoke_envs = [env for cmd, env in captured if cmd and cmd[0] == "gemini"]
        assert smoke_envs, "the smoke `gemini` invocation was not run"
        env_used = smoke_envs[0]
        assert "GEMINI_API_KEY" in env_used, (
            f"dispatch_env GEMINI_API_KEY must be injected into smoke subprocess env. "
            f"Got keys: {sorted(env_used.keys())}"
        )
        assert env_used["GEMINI_API_KEY"] == "test-smoke-key"


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
