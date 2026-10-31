"""Transport for native thread promotion; shared writers own persisted effects."""
from typing import Optional

from fno.agents.spawn_overlay_client import SpawnOverlayUnavailable, spawn_overlay_call


def lead_typed_message(message: str, level: Optional[int], scope: Optional[str],
                       revive: bool, harness: str = "claude") -> tuple[str, bool]:
    from fno.agents.dispatch import DispatchAskError

    try:
        answer = spawn_overlay_call({"kind": "spawn-team", "op": "seed", "message": message,
                                     "level": level, "scope": scope, "revive": revive, "harness": harness})
    except SpawnOverlayUnavailable as exc:
        raise DispatchAskError(f"--promote: {exc}", exit_code=2) from exc
    return answer["message"], answer["typed"]


