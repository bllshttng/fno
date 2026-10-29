"""Tests for load_providers and save_providers.

Run: cd cli && uv run pytest src/fno/adapters/providers/test_loader.py -v
"""
from __future__ import annotations

import logging
from pathlib import Path

import pytest
import tomli_w
import tomllib


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


# ---------------------------------------------------------------------------
# Fixture helpers
# ---------------------------------------------------------------------------

def _valid_providers_block(active: str = "claude-primary") -> dict:
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
                        "credentials_source": "~/.claude",
                        "priority": 10,
                    },
                    {
                        "id": "gemini-backup",
                        "name": "Gemini Backup",
                        "harness": "gemini",
                        "auth": "api_key",
                        "env": {"GEMINI_API_KEY": "key"},
                        "priority": 20,
                    },
                ],
            }
        }
    }


# ---------------------------------------------------------------------------
# AC01.1-HP: Valid config loads cleanly
# ---------------------------------------------------------------------------

class TestLoadProvidersValid:


    def test_unknown_record_metadata_loads_warns_and_round_trips(
        self, tmp_path: Path, monkeypatch: pytest.MonkeyPatch, caplog: pytest.LogCaptureFixture
    ):
        """AC1-HP: route-backed records retain unknown metadata on round trips."""
        from fno.adapters.providers.loader import load_providers, save_providers

        block = {
            "config": {
                "accounts": {
                    "active": "zai-primary",
                    "records": [
                        {
                            "id": "zai-primary",
                            "name": "Zai Primary",
                            "harness": "claude",
                            "auth": "api_key",
                            "route": "zai/glm-5.3[1m]",
                            "operator_note": "route-owned",
                        }
                    ],
                }
            }
        }
        settings = tmp_path / ".fno" / "config.toml"
        _write_settings(settings, block)

        with caplog.at_level(logging.WARNING, logger="fno.adapters.providers.loader"):
            result = load_providers(repo_root=tmp_path)

        record = result.by_id["zai-primary"]
        assert record.model_dump()["route"] == "zai/glm-5.3[1m]"
        assert record.model_dump()["operator_note"] == "route-owned"
        assert "zai-primary" in caplog.text
        assert "operator_note" in caplog.text

        monkeypatch.setenv("PWD", str(tmp_path))
        monkeypatch.chdir(tmp_path)
        save_providers(result, scope="project")
        round_tripped = load_providers(repo_root=tmp_path).by_id["zai-primary"]
        assert round_tripped.model_dump()["route"] == "zai/glm-5.3[1m]"
        assert round_tripped.model_dump()["operator_note"] == "route-owned"


    def test_auto_switch_round_trips_through_save(self, tmp_path: Path, monkeypatch):
        """Peer review (PR#366): save_providers serializes auto_switch=True off the
        object so a write-back never silently disarms the feature.

        save_providers(scope="project") targets ``$PWD/.fno/config.toml``, and
        monkeypatch.chdir does NOT update $PWD - so PWD is pinned to tmp_path here
        or the write escapes the sandbox and clobbers the real config."""
        from fno.adapters.providers.loader import load_providers, save_providers
        from fno.adapters.providers.model import ProvidersConfig

        settings = tmp_path / ".fno" / "config.toml"
        _write_settings(settings, _valid_providers_block())
        monkeypatch.setenv("PWD", str(tmp_path))
        monkeypatch.chdir(tmp_path)
        cfg = load_providers(repo_root=tmp_path)
        armed = ProvidersConfig(records=cfg.records, active=cfg.active, auto_switch=True)
        save_providers(armed, scope="project")
        assert settings.read_text().count("auto_switch") == 1   # written to the sandbox file
        assert load_providers(repo_root=tmp_path).auto_switch is True


# ---------------------------------------------------------------------------
# AC01.3-EDGE: Empty list / missing section is not an error
# ---------------------------------------------------------------------------

