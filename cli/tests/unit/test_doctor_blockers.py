"""Unit tests for fno doctor's single blocker authority, `_blockers()`.

One pure function reading the assembled `result` dict that `fno doctor` and
the setup wizard both call (x-75dc): see `_emit_blockers` in doctor.py and
`report_machine_blockers` in setup_cli.py.
"""
from __future__ import annotations

import pytest

from fno.doctor import _blockers


def _clean_result(**overrides):
    base = {
        "status": "fresh",
        "launch_agents": {"applicable": True, "dead": []},
        "fd_limit": {"verdict": "ok"},
        "plugin_hooks": {"failed": 0},
        "plugin_cache": {"status": "fresh"},
    }
    base.update(overrides)
    return base


def test_missing_report_keys_do_not_raise_and_produce_no_lines():
    # A report that never ran (e.g. the non-darwin launch_agents probe) is
    # absent evidence, not a blocker - the never-cry-wolf rule that governs
    # _verdict governs this too.
    assert _blockers({}) == []
    assert _blockers({"launch_agents": {}}) == []


@pytest.mark.parametrize(
    "overrides, expect_substring",
    [
        ({"status": "stale"}, "behind source"),
        (
            {
                "launch_agents": {
                    "applicable": True,
                    "dead": [{"label": "sh.fno.groom"}],
                }
            },
            "exit unknown",
        ),
        ({"fd_limit": {"verdict": "low", "launchd_soft": 256}}, "256"),
        ({"plugin_hooks": {"failed": 2}}, "2 plugin hook"),
        (
            {"plugin_cache": {"status": "stale", "roots": [{"blocker": "plugin root /s (live) differs"}]}},
            "plugin root /s (live) differs",
        ),
    ],
)
def test_each_signal_produces_a_blocker_line(overrides, expect_substring):
    result = _clean_result(**overrides)
    blockers = _blockers(result)
    assert len(blockers) == 1
    assert expect_substring in blockers[0]
