"""Transport for native thread promotion; shared writers own persisted effects."""
from pathlib import Path
import sys
from typing import Optional

from fno.agents.role import calling_agent_row, plan_spawn_role, spawn_team_write
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


def plan_thread_promotion(message: str, level: int, scope: Optional[str], succession: bool,
                          take_over: Optional[str], harness: str, parent_edge: Optional[tuple],
                          *, name: str, revive: bool = False) -> dict:
    from fno.agents.dispatch import DispatchAskError, _capture_parent_edge

    caller = calling_agent_row()
    refusal, plan = plan_spawn_role(scope or "", caller, succession, proposed_name=name, take_over=take_over)
    if refusal is not None:
        raise DispatchAskError(f"--promote: {refusal}", exit_code=2)
    message, typed = lead_typed_message(message, level, scope, revive, harness)
    return {"message": message, "typed": typed, "plan": plan, "caller": getattr(caller, "name", None),
            "level": level, "scope": scope, "grantor": (parent_edge or _capture_parent_edge())[0] or "human",
            "harness": harness}


def settle_thread_promotion(ask: dict, *, name: str, cwd: Path, session_id: str) -> None:
    """Rust's spawn-team settle op stamps, arms, journals and prints the receipt."""
    from fno.lead.state import lead_loop_enabled

    try:
        spawn_team_write("settle", **ask, name=name, cwd=str(cwd), session_id=session_id,
                         lead_enabled=lead_loop_enabled())
    except SpawnOverlayUnavailable as exc:
        print(f"spawn: promotion settlement failed after launch: {exc}; {name!r} runs "
              "without a confirmed role. Grant it with fno agents org promote.", file=sys.stderr)