class TestLoadProvidersEmpty:


    def test_global_config_pollution_does_not_leak(
        self, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
    ):
        """AC2-FR (ab-a1118224): a globally-configured ~/.fno/settings.yaml
        must not leak into ``repo_root=tmp_path`` test isolation.

        Simulates the failure mode the developer was seeing locally: a user
        with their own provider config sees this test class fail with stray
        records because the loader falls back to ``Path.home() /
        .fno / settings.yaml``. The ``FNO_GLOBAL_SETTINGS_PATH`` env
        var override (set to a fake home with a populated providers block
        here) demonstrates that the override is the seam tests use to stay
        clean. The autouse conftest's ``/dev/null`` pin handles the default
        case; this test exercises the seam directly.
        """
        from fno.adapters.providers.loader import load_providers

        # Fake "global" config with a populated providers block.
        fake_global = tmp_path / "fake_home" / ".fno" / "config.toml"
        _write_settings(fake_global, _valid_providers_block(active="claude-primary"))

        # Point the loader's "global" candidate at the fake. Project-local
        # (tmp_path / .fno / settings.yaml) is absent.
        monkeypatch.setenv("FNO_GLOBAL_SETTINGS_PATH", str(fake_global))

        # The loader sees the global config because project-local is absent.
        result = load_providers(repo_root=tmp_path)
        assert result.active == "claude-primary"
        assert len(result.records) == 2  # claude-primary, gemini-backup

        # Now flip the override to /dev/null (mirrors the conftest default).
        # The loader returns empty because both candidates are non-existent
        # (project-local absent, global == /dev/null which parses to nothing).
        monkeypatch.setenv("FNO_GLOBAL_SETTINGS_PATH", "/dev/null")
        result = load_providers(repo_root=tmp_path)
        assert result.records == []
        assert result.active is None


# ---------------------------------------------------------------------------
# AC01.2-ERR: Invalid record raises ProviderConfigError naming the id
# ---------------------------------------------------------------------------


# ---------------------------------------------------------------------------
# Precedence: project-local overrides global
# ---------------------------------------------------------------------------


# ---------------------------------------------------------------------------
# AC01.5-EDGE: active references non-existent id
# ---------------------------------------------------------------------------


# ---------------------------------------------------------------------------
# save_providers: atomic write + key preservation
# ---------------------------------------------------------------------------

class TestSaveProviders:
    def test_save_project_scope_writes_settings(self, tmp_path: Path, monkeypatch):
        """save_providers with scope='project' writes to cwd/.fno/settings.yaml."""
        from fno.adapters.providers.loader import save_providers
        from fno.adapters.providers.model import ProviderRecord, ProvidersConfig

        # Work in tmp_path as cwd; set PWD too so loader uses the same path.
        monkeypatch.setenv("PWD", str(tmp_path))
        monkeypatch.chdir(tmp_path)

        record = ProviderRecord(
            id="claude-saved",
            name="Saved",
            harness="claude",
            auth="oauth_dir",
            credentials_source=Path("~/.claude"),
        )
        cfg = ProvidersConfig(records=[record], active="claude-saved")

        save_providers(cfg, scope="project")

        output = tmp_path / ".fno" / "config.toml"
        assert output.exists()
        data = tomllib.loads(output.read_text())
        providers = data["accounts"]
        assert providers["active"] == "claude-saved"
        assert len(providers["records"]) == 1
        assert providers["records"][0]["id"] == "claude-saved"

    def test_save_preserves_other_keys(self, tmp_path: Path, monkeypatch):
        """save_providers preserves existing non-providers keys in settings.yaml."""
        from fno.adapters.providers.loader import save_providers
        from fno.adapters.providers.model import ProviderRecord, ProvidersConfig

        monkeypatch.setenv("PWD", str(tmp_path))
        monkeypatch.chdir(tmp_path)

        # Write existing settings with other keys
        existing = tmp_path / ".fno" / "config.toml"
        _write_settings(
            existing,
            {
                "config": {
                    "v2_enabled": True,
                    "some_other_setting": "preserved",
                }
            },
        )

        record = ProviderRecord(
            id="new-record",
            name="New",
            harness="claude",
            auth="oauth_dir",
            credentials_source=Path("~/.claude"),
        )
        cfg = ProvidersConfig(records=[record], active=None)

        save_providers(cfg, scope="project")

        data = tomllib.loads(existing.read_text())
        # Other config keys preserved
        assert data["v2_enabled"] is True
        assert data["some_other_setting"] == "preserved"
        # providers block updated
        assert "accounts" in data
        assert len(data["accounts"]["records"]) == 1

    def test_save_preserves_provider_subkeys(self, tmp_path: Path, monkeypatch):
        """save_providers keeps provider subkeys it does not rebuild (x-5d3e).

        A normal provider edit (use/add/remove) must not silently drop
        config.providers.quota / combos / failover - rebuilding the block from
        only records+active would turn an operator's defer_dispatch back off.
        """
        from fno.adapters.providers.loader import save_providers
        from fno.adapters.providers.model import ProviderRecord, ProvidersConfig

        monkeypatch.setenv("PWD", str(tmp_path))
        monkeypatch.chdir(tmp_path)

        existing = tmp_path / ".fno" / "config.toml"
        _write_settings(
            existing,
            {
                "config": {
                    "providers": {
                        "records": [],
                        "quota": {"defer_dispatch": True, "defer_threshold_pct": 80},
                        "combos": {"c1": {"providers": ["a"]}},
                    }
                }
            },
        )

        record = ProviderRecord(
            id="a", name="A", harness="claude", auth="oauth_dir",
            credentials_source=Path("~/.claude"),
        )
        save_providers(ProvidersConfig(records=[record], active="a"), scope="project")

        data = tomllib.loads(existing.read_text())
        assert data["accounts"]["quota"] == {"defer_dispatch": True, "defer_threshold_pct": 80}
        assert data["accounts"]["combos"] == {"c1": {"providers": ["a"]}}
        assert data["accounts"]["active"] == "a"

    def test_save_global_scope(self, tmp_path: Path, monkeypatch):
        """save_providers with scope='global' writes to home/.fno/settings.yaml."""
        from fno.adapters.providers.loader import save_providers
        from fno.adapters.providers.model import ProviderRecord, ProvidersConfig

        fake_home = tmp_path / "home"
        monkeypatch.setattr(Path, "home", staticmethod(lambda: fake_home))

        record = ProviderRecord(
            id="global-write",
            name="Global Write",
            harness="gemini",
            auth="api_key",
            env={"GEMINI_API_KEY": "k"},
        )
        cfg = ProvidersConfig(records=[record], active="global-write")

        save_providers(cfg, scope="global")

        output = fake_home / ".fno" / "config.toml"
        assert output.exists()
        data = tomllib.loads(output.read_text())
        assert data["accounts"]["active"] == "global-write"


