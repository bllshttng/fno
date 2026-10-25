"""The machine-lane channel shim: one ``command_call("fleet-task", ...)``.

The reconcile fold for the machine writers (heal, pr-nudge, the watchdog
lanes) lives in ``crates/fno-agents/src/fleet_task.rs``; Python sends one
payload and returns the outcome. ``already_asked`` and ``dedupe_key``
survive here for the writers that stay questions.
"""
from __future__ import annotations

import hashlib
from pathlib import Path

MARKER = "watchdog-unfinished-work"
MAX_LISTED_ROWS = 10

#: Severity order for the question's rows and its ask line: the same order
#: the report's digest uses, so "clear the top finding first" names the
#: finding the digest lists first, not whichever sorts alphabetically.
from fno.agents.unfinished_work import DIMENSIONS as _DIMENSIONS  # noqa: E402

_SEVERITY = {kind: i for i, kind in enumerate(_DIMENSIONS)}


def dedupe_key(identities: "list[str]") -> str:
    joined = "\n".join(sorted(set(identities)))
    return hashlib.sha256(joined.encode("utf-8")).hexdigest()[:12]


def _unique(findings):
    return sorted(
        {f"{f.kind}:{f.subject}": f for f in findings}.values(),
        key=lambda f: (_SEVERITY.get(f.kind, len(_SEVERITY)), f.subject),
    )


def question_text(findings, key: str, unknown_dimensions=()) -> str:
    unique = _unique(findings)
    shown = [
        f"{f.kind} {f.subject}: {f.basis} -> clear: {f.clear_command}"
        for f in unique[:MAX_LISTED_ROWS]
    ]
    if len(unique) > MAX_LISTED_ROWS:
        shown.append(f"and {len(unique) - MAX_LISTED_ROWS} more")
    # An incomplete scan escalates what it DID reach and names what it did
    # not. Withholding the whole ask instead made the sweep permanently mute
    # wherever a deleted worktree root can never be fetched again.
    caveat = ""
    if unknown_dimensions:
        caveat = (
            f" The scan was INCOMPLETE - {', '.join(sorted(unknown_dimensions))} "
            f"went unread, so more findings may exist."
        )
    return (
        f"[{MARKER}:{key}] The fleet watchdog found {len(unique)} "
        f"unfinished-work finding(s). Each names the one command that clears "
        f"it.{caveat} Findings: {'; '.join(shown)}"
    )


def _ask_line(findings) -> str:
    return _unique(findings)[0].clear_command


def already_asked(root: Path, key: str, *, marker: str = MARKER) -> "str | None":
    """The id of the open question carrying ``[<marker>:<key>]``, else None.

    One shared dedupe fold for every durable question emitter: a marker key
    asked once stays asked, and a second emitter with its own marker reuses
    this fold rather than growing a second copy of it. The open-question read
    is the native listing (`fno-agents compaction outstanding --json`), the
    same fold the mux needs-me queue serves; a listing that cannot run reads
    as un-asked, and the ask door's own dedup refusal catches the duplicate.
    """
    _ = root  # The question store is machine-wide; the listing folds it whole.
    import json
    import subprocess

    from fno.rust_binary import resolve_binary

    binary = resolve_binary()
    if binary is None:
        return None
    try:
        proc = subprocess.run(
            [str(binary), "compaction", "outstanding", "--json"],
            capture_output=True,
            text=True,
            timeout=30,
            check=False,
        )
    except (OSError, subprocess.TimeoutExpired):
        return None
    if proc.returncode != 0:
        return None
    try:
        payload = json.loads(proc.stdout)
    except ValueError:
        return None
    needle = f"[{marker}:{key}]"
    for question in payload.get("questions") or ():
        qid = question.get("id")
        if qid and needle in (question.get("question") or ""):
            return qid
    return None


def reconcile_channel(
    pairs, *, root: Path, session_id: "str | None", cwd: Path,
    marker: str, subject: str, identities: "list[str]",
    question, ask, asker: "str | None" = None,
) -> "tuple[str, str]":
    """Reconcile ONE lane's fleet task to the measured ``pairs`` through the
    Rust fleet-task door: same key is a duplicate, a changed set supersedes,
    an empty set closes. ``question``/``ask`` are callables taking the
    dedupe ``key``; outcome in ``none | duplicate | asked | closed``. A
    failed transport raises the ``command_call`` refusal and writes nothing."""
    from fno.rust_binary import command_call

    key = dedupe_key(identities)
    answer = command_call(
        "fleet-task",
        {
            "op": "reconcile",
            "lane": marker,
            "key": key,
            "cwd": str(cwd),
            "text": question(key),
            "run": ask(key),
            "empty": not pairs,
        },
    )
    return (answer["outcome"], answer.get("id", ""))


def escalate_unfinished(
    findings,
    *,
    root: Path,
    session_id: "str | None",
    cwd: Path,
    unknown_dimensions=(),
) -> "tuple[str, str]":
    if not findings:
        outcome, qid = reconcile_channel(
            [],
            root=root,
            session_id=session_id,
            cwd=cwd,
            marker=MARKER,
            subject="unfinished-work",
            identities=[],
            question=lambda _key: "",
            ask=lambda _key: "",
        )
        return ("none", "") if outcome == "none" else (outcome, qid)

    unique = _unique(findings)
    outcome, qid = reconcile_channel(
        unique,
        root=root,
        session_id=session_id,
        cwd=cwd,
        marker=MARKER,
        subject="unfinished-work",
        identities=[f"{f.kind}:{f.subject}" for f in unique],
        question=lambda key: question_text(unique, key, unknown_dimensions),
        ask=lambda _key: f"clear the top finding first: {_ask_line(unique)}",
    )
    return ("recorded", qid) if outcome == "asked" else (outcome, qid)
