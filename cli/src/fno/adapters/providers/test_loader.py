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
# Task 1.2: atomic_mutate_settings - read+mutate+write under exclusive lock.
# Phase 01 of provider rotation failover (ab-9728b70b).
# ---------------------------------------------------------------------------

class TestAtomicMutate:
    def test_hp1_single_writer_mutation_persists(self, tmp_path: Path):
        from fno.adapters.providers.loader import atomic_mutate_settings

        settings_path = tmp_path / "config.toml"
        _write_settings(settings_path, {
            "config": {"providers": {"active": "foo", "records": []}},
        })

        def mutator(d: dict) -> dict:
            d["providers"]["active"] = "bar"
            return d

        atomic_mutate_settings(mutator, settings_path=settings_path)

        loaded = tomllib.loads(settings_path.read_text())
        assert loaded["providers"]["active"] == "bar"

    def test_hp1b_other_keys_preserved(self, tmp_path: Path):
        from fno.adapters.providers.loader import atomic_mutate_settings

        settings_path = tmp_path / "config.toml"
        _write_settings(settings_path, {
            "config": {
                "providers": {"active": "foo", "records": []},
                "other_section": {"keep": "me"},
            },
            "top_level_other": "alive",
        })

        def mutator(d: dict) -> dict:
            d["providers"]["active"] = "bar"
            return d

        atomic_mutate_settings(mutator, settings_path=settings_path)

        loaded = tomllib.loads(settings_path.read_text())
        assert loaded["other_section"]["keep"] == "me"
        assert loaded["top_level_other"] == "alive"

    def test_hp2_read_during_mutation_sees_consistent_state(self, tmp_path: Path):
        """While a writer is mid-mutation, a non-locking reader sees either the
        pre-write or post-write state, never a half-written file. Verified by
        spawning a writer in a thread that holds the lock, reading from the
        main thread, and asserting the file is parseable YAML at every read.
        """
        import threading
        import time

        from fno.adapters.providers.loader import atomic_mutate_settings

        settings_path = tmp_path / "config.toml"
        _write_settings(settings_path, {
            "config": {"providers": {"active": "foo", "records": []}},
        })

        # Writer holds the lock for ~80ms inside mutator; reader polls during.
        def slow_mutator(d: dict) -> dict:
            time.sleep(0.08)
            d["providers"]["active"] = "bar"
            return d

        observed_states = []
        write_done = threading.Event()

        def observe():
            try:
                text = settings_path.read_text()
                parsed = tomllib.loads(text)
                observed_states.append(parsed["providers"]["active"])
            except (tomllib.TOMLDecodeError, KeyError, TypeError):
                observed_states.append("CORRUPT")

        def reader_loop():
            # Poll for the whole mutation, then take one GUARANTEED post-write
            # read. A fixed iteration count raced the writer: 20 polls at 5ms is
            # ~100ms of budget against an 80ms mutation, so on a loaded runner
            # (xdist, shared CI box) the reader finished before the write landed
            # and "bar" was never observed - a green-or-red coin flip that says
            # nothing about the atomicity this test exists to check.
            while not write_done.wait(timeout=0.005):
                observe()
            observe()

        reader_thread = threading.Thread(target=reader_loop)
        reader_thread.start()

        atomic_mutate_settings(slow_mutator, settings_path=settings_path)
        write_done.set()
        reader_thread.join()

        # Every observation must be either "foo" (pre) or "bar" (post),
        # never CORRUPT.
        assert "CORRUPT" not in observed_states
        assert set(observed_states).issubset({"foo", "bar"})
        assert "bar" in observed_states  # final read should see the new value

    def test_err1_mutator_exception_leaves_file_unchanged(self, tmp_path: Path):
        from fno.adapters.providers.loader import atomic_mutate_settings

        settings_path = tmp_path / "config.toml"
        _write_settings(settings_path, {
            "config": {"providers": {"active": "foo", "records": []}},
        })
        original_text = settings_path.read_text()

        def boom(d: dict) -> dict:
            d["providers"]["active"] = "wont-stick"
            raise RuntimeError("mutator boom")

        with pytest.raises(RuntimeError, match="mutator boom"):
            atomic_mutate_settings(boom, settings_path=settings_path)

        assert settings_path.read_text() == original_text

    def test_edge1_concurrent_mutators_are_serialized(self, tmp_path: Path):
        """Two parallel sessions racing on settings.yaml swap. Cites what-if
        finding #4: 'Two parallel sessions racing on settings.yaml swap.'

        GIVEN sessions A and B both call atomic_mutate_settings to swap the
        active provider WHEN both run concurrently THEN exactly one wins
        the lock first, settings.yaml is parseable YAML at every observable
        instant, and the final file reflects exactly one of the contributed
        states.
        """
        import threading
        import time

        from fno.adapters.providers.loader import atomic_mutate_settings

        settings_path = tmp_path / "config.toml"
        _write_settings(settings_path, {
            "config": {"providers": {"active": "foo", "records": []}},
        })

        def mutator_factory(target: str):
            def m(d: dict) -> dict:
                # Hold the lock briefly so the threads actually contend.
                time.sleep(0.01)
                d["providers"]["active"] = target
                return d
            return m

        threads = []
        for target in ("bar", "baz"):
            t = threading.Thread(
                target=lambda tg=target: atomic_mutate_settings(
                    mutator_factory(tg), settings_path=settings_path,
                ),
            )
            threads.append(t)
        for t in threads:
            t.start()
        for t in threads:
            t.join()

        loaded = tomllib.loads(settings_path.read_text())
        assert loaded["providers"]["active"] in {"bar", "baz"}

    def test_edge1b_high_contention_race(self, tmp_path: Path):
        """Many threads × many mutations: file must always parse, final value
        must come from one of the contributors, no lost-update corruption."""
        import threading
        from fno.adapters.providers.loader import atomic_mutate_settings

        settings_path = tmp_path / "config.toml"
        _write_settings(settings_path, {
            "config": {"providers": {"active": "init", "counter": 0, "records": []}},
        })

        def increment(d: dict) -> dict:
            d["providers"]["counter"] += 1
            return d

        def worker():
            for _ in range(50):
                atomic_mutate_settings(increment, settings_path=settings_path)

        threads = [threading.Thread(target=worker) for _ in range(10)]
        for t in threads:
            t.start()
        for t in threads:
            t.join()

        loaded = tomllib.loads(settings_path.read_text())
        # Every thread did 50 increments, all under the exclusive lock.
        # No lost updates: counter == 10*50 = 500.
        assert loaded["providers"]["counter"] == 500

    def test_edge2_orphan_tempfiles_ignored(self, tmp_path: Path):
        """A writer that crashed before os.replace leaves a `.tmp.*` orphan
        in the parent directory. The next reader/writer must ignore it and
        operate on the canonical settings.yaml only."""
        from fno.adapters.providers.loader import atomic_mutate_settings

        settings_path = tmp_path / "config.toml"
        _write_settings(settings_path, {
            "config": {"providers": {"active": "foo", "records": []}},
        })

        # Plant an orphan tempfile
        orphan = tmp_path / ".config.toml.tmp.99999.0"
        orphan.write_text("garbage: not_yaml: ::: invalid")

        def mutator(d: dict) -> dict:
            d["providers"]["active"] = "bar"
            return d

        atomic_mutate_settings(mutator, settings_path=settings_path)

        loaded = tomllib.loads(settings_path.read_text())
        assert loaded["providers"]["active"] == "bar"
        # Orphan still on disk (no auto-cleanup is part of this task) but the
        # canonical file is intact.
        assert settings_path.exists()


