"""Busy mode: the CLOCK behind a ``delivery_policy = "bus-only"`` hold.

The HOLD itself is not here. It is the registry flag (schema v14) that
:func:`fno.agents.dispatch._delivery_policy_refusal` already enforces before
any transport call on every injector lane. This module owns only the two
things that flag cannot express on its own: WHEN the hold ends, and whether
it ends at all. The split is deliberate: the clock is a sidecar so the flag
stays the sole enforcement authority without a schema bump threaded through
the Rust ``RegistryEntry`` at nine construction sites.

Three sidecar states, and only one of them ever expires:

- **absent** - no clock was written for this flag, so it is not a busy-mode
  hold at all. It is a policy stamped by ``fno agents register
  --delivery-policy bus-only``, which has no clock by construction. Never
  lapses. See :func:`lapsed` for why lifting it instead was tried and reversed.
- **``until: null``** - the same permanent policy, recorded explicitly so the
  DND column can say "held" rather than leaving the operator to guess.
- **``until: <iso8601>``** - a timed busy-mode hold. ``clock_kind`` is ``idle``
  for the prompt-resetting clock or ``wall`` for a fixed deadline. Idle clocks
  also carry an immutable ``ceiling``. Lapses at the effective deadline, and
  only this state lapses.
"""
from __future__ import annotations

import json
import subprocess
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import TYPE_CHECKING, Optional

if TYPE_CHECKING:
    pass

from fno import paths

CLOCK_IDLE = "idle"
CLOCK_WALL = "wall"
CONVERSATION_SOURCE = "conversation"  # the Rust conversation arm's mark


@dataclass(frozen=True)
class Hold:
    """One session's hold clock. ``clock_kind`` separates idle and wall time."""

    handle: str
    until: Optional[datetime]
    window_s: Optional[int]
    clock_kind: str = CLOCK_IDLE
    ceiling: Optional[datetime] = None
    source: Optional[str] = None


def hold_dir() -> Path:
    """``~/.fno/mail-hold/`` - one file per handle.

    A subfolder rather than a top-level state-root file: the contents are
    session-keyed and short-lived, which is exactly the split
    ``docs/state-root-inventory.md`` draws.
    """
    return paths.state_dir() / "mail-hold"


def hold_path(handle: str) -> Path:
    return hold_dir() / f"{handle}.json"


def _hold_transport(argv: list[str]) -> str:
    """One mail-hold write through the native door (the receipts pattern:
    Python keeps transports, the clock assembly lives in Rust). A door
    failure raises VerbUnavailable - the verb is the single writer, so there
    is no Python fallback to drift from it."""
    from fno.rust_binary import VerbUnavailable, resolve_binary

    binary = resolve_binary()
    if binary is None:
        raise VerbUnavailable("fno-agents binary not found; run `fno doctor update`")
    proc = subprocess.run(
        [str(binary), "mail-hold", *argv],
        capture_output=True,
        text=True,
        timeout=10,
    )
    if proc.returncode != 0:
        raise VerbUnavailable(
            (proc.stderr or "fno-agents mail-hold failed").strip()[:200]
        )
    return proc.stdout.rstrip("\n")


def _hold_query(argv: list[str]) -> Optional[dict]:
    """The verb's one-line JSON state, or None when it printed nothing
    (no clock, a permanent policy, or a lapsed one)."""
    stdout = _hold_transport(argv)
    return json.loads(stdout) if stdout else None


def _describe(target) -> Optional[dict]:
    """One describe read over the candidate sweep (first clock wins), or None
    when the door is down - the read posture: a clock read never raises into
    a caller."""
    try:
        return _hold_query(["--describe", *candidate_keys(target)])
    except Exception:  # noqa: BLE001 - a clock read never breaks a caller
        return None


def _now() -> datetime:
    return datetime.now(timezone.utc)


def _parse(stamp: str) -> Optional[datetime]:
    try:
        parsed = datetime.fromisoformat(stamp.replace("Z", "+00:00"))
    except (AttributeError, TypeError, ValueError):
        return None
    return parsed if parsed.tzinfo else parsed.replace(tzinfo=timezone.utc)


def _hold_from_state(handle: str, state: dict) -> Hold:
    """The Hold a ``--read``/``--read-first`` state dict describes.

    ``handle`` is the candidate the caller asked about: the verb's JSON does
    not echo which key carried the clock, and no reader consumes the field.
    """
    until = state.get("until")
    ceiling = state.get("ceiling")
    source = state.get("source")
    window = state.get("window_s")
    return Hold(
        handle=handle,
        until=_parse(until) if isinstance(until, str) else None,
        window_s=window if isinstance(window, int) else None,
        clock_kind=state.get("clock_kind", CLOCK_IDLE),
        ceiling=_parse(ceiling) if isinstance(ceiling, str) else None,
        source=source if isinstance(source, str) else None,
    )


