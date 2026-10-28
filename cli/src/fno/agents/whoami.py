"""fno.agents.whoami: the session-to-row matchers ``fno agents whoami`` shared.

The command itself is native (crates/fno-agents/src/registry_commands.rs). What
stays here is read by Python callers: the harness-scoped session matcher the
role authorization and succession checks use, and the held-node reader the
carveout stamp uses. Read-only by construction.
"""
from __future__ import annotations

import re
from pathlib import Path
from typing import Optional

from fno.agents.registry import AgentEntry


def _row_harness(entry: AgentEntry) -> str:
    """This row's harness for matching (: harness is the sole identity axis;
    a legacy row's provider is back-filled into harness at load)."""
    return (getattr(entry, "harness", None) or "").lower()


def _find_by_session(
    registry: list[AgentEntry],
    session_uuid: str,
    harness: Optional[str] = None,
) -> Optional[AgentEntry]:
    """Match a registry row whose recorded session id equals ``session_uuid``.

    Session ids are provider-local: the SAME id may be registered under different
    harnesses, so when ``harness`` is known matching is SCOPED to rows of
    that harness and to that harness's own id fields. This prevents a codex/gemini
    id from matching a same-id claude row, and stops a non-claude id from falling
    through to the claude short-id prefix (a 32-bit jobId prefix that could collide).

    Two passes, most-specific first:
    1. Exact match against the harness's own id fields (canonical
       ``harness_session_id`` plus that harness's legacy field).
    2. Prefix match against the 8-hex jobId in ``short_id`` - CLAUDE ONLY, because the
       jobId is a 32-bit prefix of a claude session UUID; a partially-captured
       claude row may carry only the short id.

    ``harness=None`` keeps the original claude-shaped scan for callers that do not
    know the harness (back-compat).
    """
    if harness:
        h = harness.lower()
        for entry in registry:
            if _row_harness(entry) != h:
                continue
            # harness_session_id is canonical (backfilled from every legacy
            # per-harness id on load); cc_session_id is a distinct claude field
            # the canonical does not cover, so it stays an extra match candidate.
            fields = [entry.harness_session_id]
            if h == "claude":
                fields.append(entry.cc_session_id)
            if session_uuid in fields:
                return entry
        if h == "claude":
            norm = session_uuid.replace("-", "").lower()
            for entry in registry:
                if _row_harness(entry) != "claude":
                    continue
                # A hand-started registered row stores the FULL uuid in short_id
                # (register_existing_session / `fno agents register`); a spawn row
                # stores the 8-hex jobId prefix. De-hyphenate so the prefix match
                # accepts either shape - else a /fno-me claude session reports
                # unregistered despite a written row.
                #
                # ponytail: the prefix match is 32 bits of jobId. A same-harness
                # row whose jobId prefix collides with this session's uuid would
                # match the wrong row - in succession (is_caller_row) that vacates
                # a role that is not the stepping_down lead's. ~1/2^32 per pair, and
                # harness scoping above already cut it to same-harness rows. The
                # upgrade is storing the FULL session id on spawn rows, not
                # tightening the matcher here: that re-diverges auth and succession.
                sid = (entry.short_id or "").replace("-", "").lower()
                if sid and norm.startswith(sid):
                    return entry
        return None

    # Unknown harness: preserve the original claude-shaped behavior.
    for entry in registry:
        if session_uuid in (
            entry.harness_session_id,
            entry.cc_session_id,
        ):
            return entry
    norm = session_uuid.replace("-", "").lower()
    for entry in registry:
        # The prefix rule holds only for claude jobIds (a uuid prefix); a
        # daemon worker's name-derived short must never prefix-match a uuid.
        if entry.harness != "claude":
            continue
        sid = (entry.short_id or "").replace("-", "").lower()
        if sid and norm.startswith(sid):
            return entry
    return None


def is_caller_row(
    row, session_uuid: Optional[str], harness: Optional[str] = None
) -> bool:
    """Is ``row`` the session that issued the current spawn - the lead whose
    role a succession would transfer?

    Delegates to :func:`_find_by_session` so the succession check can NEVER
    diverge from the authorization check (:func:`calling_agent_row` runs the same
    ``_find_by_session`` over the full registry). The matching is harness-scoped
    (a codex session id cannot match a claude row and vacate an unrelated role),
    and for claude spans ``harness_session_id``, ``cc_session_id``, and the
    ``short_id`` prefix - the complete matcher. Re-implementing any of it here is
    the bug: a partial copy diverged from the auth check and either declined a
    legitimate departure (a field missed) or transferred the wrong role (no
    harness scoping). One matcher, two callers; if the auth check found the lead,
    this finds the same lead.
    """
    if not session_uuid:
        return False
    return _find_by_session([row], session_uuid, harness) is not None


def _scan_field(text: str, key: str) -> Optional[str]:
    """Return the value of the first ``<key>: <value>`` line in ``text``.

    Strips only a MATCHED surrounding quote pair, so a value with a lone
    leading/trailing quote is preserved rather than mangled. ``None`` if the
    key is absent.
    """
    pattern = re.compile(rf"^{re.escape(key)}:\s*(\S+)")
    for line in text.splitlines():
        match = pattern.match(line)
        if match:
            value = match.group(1).strip()
            if len(value) >= 2 and value[0] == value[-1] and value[0] in ('"', "'"):
                value = value[1:-1]
            return value
    return None


def find_held_node(cwd: str = ".", session_uuid: Optional[str] = None) -> Optional[str]:
    """The backlog node THIS worker holds, as ``node:<id>`` — or ``None``.

    A dispatched ``/target`` worker records its bound node in the local
    ``.fno/target-state.md`` body as ``graph_node_id: <id>``. But the file
    belongs to whatever session owns that worktree, NOT necessarily the caller:
    a stale manifest, a reused cwd, or an env-only mesh worker booted in someone
    else's worktree would otherwise let us emit a node this worker does not hold.

    So we attribute the node ONLY when ownership is proven: the manifest's
    ``claude_transcript_id`` must equal this process's ``session_uuid``
    (``CLAUDE_CODE_SESSION_ID``). The claim holder is keyed on the target
    *session* id (not the mesh name/session), so there is no registry-side join;
    the transcript id is the one field that ties the manifest to the live
    process. Without ``session_uuid`` (e.g. a codex/gemini worker) or on any
    mismatch we return ``None`` rather than guess. Returns ``None`` too when the
    file is absent, the field is missing, or it is the ``null`` sentinel.
    """
    if not session_uuid:
        return None
    state = Path(cwd) / ".fno" / "target-state.md"
    try:
        if not state.is_file():
            return None
        text = state.read_text(encoding="utf-8")
    except OSError:
        return None
    # Current key is claude_session_id; fall back to the pre-rename
    # claude_transcript_id for one release so in-flight manifests still match.
    manifest_claude_sid = _scan_field(text, "claude_session_id") or _scan_field(
        text, "claude_transcript_id"
    )
    if manifest_claude_sid != session_uuid:
        return None  # manifest is not this worker's session — never guess
    value = _scan_field(text, "graph_node_id")
    if value and value.lower() != "null":
        return f"node:{value}"
    return None
