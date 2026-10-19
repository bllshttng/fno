"""Integration tests for `fno setup migrate-paths`.

Tests cover:
- AC2b-EDGE: Race - two threads call run_migration concurrently; filelock serializes
- AC2b-FR: Idempotent re-run without --force -> no-op
- Existing keys survive both the --force merge and the non-force backup path

All tests pass `settings_root=` directly for isolation.
Autouse fixture pins FNO_REPO_ROOT (feedback_fno_repo_root_leaks_between_tests).
"""
from __future__ import annotations

import threading
import time
from pathlib import Path
from typing import Generator

import pytest
import yaml


# ---------------------------------------------------------------------------
# Autouse fixture: isolate FNO_REPO_ROOT, clear settings cache
# ---------------------------------------------------------------------------


@pytest.fixture(autouse=True)
def _isolate(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Generator[None, None, None]:
    """Pin repo root + clear caches before each test."""
    monkeypatch.setenv("FNO_REPO_ROOT", str(tmp_path))
    monkeypatch.setenv("FNO_SKIP_MIGRATION", "1")
    monkeypatch.delenv("FNO_CONFIG", raising=False)
    yield


def _settings_root(tmp_path: Path) -> Path:
    """Return the isolated settings_root for this test."""
    return tmp_path / ".fno"


# ---------------------------------------------------------------------------
# AC2b-EDGE: Race - two threads; filelock serializes
# ---------------------------------------------------------------------------


def test_concurrent_migration_serializes(tmp_path: Path) -> None:
    """AC2b-EDGE: Two concurrent calls; filelock serializes; settings.yaml is valid."""
    from fno.setup.migrate_paths import run_migration

    settings_root = _settings_root(tmp_path)
    results: list[int] = []
    errors: list[Exception] = []

    def _run() -> None:
        try:
            rc = run_migration(settings_root=settings_root)
            results.append(rc)
        except Exception as e:
            errors.append(e)

    t1 = threading.Thread(target=_run)
    t2 = threading.Thread(target=_run)
    t1.start()
    t2.start()
    t1.join(timeout=15)
    t2.join(timeout=15)

    assert not errors, f"Errors during concurrent migration: {errors}"
    assert all(rc == 0 for rc in results), f"All runs should exit 0; got {results}"

    settings_file = settings_root / "settings.yaml"
    assert settings_file.exists(), "settings.yaml must exist after concurrent migration"
    data = yaml.safe_load(settings_file.read_text(encoding="utf-8"))
    assert isinstance(data, dict), "settings.yaml must be valid YAML after concurrent run"
    assert data.get("schema_version") == 1

    sentinel = settings_root / ".path-migration-done"
    assert sentinel.exists(), "Sentinel must exist after concurrent migration"


# ---------------------------------------------------------------------------
# AC2b-FR: Idempotent re-run without --force -> no-op
# ---------------------------------------------------------------------------


def test_idempotent_noop_with_sentinel(tmp_path: Path) -> None:
    """AC2b-FR: With sentinel present and no --force, migration is a no-op."""
    from fno.setup.migrate_paths import run_migration

    settings_root = _settings_root(tmp_path)

    # First run
    rc = run_migration(settings_root=settings_root)
    assert rc == 0

    settings_file = settings_root / "settings.yaml"
    assert settings_file.exists()
    mtime_before = settings_file.stat().st_mtime

    # Sleep briefly to allow mtime resolution to distinguish runs
    time.sleep(0.01)

    # Second run without --force should be a no-op
    rc2 = run_migration(settings_root=settings_root)
    assert rc2 == 0, "No-op run should exit 0"

    mtime_after = settings_file.stat().st_mtime
    assert mtime_after == mtime_before, (
        "settings.yaml should not be touched on idempotent re-run"
    )


# ---------------------------------------------------------------------------
# Finding 1 (P1-CRITICAL): Preserve existing settings keys during migration
# ---------------------------------------------------------------------------


def test_migration_preserves_existing_settings_keys(tmp_path: Path) -> None:
    """AC1-HP: run_migration(force=True) must NOT drop keys outside the detected schema.

    A user may have config.providers.active, do_target, config.budget_cap, etc.
    in their settings.yaml. The migration must deep-merge detected values ON TOP
    of the existing file contents, preserving all other keys.
    """
    from fno.setup.migrate_paths import run_migration

    settings_root = _settings_root(tmp_path)
    settings_root.mkdir(parents=True)

    # Write a settings.yaml with keys outside the schema
    existing_content = (
        "schema_version: 1\n"
        "config:\n"
        "  providers:\n"
        "    active: my-provider\n"
        "  state_dir: '~/old/'\n"
        "do_target:\n"
        "  foo: bar\n"
    )
    settings_file = settings_root / "settings.yaml"
    settings_file.write_text(existing_content, encoding="utf-8")

    rc = run_migration(force=True, settings_root=settings_root)
    assert rc == 0, f"Expected exit 0, got {rc}"

    data = yaml.safe_load(settings_file.read_text(encoding="utf-8"))
    assert isinstance(data, dict)

    # config.providers.active must be preserved
    providers = data.get("config", {}).get("providers", {})
    assert providers.get("active") == "my-provider", (
        f"config.providers.active was dropped! Got config: {data.get('config')}"
    )

    # do_target.foo must be preserved
    do_target = data.get("do_target", {})
    assert do_target.get("foo") == "bar", (
        f"do_target.foo was dropped! Got top-level keys: {list(data.keys())}"
    )


# ---------------------------------------------------------------------------
# Finding C (P1-CRITICAL): Non-force backup path must preserve existing keys
# ---------------------------------------------------------------------------


def test_non_force_backup_preserves_existing_keys(tmp_path: Path) -> None:
    """AC-C-HP: Non-force migration with existing settings.yaml (no sentinel) preserves keys.

    Bug: backup-rename happens BEFORE write_settings_yaml reads existing keys.
    settings_path.rename(backup_path) moves the file away, then
    write_settings_yaml calls _render_settings_yaml(existing_path=settings_path)
    which cannot open the now-gone settings_path, so existing keys are silently dropped.

    Fix: pass backup_path to write_settings_yaml as the source of existing keys.
    """
    from fno.setup.migrate_paths import run_migration

    settings_root = _settings_root(tmp_path)
    settings_root.mkdir(parents=True)

    # Write settings.yaml with user-added keys, WITHOUT a sentinel
    # (absence of sentinel = hand-edited, should be backed up then merged)
    existing_content = {
        "schema_version": 1,
        "config": {
            "providers": {"active": "x"},
        },
        "do_target": {"foo": "bar"},
    }
    import yaml as _yaml
    settings_file = settings_root / "settings.yaml"
    settings_file.write_text(_yaml.safe_dump(existing_content), encoding="utf-8")
    # Critically: NO sentinel file - this triggers the backup path
    assert not (settings_root / ".path-migration-done").exists()

    rc = run_migration(settings_root=settings_root)
    assert rc == 0, f"Expected exit 0, got {rc}"

    # Backup must have been created (confirms we hit the non-force path)
    backups = list(settings_root.glob("backups/settings.yaml.bak.*"))
    assert backups, "A backup should exist (confirms non-force backup path was taken)"

    # Final settings.yaml must retain user keys from the backed-up file
    data = _yaml.safe_load(settings_file.read_text(encoding="utf-8"))
    assert isinstance(data, dict)

    providers = data.get("config", {}).get("providers", {})
    assert providers.get("active") == "x", (
        f"config.providers.active was dropped during non-force backup migration! "
        f"Got config: {data.get('config')}"
    )

    do_target = data.get("do_target", {})
    assert do_target.get("foo") == "bar", (
        f"do_target.foo was dropped during non-force backup migration! "
        f"Got top-level keys: {list(data.keys())}"
    )