def read(handle: str) -> Optional[Hold]:
    """This handle's clock, or None when there is no readable one.

    A corrupt or unparseable file reads as None, the same as an absent one:
    the verb answers an empty stdout for both. Both mean "no clock", and
    neither is evidence that a hold is running.

    Never raises. The catch is deliberately broad because this read runs on
    the status path, where a missing binary or a failed transport must
    degrade to "no clock", never break the verb answering.
    """
    try:
        state = _hold_query(["--read", handle])
    except Exception:  # noqa: BLE001 - a clock read never breaks a caller
        return None
    return _hold_from_state(handle, state) if state else None


def lapsed(handle) -> bool:
    """True when a TIMED hold for ``handle`` has run out. Only ever timed.

    An absent or unreadable clock reads as NOT lapsed. Lifting a flag that has
    no clock silently revokes the delivery policy of every row stamped by
    ``fno agents register --delivery-policy bus-only``, which has no clock by
    construction - a shipped no-paste guarantee broken on rows nobody touched.
    The fear that argument answers, a hold whose timer died holding mail
    forever, does not describe this design: held mail is durable on the bus
    and surfaces at the next SessionStart or turn boundary, so a lost clock
    costs a stall bounded by the operator's next prompt, never a lost message.
    Auto-expire stays real through carriers that do not depend on this file
    surviving: the detached release timer, and the Rust verb's
    turn-boundary tidy of lapsed clocks.

    Pure read. It never mutates the registry, so it cannot deadlock a caller
    that already holds the registry lock and cannot raise into the gate.
    """
    state = _describe(handle)
    return bool(state and state.get("lapsed"))


def candidate_keys(target) -> tuple:
    """Every key a clock for ``target`` could sit under, from any address form.

    The ONE resolution rule every READER shares. ``target`` is a registry row
    or any token that addresses one: a name, a short id, a full session id, or
    the canonical handle.

    This exists because three readers each keyed differently and each looked
    correct on claude, where the name, the short id and the canonical handle
    tend to coincide. On codex they do not, so the DND column read a spawn
    label, the bounce read whatever the sender typed, and the gate read three
    forms no writer uses. Same defect, three places, one fix.
    """
    if hasattr(target, "harness_session_id") or hasattr(target, "name"):
        return addresses(target)
    token = str(target)
    entry = resolve_entry(token)
    if entry is None:
        return (token,)
    keys = addresses(entry)
    return keys if token in keys else (token,) + keys


def read_any(target) -> Optional[Hold]:
    """The clock for ``target`` under whichever of its keys carries one.

    One door read over the whole candidate sweep: the sweep itself stays in
    Python because it needs the registry entry, the FIND moves to the verb.
    """
    keys = candidate_keys(target)
    try:
        state = _hold_query(["--read-first", *keys])
    except Exception:  # noqa: BLE001 - a clock read never breaks a caller
        return None
    return _hold_from_state(keys[0], state) if state else None


def dnd_label(handle) -> Optional[str]:
    """What the DND column shows for a row whose flag IS ``bus-only``.

    Defined in terms of :func:`lapsed`, so the column and the delivery gate can
    never disagree about whether mail is moving. That matters more than the
    string: a column that says one thing while the gate does another is worse
    than no column, and this node exists because state was modelled and never
    shown.

    ``None`` when mail flows despite the flag, which is the lapsed-timed-hold
    case and the one state where the flag is stale. ``"held"`` when the hold
    has no end to show. A duration whenever there is one, because a hold with
    no visible end is what the operator asked to avoid.
    """
    state = _describe(handle)
    if state is None:
        # Door down: the same degrade the in-process read had - the column
        # claims "held" rather than claiming mail flows while the flag stands.
        return "held"
    if state.get("lapsed"):
        return None
    auto = state.get("source") == CONVERSATION_SOURCE
    return (state.get("remaining") or "held") + (" (auto)" if auto else "")


def remaining_label(handle) -> Optional[str]:
    """The compact remaining-time string alone, or None when there is no clock.

    Callers rendering the operator-facing column want :func:`dnd_label`, which
    answers the question the column asks. This one answers only "how long is
    left", and returns None for a row that is held with no end recorded.
    """
    state = _describe(handle)
    return state.get("remaining") if state else None


def gate_answer_in_process(token: str) -> Optional[str]:
    """C15 own pass and lapsed-clock rule, for when no gate binary resolves."""
    from fno.agents import dispatch
    from fno.agents.self_stamp import resolve_self_session_id
    own = resolve_self_session_id()
    if (own and own.casefold() == token.casefold()) or lapsed(token):
        return None
    return dispatch.BUS_ONLY_POLICY