# ---------------------------------------------------------------------------
# Fix 1 regression: save_providers refuses to overwrite a corrupt settings.yaml
# ---------------------------------------------------------------------------

class TestSaveProvidersCorruptFile:
    def test_save_providers_refuses_to_overwrite_corrupt_settings(
        self, tmp_path: Path, monkeypatch
    ):
        """save_providers raises ProviderConfigError when settings.yaml has a YAML syntax
        error, and leaves the original (corrupt) file unchanged on disk."""
        from fno.adapters.providers.loader import save_providers
        from fno.adapters.providers.model import (
            ProviderConfigError,
            ProviderRecord,
            ProvidersConfig,
        )

        monkeypatch.setenv("PWD", str(tmp_path))
        monkeypatch.chdir(tmp_path)

        # Write a settings.yaml that has valid keys AND a syntactically broken line.
        corrupt_content = (
            "config:\n"
            "  budget:\n"
            "    daily_limit: 5.00\n"
            "  : this_is_broken_yaml: [unclosed\n"  # deliberately malformed
        )
        fno_dir = tmp_path / ".fno"
        fno_dir.mkdir(parents=True, exist_ok=True)
        settings_path = fno_dir / "config.toml"
        settings_path.write_text(corrupt_content, encoding="utf-8")

        original_bytes = settings_path.read_bytes()

        record = ProviderRecord(
            id="new-provider",
            name="New Provider",
            harness="claude",
            auth="oauth_dir",
            credentials_source=Path("~/.claude"),
        )
        cfg = ProvidersConfig(records=[record], active=None)

        with pytest.raises(ProviderConfigError) as exc_info:
            save_providers(cfg, scope="project")

        # Error message must mention the broken file
        assert settings_path.name in str(exc_info.value) or str(settings_path) in str(exc_info.value)

        # Original file must be untouched
        assert settings_path.read_bytes() == original_bytes

        # No temp file leaked under .fno/
        leaked = [f for f in fno_dir.iterdir() if f != settings_path]
        assert leaked == [], f"Temp file(s) leaked: {leaked}"


# ---------------------------------------------------------------------------
# Fix 2 regression: load_providers expands tilde in credentials_source
# ---------------------------------------------------------------------------


# ---------------------------------------------------------------------------
# Fix 4 regression: load_providers rejects duplicate ids in YAML
# ---------------------------------------------------------------------------


# ---------------------------------------------------------------------------
# PWD env var respected (Gemini Code Assist MEDIUM finding PR #199)
# ---------------------------------------------------------------------------

