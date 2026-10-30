"""What a durable mail receipt says about the recipient's reachability.

Extracted from ``fno.mail.cli`` (file-budget: that module is shrink-only) so
the demotion-receipt logic has a home named by the question it answers: the
stderr recovery warning every durable floor prints, its live-lane reason
vocabulary, and the transcript-age suffix a bare live-miss carries.
"""

from __future__ import annotations

import subprocess
import sys
from typing import Optional

def _render(argv: list[str]) -> str:
    """One mail-receipt read through the native door (the style-check
    pattern: Python keeps transports, the assembly lives in Rust). The
    receipt vocabulary is single-source, so a missing binary refuses rather
    than falling back to a second renderer."""
    from fno.rust_binary import CommandUnavailable, resolve_binary

    binary = resolve_binary()
    if binary is None:
        raise CommandUnavailable("fno-agents binary not found; run `fno doctor update`")
    proc = subprocess.run(
        [str(binary), "mail-receipt", *argv],
        capture_output=True,
        text=True,
        timeout=10,
    )
    if proc.returncode != 0:
        raise CommandUnavailable(
            (proc.stderr or "fno-agents mail-receipt failed").strip()[:200]
        )
    return proc.stdout.rstrip("\n")


def _flag(name: str, value: Optional[str]) -> list[str]:
    return [f"--{name}", value] if value is not None else []


def _live_miss_age_suffix(recipient: str) -> str:
    """The transcript-age suffix a live-miss or transcript- reason carries (AC8).

    A bare live-miss reads the same for a transient miss to a genuinely live
    peer (re-send works) and for a session that stood down hours ago (nothing
    will drain it); the age is the discriminator. An unreadable transcript
    prints unknown, never 0s. Three lanes emit that receipt (the name lane,
    the job lane, the registered-agent lane), so the suffix lives here once.
    """
    from fno.agents.session_truth import fmt_age, resolve_session_truth

    age_s = resolve_session_truth(recipient).get("last_activity_age_s")
    if age_s is None:
        return ", transcript age unknown"
    return f", transcript quiet {fmt_age(age_s)}"


# Live-lane failures where the recipient WAS live and reachable but the inject
# did not confirm (node). For these the durable preamble must NOT say
# "is not live" -- the recipient was live, so that wording read as a liveness
# lie and cost a wrong hypothesis on measured evidence. The receipt names the
# real cause instead. ``no-confirm-source`` joins that set: the keeper
# lane refuses it only AFTER the socket connect, so the recipient was reachable
# and the honest receipt is "unconfirmed", not "is not live".
_LIVE_LANE_FAILURE_REASONS = frozenset(
    {
        "not-confirmed",
        "attach-failed",
        "io-error",
        "mux-send-failed",
        "unsafe-text",
        "no-confirm-source",
        "left-in-composer",
    }
)


def _is_live_lane_failure(reason: Optional[str]) -> bool:
    if not reason:
        return False
    return any(
        token in _LIVE_LANE_FAILURE_REASONS or token.startswith("mux-send-failed-")
        for token in reason.split(";")
    )


def durable_window_clause(owner: Optional[str]) -> str:
    """The drain-window tail every ``queued (durable)`` receipt carries.

    ``queued (durable)`` says nothing about time, so "not yet" and "never"
    read identically. The tail quotes the owner-class horizon the stranded
    sweep enforces (one bound, one table); an unknown class prints nothing,
    since a guessed constant is this defect in friendlier dress.
    """
    from fno.inbox.store import owner_ttl_hours

    hours = owner_ttl_hours(owner or "")
    return _render(["window", "--ttl-hours", repr(hours)])


def json_receipt(
    msg_id: str,
    *,
    to: str,
    status: str,
    subject: Optional[str] = None,
) -> str:
    """The default send receipt: one JSON line, SendMessage-shaped.

    Four keys, always: ``msg_id`` (the reply handle), ``subject`` (the
    sender's ``--subject``, null when none), ``to``, and ``status`` carrying
    the delivery verdict in the receipt vocabulary every reader already
    greps (``delivered (hosted)``, ``queued (durable)``, ``typed``)."""
    return _render(
        [
            "json",
            "--msg-id", msg_id,
            "--to", to,
            "--status", status,
            *(_flag("subject", subject)),
        ]
    )


