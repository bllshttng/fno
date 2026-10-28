"""Tests for ProviderRuntimeState (per-provider backoff state).

Run: cd cli && uv run pytest src/fno/adapters/providers/test_runtime_state.py -v

Plan A of provider failover hardening. Distinct from
phase-scoped failover-state.json: runtime_state survives target spawns
within a megawalk campaign and tracks per-provider exponential backoff
plus 1h-stale TTL.
"""
from __future__ import annotations

import json
import time
from pathlib import Path

import pytest

from fno.adapters.providers.runtime_state import (
    MAX_BACKOFF_LEVEL,
    PROVIDER_HEALTH_TTL_SECONDS,
    ProviderHealth,
    read_state,
)


def _seed(path: Path, **blocks: dict) -> None:
    """Write a runtime-state file as the Rust writer leaves it."""
    path.write_text(json.dumps(blocks), encoding="utf-8")


@pytest.fixture
def state_path(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Path:
    """Redirect runtime_state to a tmp path for every test.

    Uses the env-var override path (which the module honors first in
    _resolve_state_path) so the override survives reloads inside tests.
    monkeypatch.setenv auto-restores at test teardown.
    """
    target = tmp_path / "provider-runtime-state.json"
    monkeypatch.setenv("FNO_RUNTIME_STATE_PATH", str(target))
    return target


class TestTtl:
    """Stale entries (older than TTL) are dropped on read."""

    def test_edge_ttl_drops_stale_entry(self, state_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
        # AC2.5-EDGE: entry with last_error_at < now - TTL is dropped on
        # next read (in memory). Disk cleanup happens on next write
        # under the lock - see test_disk_cleanup_happens_under_lock.
        state_path.parent.mkdir(parents=True, exist_ok=True)
        stale_payload = {
            "schema_version": 1,
            "provider_health": {
                "X": {
                    "provider_id": "X",
                    "backoff_level": 5,
                    "rate_limited_until": None,
                    "last_error_at": time.time() - PROVIDER_HEALTH_TTL_SECONDS - 100,
                },
                "Y": {
                    "provider_id": "Y",
                    "backoff_level": 1,
                    "rate_limited_until": None,
                    "last_error_at": time.time() - 60,  # within TTL
                },
            },
        }
        state_path.write_text(json.dumps(stale_payload), encoding="utf-8")

        state = read_state()
        # Stale X dropped, fresh Y kept.
        assert "X" not in state.provider_health
        assert state.provider_health["Y"].backoff_level == 1


class TestDiskCleanupOnWrite:
    """Only the locked writer cleans the disk; a read never writes."""

    def test_read_state_does_not_write_to_disk(
        self, state_path: Path
    ) -> None:
        # Pre-Gemini-fix regression: read_state used to rewrite the file
        # to drop stale entries, racing concurrent writers. read_state
        # is now a pure read - it must NOT touch disk.
        state_path.parent.mkdir(parents=True, exist_ok=True)
        payload = {
            "schema_version": 1,
            "provider_health": {
                "STALE": {
                    "provider_id": "STALE",
                    "backoff_level": 1,
                    "rate_limited_until": None,
                    "last_error_at": time.time() - PROVIDER_HEALTH_TTL_SECONDS - 100,
                }
            },
        }
        state_path.write_text(json.dumps(payload), encoding="utf-8")
        before_mtime = state_path.stat().st_mtime

        # Multiple reads must not change mtime.
        for _ in range(3):
            state = read_state()
            assert "STALE" not in state.provider_health  # in-memory drop

        after_mtime = state_path.stat().st_mtime
        assert before_mtime == after_mtime, (
            "read_state rewrote the file - the lock-free write race is back"
        )


class TestProviderHealthValidation:
    """ProviderHealth __post_init__ enforces backoff_level range + non-empty id."""

    def test_rejects_empty_provider_id(self) -> None:
        with pytest.raises(ValueError, match="provider_id"):
            ProviderHealth(provider_id="")

    def test_rejects_negative_backoff_level(self) -> None:
        with pytest.raises(ValueError, match="backoff_level"):
            ProviderHealth(provider_id="X", backoff_level=-1)

    def test_rejects_above_max_backoff_level(self) -> None:
        with pytest.raises(ValueError, match="backoff_level"):
            ProviderHealth(provider_id="X", backoff_level=MAX_BACKOFF_LEVEL + 1)


# ─────────────────────────────────────────────────────────────────────
# Plan A1: per-model lockout granularity
# ─────────────────────────────────────────────────────────────────────


class TestProviderHealthModelLocks:
    """ProviderHealth.model_locks validation in __post_init__."""

    def test_ac1_1_construction_with_model_locks(self) -> None:
        # AC1.1-HP: fresh ProviderHealth with model_locks entry.
        ts = time.time() + 30
        h = ProviderHealth(
            provider_id="p1",
            model_locks={"claude-opus-4-7": ts},
        )
        assert h.model_locks == {"claude-opus-4-7": ts}

    def test_ac1_2_rejects_non_positive_timestamp(self) -> None:
        # AC1.2-ERR: __post_init__ rejects non-positive lock timestamps.
        with pytest.raises(ValueError, match="opus"):
            ProviderHealth(provider_id="p1", model_locks={"opus": -100})

    def test_ac1_2_rejects_zero_timestamp(self) -> None:
        with pytest.raises(ValueError, match="model_locks"):
            ProviderHealth(provider_id="p1", model_locks={"opus": 0})

    def test_ac1_3_rejects_empty_model_id(self) -> None:
        # AC1.3-EDGE: empty model identifier rejected.
        with pytest.raises(ValueError, match="model_locks"):
            ProviderHealth(provider_id="p1", model_locks={"": 100.0})

    def test_rejects_non_numeric_timestamp(self) -> None:
        # Defensive: a string timestamp would silently break math.
        with pytest.raises(ValueError, match="model_locks"):
            ProviderHealth(
                provider_id="p1", model_locks={"opus": "soon"},  # type: ignore[dict-item]
            )

    def test_empty_default(self) -> None:
        # Default factory yields empty dict, never shared across instances.
        h1 = ProviderHealth(provider_id="p1")
        h2 = ProviderHealth(provider_id="p2")
        assert h1.model_locks == {}
        assert h2.model_locks == {}
        assert h1.model_locks is not h2.model_locks


class TestModelLocksTTL:
    """Stale model_locks are dropped together with their parent record."""

    def test_ac4_1_stale_record_drops_all_model_locks(
        self, state_path: Path
    ) -> None:
        # AC4.1-EDGE: ProviderHealth older than TTL is dropped wholesale,
        # taking its model_locks with it.
        now = time.time()
        stale_age = now - PROVIDER_HEALTH_TTL_SECONDS - 100
        state_path.parent.mkdir(parents=True, exist_ok=True)
        payload = {
            "schema_version": 1,
            "provider_health": {
                "X": {
                    "provider_id": "X",
                    "backoff_level": 1,
                    "rate_limited_until": None,
                    "last_error_at": stale_age,
                    "model_locks": {"opus": now + 60},
                }
            },
        }
        state_path.write_text(json.dumps(payload), encoding="utf-8")

        state = read_state(now=now)
        # Whole record dropped because last_error_at < cutoff.
        assert "X" not in state.provider_health


class TestModelLocksRoundTrip:
    """The parser degrades a malformed model_locks entry instead of raising."""

    def test_parse_treats_non_dict_falsy_model_locks_as_empty(
        self, state_path: Path, caplog: pytest.LogCaptureFixture,
    ) -> None:
        # A JSON `[]` (falsy non-dict) must hit the same warning path as
        # `[1,2]` (truthy non-dict). Earlier `or {}` short-circuit silently
        # rewrote `[]` to `{}` without logging.
        import logging

        now = time.time()
        state_path.parent.mkdir(parents=True, exist_ok=True)
        payload = {
            "schema_version": 1,
            "provider_health": {
                "X": {
                    "provider_id": "X",
                    "backoff_level": 0,
                    "rate_limited_until": None,
                    "last_error_at": now,
                    "model_locks": [],  # JSON array, falsy non-dict
                }
            },
        }
        state_path.write_text(json.dumps(payload), encoding="utf-8")

        with caplog.at_level(logging.WARNING, logger="fno.adapters.providers.runtime_state"):
            state = read_state()
        # The record is kept; model_locks is empty (degraded gracefully).
        assert "X" in state.provider_health
        assert state.provider_health["X"].model_locks == {}
        # And the warning must have fired - this is the contract that the
        # `or {}` short-circuit broke.
        assert any(
            "model_locks" in rec.message and "not a dict" in rec.message
            for rec in caplog.records
        ), "expected non-dict warning for falsy non-dict model_locks"

    def test_parse_drops_invalid_model_lock_entries(
        self, state_path: Path
    ) -> None:
        # On-disk file with one valid + two invalid model_locks; the parser
        # keeps the valid one and drops the others without raising.
        now = time.time()
        state_path.parent.mkdir(parents=True, exist_ok=True)
        payload = {
            "schema_version": 1,
            "provider_health": {
                "X": {
                    "provider_id": "X",
                    "backoff_level": 1,
                    "rate_limited_until": None,
                    "last_error_at": now,
                    "model_locks": {
                        "opus": now + 60,  # valid
                        "": now + 60,      # empty key -> drop
                        "sonnet": -1,      # non-positive -> drop
                    },
                }
            },
        }
        state_path.write_text(json.dumps(payload), encoding="utf-8")

        state = read_state()
        h = state.provider_health["X"]
        assert "opus" in h.model_locks
        assert "" not in h.model_locks
        assert "sonnet" not in h.model_locks


class TestEveryLockWriterHarvestsTheReset:
    """A producer on one of N paths leaves the feature inert on the others.

    Every path that writes the provider lock must carry the harvested reset,
    or the taxonomy field is decorative. These pin the wiring.
    """

    _BODY = "rate limit exceeded; resets at 2026-08-18T07:19:38+08:00"

    def _expected(self) -> float:
        from datetime import datetime

        return datetime.fromisoformat("2026-08-18T07:19:38+08:00").timestamp()

    def test_the_one_call_harvest_matches_normalize(self) -> None:
        from fno.adapters.providers.error_taxonomy import normalize, reset_epoch_from

        assert reset_epoch_from(self._BODY) == normalize(429, None, self._BODY).resets_at
        assert reset_epoch_from(None) is None
        assert reset_epoch_from("") is None

    def test_note_quota_death_writes_the_harvested_reset(
        self, state_path: Path, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        import fno.adapters.providers.runtime_state as rs
        from fno.agents import dispatch

        seen: dict[str, object] = {}
        monkeypatch.setattr(
            rs, "update_provider_health",
            lambda provider_id, rule, model=None, now=None, resets_at=None:
                seen.setdefault("resets_at", resets_at),
            raising=True,
        )
        monkeypatch.setattr(
            dispatch, "_account_id_for_env", lambda env: "zai", raising=True,
        )

        dispatch.note_quota_death(None, f"Claude usage limit reached. {self._BODY}")
        assert seen["resets_at"] == pytest.approx(self._expected())

    def test_a_naive_stamp_needs_the_records_timezone_at_the_writer(
        self, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        # The record is the only thing that knows the zone, and the writer is
        # the first point that knows the record.
        import fno.adapters.providers.runtime_state as rs
        from fno.adapters.providers.error_taxonomy import reset_epoch_from

        body = "Claude usage limit reached. Resets 2026-08-18 07:19:38"
        assert reset_epoch_from(body, None) is None

        monkeypatch.setattr(
            rs, "record_reset_timezone", lambda pid: "Asia/Singapore", raising=True,
        )
        assert reset_epoch_from(body, rs.record_reset_timezone("zai")) == (
            pytest.approx(self._expected())
        )

    def test_an_unreadable_config_refuses_the_zone_and_never_raises(self) -> None:
        import fno.adapters.providers.runtime_state as rs

        assert rs.record_reset_timezone("no-such-record") is None


class TestHealthTTLBoundsReactiveLocks:
    """A future lock cannot outlive the health observation that created it."""

    def test_an_expired_lock_still_ages_out(self, state_path: Path) -> None:
        # The reprieve is for a LIVE lock only; the TTL still garbage-collects
        # a long-quiet provider's backoff level.
        now = time.time()
        _seed(state_path, provider_health={"P": {"backoff_level": 1, "rate_limited_until": now + 2, "last_error_at": now}})
        later = now + PROVIDER_HEALTH_TTL_SECONDS + 60
        assert "P" not in read_state(now=later).provider_health


class TestMachineWideStatePath:
    """Health and usage live in ONE machine-wide file, not one per cwd (M5)."""

    def _machine(self, tmp_path, monkeypatch) -> Path:
        import fno.paths as paths_mod

        machine = tmp_path / "machine" / "provider-runtime-state.json"
        monkeypatch.delenv("FNO_RUNTIME_STATE_PATH", raising=False)
        monkeypatch.setattr(paths_mod, "runtime_state_json", lambda: machine)
        return machine

    def test_state_written_from_one_cwd_reads_from_another(self, tmp_path, monkeypatch):
        """AC19-ERR: quota is per-account and machine-wide, so a lock on the
        machine file reads from any cwd."""
        machine = self._machine(tmp_path, monkeypatch)
        now = time.time()
        machine.parent.mkdir(parents=True)
        _seed(machine, provider_health={"acct-1": {
            "backoff_level": 1, "rate_limited_until": now + 3600, "last_error_at": now,
        }})
        cwd_b = tmp_path / "b"
        cwd_b.mkdir()
        monkeypatch.chdir(cwd_b)
        assert "acct-1" in read_state(now=now).provider_health

