"""High-level claim verbs.

Seven operations on top of io + staleness:

    acquire_claim     - try to take a claim; idempotent re-acquire,
                        stale recovery, live-other detection.
    release_claim     - drop a claim we own.
    refresh_claim     - extend TTL on a claim we own (no-op for PID-liveness).
    claim_status      - inspect a single key.
    list_claims       - enumerate all live (and optionally stale) claims.
    force_release_claim - administrative override, always succeeds.

Rust owns claim persistence and audit receipts in graph.db. Python is a transport client.
"""

from __future__ import annotations

from pathlib import Path
from typing import Any, Callable, Optional

from fno.rust_binary import (
    CLAIM_UNAVAILABLE,
    ClaimContended,
    ClaimHeldByOther,
    ClaimValidationError,
    ClaimVerdictError,
    ClaimVerdictUnavailable,
    ForceReleaseOutcome,
    HolderMismatch,
    _configured_claim_root,
    _validate_inputs,
    claim_op as _native_claim,
    claim_status_payload,
    native_acquire,
    native_claims_root,
    native_force_release,
    native_release,
    native_root_flags as _native_root_flags,
)
from .io import (ClaimAlreadyHeld, ClaimCorrupted, ClaimGoneAway, claim_path)
from .verdict import (
    ClaimSweepOmission,
    claim_verdicts,
)
from .types import (MAX_ENCODED_FILENAME_BYTES, MAX_KEY_LENGTH, MAX_TTL_MS, MIN_TTL_MS, Claim, ClaimState)


class RebindRefused(Exception):
    """``compare_and_rebind`` refused to move the claim (fail-closed).

    Native resume never silently believes it owns a claim: every path that is
    not an affirmative same-holder local rebind raises this with a named
    reason. Carries the observed ``state``/``holder``/``pid`` so the caller can
    render a loud, specific refusal.
    """

    def __init__(
        self,
        reason: str,
        *,
        state: Optional[str] = None,
        holder: Optional[str] = None,
        pid: Optional[int] = None,
    ) -> None:
        self.reason = reason
        self.state = state
        self.holder = holder
        self.pid = pid
        super().__init__(reason)


# Re-export low-level exceptions so callers can ``from fno.claims import ClaimGoneAway``.
__all__ = [
    "CLAIM_UNAVAILABLE",
    "ClaimAlreadyHeld",
    "ClaimContended",
    "ClaimCorrupted",
    "ClaimGoneAway",
    "ClaimHeldByOther",
    "ClaimSweepOmission",
    "ClaimValidationError",
    "ClaimVerdictError",
    "ClaimVerdictUnavailable",
    "ForceReleaseOutcome",
    "HolderMismatch",
    "RebindRefused",
    "acquire_claim",
    "claim_status",
    "compare_and_rebind",
    "force_release_claim",
    "list_claims",
    "list_claims_with_counts",
    "sweep_verdict",
    "refresh_claim",
    "release_claim",
]


def compare_and_rebind(
    key: str,
    expected_holder: str,
    *,
    new_holder: Optional[str] = None,
    new_reason: Optional[str] = None,
    new_harness: Optional[str] = None,
    new_metadata: Optional[dict] = None,
    new_pid: Optional[int] = None,
    new_pid_unavailable: bool = False,
    ttl_ms: Optional[int] = None,
    root: Optional[Path] = None,
    emit: bool = True,
    fno_id: Optional[str] = None,
    harness_tag: Optional[str] = None,
    harness_session_id: Optional[str] = None,
) -> tuple[Claim, str]:
    del emit, fno_id, harness_tag
    import json
    flags = ["--holder", new_holder or expected_holder, "--handover-from", expected_holder, "--bind-only"]
    for flag,value in [("--reason",new_reason),("--harness",new_harness),("--session-id",harness_session_id),("--pid",new_pid),("--ttl-ms",ttl_ms)]:
        if value is not None:
            flags.extend((flag,str(value)))
    if new_metadata is not None:
        flags.extend(("--metadata",json.dumps(new_metadata)))
    if new_pid_unavailable:
        flags.append("--pid-unavailable")
    flags.extend(_native_root_flags(root or _configured_claim_root()))
    try:
        payload = _native_claim("acquire",key,flags)
    except ClaimVerdictError as exc:
        # The refusal names its reason; the observed row is the status read.
        try:
            seen = claim_status(key, root=root)
        except Exception:  # noqa: BLE001 - the refusal stands without detail
            seen = {}
        raise RebindRefused(
            str(exc), state=seen.get("state"), holder=seen.get("holder"), pid=seen.get("pid")
        ) from exc
    return _native_claim_model(payload), str(payload["mode"])


