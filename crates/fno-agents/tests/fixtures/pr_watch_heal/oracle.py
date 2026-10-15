#!/usr/bin/env python3
"""The capture-mode oracle for the pr-watch heal parity test.

Replays the `fno do pr watch heal` leaf body (the gates, the cure, the
output contract) over the same fixture the Rust verb reads, with the
liveness probe, the single-flight, the tick-in-flight answer and the
launchctl steps injected through env pins. The goldens freeze the live
Python leaf's bytes; the body itself was replaced by a forward in the
same change, so this replay is the leg the goldens stand for.

Pins, read only here:
- ``FNO_TEST_HEAL_PROBE``: a JSON dict the liveness probe returns; unset
  reads as a probe that raised (the leaf proceeds on an empty report).
- ``FNO_TEST_HEAL_FLIGHT``: ``held`` makes the single-flight report held.
- ``FNO_TEST_PR_WATCH_LAUNCHCTL`` / ``FNO_TEST_PR_WATCH_TICK_PID``: the
  bounce pins the refresh oracle uses.
"""

import argparse
import json
import os
from pathlib import Path
from unittest import mock


class _StubFlight:
    """The gate result the leaf reads: `held`, and a release that is a
    no-op (the native gate owns the real release)."""

    def __init__(self, held: bool) -> None:
        self.held = held

    def release(self) -> None:
        pass


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--case", required=True, help="fixture case directory")
    args = ap.parse_args()

    from fno.config import load_settings
    from fno.pr_watch import _install as m
    from fno.pr_watch import cli as cli_mod

    agents_dir = Path(args.case) / "LaunchAgents"

    probe_raw = os.environ.get("FNO_TEST_HEAL_PROBE")
    probe = json.loads(probe_raw) if probe_raw else None
    if probe is None:
        def _probe():
            raise RuntimeError("probe down")
    else:
        def _probe():
            return probe

    held = os.environ.get("FNO_TEST_HEAL_FLIGHT") == "held"

    scripted = json.loads(os.environ.get("FNO_TEST_PR_WATCH_LAUNCHCTL", "[]"))

    def fake_launchctl(*a, timeout_s=10.0):
        if not scripted:
            raise AssertionError("unexpected launchctl call")
        rc, timed = scripted.pop(0)
        return rc, timed

    tick_pid = os.environ.get("FNO_TEST_PR_WATCH_TICK_PID") or None

    settings = load_settings()
    if not settings.pr_watch.enabled:
        print("pr-watch heal: disabled; nothing to heal")
        return 0

    # A bounce that has not had its first tick yet is not a wedge to cure:
    # bouncing again re-arms the healthy-pending grace over the same fault
    # and hides it for another 2x interval.
    try:
        report = _probe()
    except Exception:  # noqa: BLE001 - a probe that cannot read never blocks a cure
        report = {}
    if report.get("bounce_pending") is True:
        print(
            f"pr-watch heal: a bounce is pending its first tick ({report.get('detail')}); "
            "skipped. Run fno do pr watch refresh to bounce anyway."
        )
        return 0

    flight = _StubFlight(held)
    if flight is None or flight.held:
        # Someone else is on it, not a reason to abort this SessionStart
        # hook with a traceback.
        print("pr-watch heal: another session is healing; skipped")
        return 0
    try:
        with mock.patch.object(
            m, "_run_launchctl_timed", fake_launchctl
        ), mock.patch.object(
            m,
            "_tick_in_flight",
            lambda run=None: int(tick_pid) if tick_pid and int(tick_pid) > 0 else None,
        ):
            msg, rc = m.refresh_watcher(
                launch_agents_dir=agents_dir,
                fno_binary="/usr/local/bin/fno",
                interval=settings.pr_watch.interval_seconds,
                defer_when_ticking=True,
                caller="heal",
                force_bounce=True,
            )
        print(f"pr-watch heal: {msg}")
        return 1 if rc != 0 else 0
    finally:
        flight.release()


if __name__ == "__main__":
    raise SystemExit(main())