# ---------------------------------------------------------------------------
# Task 1.3: read_active_provider_atomic - shared-lock atomic snapshot read.
# Phase 01 of provider rotation failover (ab-9728b70b).
# ---------------------------------------------------------------------------

class TestAtomicRead:
    def _seed(self, settings_path: Path, *, active: str = "claude-primary") -> None:
        _write_settings(settings_path, {
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
                            "base_url": "https://api.anthropic.com",
                        },
                        {
                            "id": "claude-secondary",
                            "name": "Claude Secondary",
                            "harness": "claude",
                            "auth": "api_key",
                            "env": {"ANTHROPIC_API_KEY": "key-B"},
                            "priority": 20,
                        },
                    ],
                }
            }
        })

    def test_hp1_returns_frozen_snapshot_for_active(self, tmp_path: Path):
        from fno.adapters.providers.loader import (
            ActiveProviderSnapshot,
            read_active_provider_atomic,
        )

        settings_path = tmp_path / "config.toml"
        self._seed(settings_path, active="claude-primary")

        snap = read_active_provider_atomic(settings_path=settings_path)

        assert isinstance(snap, ActiveProviderSnapshot)
        assert snap.id == "claude-primary"
        assert snap.harness == "claude"
        assert snap.auth == "oauth_dir"
        assert snap.base_url == "https://api.anthropic.com"
        # Snapshot is frozen
        with pytest.raises((AttributeError, Exception)):
            snap.id = "tampered"  # type: ignore[misc]

    def test_hp2_concurrent_readers_do_not_serialize(self, tmp_path: Path):
        """Multiple shared-lock readers proceed without blocking each other.

        Sanity bound: 10 concurrent reads should complete much faster than
        10 sequential reads at the same artificial latency. We verify the
        ratio is well under 10×.
        """
        import threading
        import time

        from fno.adapters.providers.loader import read_active_provider_atomic

        settings_path = tmp_path / "config.toml"
        self._seed(settings_path)

        # Warm up file cache
        read_active_provider_atomic(settings_path=settings_path)

        # Sequential baseline: 10 reads
        sequential_start = time.perf_counter()
        for _ in range(10):
            read_active_provider_atomic(settings_path=settings_path)
        sequential_dur = time.perf_counter() - sequential_start

        # Concurrent: 10 readers in parallel
        barrier = threading.Barrier(10)

        def reader():
            barrier.wait()
            read_active_provider_atomic(settings_path=settings_path)

        concurrent_start = time.perf_counter()
        threads = [threading.Thread(target=reader) for _ in range(10)]
        for t in threads:
            t.start()
        for t in threads:
            t.join()
        concurrent_dur = time.perf_counter() - concurrent_start

        # Concurrent must NOT be massively slower than sequential. If the
        # shared lock degenerated to exclusive (forcing serialization),
        # 10 threads would take ~10x sequential time. Allow up to 3x to
        # absorb scheduler noise on a busy CI machine while still catching
        # an actual serialization regression. The structural invariant
        # (LOCK_SH not LOCK_EX) is the load-bearing test in
        # test_edge1_writer_blocks_reader_then_reader_sees_post_state.
        assert concurrent_dur < max(sequential_dur * 3.0, 0.5), (
            f"Shared-lock readers appear serialized: "
            f"sequential={sequential_dur:.3f}s, concurrent={concurrent_dur:.3f}s"
        )

    def test_err1_lock_file_missing_is_created_on_demand(self, tmp_path: Path):
        from fno.adapters.providers.loader import (
            _settings_lock_path,
            read_active_provider_atomic,
        )

        settings_path = tmp_path / "config.toml"
        self._seed(settings_path)

        lock_path = _settings_lock_path(settings_path)
        assert not lock_path.exists()

        snap = read_active_provider_atomic(settings_path=settings_path)
        assert snap.id == "claude-primary"
        assert lock_path.exists()  # created on demand

    def test_edge1_writer_blocks_reader_then_reader_sees_post_state(
        self, tmp_path: Path,
    ):
        """Cites what-if finding #6: 'Auth-mismatch cascade from non-atomic
        credential swap.' GIVEN session A is mid-mutate (LOCK_EX) WHEN
        session B calls read_active_provider_atomic THEN B blocks until A
        finishes, then reads either pre- or post-swap state, NEVER a
        mismatched (active_id, auth) pair.
        """
        import threading
        import time

        from fno.adapters.providers.loader import (
            atomic_mutate_settings,
            read_active_provider_atomic,
        )

        settings_path = tmp_path / "config.toml"
        self._seed(settings_path, active="claude-primary")

        # Writer holds the lock for ~80ms while it changes both active and
        # the records.  A reader during that window must block until the
        # writer releases, then see the post-swap state.
        writer_started = threading.Event()

        def slow_swap(d: dict) -> dict:
            writer_started.set()
            time.sleep(0.08)
            d["providers"]["active"] = "claude-secondary"
            return d

        observed = []

        def reader_after_writer():
            # Wait for the writer to be inside the lock before reading.
            writer_started.wait(timeout=1.0)
            time.sleep(0.005)  # ensure writer has actually called flock
            snap = read_active_provider_atomic(settings_path=settings_path)
            observed.append(snap)

        reader_t = threading.Thread(target=reader_after_writer)
        reader_t.start()
        atomic_mutate_settings(slow_swap, settings_path=settings_path)
        reader_t.join()

        assert len(observed) == 1
        snap = observed[0]
        # Reader saw post-swap state (writer finished, then reader unblocked).
        assert snap.id == "claude-secondary"
        # Critical invariant: id and auth come from the SAME record. Never
        # (claude-secondary, oauth_dir) - that would be the auth-mismatch.
        assert snap.auth == "api_key"

    def test_edge2_active_id_missing_record_raises_structured_error(
        self, tmp_path: Path,
    ):
        from fno.adapters.providers.loader import (
            MissingActiveProvider,
            read_active_provider_atomic,
        )

        settings_path = tmp_path / "config.toml"
        # active points to a record id that isn't in records
        _write_settings(settings_path, {
            "config": {
                "providers": {
                    "active": "ghost",
                    "records": [
                        {
                            "id": "claude-primary",
                            "name": "Claude Primary",
                            "harness": "claude",
                            "auth": "oauth_dir",
                            "credentials_source": "~/.claude",
                        },
                    ],
                }
            }
        })

        with pytest.raises(MissingActiveProvider) as exc_info:
            read_active_provider_atomic(settings_path=settings_path)
        # Bad id is named in the exception
        assert "ghost" in str(exc_info.value)

    def test_edge2b_no_active_set_raises_structured_error(self, tmp_path: Path):
        from fno.adapters.providers.loader import (
            MissingActiveProvider,
            read_active_provider_atomic,
        )

        settings_path = tmp_path / "config.toml"
        _write_settings(settings_path, {
            "config": {"providers": {"active": None, "records": []}},
        })

        with pytest.raises(MissingActiveProvider):
            read_active_provider_atomic(settings_path=settings_path)

    def test_pricing_slot_returns_when_present(self, tmp_path: Path):
        """The snapshot exposes a `pricing` field that is None when the
        record has no pricing entry, and a dict when it does. The schema
        for pricing lands in task 2.2; the slot must already exist on the
        snapshot type so subprocess env snapshots in task 2b.1 can carry
        it forward without a follow-up shape change."""
        from fno.adapters.providers.loader import read_active_provider_atomic

        settings_path = tmp_path / "config.toml"
        _write_settings(settings_path, {
            "config": {
                "providers": {
                    "active": "claude-primary",
                    "records": [
                        {
                            "id": "claude-primary",
                            "name": "Claude Primary",
                            "harness": "claude",
                            "auth": "oauth_dir",
                            "credentials_source": "~/.claude",
                            "pricing": {"input_per_million": 3.0, "output_per_million": 15.0},
                        }
                    ],
                }
            }
        })

        snap = read_active_provider_atomic(settings_path=settings_path)
        assert snap.pricing == {"input_per_million": 3.0, "output_per_million": 15.0}

    def test_pricing_slot_is_none_when_absent(self, tmp_path: Path):
        from fno.adapters.providers.loader import read_active_provider_atomic

        settings_path = tmp_path / "config.toml"
        self._seed(settings_path)
        snap = read_active_provider_atomic(settings_path=settings_path)
        assert snap.pricing is None


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