#: Holder prefix marking a claim taken by `fno agents spawn --node` on behalf of
#: a worker that does not exist yet. The handover branch accepts ONLY this
#: prefix as a replaceable prior holder. WHAT THIS IS NOT: a secret - what the
#: prefix restricts is the BLAST RADIUS: only a launch-window claim can be
#: taken over this way, never a working session's `target-session:` claim,
#: which `ClaimHeldByOther` still protects. The window is TTL-bound, so the
#: exposure is bounded. Closing it properly needs a secret the worker alone
#: holds, which is its own change. The constant lives in :mod:`fno.rust_binary`
#: beside the transport; this module re-exports it.
from fno.rust_binary import HANDOVER_HOLDER_PREFIX  # noqa: E402 - re-export

#: The requeue pseudo-holder (`backlog/requeue`): the session that takes the
#: node, not an agent. `holder_agent_name` is the one resolver.
TARGET_SESSION_HOLDER_PREFIX = "target-session:"

#: A subagent planner holds node:<id> under this prefix between `session open`
#: and `session close`, mirroring target-session. Resolved in the same branch.
BLUEPRINT_HOLDER_PREFIX = "blueprint-session:"


def holder_agent_name(holder: Optional[str], rows: Any) -> Optional[str]:
    """Resolve a claim holder to the agent behind it, or None.

    None means no row in ``rows`` stands behind the name: a skip, not a
    failure. ``rows`` is the caller's read; no agents import here.
    """
    if not holder:
        return None

    if holder.startswith(HANDOVER_HOLDER_PREFIX):
        name = holder[len(HANDOVER_HOLDER_PREFIX):]
        return name if any(row.name == name for row in rows) else None
    if holder.startswith((TARGET_SESSION_HOLDER_PREFIX, BLUEPRINT_HOLDER_PREFIX)):
        sid = holder.split(":", 1)[1]
        row = next(
            (
                r
                for r in rows
                if sid in {
                    r.harness_session_id,
                    getattr(r, "session_id", None),
                    getattr(r, "cc_session_id", None),
                }
            ),
            None,
        )
        return row.name if row else None
    return holder

#: Suffix of the per-claim recovery mutex directory. One definition: this
#: string was written out at six call sites, and a seventh (the dispatch
#: guard's targeted recovery) is what made the duplication worth collapsing.
#: A caller that spells it differently takes a DIFFERENT lock and silently
#: serializes against nobody.
RECOVERY_LOCK_SUFFIX = ".recovery.d"




def _claim_verdict(claim: Claim, *, root: Optional[Path] = None) -> dict[str, Any]:
    """Read one claim verdict from the native batch door."""
    verdict = claim_verdicts([claim.key], root=root).get(claim.key)
    if verdict is None:
        raise ClaimVerdictError(f"native verdict omitted readable claim {claim.key!r}")
    return verdict


def _claim_state(claim: Claim, *, root: Optional[Path] = None) -> ClaimState:
    """Project a native row into the Python enum used by control flow."""
    try:
        return ClaimState(_claim_verdict(claim, root=root).get("state", "corrupted"))
    except ValueError:
        return ClaimState.CORRUPTED


def sweep_verdict(
    claim: Claim,
    *,
    abandonment_probe: Optional[Callable[..., Optional[bool]]] = None,
    node_settlement: Optional[Callable[..., Optional[bool]]] = None,
    native_verdict: Optional[dict[str, Any]] = None,
) -> tuple[bool, str]:
    """Can this claim be archived, and if not, which bucket says why?

    ``node_settlement`` runs FIRST on a ``node:`` claim and answers the
    question a pid cannot: is the claim's own node still the holder's
    workplace? A node that closed under the claim, or a holder whose roster
    row resolves to a DIFFERENT node, is positive evidence of abandonment -
    measured on the live 2026-08-21 specimen where a session that finished
    one node and moved to the next kept a LIVE claim on the dead one for 16
    hours, because "holder alive" was the only question asked.
    ``True`` settles (reapable); anything else falls through to liveness,
    which stays the authority for every unsettled shape.

    THE single reap decision. The sweep's lock-free triage, its under-mutex
    re-verify, and the spawn guard's targeted recovery all call this, so no two
    of them can drift into different answers about the same claim - the shape
    the first pitfalls entry names.

    Buckets: ``""`` (reapable), ``"live"``, ``"offhost"``, ``"suspect"`` (no
    probe was supplied), ``"suspect_alive"`` (a worker is on the node) and
    ``"suspect_unprobed"`` (the probe could not run). The last two are kept
    apart deliberately: one is a measurement and the other is its absence.
    """
    verdict = native_verdict or _claim_verdict(claim)
    if node_settlement is not None and claim.key.startswith("node:"):
        # A settlement instrument that raises answers nothing; a broken probe
        # must never become a verdict. The CLI-built settlement never raises
        # by contract - this is the belt under it.
        try:
            if node_settlement(claim, native_verdict=verdict) is True:
                return True, ""
        except Exception:  # noqa: BLE001 - unknown keeps
            pass
    if "provably_dead" not in verdict or "bucket" not in verdict:
        raise RuntimeError("native claim verdict omitted sweep fields")
    provably_dead = bool(verdict["provably_dead"])
    bucket = str(verdict["bucket"] or "")
    if provably_dead or bucket != "suspect":
        return provably_dead, bucket
    if abandonment_probe is None or not claim.key.startswith("node:"):
        return False, "suspect"
    probe_verdict = abandonment_probe(claim, native_verdict=verdict)
    if probe_verdict is True:
        return True, ""
    # False: a live worker holds this node. None: the probe could not run,
    # which is unknown, and unknown keeps.
    return False, "suspect_alive" if probe_verdict is False else "suspect_unprobed"


