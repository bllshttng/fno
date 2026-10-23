"""Provider combos: named ordered lists with rotation strategies.

Provider-rotation Plan B. Composes on top of Plan A
 ProviderHealth + cooldown substrate.

A Combo is a named ordered list of provider IDs with a strategy:

* ``fallback``: sequential try-next-on-error (the default; matches
  pre-combo single-provider semantics when the list has one entry).
* ``round_robin``: time-sliced cycle with a per-combo cursor that
  sticks for ``sticky_limit`` calls before advancing. The cursor lives in
  ``provider-runtime-state.json``; the combo list view reads it.

"""
from __future__ import annotations

import dataclasses
import hashlib
import logging
from typing import Any, Collection, Literal

from fno.adapters.providers.model import ProviderConfigError

logger = logging.getLogger(__name__)


_VALID_STRATEGIES = ("fallback", "round_robin")


class ComboNotFoundError(ProviderConfigError):
    """Raised when a referenced combo does not exist at dispatch time.

    Distinct from generic ProviderConfigError so callers (dispatch_target,
    CLI commands) can catch it and fall through to the no-combo path
    without swallowing other config-shape errors.
    """


@dataclasses.dataclass(frozen=True)
class Combo:
    """Named ordered provider list with rotation strategy.

    Loaded from ``config.providers.combos.<name>`` in settings.yaml.
    The ``providers`` tuple elements MUST be valid IDs from
    ``config.providers.records`` - cross-validated by ``load_combos``.
    """

    name: str
    strategy: Literal["fallback", "round_robin"] = "fallback"
    sticky_limit: int = 1
    providers: tuple[str, ...] = ()

    def __post_init__(self) -> None:
        if not self.providers:
            raise ValueError(
                f"Combo {self.name!r} has empty providers list (combos must "
                "name at least one provider)"
            )
        if self.strategy not in _VALID_STRATEGIES:
            raise ValueError(
                f"Combo {self.name!r} has invalid strategy "
                f"{self.strategy!r}; valid: {_VALID_STRATEGIES}"
            )
        # Clamp sticky_limit to 1 minimum (the sticky cursor never sits on
        # a zero-width budget).
        if self.sticky_limit < 1:
            object.__setattr__(self, "sticky_limit", 1)


def compute_providers_hash(providers: tuple[str, ...]) -> str:
    """Stable short hash of an ordered providers tuple.

    Used by the cursor system to detect mid-session combo edits: when
    the user adds/removes/reorders providers, the stored cursor's hash
    no longer matches and the cursor resets cleanly to index 0.
    Order-sensitive because rotation is meaningful only relative to a
    fixed order.
    """
    payload = "|".join(providers).encode("utf-8")
    return hashlib.sha256(payload).hexdigest()[:16]


def next_healthy_provider(
    combo: Combo,
    *,
    exclude: Collection[str] = (),
    quota: Any = None,
    repo_root: Any = None,
) -> str | None:
    """First combo member whose live headroom is not EXHAUSTED, in combo order.

    The exhaustion-failover primitive. Reuses the SAME live ``headroom()``
    read as the dispatch ordering (never a stale snapshot), skipping any id
    in ``exclude`` (the already-known-exhausted provider) and any member reading
    EXHAUSTED. OK / LOW / UNKNOWN all count as healthy TARGETS - fail-open, since
    UNKNOWN never means exhausted, so a GLM/gemini record with no headroom
    signal is a valid failover destination. Returns ``None`` when every member is
    excluded or exhausted (the caller defers - defer is the floor).

    Walks ``combo.providers`` in the operator's configured priority order, NOT the
    round-robin cursor: exhaustion failover wants primary-then-secondary, a
    different concern from the round-robin rotation (and it stays pure - no
    cursor side effect).
    """
    from fno.adapters.providers.loader import load_quota_config
    from fno.adapters.providers.runtime_state import HeadroomState, headroom

    # ``repo_root`` scopes both reads to the repository the launch concerns. A
    # cross-project failover that judged health from the DISPATCHER's project
    # would pick an exhausted destination, or skip a healthy one, off another
    # project's snapshot.
    if quota is None:
        quota = load_quota_config(repo_root=repo_root)
    excluded = set(exclude)
    for pid in combo.providers:
        if pid in excluded:
            continue
        st = headroom(
            pid,
            ttl_seconds=quota.probe_ttl_seconds,
            threshold_pct=quota.defer_threshold_pct,
            repo_root=repo_root,
        ).state
        if st is not HeadroomState.EXHAUSTED:
            return pid
    return None
