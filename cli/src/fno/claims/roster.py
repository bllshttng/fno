from __future__ import annotations

from datetime import datetime

from fno.rust_binary import (  # noqa: F401 - re-exported import sites
    RosterReading,
    _finished_row_states,
    read_roster,
)


def classify_workers(workers: list) -> tuple[list, list, dict]:
    """(engaged, unmeasurable, verdicts-by-name) for one roster read."""
    from fno.agents.reachability import REACHABLE, UNKNOWN

    verdicts = {w.get("name") or "": _worker_reachability(w).verdict for w in workers}
    return (
        [w for w in workers if verdicts.get(w.get("name") or "") == REACHABLE],
        [w for w in workers if verdicts.get(w.get("name") or "") == UNKNOWN],
        verdicts,
    )


def _worker_reachability(worker: dict):
    """One roster row through the ONE shared predicate (task 1.1).

    REACHABLE engaged, UNREACHABLE finished, UNKNOWN its own arm - never
    engaged-by-default. The transcript outranks the supervisor word for EVERY
    row (a finished worker's row never leaves `working`); a terminal word
    with no transcript stays positive evidence of the end.
    """
    from fno.agents.reachability import (
        TRANSCRIPT_EVIDENCE_S,
        classify_reachability,
        pane_falsifier,
        pid_falsifier,
    )

    state = worker.get("state")
    try:
        import time

        from fno.agents.watchdog import classify_tail, harness_for_session, tail_facts

        facts = tail_facts(
            worker.get("row_id") or "", worker.get("cwd") or "",
            agent=harness_for_session(worker.get("row_id") or ""),
        )
    except Exception:  # noqa: BLE001 - an unreadable transcript answers nothing
        facts = None
    # The pid falsifier fires only on an INCARNATION-PROVEN pid (a recorded
    # start token, the same bound the claims layer's hybrid arm uses): a bare
    # stale pid on a live worker must never read as death. A FRESH tail is a
    # resumed session's witness over even a proven corpse.
    pid = worker.get("pid")
    proven = pid is not None and worker.get("pid_start_time") is not None
    falsifier = pid_falsifier(pid, worker.get("pid_start_time")) if proven else None
    if falsifier is None:
        falsifier = pane_falsifier(worker.get("mux"))
    stamp = worker.get("stopped_at")
    try:
        stop_epoch = datetime.fromisoformat(stamp.replace("Z", "+00:00")).timestamp() if stamp else None
    except (AttributeError, ValueError):
        stop_epoch = None
    if facts is None:
        # No transcript: the supervisor word is the only evidence. An active
        # word stays reachable; a terminal word positively ended the row.
        if falsifier is not None:
            return classify_reachability(truth_state=None, age_s=None, falsifier=falsifier)
        if state in ("working", "watching", "your-move"):
            return classify_reachability(truth_state=state, age_s=None, falsifier=None)
        finished = f"finished-state:{state}" if state in _finished_row_states() else None
        return classify_reachability(truth_state=None, age_s=None, falsifier=finished)
    if facts.last_event_epoch is None:
        # A transcript PRESENT but undatable: the measured wrong answer read
        # this as engaged. It is UNKNOWN - a verdict about the instrument -
        # never engaged-by-default and never positively finished.
        return classify_reachability(truth_state=None, age_s=None, falsifier=falsifier)
    age = int(max(0.0, time.time() - facts.last_event_epoch))
    if falsifier is not None and age <= TRANSCRIPT_EVIDENCE_S:
        falsifier = None
    if stop_epoch is not None and facts.last_event_epoch <= stop_epoch:
        falsifier = f"stopped:{worker.get('stopped_at')}"
    if falsifier is None and age > TRANSCRIPT_EVIDENCE_S and state in _finished_row_states():
        # A terminal word with a silent tail is positive evidence of the end.
        falsifier = f"finished-state:{state}"
    return classify_reachability(
        truth_state=classify_tail(facts.last_role, facts.last_text, age),
        age_s=age,
        falsifier=falsifier,
    )
