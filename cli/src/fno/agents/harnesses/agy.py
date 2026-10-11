"""Antigravity CLI (``agy``) conversation-id adapter.

``--conversation`` resumes and nothing creates, so the id comes from a
print-mode turn launched with ``--output-format stream-json``: line 1 is
``{"event":"init","conversation_id":...}``, long before any exit. The turn's
argv is the Rust owner's answer (crates/fno-agents/src/agy_launch.rs) and
carries the spawn's selected axes. Lane notes: docs/architecture/thread-lanes.md."""
from __future__ import annotations

import json
import re
import subprocess
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


def _scan_stream(stdout: bytes) -> str:
    """The first conversation_id in the captured stream output: the init
    event carries it on line 1. Empty when no line carries one."""
    for raw in stdout.split(b"\n"):
        try:
            obj = json.loads(raw.decode("utf-8", errors="replace"))
        except ValueError:
            continue
        if isinstance(obj, dict) and obj.get("conversation_id"):
            return str(obj["conversation_id"])
    return ""


def create_conversation(
    cwd: Path | str,
    *,
    model: str | None = None,
    effort: str | None = None,
    permission_mode: str | None = None,
    yolo: bool = False,
    timeout_s: float = MINT_TIMEOUT_S,
) -> str:
    """Mint an agy conversation id from the mint turn's stream-json output.
    The argv is the Rust owner's answer, so the mint carries the same model,
    effort and permission posture the spawn selected. The turn answers but
    never exits on agy 1.3.3 (the harness Stop hook holds the process), so the
    id is read from the captured stream - the init line - and the process is
    killed; communicate's retry after the kill keeps every captured byte."""
    from fno.agents.spawn_axes_client import agy_mint_argv

    argv = agy_mint_argv(model, effort, permission_mode, yolo)
    try:
        proc = subprocess.Popen(  # noqa: S603
            argv, cwd=str(cwd), stdout=subprocess.PIPE, stderr=subprocess.PIPE
        )
    except OSError as exc:
        raise AgySessionError(f"agy could not start: {exc}") from exc
    timed_out = False
    try:
        stdout, stderr = proc.communicate(timeout=timeout_s)
    except subprocess.TimeoutExpired:
        # The id is in the output captured so far; the retry keeps it.
        timed_out = True
        proc.kill()
        stdout, stderr = proc.communicate()
    conversation_id = _scan_stream(stdout or b"")
    if conversation_id:
        return require_conversation_id(conversation_id)
    detail = (stderr or b"").decode("utf-8", errors="replace").strip()
    tail = (stdout or b"").decode("utf-8", errors="replace").strip()[:400]
    if timed_out:
        raise AgySessionError(
            f"agy minted no conversation id within {timeout_s:g}s; an "
            "unauthenticated or wedged binary never prints its init line"
        )
    if proc.returncode != 0:
        raise AgySessionError(
            f"agy exited {proc.returncode} while minting a conversation "
            f"id: {detail or tail or 'no output'}"
        )
    raise AgySessionError(
        "agy print-mode output carried no conversation_id: "
        f"{tail or detail or 'no output'}"
    )


def conversation_store_path(conversation_id: str) -> Path:
    """Where agy keeps this conversation - its OWN store, so a journey asserts
    the spawn's id against the harness, not against fno echoing itself."""
    store = Path.home() / ".gemini" / "antigravity-cli" / "conversations"
    return store / f"{require_conversation_id(conversation_id)}.db"
