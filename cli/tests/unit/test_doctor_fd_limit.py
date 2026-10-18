"""Unit tests for the fno doctor open-file-limit advisory.

The report exists because two observers disagreed and both were right: a
login shell reads 1048576 while every launchd-spawned worker runs at 256.
The verdict computation must be testable without a Mac, so the launchctl
line is parsed by a pure function and the platform probe is monkeypatched.
"""
from __future__ import annotations

import resource
import sys
from typing import Any, Optional

import pytest

from fno import doctor
from fno.doctor import _fd_limit_report


def _report_for(
    monkeypatch: pytest.MonkeyPatch,
    soft: int,
    launchctl: Optional[tuple[int, str]],
) -> dict[str, Any]:
    monkeypatch.setattr(sys, "platform", "darwin")
    monkeypatch.setattr(
        resource, "getrlimit", lambda _which: (soft, resource.RLIM_INFINITY)
    )

    def fake_bounded(argv: list[str]) -> Optional[tuple[int, str, str]]:
        if argv[0] == "launchctl":
            return (launchctl[0], launchctl[1], "") if launchctl else (1, "", "")
        if argv[0] == "sysctl":
            return (0, "491520\n", "")
        return None

    monkeypatch.setattr(doctor, "_bounded_command", fake_bounded)
    return _fd_limit_report()


def test_low_launchd_default_verdict_low(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """AC3 shape: healthy shell beside a starving launchd session default."""
    report = _report_for(monkeypatch, 1048576, (0, "maxfiles\t256\tunlimited\n"))
    assert report["soft"] == 1048576
    assert report["launchd_soft"] == 256
    assert report["kern_maxfiles"] == 491520
    assert report["verdict"] == "low"


def test_linux_unlimited_soft_is_not_low(monkeypatch: pytest.MonkeyPatch) -> None:
    """Linux spells RLIM_INFINITY as -1; unlimited must never read as low."""
    monkeypatch.setattr(sys, "platform", "linux")
    monkeypatch.setattr(
        resource, "getrlimit", lambda _which: (resource.RLIM_INFINITY, resource.RLIM_INFINITY)
    )
    monkeypatch.setattr(doctor, "_bounded_command", lambda _argv: None)
    report = _fd_limit_report()
    assert report["soft"] == resource.RLIM_INFINITY
    assert report["verdict"] == "ok"
