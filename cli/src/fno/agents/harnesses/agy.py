"""Antigravity CLI (``agy``) conversation-id adapter.

``--conversation`` resumes and nothing creates, so the id comes from a
print-mode turn launched with ``--output-format stream-json``: line 1 is
``{"event":"init","conversation_id":...}``, long before any exit. The turn
runs in the Rust runtime (crates/fno-agents/src/agy_mint.rs) over the argv
owner's answer (agy_launch.rs); this module validates the answer and raises
the harness-facing refusals. Lane notes: docs/architecture/thread-lanes.md."""
from __future__ import annotations

import re
from pathlib import Path

from fno.agents.dispatch import DispatchAskError

AGY_BINARY = "agy"
# A ceiling, not a budget: the measured mint prints its init line in ~3s and
# the capture returns on it. Inside the row's declared binding window
# (timeout_ms = 60000), so a wedged `agy` raises the refusal below.
MINT_TIMEOUT_S = 45.0
_UUID_RE = re.compile(
    r"^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-"
    r"[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$"
)


class AgySessionError(DispatchAskError):
    """agy returned or received no usable conversation id."""

    def __init__(self, message: str) -> None:
        super().__init__(message, exit_code=2)


def require_conversation_id(conversation_id: str) -> str:
    """Validate before any process launches on it: a truncated id names a
    DIFFERENT conversation to agy, never a resume."""
    text = (conversation_id or "").strip()
    if not _UUID_RE.match(text):
        raise AgySessionError(
            f"agy conversation id {conversation_id!r} is not a full UUID; "
            "agy resumes by exact id and a partial one silently opens a "
            "different conversation"
        )
    return text


def create_conversation(
    cwd: Path | str,
    *,
    model: str | None = None,
    effort: str | None = None,
    permission_mode: str | None = None,
    yolo: bool = False,
    timeout_s: float = MINT_TIMEOUT_S,
) -> str:
    """Mint an agy conversation id by running one stream-json print turn. The
    runtime reads the id from the stream (the init line) and kills the held
    process - the turn answers but never exits on agy 1.3.3; an id that fails
    validation never reaches a spawn."""
    from fno.agents.spawn_axes_client import _answer_or_raise, spawn_axes_call

    ask = {
        "cwd": str(cwd),
        "model": model or "",
        "effort": effort or "",
        "permission_mode": permission_mode or "",
        "yolo": bool(yolo),
        "timeout_s": timeout_s,
    }
    answer = spawn_axes_call({"agy_mint_run": ask})
    return require_conversation_id(str(_answer_or_raise(answer, "conversation_id")))


def conversation_store_path(conversation_id: str) -> Path:
    """Where agy keeps this conversation - its OWN store, so a journey asserts
    the spawn's id against the harness, not against fno echoing itself."""
    store = Path.home() / ".gemini" / "antigravity-cli" / "conversations"
    return store / f"{require_conversation_id(conversation_id)}.db"