def bounce_reason(recipient) -> Optional[str]:
    """The busy-mode receipt a sender reads, or None when no hold is running.

    One-call port of the receipt body to the Rust hold gate (``mail_hold.rs``
    ``gate``, law d-b6cc1a2a): the gate owns the C16 receipt shapes - the
    conversation receipt, the wall and grace "held until about HH:MM", and
    the idle "or later" - in the recipient's local time. ``None`` keeps the
    caller's existing text: a hand-stamped hold with no clock really does
    surface at a turn boundary. Never raises.
    """
    from fno import rust_binary

    # A row cannot ride the token door (str(row) matches no gate key).
    row_like = hasattr(recipient, "harness_session_id") or hasattr(recipient, "name")
    binary = rust_binary.resolve_installed_binary()
    if row_like or binary is None:
        hold = read_any(recipient)
        if hold is None or hold.until is None or hold.until <= _now():
            return None
        return clock_description(hold)
    try:
        proc = subprocess.run(
            [str(binary), "mail-hold", "--gate", "--session", str(recipient)],
            capture_output=True,
            text=True,
            timeout=10,
        )
        return json.loads(proc.stdout).get("receipt")
    except Exception:  # noqa: BLE001 - a receipt read never breaks the send path
        return None


def clock_description(hold: Optional[Hold]) -> str:
    """Name the active clock and its fixed boundary for operator receipts."""
    if hold is None or hold.until is None:
        return "no expiry"
    deadline = hold.until.strftime("%H:%M:%S UTC")
    if hold.clock_kind == CLOCK_WALL:
        return f"wall clock, fixed deadline {deadline}"
    ceiling = hold.ceiling.strftime("%H:%M:%S UTC") if hold.ceiling else "legacy unbounded"
    return f"quiet minutes idle clock, ceiling {ceiling}"


def addresses(entry) -> tuple:
    """Every token a hold clock for ``entry`` could be filed under.

    ONE matching rule, shared by the resolver, the policy write, and the
    delivery gate's expiry check. Writers key the full session identity key,
    which is collision-free where the first-eight is not: codex UUIDv7 ids in
    one 65.536-second window share their first eight. The first-eight token
    stays in the sweep for clocks written before that migration.
    """
    from fno.harness_identity import canonical_handle, session_identity_key

    session_id = getattr(entry, "harness_session_id", None)
    # The identity key leads: a live clock under the new key must outrank a
    # legacy alias file, or a lapsed pre-migration clock lifts a hold the new
    # clock still enforces.
    tokens: list = []
    if session_id:
        try:
            tokens.append(session_identity_key(session_id))
        except Exception:  # noqa: BLE001 - a malformed id contributes no address
            pass
    tokens += [
        getattr(entry, "name", None),
        getattr(entry, "short_id", None),
        session_id,
    ]
    if session_id:
        try:
            tokens.append(canonical_handle(session_id))
        except Exception:  # noqa: BLE001 - a malformed id contributes no address
            pass
    ordered: list = []
    for token in tokens:
        if token and token not in ordered:
            ordered.append(token)
    return tuple(ordered)


def resolve_entry(handle: str):
    """The registry row ``handle`` addresses, freshly loaded, or None.

    Load it AFTER the policy is cleared, never before. ``_deliver_live`` and
    every lane under it consult ``_delivery_policy_refusal``, and that gate
    reads ``delivery_policy`` straight off the object it is handed. A row
    captured while the flag was still set carries a stale ``bus-only`` and the
    release is refused by the very gate that held the mail.
    """
    from fno.agents.registry import load_registry

    try:
        entries = load_registry()
    except Exception:  # noqa: BLE001 - a registry hiccup is a missed lane, not a crash
        return None
    for entry in entries:
        if handle in addresses(entry):
            return entry
    return None


def _emit_drain_marker(
    msg_id: str,
    recipient: str,
    address_form: str,
    sender: "str | None",
    reason: str = "printed",
) -> None:
    """Best-effort ``agent_mail_drained`` receipt, one per drained message id (W1.1).

    Lets a sender join ``events.jsonl`` on ``msg_id`` to a terminal 'drained'
    state; ``reason`` distinguishes printed from skipped-duplicate (W2).
    Swallowed on any failure: a missing receipt degrades to the cursor fallback.
    """
    from fno.agents import events

    try:
        events.emit(
            events.KIND_AGENT_MAIL_DRAINED,
            msg_id=msg_id,
            recipient=recipient,
            address_form=address_form,
            sender=sender or "",
            reason=reason,
        )
    except (OSError, ValueError, TypeError):
        pass