class TestMutableAccountsBlockShapes:
    """The write helper must reach the block in every shape the READER accepts.

    `_extract_accounts_block` accepts four: {flat,wrapped} x {accounts,providers}.
    A writer that handles fewer does not fail loudly - it installs a fresh empty
    top-level block, the caller's edit lands there, and `_flatten_config` then
    merges the untouched original over it. The combo write or failover flip
    reports success against a file that never changed.
    """

    @pytest.mark.parametrize(
        "raw",
        [
            pytest.param({"accounts": {"active": "a"}}, id="flat-canonical"),
            pytest.param({"config": {"accounts": {"active": "a"}}}, id="wrapped-canonical"),
            pytest.param({"providers": {"active": "a"}}, id="flat-legacy"),
            pytest.param({"config": {"providers": {"active": "a"}}}, id="wrapped-legacy"),
            pytest.param({"target": {"x": 1}}, id="absent"),
        ],
    )
    def test_mutation_survives_the_flatten(self, raw):
        from fno.adapters.providers.loader import (
            _flatten_config,
            mutable_accounts_block,
        )

        mutable_accounts_block(raw)["active_combo"] = "written"
        flat = _flatten_config(raw)

        assert flat["accounts"]["active_combo"] == "written"
        # Moved, not copied: no pre-rename block survives to split the state.
        assert "providers" not in flat
        assert "providers" not in raw.get("config", {})

    def test_preexisting_keys_are_preserved_not_replaced(self):
        """The helper must adopt the existing block, never install a fresh one."""
        from fno.adapters.providers.loader import (
            _flatten_config,
            mutable_accounts_block,
        )

        raw = {"config": {"accounts": {"active": "a", "records": [{"id": "keep"}]}}}
        mutable_accounts_block(raw)["active_combo"] = "written"
        flat = _flatten_config(raw)

        assert flat["accounts"]["active"] == "a"
        assert flat["accounts"]["records"] == [{"id": "keep"}]
        assert flat["accounts"]["active_combo"] == "written"


