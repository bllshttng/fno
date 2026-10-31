"""Which model a session's transcript says answered, and where that file is.

A provenance fact, not a runtime one: the question "what did this session
ACTUALLY answer as" is asked by the agent runtime (``fno agents truth``) AND by
the graph writer that stamps a node's session rows, which sit on opposite sides
of the runtime boundary. It lives here, next to :mod:`fno.provenance.resolver`,
because a transcript pointer and what the transcript says about itself are the
same concern; ``fno.agents.session_truth`` re-exports both names so its own
callers are unaffected.

A route stamped at spawn records INTENT, so it reports the intended model in
exactly the case an operator suspects a silent fallback (an ``ANTHROPIC_MODEL``
surviving without its ``ANTHROPIC_BASE_URL``). The transcript reports what the
vendor answered as. Read-only; never writes; never raises.
"""
from __future__ import annotations

from pathlib import Path
from typing import Any, Optional


FILE_BACKED_HARNESSES = frozenset({"claude", "codex"})
_MODEL_TAIL_BYTES = 256 * 1024


def resolve_transcript_path(
    agent: str,
    session_id: str,
    cwd: str,
    projects_root: Optional[Path] = None,
    codex_sessions_dir: Optional[Path] = None,
) -> Optional[Path]:
    """The session's own transcript FILE, for the harnesses that keep one.

    Only claude and codex are file-backed; opencode keeps a shared SQLite store
    with no per-session file, and an unsupported harness resolves to nothing.
    Resolved once per call and handed to every reader below, so the tail read,
    the age probe, and the model read all agree on which file they are looking
    at instead of each running its own discovery.

    Public because :func:`observed_model` takes a path rather than finding one,
    so every caller outside this module needs this to reach it -- see
    ``fno.graph.store._observe_model``. Never raises; an unresolvable pointer is
    ``None``, which ``observed_model`` reports as ``no-transcript``.
    """
    if agent not in FILE_BACKED_HARNESSES:
        return None
    try:
        from fno.provenance.resolver import resolve_transcript

        rt = resolve_transcript(
            agent,
            session_id,
            cwd,
            projects_root=projects_root,
            codex_sessions_dir=codex_sessions_dir,
        )
    except Exception:  # noqa: BLE001 — discovery failure -> no path, never raise
        return None
    if not rt.resolved or not rt.transcript_path:
        return None
    return Path(rt.transcript_path)


def observed_model(agent: str, transcript_path: Optional[Path]) -> dict[str, Any]:
    """Read the native provenance receipt without raising into reporting callers."""
    if agent not in FILE_BACKED_HARNESSES:
        return {"kind": "not-file-backed"}
    if transcript_path is None:
        return {"kind": "no-transcript"}

    from fno.rust_binary import call_binary_json

    args = ["observed-model", "--agent", agent]
    if transcript_path is not None:
        args.extend(["--path", str(transcript_path)])
    error, receipt = call_binary_json("attest", args)
    if error is not None:
        return {"kind": "unreadable", "reason": error}
    if not isinstance(receipt, dict):
        return {"kind": "unreadable", "reason": "unreadable JSON receipt"}
    return receipt


def observed_model_for_session(agent: str, session_id: str, cwd: str) -> dict[str, Any]:
    """:func:`observed_model` for a session POINTER, keeping a broken resolver
    distinguishable from an absent transcript.

    :func:`resolve_transcript_path` answers "which file" and collapses every
    failure to ``None``, which the reader then reports as ``no-transcript``.
    That is right for a caller who only wants a path, and wrong for a caller
    recording durable evidence: a resolver that failed on permissions, a
    schema-drifted store, or an unreadable directory becomes indistinguishable
    from a session that simply has no transcript yet. An absence has two
    explanations and a row built on one cannot tell them apart, which is the
    whole reason the variant vocabulary exists.

    So this resolves with the reason in hand and maps it::

        reason "error" / a raised resolver  -> {"kind": "unreadable", "reason": ...}
        harness-not-supported               -> {"kind": "not-file-backed"}
        anything else unresolved            -> {"kind": "no-transcript"}

    Never raises.
    """
    if agent not in FILE_BACKED_HARNESSES:
        return {"kind": "not-file-backed"}
    try:
        from fno.provenance.resolver import resolve_transcript

        rt = resolve_transcript(agent, session_id, cwd)
    except Exception as exc:  # noqa: BLE001 - a broken resolver is a NAMED unknown
        return {"kind": "unreadable", "reason": f"resolver raised: {exc}"}
    if not rt.resolved or not rt.transcript_path:
        if rt.reason == "error":
            return {"kind": "unreadable", "reason": "transcript resolution failed"}
        if rt.reason == "harness-not-supported":
            return {"kind": "not-file-backed"}
        return {"kind": "no-transcript"}
    return observed_model(agent, Path(rt.transcript_path))