def demotion_receipt(
    msg_id: str,
    *,
    reason: Optional[str],
    owner: Optional[str],
    target: Optional[str] = None,
    project: Optional[str] = None,
    age_target: Optional[str] = None,
    subject: Optional[str] = None,
) -> str:
    """The stdout receipt for a durable demotion (refined by x-aaaa)."""
    from fno.inbox.store import owner_ttl_hours

    age_of = age_target if age_target is not None else target
    suffix = _live_miss_age_suffix(age_of) if age_of is not None else None
    return _render(
        [
            "demotion",
            "--msg-id", msg_id,
            *(_flag("reason", reason)),
            "--ttl-hours", repr(owner_ttl_hours(owner or "")),
            *(_flag("target", target)),
            *(_flag("project", project)),
            *(_flag("age-of", age_of)),
            *(_flag("suffix", suffix)),
            *(_flag("subject", subject)),
        ]
    )


def print_project_demotion(result, to_project: str, subject: Optional[str] = None) -> None:
    """Stdout receipt(s) for a --to-project send that wrote durable.

    A resolved live peer demoted to durable is addressed to that PEER (same
    dispatch_send as the by-name lane, same cause); a bus-only peer gets the
    designed-queue receipt; no peer queues to the project inbox itself.
    """
    if result.recipient is not None:
        from fno.agents.dispatch import BUS_ONLY_POLICY

        if result.reason == BUS_ONLY_POLICY:
            from fno.mail import hold as _hold

            _note = _hold.bounce_reason(result.recipient)
            print(json_receipt(
                result.msg_id,
                to=result.recipient,
                status=(
                    f"queued (durable) [project {to_project}] "
                    f"[{_note or 'DND (bus-only): recipient polls the bus at each turn boundary'}]"
                )
                + durable_window_clause(result.durable_owner),
                subject=subject,
            ))
            if _note:
                print(f"`fno agents mail withdraw {result.msg_id}` retracts it.")
        else:
            _warn_deferred(result.recipient, reason=result.reason)
            print(demotion_receipt(
                result.msg_id,
                reason=result.reason, owner=result.durable_owner,
                target=result.recipient, project=to_project,
                subject=subject,
            ))
        return
    from fno.inbox.store import DurableOwner

    _warn_deferred(to_project, project=True)
    print(json_receipt(
        result.msg_id,
        to=to_project,
        status=(
            "queued (durable) [param-forced: --to-project]"
            + durable_window_clause(DurableOwner.INBOX_DRAIN.value)
        ),
        subject=subject,
    ))


def _warn_deferred(target: str, *, project: bool = False, reason: Optional[str] = None) -> None:
    """Fail loud on a dead-letter miss: the envelope hit only the durable floor
    with no live inject path, so the sender learns delivery deferred instead of
    the message vanishing silently until the recipient's next SessionStart drain.

    The durable copy is RECOVERY, not delivery - it waits on a drain the
    recipient may never run. So this names the recovery ladder rather than
    leaving the sender to wait: a session that is merely idle can be brought
    back and re-sent to immediately, which beats waiting on a drain every time.

    It leads with `peek`, not `resume`, because the fallback fires on an
    UNCONFIRMED live inject, not a proven failure: a busy recipient can record
    the injected turn past the confirm budget and receive it anyway, so a blind
    re-send is the documented double-delivery edge rather than a fix.

    ``reason`` is the live lane's own cause (node). When it names a
    live-lane failure (see :data:`_LIVE_LANE_FAILURE_REASONS`) the recipient WAS
    live and reachable, so the preamble says so and names the cause rather than
    claiming "is not live" -- a receipt naming the wrong cause is worse than one
    naming none, because it sends the reader to diagnose a recipient that was
    never the problem. A None or unreachable reason repeats a transcript verdict.

    A lock timeout gets its own arm for the same reason. The per-agent flock is
    shared by every command that touches the agent (send, ask, spawn, stop, rm), so
    a timeout says nothing about the recipient's liveness in EITHER direction.
    The not-live copy would send the reader to resurrect a session that is
    working fine; naming the holder a peer sender would tell the reader a
    just-stopped session is fine. The arm names neither and points at `peek`.

    Warning only - the durable enqueue succeeded, so exit stays 0."""
    from fno.agents.dispatch import LOCK_TIMEOUT_REASON
    from fno.mail.deferred_liveness import deferred_liveness_head

    if project:
        arm = "project"
    elif reason == LOCK_TIMEOUT_REASON:
        arm = "lock"
    elif _is_live_lane_failure(reason):
        arm = "live"
    else:
        arm = "plain"
    head = deferred_liveness_head(target) if arm == "plain" else None
    print(
        _render(
            [
                "warn-deferred",
                "--target", target,
                "--arm", arm,
                *(_flag("reason", reason)),
                *(_flag("head", head)),
            ]
        ),
        file=sys.stderr,
    )