def _native_claim_model(payload: dict[str, Any]) -> Claim:
    body = payload.get("claim", payload)
    if not isinstance(body, dict):
        raise ClaimVerdictError("fno-agents claim returned no claim object")
    return Claim.model_validate(body)


def acquire_claim(
    key: str,
    holder: str,
    *,
    reason: Optional[str] = None,
    ttl_ms: Optional[int] = None,
    metadata: Optional[dict[str, Any]] = None,
    pid: Optional[int] = None,
    pid_unavailable: bool = False,
    host: Optional[str] = None,
    harness: Optional[str] = None,
    pid_provenance: Optional[str] = None,
    harness_session_id: Optional[str] = None,
    root: Optional[Path] = None,
    _attempt: int = 0,
) -> Claim:
    del _attempt
    return _native_claim_model(
        native_acquire(
            key, holder,
            reason=reason, ttl_ms=ttl_ms, metadata=metadata, pid=pid,
            pid_unavailable=pid_unavailable, host=host, harness=harness,
            pid_provenance=pid_provenance, harness_session_id=harness_session_id,
            root=root,
        )
    )


def release_claim(
    key: str,
    holder: str,
    *,
    strict: bool = False,
    root: Optional[Path] = None,
) -> Optional[Claim]:
    receipt = native_release(key, holder, strict=strict, root=root)
    return _native_claim_model(receipt) if receipt is not None else None


def refresh_claim(
    key: str,
    holder: str,
    *,
    ttl_ms: Optional[int] = None,
    root: Optional[Path] = None,
    _attempt: int = 0,
) -> Optional[Claim]:
    del _attempt
    _validate_inputs(key, holder, ttl_ms)
    native_root = root or _configured_claim_root()
    flags = _native_root_flags(native_root)
    status = _native_claim("status", key, flags)
    if status.get("holder") and status.get("holder") != holder:
        raise HolderMismatch(holder, status["holder"], key)
    state = status.get("state")
    if state == "stale":
        raise ClaimValidationError(
            f"claim {key!r} expired and its holder reads dead; refusing to resurrect it"
        )
    if ttl_ms is None:
        if state == "free":
            raise ClaimGoneAway(str(claim_path(key, root=root)))
        if state == "corrupted":
            raise ClaimCorrupted(str(status.get("error") or key))
        if status.get("expires_at") is None:
            return None
        ttl_ms = MIN_TTL_MS
    payload = _native_claim(
        "renew",
        key,
        ["--holder", holder, "--ttl-ms", str(ttl_ms), *flags],
    )
    if payload.get("refreshed") is False or payload.get("outcome") == "unchanged":
        return None
    return _native_claim_model(payload)


def claim_status(key: str, *, root: Optional[Path] = None) -> dict[str, Any]:
    return claim_status_payload(key, root=root)


def list_claims(
    *,
    prefix: Optional[str] = None,
    include_stale: bool = False,
    root: Optional[Path] = None,
) -> list[dict[str, Any]]:
    flags = _native_root_flags(root or _configured_claim_root())
    if prefix is not None:
        flags.extend(("--prefix", prefix))
    if include_stale:
        flags.append("--include-stale")
    payload = _native_claim("list", "", flags)
    return payload.get("rows", []) if isinstance(payload.get("rows"), list) else []


def list_claims_with_counts(
    *,
    prefix: Optional[str] = None,
    include_stale: bool = False,
    root: Optional[Path] = None,
) -> tuple[list[dict[str, Any]], dict[str, int], dict[str, str]]:
    rows = list_claims(prefix=prefix, include_stale=True, root=root)
    counts = {state: 0 for state in ("live", "suspect", "stale", "corrupted", "free")}
    states: dict[str, str] = {}
    for row in rows:
        state = str(row.get("state") or "corrupted")
        counts[state] = counts.get(state, 0) + 1
        if isinstance(row.get("key"), str):
            states[row["key"]] = state
    if not include_stale:
        rows = [row for row in rows if row.get("state") in {"live", "suspect"}]
    counts["total"] = sum(counts.values())
    return rows, counts, states


def force_release_claim(
    key: str,
    reason: str,
    *,
    root: Optional[Path] = None,
    holding_recovery_lock: bool = False,
    expected_claim: Optional[Claim] = None,
) -> ForceReleaseOutcome:
    return native_force_release(
        key, reason,
        root=root,
        holding_recovery_lock=holding_recovery_lock,
        expected_claim=expected_claim.model_dump() if expected_claim is not None else None,
    )
