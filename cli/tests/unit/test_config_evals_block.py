"""EvalsBlock day keys: defaults and fail-safe degradation.

The two keys (schedule_days, stale_days) are the demand side of the eval
bank: how often the pr-watch tick runs the regression tier, and the age at
which the newest regression run reads STALE. A malformed value must never
break load_settings(); it degrades to the modeled default with a WARNING,
the block's existing posture. The config-get read of these keys is the
native door's contract, frozen by the parity goldens.
"""
from __future__ import annotations

import logging



def test_bad_value_degrades_to_default_with_warning(caplog) -> None:
    from fno.config import EvalsBlock

    with caplog.at_level(logging.WARNING, logger="fno.config"):
        b = EvalsBlock(schedule_days="weekly", stale_days=None)
    assert b.schedule_days == 7
    assert b.stale_days == 7
    assert "schedule_days" in caplog.text
    assert "weekly" in caplog.text


def test_zero_schedule_disables_scheduled_run() -> None:
    from fno.config import EvalsBlock

    assert EvalsBlock(schedule_days=0).schedule_days == 0