class TestMutableAccountsBlockCompetingBlocks:
    """A transitional file can hold more than one account block at once.

    The canonical-present fast path used to return before draining the others,
    which is not inert: a surviving `config.accounts` is merged OVER the edited
    top-level block by `_flatten_config` (the write is lost while the caller
    reports success), and a surviving `providers` splits the state.
    """

    @pytest.mark.parametrize(
        "raw",
        [
            pytest.param(
                {"accounts": {"active": "top"}, "config": {"accounts": {"active": "nested"}}},
                id="canonical-flat-plus-canonical-wrapped",
            ),
            pytest.param(
                {"accounts": {"active": "top"}, "providers": {"active": "legacy"}},
                id="canonical-flat-plus-legacy-flat",
            ),
            pytest.param(
                {"accounts": {"active": "top"}, "config": {"providers": {"active": "legacy"}}},
                id="canonical-flat-plus-legacy-wrapped",
            ),
        ],
    )
    def test_competitors_are_drained_so_the_write_survives(self, raw):
        from fno.adapters.providers.loader import (
            _flatten_config,
            mutable_accounts_block,
        )

        mutable_accounts_block(raw)["active_combo"] = "written"
        flat = _flatten_config(raw)

        assert flat["accounts"]["active_combo"] == "written"
        # The canonical top-level block still wins on precedence, matching the
        # reader, and no competitor survives to shadow or split it.
        assert flat["accounts"]["active"] == "top"
        assert "providers" not in flat
        assert "accounts" not in raw.get("config", {})
        assert "providers" not in raw.get("config", {})


