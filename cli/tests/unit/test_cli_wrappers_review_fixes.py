"""``propagate_returncode`` normalises negative signal codes (SIGKILL -> 137)."""
from __future__ import annotations

from fno._subprocess_util import propagate_returncode


def test_propagate_returncode_normalises_negative_signal_codes() -> None:
    # SIGKILL -> 137, SIGTERM -> 143
    assert propagate_returncode(-9) == 137
    assert propagate_returncode(-15) == 143
