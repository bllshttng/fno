"""Read-time revocation of merge-gating configuration opt-outs.

Core-layer code by necessity, not by accident: the instrument is a claim,
and the values it guards live in config files. ``fno.config`` may not
import the claims layer, so it loads this module through importlib. A
merge-gating opt-out holds only while its ``config-optout:<key>`` claim is
live; any other state drops the value so the default gate applies. The
write-time lease and the reaper's restore are native, in the config writer
(crates/fno/src/config_cli/write/).
"""

from __future__ import annotations

import logging
from typing import Any

from fno.claims import ClaimState, claim_status, native_claims_root
from fno.config.optouts import (
    MERGE_GATING_OPTOUT_DEFAULTS,
    MERGE_GATING_OPTOUTS,
    _drop_raw_leaf,
    _raw_leaf,
)

_LOG = logging.getLogger(__name__)


# ---------------------------------------------------------------------------
# read-time revocation
# ---------------------------------------------------------------------------


def _claim_state(key: str) -> str:
    """Read the opt-out instrument, distinguishing unreadable from absent."""
    try:
        root = native_claims_root(f"config-optout:{key}")
        return str(
            claim_status(f"config-optout:{key}", root=root).get(
                "state", "unreadable"
            )
        )
    except Exception:  # noqa: BLE001 - unreadable instrument revokes the opt-out
        return "unreadable"


def revoke_unbacked_optouts(raw: dict[str, Any]) -> dict[str, Any]:
    """Drop merge-gating opt-outs unless their global claim is LIVE.

    This is deliberately a read-time guard. A failed or delayed reaper leaves
    the file value inert, so a fresh process still requires the gate.
    """
    for key, optout in MERGE_GATING_OPTOUTS.items():
        present, value = _raw_leaf(raw, key)
        # Explicit presence matters for optional_apps: absent means the model's
        # built-in list, while [] is the actual opt-out.
        if not present or value != optout:
            continue
        state = _claim_state(key)
        if state == ClaimState.LIVE.value:
            continue
        _drop_raw_leaf(raw, key)
        _LOG.warning(
            "merge-gating opt-out %s revoked: claim instrument is %s; "
            "resolved default is %r",
            key,
            state,
            MERGE_GATING_OPTOUT_DEFAULTS[key],
        )
    return raw