class TestShadowedAccountBlockIsAnnounced:
    """Draining a shadowed block is correct; doing it silently is not.

    The sharp case is an EMPTY canonical block shadowing a populated duplicate:
    the empty one wins on precedence (it is a dict) and the records go with it.
    """

    def test_empty_canonical_shadowing_populated_nested_warns(self, caplog):
        from fno.adapters.providers.loader import mutable_accounts_block

        raw = {"accounts": {}, "config": {"accounts": {"records": [{"id": "a"}]}}}
        with caplog.at_level("WARNING", logger="fno.adapters.providers.loader"):
            mutable_accounts_block(raw)
        assert "discarding shadowed account block" in caplog.text

    def test_no_competitor_is_silent(self, caplog):
        from fno.adapters.providers.loader import mutable_accounts_block

        raw = {"accounts": {"active": "a"}}
        with caplog.at_level("WARNING", logger="fno.adapters.providers.loader"):
            mutable_accounts_block(raw)
        assert "discarding shadowed" not in caplog.text

    def test_adopted_legacy_block_is_not_reported_as_discarded(self, caplog):
        """A legacy block that gets MOVED onto the canonical key is kept, not
        dropped, so warning about it would be a lie."""
        from fno.adapters.providers.loader import mutable_accounts_block

        raw = {"providers": {"records": [{"id": "a"}]}}
        with caplog.at_level("WARNING", logger="fno.adapters.providers.loader"):
            block = mutable_accounts_block(raw)
        assert block["records"] == [{"id": "a"}]
        assert "discarding shadowed" not in caplog.text
