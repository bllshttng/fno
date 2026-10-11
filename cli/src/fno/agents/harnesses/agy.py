"""Antigravity CLI (``agy``) conversation-id adapter.

``--conversation`` resumes and nothing creates, so the id comes from a
print-mode turn launched with ``--output-format stream-json``: line 1 is
``{"event":"init","conversation_id":...}``. On agy 1.3.3 the turn answers but
never exits (the harness Stop hook holds the process), so the exit envelope
never arrives: the mint reads the init line, terminates the process, and never
waits for it. The turn's argv is the Rust owner's answer
(crates/fno-agents/src/agy_launch.rs) and carries the spawn's selected axes.
Lane notes: docs/architecture/thread-lanes.md."""
from __future__ import annotations

import json
import os
import re
import select
import subprocess
import tempfile
import time
from pathlib import Path

from fno.agents.dispatch import DispatchAskError

AGY_BINARY = "agy"
# A ceiling, not a budget: the measured mint prints its init line in ~3s and
# the read loop returns on the id, so the full 45s is only paid by a wedged or
# unauthenticated binary. Inside the row's declared binding window
# (timeout_ms = 60000), so a wedged `agy` raises the refusal below instead of
# dying to a caller with a shorter fuse.
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


def _conversation_id_from_line(raw: bytes) -> tuple[str, bool]:
    """(conversation_id, saw_json_object) of one stream-json line: one parse
    answers both questions. Empty id when the line is not JSON or carries no
    such key (harness chatter, the final result)."""
    try:
        obj = json.loads(raw.decode("utf-8", errors="replace"))
    except ValueError:
        return "", False
    if isinstance(obj, dict):
        cid = obj.get("conversation_id")
        if cid:
            return str(cid), True
        return "", True
    return "", False


def _reap(proc: subprocess.Popen) -> None:
    """Never leave the mint process behind: the 1.3.3 print turn answers but
    the harness Stop hook holds it alive, so a found id terminates it; a
    wedged one escalates to kill."""
    if proc.poll() is not None:
        return
    proc.terminate()
    try:
        proc.wait(timeout=5)
    except subprocess.TimeoutExpired:
        proc.kill()
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            pass


def _stream_conversation_id(
    proc: subprocess.Popen, timeout_s: float
) -> tuple[str, bool, str, bool]:
    """Read the mint turn's stdout until a line carries a conversation_id.

    Returns (conversation_id, saw_json, output_tail, timed_out). The select
    loop never blocks past the deadline, so a binary that never exits (the
    1.3.3 print turn) still hands back the id printed on line 1.
    """
    stdout = proc.stdout
    if stdout is None:  # pragma: no cover - Popen(stdout=PIPE) always sets it
        raise AgySessionError("agy mint turn produced no stdout pipe")
    fd = stdout.fileno()
    deadline = time.monotonic() + timeout_s
    pending = b""
    tail = bytearray()
    saw_json = False
    while True:
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            return "", saw_json, tail.decode("utf-8", errors="replace"), True
        ready, _, _ = select.select([fd], [], [], min(remaining, 0.5))
        if not ready:
            continue
        chunk = os.read(fd, 65536)
        if not chunk:
            break  # EOF: pipe closed and drained
        pending += chunk
        while b"\n" in pending:
            line, pending = pending.split(b"\n", 1)
            conversation_id, saw_json_line = _conversation_id_from_line(line)
            saw_json = saw_json or saw_json_line
            if conversation_id:
                tail_str = tail.decode("utf-8", errors="replace")
                return conversation_id, saw_json, tail_str, False
            if line.strip():
                tail.extend(line[:400])
                if len(tail) > 4096:
                    del tail[: len(tail) - 4096]
    if pending.strip():
        conversation_id, saw_json_line = _conversation_id_from_line(pending)
        saw_json = saw_json or saw_json_line
        if conversation_id:
            tail_str = tail.decode("utf-8", errors="replace")
            return conversation_id, saw_json, tail_str, False
        tail.extend(pending[:400])
    return "", saw_json, tail.decode("utf-8", errors="replace"), False


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
    reader takes the first line carrying a conversation_id - the init event on
    line 1 - and terminates the process; a cleanly-exiting binary would be
    reaped through the same read loop."""
    from fno.agents.spawn_axes_client import agy_mint_argv

    argv = agy_mint_argv(model, effort, permission_mode, yolo)
    with tempfile.TemporaryFile() as stderr_file:
        try:
            proc = subprocess.Popen(  # noqa: S603
                argv, cwd=str(cwd), stdout=subprocess.PIPE, stderr=stderr_file
            )
        except OSError as exc:
            raise AgySessionError(f"agy could not start: {exc}") from exc
        try:
            conversation_id, saw_json, tail, timed_out = _stream_conversation_id(
                proc, timeout_s
            )
        finally:
            _reap(proc)
        stderr_file.seek(0)
        detail = stderr_file.read().decode("utf-8", errors="replace").strip()

    if conversation_id:
        return require_conversation_id(conversation_id)
    if timed_out:
        raise AgySessionError(
            f"agy minted no conversation id within {timeout_s:g}s; an "
            "unauthenticated or wedged binary never prints its init line"
        )
    if proc.returncode != 0:
        raise AgySessionError(
            f"agy exited {proc.returncode} while minting a conversation "
            f"id: {detail or tail[:400] or 'no output'}"
        )
    if not saw_json:
        raise AgySessionError(
            "agy print-mode output was not JSON, so no conversation id could "
            f"be read: {tail[:400] or 'no output'}"
        )
    raise AgySessionError(
        "agy print-mode JSON carried no conversation_id line: "
        f"{tail[:400] or 'no output'}"
    )


def conversation_store_path(conversation_id: str) -> Path:
    """Where agy keeps this conversation - its OWN store, so a journey asserts
    the spawn's id against the harness, not against fno echoing itself."""
    store = Path.home() / ".gemini" / "antigravity-cli" / "conversations"
    return store / f"{require_conversation_id(conversation_id)}.db"
