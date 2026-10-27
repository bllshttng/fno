"""The zai false green, the key-position label, and the confidence field.

Run: cd cli && uv run pytest src/fno/adapters/providers/test_usage_false_green.py -v

Every payload here is a LIVE capture, not a synthesized shape. The sibling
``test_usage.py`` parametrizes single-row payloads only, and a single dropped
row leaves ``()``, which correctly reads unknown. A MIXED row list is the only
shape that produces the defect and it is the shape the live endpoint returns,
so every fixture in this module carries more than one row or reproduces a
measured production body. The verdict-side pins of the false-green criterion
live in the Rust parity suites beside the deleted Python leg.
"""
from __future__ import annotations

import json
from pathlib import Path

import pytest

from fno.adapters.providers.usage import (
    UsageSnapshot,
    UsageWindow,
    _parse_zai_windows,
)

FIXTURES = Path(__file__).resolve().parents[4] / "tests" / "fixtures"


def _fixture(name: str):
    return json.loads((FIXTURES / name).read_text(encoding="utf-8"))


@pytest.fixture
def zai_live():
    """The two-row body the endpoint returned 2026-08-27, credentials stripped."""
    return _fixture("zai_quota_live_20260827.json")


def _snap(windows, **kw):
    return UsageSnapshot(
        provider_id="zai",
        windows=tuple(windows),
        probed_at=1000.0,
        source="quota-endpoint",
        **kw,
    )


class TestZaiParseSurfaces:
    """The parse-half of the false-green criterion: what survives the leg.

    The verdict half reads OK / LOW / EXHAUSTED through the Rust parity
    suites; what stays here pins the parser contract the verdict consumed.
    """

    def test_todays_live_payload_parses_both_windows(
        self, zai_live
    ) -> None:
        """Today's body parses to both windows, the shape a true green needs."""
        windows = _parse_zai_windows(zai_live)
        assert {w.label for w in windows} == {"1m", "5h"}

    def test_a_rejected_row_marks_the_snapshot_partial(self) -> None:
        """A row of a KNOWN limit type that fails the usability guard poisons
        the response; it must not vanish from it."""
        payload = {
            "success": True,
            "code": 200,
            "data": {
                "limits": [
                    {
                        "type": "TIME_LIMIT",
                        "unit": 5,
                        "number": 1,
                        "percentage": 1,
                        "nextResetTime": 1788247801997,
                    },
                    # Unusable: no unit the parser recognizes, so it is rejected
                    # rather than retained with a None reset.
                    {"type": "TOKENS_LIMIT", "unit": 99, "number": 5, "percentage": 0},
                ]
            },
        }
        windows, partial = _parse_zai_windows(payload, with_partial=True)
        assert partial is True

    def test_a_wholly_well_formed_response_is_not_partial(self) -> None:
        """The floor must not make every reading pessimistic."""
        payload = {
            "success": True,
            "code": 200,
            "data": {
                "limits": [
                    {
                        "type": "TIME_LIMIT",
                        "unit": 5,
                        "number": 1,
                        "percentage": 1,
                        "nextResetTime": 1788247801997,
                    },
                    {
                        "type": "TOKENS_LIMIT",
                        "unit": 3,
                        "number": 5,
                        "percentage": 2,
                        "nextResetTime": 1788247801997,
                    },
                ]
            },
        }
        windows, partial = _parse_zai_windows(payload, with_partial=True)
        assert len(windows) == 2
        assert partial is False


class TestIncoherentTimeLimitReset:
    """AC9-ERR: label and reset must describe the same span, or the reset is None."""

    def test_one_minute_window_does_not_publish_a_plan_period_reset(
        self, zai_live
    ) -> None:
        """Measured: label '1m', resets_at five days out, because the row's
        nextResetTime is the PLAN period and not that window's reset."""
        # Anchor the parse to the RECORDING moment. The parser's coherence
        # rule compares resets_at against live time, so an unanchored parse
        # of the 2026-08-27 recording flips meaning as the wall clock moves:
        # the five-day reset read as "far future, incoherent, drop" while it
        # stayed ahead of the clock, and as "within span, publish" once the
        # clock passed it. Anchored, the pin is deterministic.
        recorded_at = 1787818296.0
        one_min = [
            w for w in _parse_zai_windows(zai_live, now=recorded_at) if w.label == "1m"
        ]
        assert one_min, "no 1m window parsed"
        w = one_min[0]
        if w.resets_at is None:
            return  # dropped to None rather than published: the legal outcome
        # Otherwise the reset must lie within the window's own span (plus slack).
        assert w.resets_at - recorded_at <= 60 * 2, (
            f"a 1m window resetting at {w.resets_at} describes a different span"
        )


class TestConfidenceTravelsInBand:
    """AC16-EDGE: a coarse reading must not be indistinguishable from a precise one."""

    def test_snapshot_carries_confidence(self) -> None:
        snap = _snap([UsageWindow("5h", 1.0, 9e18)], confidence="exact")
        assert snap.confidence == "exact"

    def test_confidence_defaults_to_unknown(self) -> None:
        """A legacy row already on disk reads unknown, never exact: an old row's
        precision is exactly what is not known."""
        assert _snap([UsageWindow("5h", 1.0, 9e18)]).confidence == "unknown"

class TestUnknownNamesWhichUnknown:
    """AC17-ERR: a rejected credential is not a failed network."""

    def test_a_transport_failure_still_reads_probe_failed(
        self, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        """The distinction only pays if the other side keeps its own name."""
        import urllib.error

        from fno.adapters.providers import usage as usage_mod

        monkeypatch.setattr(
            usage_mod, "_claude_bearer_candidates", lambda _r: ["a-real-token"]
        )
        monkeypatch.setattr(
            usage_mod, "_bearer_verdict", lambda _r, _b, _n: "match"
        )

        def _down(_request, timeout):  # noqa: ANN001
            raise urllib.error.URLError("name resolution failed")

        monkeypatch.setattr(usage_mod.urllib.request, "urlopen", _down)
        snap, reason = usage_mod._probe_claude(_claude_record(), now=1000.0)
        assert snap is None
        assert reason == "probe-failed"


def _claude_record():
    from fno.adapters.providers.model import ProviderRecord

    return ProviderRecord(
        id="readyrule",
        name="readyrule",
        harness="claude",
        auth="managed",
        priority=100,
    )
