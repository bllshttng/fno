"""The refusal deadline and the silent failover (x-763a, changes 11 and 12).

Run: cd cli && uv run pytest tests/test_quota_refusal_and_failover.py -v

The fixture is the machine's own recorded events, not synthesized payloads:
38 `worker_silent` records carrying a real 429. Handles and vendor request ids
are stripped; the message shapes are untouched.
"""
from __future__ import annotations

import json
from pathlib import Path

import pytest

from fno.adapters.providers.error_taxonomy import (
    ErrorClass,
    normalize,
    reset_window_seconds_from,
)

FIXTURES = Path(__file__).resolve().parent / "fixtures"


def _records(name: str) -> list[dict]:
    lines = (FIXTURES / name).read_text(encoding="utf-8").splitlines()
    return [json.loads(line) for line in lines if line.strip()]


@pytest.fixture
def silent():
    return _records("quota_429_worker_silent.jsonl")


class TestTheWindowNameIsTheTrustworthyHalf:
    """AC18-HP / AC19-ERR / AC20-ERR."""

    def test_a_named_window_yields_a_real_deadline(self, silent) -> None:
        """AC18-HP. Replayed over the real 429 corpus: every message naming a
        window produces an epoch, and it is the observation time plus that
        window."""
        assert silent, "the recorded 429 corpus is empty"
        checked = 0
        for record in silent:
            body = record["data"]["last_message"]
            if "for 5 hour" not in body:
                continue
            observed = 1_787_000_000.0
            err = normalize(429, None, body, now=observed)
            assert err.error_class is ErrorClass.PROVIDER_4XX_QUOTA
            assert err.resets_at == observed + 5 * 3600.0
            assert err.reset_is_derived is True
            checked += 1
        assert checked >= 20, f"only {checked} records named a five-hour window"

    @pytest.mark.parametrize(
        ("status", "body"),
        [
            (401, "Token invalid. Sessions are valid for 30 minutes."),
        ],
    )
    def test_only_a_quota_refusal_derives_a_deadline(self, status, body) -> None:
        """`resets_at` is written straight through as the provider lock, and
        `for <n> <unit>` is ordinary English that appears in bodies naming no
        rate-limit window.

        Unscoped, the 401 body here derived a 30-minute epoch and
        `update_provider_health` wrote it as a real provider lock in place of
        the rule's own backoff step. Only a quota refusal is making a claim
        about a window.
        """
        err = normalize(status, None, body, now=1_787_000_000.0)
        assert err.error_class is not ErrorClass.PROVIDER_4XX_QUOTA
        assert err.reset_is_derived is False
        assert err.resets_at is None

    def test_a_resolved_stamp_still_wins_over_the_derived_bound(self) -> None:
        """AC13-EDGE in miniature: an unambiguous epoch is not a bound, so it
        is never replaced by one."""
        body = "Usage limit reached for 5 hour. Resets at 2026-08-27T21:09:58+08:00"
        err = normalize(429, None, body, now=1_787_000_000.0)
        assert err.reset_is_derived is False
        assert err.resets_at != 1_787_000_000.0 + 5 * 3600.0

    @pytest.mark.parametrize(
        ("body", "seconds"),
        [
            ("limit reached for 5 hour", 5 * 3600.0),
            ("rate limit reached for hour", 3600.0),
            ("nothing named here", None),
            ("limit reached for 0 hours", None),
            ("limit reached for 99 weeks", None),  # past the sane horizon
        ],
    )
    def test_window_name_vocabulary(self, body, seconds) -> None:
        assert reset_window_seconds_from(body) == seconds


class TestFailoverNamesItsGiveUp:
    """AC21-ERR. Twenty recorded rotations, twenty abandonments, nineteen mute."""

    def test_an_abandonment_without_a_reason_is_refused_at_the_writer(self) -> None:
        from fno.events.store_client import ValidationError, build_event as _build

        with pytest.raises(ValidationError):
            _build(
                "failover_swapped",
                "daemon",
                {"short_id": "79ebf387", "redispatched": False},
            )

class TestSessionUuidFallsBackToTheRegistryRow:
    """AC22-HP. The supervisor's file is gone by the time recovery runs; the
    row that outlived it still carries the UUID."""

    def test_the_candidate_row_answers_when_the_session_file_is_gone(
        self, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        from fno import recovery

        monkeypatch.setattr(
            "fno.agents.harnesses._claude_session_registry.resolve_session_uuid",
            lambda _s: None,
        )

        class _Row:
            short_id = "79ebf387"
            harness_session_id = "79ebf387-4c0b-43bd-b9e3-ccc4e74ed5ce"

        monkeypatch.setattr(
            "fno.agents.registry.load_registry", lambda *a, **k: [_Row()]
        )
        got = recovery._resolve_session_uuid("79ebf387")
        assert got == "79ebf387-4c0b-43bd-b9e3-ccc4e74ed5ce"

    @pytest.mark.parametrize(
        ("value", "accepted"),
        [
            # The row's own short id, not the resume key: too short.
            ("79ebf387", False),
            # A real UUID, but another worker's: a resume onto it would drive
            # the wrong session.
            ("dbd7f862-9cad-4de6-b0ec-952d577e68d5", False),
            ("79ebf387-4c0b-43bd-b9e3-ccc4e74ed5ce", True),
        ],
    )
    def test_only_a_full_uuid_that_matches_the_short_id_is_accepted(
        self, monkeypatch: pytest.MonkeyPatch, value, accepted
    ) -> None:
        """A truncated or unrelated field must never reach --resume.

        Exercised against ``_session_uuid_from_registry`` itself, over a
        registry holding exactly the row under test. Going through
        ``_resolve_session_uuid`` with this function stubbed out would test the
        stub rather than the guard.
        """
        from fno import recovery

        class _Row:
            short_id = "79ebf387"
            harness_session_id = value

        monkeypatch.setattr(
            "fno.agents.registry.load_registry", lambda *a, **k: [_Row()]
        )
        got = recovery._session_uuid_from_registry("79ebf387")
        assert got == (value if accepted else None)
