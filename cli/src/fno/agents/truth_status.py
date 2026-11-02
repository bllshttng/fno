"""fno-truth worker status: derive Working-while-waiting from claim liveness.

A bg ``/target`` worker reads as "Idle" in fleet surfaces whenever it is
between turns, even while its real work (CI, smoke tests, preflight builds)
runs externally - the harness classifier sees only turn boundaries. fno holds
the truth it cannot: the ``node:<id>`` claim liveness (the sole durable
liveness signal) and per-session ``loop_check`` event recency.

This module is a read-only derivation over those two existing signals. It
never writes claims, events, or registry state, and it fails quiet: an
unresolvable node join returns ``unknown`` so the caller renders exactly as
today (Locked Decision 5).

State table (Locked Decision 2 - ``working`` requires BOTH claim-live AND a
recent fire; claim-live alone is ``waiting``):

    claim live  + recent loop_check   -> working
    claim live  + no recent fire      -> waiting
    claim suspect (in-TTL, dead pid)  -> suspect
    claim stale                       -> stalled
    claim free / corrupted / no node  -> unknown

``done`` is intentionally not derived: a finished worker has RELEASED its
claim (state ``free``), so there is no holder left to recover its session id,
and the harness row already shows the terminal state. ``unknown`` hands it off
(AC7). See the node's Open Questions.
"""

from __future__ import annotations

from typing import Optional


_HOLDER_PREFIX = "target-session:"


def _session_from_holder(holder: Optional[str]) -> Optional[str]:
    if holder and holder.startswith(_HOLDER_PREFIX):
        sid = holder[len(_HOLDER_PREFIX) :]
        return sid or None
    return None