class TestPWDRespected:
    """Anchor discovery with repo_root=None: save_providers uses PWD (the
    legacy write anchor); load_providers resolves the repo root, the anchor
    `config set --local` writes through."""

    def test_load_providers_uses_pwd_env_var(self, tmp_path: Path, monkeypatch):
        """load_providers(repo_root=None) resolves the repo root via
        fno.paths.resolve_repo_root (FNO_REPO_ROOT is its test hook), so reads
        share one anchor with `config set --local` writes."""
        from fno.adapters.providers.loader import load_providers

        settings = tmp_path / ".fno" / "config.toml"
        _write_settings(
            settings,
            {
                "config": {
                    "providers": {
                        "active": "claude-pwd-test",
                        "records": [
                            {
                                "id": "claude-pwd-test",
                                "name": "Claude PWD Test",
                                "harness": "claude",
                                "auth": "oauth_dir",
                                "credentials_source": "~/.claude",
                            },
                        ],
                    }
                }
            },
        )

        # Pin the repo root at tmp_path (settings lives there). Readers anchor
        # on resolve_repo_root - the same resolver `config set --local` writes
        # through - and FNO_REPO_ROOT is its test hook.

        monkeypatch.setenv("FNO_REPO_ROOT", str(tmp_path))
        config = load_providers()  # repo_root=None - resolves FNO_REPO_ROOT
        assert "claude-pwd-test" in config.by_id, (
            "load_providers with repo_root=None must discover settings via the repo root"
        )

    def test_save_providers_uses_pwd_env_var(self, tmp_path: Path, monkeypatch):
        """save_providers(scope='project') must write to PWD/.fno/settings.yaml,
        not to cwd/.fno/settings.yaml."""
        from fno.adapters.providers.loader import save_providers
        from fno.adapters.providers.model import ProvidersConfig, ProviderRecord

        monkeypatch.setenv("PWD", str(tmp_path))
        monkeypatch.chdir(tmp_path.parent)  # cwd != PWD

        record = ProviderRecord(
            id="save-pwd-test",
            name="Save PWD Test",
            harness="claude",
            auth="oauth_dir",
            credentials_source=Path.home() / ".claude",
        )
        config = ProvidersConfig(records=[record], active=None)
        save_providers(config, scope="project")

        expected_path = tmp_path / ".fno" / "config.toml"
        assert expected_path.exists(), (
            f"save_providers must write to PWD/.fno/settings.yaml, not cwd. "
            f"Expected {expected_path}"
        )


# ---------------------------------------------------------------------------
# Task 1.1: config.agents.<name>.provider — per-agent provider binding
# Part of: ab-978e93ed (per-agent sigma-review routing, Spec 3)
# ---------------------------------------------------------------------------

class TestLoadAgents:


    def test_reserved_keys_match_agents_block_schema(self):
        """The loader cannot import fno.config at module scope (bootstrap order),
        so the reserved-key set is a literal. This is the guard that keeps it from
        drifting: set equality, so a REMOVED field fails too."""
        from fno.config import AgentsBlock

        from fno.adapters.providers.loader import _AGENTS_RESERVED_KEYS

        # The comparison is Python field NAME against TOML KEY, which holds only
        # while no field carries an alias. ProviderRecord in this same module
        # renames with AliasChoices("harness", "cli"), so the pattern is live
        # here. Were AgentsBlock.defaults ever renamed that way, updating the
        # literal to the new field name would keep this green while dropping the
        # still-valid `defaults` key out of the exclusion - reintroducing exactly
        # the bug the set exists to prevent. Refuse that setup rather than handle
        # it: this fails, and whoever adds the alias switches to an alias-aware
        # key set.
        assert all(
            f.validation_alias is None and f.alias is None
            for f in AgentsBlock.model_fields.values()
        ), "AgentsBlock gained a field alias; compare alias-aware keys, not names"

        # `max_lanes` is the ONE tolerated legacy spelling (renamed to
        # provider_limits in x-3f84 W5; still parsed by the block's
        # before-validator, so it must stay reserved against provider pins).
        assert set(_AGENTS_RESERVED_KEYS) == set(AgentsBlock.model_fields) | {"max_lanes"}

    def test_reserved_key_carrying_a_pin_warns(self, tmp_path: Path, caplog):
        """A reserved key that is ALSO binding-shaped is skipped, but not
        silently: without a diagnostic the operator's worker routes to the
        default account and nothing anywhere says why."""
        from fno.adapters.providers.loader import load_providers

        base = _valid_providers_block()
        base["agents"] = {"codex": {"provider": "claude-primary"}}
        settings = tmp_path / ".fno" / "config.toml"
        _write_settings(settings, base)

        with caplog.at_level(logging.WARNING):
            result = load_providers(repo_root=tmp_path)

        assert result.agents == {}
        assert any("reserved settings key" in r.message for r in caplog.records)

    def test_defaults_block_does_not_warn(self, tmp_path: Path, caplog):
        """The shipped spawn default must stay quiet: it carries `provider` by
        design, so warning there would fire on every valid config."""
        from fno.adapters.providers.loader import load_providers

        harness_default = "codex"
        base = _valid_providers_block()
        base["agents"] = {"defaults": {"provider": harness_default}}
        settings = tmp_path / ".fno" / "config.toml"
        _write_settings(settings, base)

        with caplog.at_level(logging.WARNING):
            result = load_providers(repo_root=tmp_path)

        assert result.agents == {}
        assert not any("reserved settings key" in r.message for r in caplog.records)
