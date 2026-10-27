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
import logging
from typing import Literal


logger = logging.getLogger(__name__)


_VALID_STRATEGIES = ("fallback", "round_robin")


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
