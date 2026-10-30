"""Provider selection and ask-dispatch orchestrator for fno agents.

Phase 1 surface:

- ``KNOWN_PROVIDERS`` - roster-backed tuple of supported provider names.
- ``is_provider_available(name)`` — wraps ``shutil.which`` for a single CLI.
- ``select_provider(name, requested_provider)`` — registry-aware selection
  that catches the "wrong provider on follow-up" mistake before any
  subprocess fires.

The spawn subprocess invocation per provider lives in
``fno.agents.harnesses.{claude,codex}``. Gemini is a legacy readable identity,
not a maintained Python dispatch provider.
"""

from __future__ import annotations

import contextvars
import json
import os
import re
import select
import shutil
import subprocess
import sys
import time
from dataclasses import dataclass, field, replace
from datetime import datetime, timezone
from pathlib import Path
from fno.agents.dispatch_errors import DispatchAskError
from typing import Any, Callable, Literal, Mapping, Optional

from fno import paths
from fno.agents import events
from fno.agents.sender_provenance import (
    _proven_self_sender,
    _resolve_sender_entry,
    stamped_provenance as _stamped_sender_provenance,
)
from fno.agents.context import EventContext, build_context
from fno.agents.harness_map import DispatchResolveError
from fno.agents.lane_heal import lane_heal as _lane_heal
from fno.agents.lock import AgentLockTimeout, hold_agent_lock
from fno.agents.harnesses.base import ReachabilityProbeError
from fno.agents.reachability import mux_ref_names_a_pane
from fno.agents.send_result import DispatchSendResult
from fno.agents.registry import (
    AgentEntry,
    AgentResolutionError,
    AgentStatus,
    RegistryVersionError,
    load_registry,
    resolve_registered_agent_across_sources,
    update_registry,
)
from fno.harness_identity import (
    canonical_handle,
    session_identity_key,
)

DispatchKind = Literal["create", "followup"]
RecipientIdentity = tuple[
    str,
    str,
    Optional[str],
    str,
    Optional[str],
    Optional[tuple[object, object]],
    str,
]
SwitchboardIdentity = dict[str, object]


def _recipient_identity_key(entry: AgentEntry) -> RecipientIdentity:
    """Snapshot every field that can select or replace a live recipient."""
    session_id = getattr(entry, "harness_session_id", None) or getattr(
        entry, "session_id", None
    )
    mux = entry.mux
    return (
        entry.name,
        entry.harness,
        session_identity_key(session_id) if session_id else None,
        entry.short_id,
        entry.mcp_channel_id,
        (
            (mux.get("session"), mux.get("pane_id"))
            if mux is not None
            else None
        ),
        entry.created_at,
    )


def _switchboard_identity(entry: AgentEntry) -> SwitchboardIdentity:
    """Identity fields the daemon must match before driving a named stream."""
    return {
        "harness": entry.harness,
        "session_id": entry.harness_session_id,
        "short_id": entry.short_id,
        "created_at": entry.created_at,
    }


def _is_codex_thread_entry(entry: AgentEntry) -> bool:
    """Whether the row is a daemon-hosted Codex app-server thread.

    Mirrors the Rust ``is_codex_thread_entry`` (daemon.rs): codex harness,
    interactive host mode (load_registry coerces a missing/null host_mode to
    "exec", so the None default here never reads as interactive), an empty
    ``short_id`` (codex rows carry the full id in ``harness_session_id``), and
    no mux ref. A codex PANE row fails the short_id/mux legs and keeps its
    real lanes.
    """
    return (
        entry.harness == "codex"
        and (entry.host_mode or "exec") == "interactive"
        and not entry.short_id
        and entry.mux is None
    )


def _update_registry_if_recipient_unchanged(
    name: str,
    expected_identity: RecipientIdentity,
    updater: Callable[[list[AgentEntry]], list[AgentEntry]],
    *,
    registry_path: Optional[Path] = None,
    registry_lock_timeout: Optional[float] = None,
    decline_reason: Optional[list[str]] = None,
) -> bool:
    """Apply a post-side-effect write only to the selected recipient.

    Exact-name lifecycle calls follow the current owner until the per-name lock
    selects a concrete row. Every call then carries that row's identity through
    the final registry-wide lock so a writer that does not honor the per-name
    lock cannot make a replacement inherit the registry mutation.

    `decline_reason`, when passed, gets one of `"row_removed"` (no row this
    name any more), `"duplicate_name"` (more than one row now shares the
    name, an ambiguous registry state distinct from either a clean removal or
    a restamp), or `"identity_changed"` (exactly one row exists but is not
    the one the caller resolved) appended on a declined write -- callers that
    need to tell those apart for telemetry (self-review finding: row-removed
    and identity-changed used to log as the same `RecipientIdentityChanged`,
    hiding a removed-entirely row behind the restamp-race label) read it
    after the call.
    """
    applied = False

    def _guarded(entries: list[AgentEntry]) -> list[AgentEntry]:
        nonlocal applied
        matches = [entry for entry in entries if entry.name == name]
        if (
            len(matches) != 1
            or _recipient_identity_key(matches[0]) != expected_identity
        ):
            if decline_reason is not None:
                if not matches:
                    decline_reason.append("row_removed")
                elif len(matches) > 1:
                    decline_reason.append("duplicate_name")
                else:
                    decline_reason.append("identity_changed")
            return entries
        applied = True
        return updater(entries)

    if registry_path is None:
        if registry_lock_timeout is None:
            update_registry(_guarded)
        else:
            update_registry(_guarded, lock_timeout=registry_lock_timeout)
    else:
        if registry_lock_timeout is None:
            update_registry(_guarded, path=registry_path)
        else:
            update_registry(
                _guarded,
                path=registry_path,
                lock_timeout=registry_lock_timeout,
            )
    return applied


# Dispatch-scoped context propagation (Task 2.1): the dispatch paths build an
# ``EventContext`` once the provider is known and stash it here so helpers emit
# context-enriched events without threading ``ctx`` everywhere. ContextVar is
# per-task/thread isolated and the set/reset token cycle never leaks across
# dispatches; helpers that don't observe it fall back to legacy ``emit``.
_DISPATCH_CTX: contextvars.ContextVar[Optional[EventContext]] = contextvars.ContextVar(
    "fno_dispatch_ctx", default=None
)


def _emit_ev(kind: str, **data: Any) -> None:
    """Emit an event with the active dispatch ``EventContext`` if set.

    Falls back to legacy ``events.emit`` when ``_DISPATCH_CTX`` is unset
    so callers outside a dispatch scope (or pre-migration code
    paths) still produce valid records.
    """
    ctx = _DISPATCH_CTX.get()
    if ctx is not None:
        events.emit_with_context(ctx, kind, **data)
    else:
        events.emit(kind, **data)


@dataclass(frozen=True)
class DispatchAskResult:
    """Return shape for the create/followup dispatch helpers.

    ``kind`` discriminates the two stdout contracts:

    - ``"create"`` — agent name was new; ``short_id`` is the provider's
      newly-minted supervisor id (e.g. claude 8-hex). ``reply`` is None.
      The CLI prints ``<short_id>\\n`` per US1's contract.
    - ``"followup"`` — reply-verbatim stdout (no trailing newline
      added) per US2 AC2-HP; used where the provider returns the reply
      on its own stdout.
    """

    kind: DispatchKind
    short_id: str
    reply: Optional[str] = None
    duration_ms: Optional[int] = None
    # v23: the create path's substitution verdict, passed through to
    # the caller's receipt. None means unknown-or-match.
    model_substituted: Optional[dict] = None

    def __post_init__(self) -> None:
        if self.kind not in ("create", "followup"):
            raise ValueError(
                f"DispatchAskResult.kind must be 'create' or 'followup', got {self.kind!r}"
            )
        if self.kind == "followup" and self.reply is None:
            raise ValueError("DispatchAskResult.reply is required when kind='followup'")


class ProviderMismatchError(RuntimeError):
    """Raised when a follow-up ``ask`` passes a provider that disagrees with the registry."""


def _check_known_provider(name: str) -> None:
    from fno.agents.harnesses import KNOWN_PROVIDERS  # call-time read (import-time crashes stale binaries)

    if name not in KNOWN_PROVIDERS:
        raise ValueError(f"unknown provider {name!r}; supported: {', '.join(KNOWN_PROVIDERS)}")


def is_provider_available(name: str) -> bool:
    """Return True iff the named provider CLI is on PATH.

    Raises ``ValueError`` if ``name`` is not in :data:`KNOWN_PROVIDERS`.
    """
    _check_known_provider(name)
    return shutil.which(name) is not None


def select_provider(name: str, requested_provider: Optional[str]) -> str:
    """Select the provider for ``fno agents ask <name>``.

    Logic:
      - If ``requested_provider`` is given, validate it against
        :data:`KNOWN_PROVIDERS`.
      - If the agent already exists in the registry:
        - No request: return the recorded provider.
        - Request matches: return it.
        - Request mismatches: raise :class:`ProviderMismatchError` with a
          message that names the agent, recorded provider, and requested
          provider. This catches the mistaken-reuse failure mode that a
          silent "ignored" path would mask.
      - If the agent is new:
        - Request given: return it.
        - No request: raise ``ValueError`` because there is nothing to
          select for a brand-new agent.
    """
    if requested_provider is not None:
        _check_known_provider(requested_provider)

    existing = next(
        (entry for entry in load_registry() if entry.name == name),
        None,
    )

    if existing is not None:
        if requested_provider is None or requested_provider == existing.harness:
            return existing.harness
        raise ProviderMismatchError(
            f"agent {name!r} is provider={existing.harness}, "
            f"refusing to follow-up as provider={requested_provider}"
        )

    if requested_provider is None:
        from fno.agents.harnesses import KNOWN_PROVIDERS  # call-time read (import-time crashes stale binaries)

        raise ValueError(
            f"provider is required for new agent {name!r}; "
            f"pass --provider one of: {', '.join(KNOWN_PROVIDERS)}"
        )
    return requested_provider


# ---------------------------------------------------------------------------
# Shared dispatch surface (validation, locking, events)
# ---------------------------------------------------------------------------


# Absorbing states: once a row reaches one, no probe may move it and no identity
# may be backfilled onto it. Named once because reconcile tests it on three
# separate paths and a set that drifts between them reopens the resurrection bug.
_TERMINAL_AGENT_STATUSES = frozenset({"orphaned", "failed", "exited", "permanent_dead"})

_NAME_MAX_LEN = 128
_SHORT_ID_NAME_SHAPE = re.compile(r"^[0-9a-f]{8}$")
_DEFAULT_LOCK_TIMEOUT = 30.0

# Every refusal that wrote nothing says this, once. The bus-lock timeout
# already carries it, so a wrapper that appends it blindly says it twice.
_NO_ENVELOPE_CLAUSE = "no durable envelope was written"

# Floor for the post-timeout durable queue's grace window. A durable write
# needs the lock because only the lock proves the recipient row is committed,
# so a queue that cannot take it refuses instead of guessing.
_LOCK_TIMEOUT_QUEUE_GRACE_SECONDS = 2.0


def _queue_grace_seconds(lock_timeout: float) -> float:
    """Seconds the post-timeout queue waits for the flock before giving up.

    A flat floor loses to the contention it exists to catch. The holder owns
    the lock for its whole live-delivery attempt, budgeted at
    `_MAIL_INJECT_LIVENESS_SCALED_TIMEOUT_S`, so a caller that gave up at the
    default 30s is usually waiting on a holder still running toward 40s: it
    would retry for two seconds, miss by eight, and drop the very message it
    came here to save. The window therefore covers the INJECT budget, measured
    from what the caller already waited.

    It does NOT cover every holder. The same flock spans a synchronous
    switchboard hop, whose read budget is `_SWITCHBOARD_READ_TIMEOUT` (130s),
    so a sender queued behind an A2A hop still exhausts this window and takes
    the exit-11 arm. Sizing for 130s would make a routine `fno agents mail send`
    block over two minutes before saying anything, which is a worse trade than
    the loud refusal. Narrowing the flock to the registry mutation is the real
    fix; it is tracked separately, for the reason the block comment in
    `dispatch_send` gives.

    Capped at that same wait, because the cap is what keeps the rule honest
    for a caller who asked for a short one: a 0.1s `lock_timeout` means "do
    not wait", and spending 42s on the fallback would answer a question
    nobody asked. Past the ceiling the floor is all that is left, and all the
    identity-consistency read needs.

    The floor outranks the cap below two seconds, so a 0.1s caller still
    blocks 2.0s here. That is deliberate and the test pins it: the grace
    window's other job is the identity-consistency read, which a sub-second
    budget cannot do at all, and a fallback that cannot verify the recipient
    writes nothing. The cap governs the range where both jobs fit.
    """
    residual = _MAIL_INJECT_LIVENESS_SCALED_TIMEOUT_S + _LOCK_TIMEOUT_QUEUE_GRACE_SECONDS
    return max(
        _LOCK_TIMEOUT_QUEUE_GRACE_SECONDS,
        min(residual - lock_timeout, lock_timeout),
    )

# Receipt reason for a message the lock-timeout lane queued. Exported because
# the receipt formatter in fno.mail.cli must branch on it: a contended lock
# means the recipient was BUSY, and the generic "not live" copy would send the
# reader to resurrect a session that is working fine.
LOCK_TIMEOUT_REASON = "agent-lock-timeout"

_FROM_NAME_MAX_LEN = 128
_FROM_NAME_DEFAULT = "fno"
_FROM_NAME_FORBIDDEN_CHARS = frozenset('"<>&')
# Exit-code taxonomy (documented here for cross-language parity with Rust Task 1.3):
#   1  subprocess failure
#   2  usage / input validation
#   11 lock timeout
#   12 registry I/O
#   13 provider refused / orphan
#   14 provider CLI not on PATH
#   15 reply timeout
#   16 unknown agent name (agent must be created via spawn/host first)
UNKNOWN_AGENT_EXIT_CODE = 16


def _validate_inputs(
    name: str, message: str, from_name: str, *, name_is_address: bool = False
) -> None:
    """Reject inputs that fail the AC1-ERR / AC1-EDGE / AC2-ERR boundary checks.

    ``name_is_address`` marks a caller whose ``name`` is a TARGET to resolve, not
    a name to create. The short-id-shape rejection below guards against NAMING an
    agent like an id; applied to a send target it rejects the canonical mailbox
    handle itself, which is the exact string `whoami` advertises.
    """
    if not name:
        raise DispatchAskError("agent name must not be empty", exit_code=2)
    if "/" in name or "\\" in name or ".." in name:
        raise DispatchAskError(
            f"agent name must not contain path separators or '..': {name!r}",
            exit_code=2,
        )
    if len(name) > _NAME_MAX_LEN:
        raise DispatchAskError(
            f"name must be <={_NAME_MAX_LEN} chars (got {len(name)})",
            exit_code=2,
        )
    if _SHORT_ID_NAME_SHAPE.match(name) and not name_is_address:
        raise DispatchAskError(
            f"agent name {name!r} must not match short-id shape "
            f"^[0-9a-f]{{8}}$ (prevents name/id collision)",
            exit_code=2,
        )
    # Reject characters that would corrupt env-var injection
    # (FNO_AGENT_SELF=<name>) on subprocess spawn. NUL bytes cause
    # subprocess.run to raise ValueError; \n/\r split a meta value
    # across lines in downstream consumers; `=` breaks the env-key=value
    # shape. Tightened in response to sigma-review H4 catching a crash
    # path when a name like "a\x00b" landed in the registry and crashed
    # every subsequent dispatch.
    _forbidden_env_chars = ("\x00", "\n", "\r", "=")
    bad = next((ch for ch in _forbidden_env_chars if ch in name), None)
    if bad is not None:
        raise DispatchAskError(
            f"agent name {name!r} contains a forbidden character "
            f"({bad!r} would corrupt subprocess env injection)",
            exit_code=2,
        )
    if not message or not message.strip():
        raise DispatchAskError("message must be non-empty", exit_code=2)
    _validate_from_name(from_name)


def _validate_from_name(from_name: str) -> None:
    """AC2-ERR: from_name must be non-empty, <=128 chars, XML-attribute-safe."""
    if not from_name:
        raise DispatchAskError("from-name must not be empty", exit_code=2)
    if len(from_name) > _FROM_NAME_MAX_LEN:
        raise DispatchAskError(
            f"from-name must be <={_FROM_NAME_MAX_LEN} chars (got {len(from_name)})",
            exit_code=2,
        )
    if any(ch in _FROM_NAME_FORBIDDEN_CHARS for ch in from_name):
        raise DispatchAskError(
            'from-name must not contain XML-unsafe characters (", <, >, &)',
            exit_code=2,
        )


def _utc_now_iso() -> str:
    return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def _derive_log_path(name: str) -> Path:
    """Stable fno-side log path for `fno agents logs <name>` (US3 plumbing)."""
    return paths.state_dir() / "agents" / "logs" / f"{name}.log"


def _touch_log_path(name: str) -> Optional[Path]:
    """Create (or reuse) the log file a mint site is about to record as a
    registry row's ``log_path`` (AC4): a ``log_path`` pointing at
    nothing is a claim, not evidence, so the file must exist before the row
    does. Returns ``None`` on a failed create (disk full, EROFS, a
    permission error) instead of raising or returning a path nothing backs,
    mirroring ``claude_ask.rs``'s ``log_file_created`` gate, so a caller
    records this leg only when it is real. Shared by every mint site
    (dispatch.py, mux_spawn.py) so the mkdir+touch idiom lives in exactly
    one place.
    """
    log_path = _derive_log_path(name)
    try:
        log_path.parent.mkdir(parents=True, exist_ok=True)
        log_path.touch(exist_ok=True)
    except OSError:
        return None
    return log_path


def _capture_spawn_trigger() -> Optional[str]:
    """The CAUSE of this spawn, distinct from :func:`_capture_parent_edge`.

    A dispatcher (think-spawn, or a machine advance) sets ``FNO_SPAWN_TRIGGER``
    in the subprocess env before shelling ``fno agents spawn``; a human running
    the command directly never sets it, so absence reads as "an operator asked
    for this." Never raises. Pops the var after reading it: a child never
    inherits its parent's spawn cause, and the worker env floor drops it too.
    """
    return (os.environ.pop("FNO_SPAWN_TRIGGER", "") or "").strip() or None


# ---------------------------------------------------------------------------
# Task 1.2: spawn command (US2 Python fallback runtime)
# ---------------------------------------------------------------------------


def validate_spawn_name(name: str) -> None:
    """Spawn-name rules for the mux-pane back half (``fno.agents.mux_spawn``).
    Raises :class:`DispatchAskError` (exit 2) on every violation.
    """
    if not name:
        raise DispatchAskError("agent name must not be empty", exit_code=2)
    if "/" in name or "\\" in name or ".." in name:
        raise DispatchAskError(
            f"agent name must not contain path separators or '..': {name!r}",
            exit_code=2,
        )
    if len(name) > _NAME_MAX_LEN:
        raise DispatchAskError(
            f"name must be <={_NAME_MAX_LEN} chars (got {len(name)})",
            exit_code=2,
        )
    if _SHORT_ID_NAME_SHAPE.match(name):
        raise DispatchAskError(
            f"agent name {name!r} must not match short-id shape "
            f"^[0-9a-f]{{8}}$ (prevents name/id collision)",
            exit_code=2,
        )
    _forbidden_env_chars = ("\x00", "\n", "\r", "=")
    bad = next((ch for ch in _forbidden_env_chars if ch in name), None)
    if bad is not None:
        raise DispatchAskError(
            f"agent name {name!r} contains a forbidden character "
            f"({bad!r} would corrupt subprocess env injection)",
            exit_code=2,
        )
def _picked_headroom_note(account_id: str) -> str:
    """The picked account's worst window, for the receipt. Never raises."""
    try:
        from fno.adapters.providers.runtime_state import read_usage

        snap = read_usage(account_id)
        if snap is not None and snap.windows:
            worst = max(snap.windows, key=lambda w: w.used_pct)
            return f"{worst.label} {worst.used_pct:.0f}%"
    except Exception:  # noqa: BLE001 - a receipt detail must never break a spawn
        pass
    return "headroom unknown"


def _pick_account_overlay(
    *,
    role: Optional[str] = None,
    route_env: Optional[Mapping[str, str]] = None,
) -> Optional[Any]:
    """Consult the picker for a spawn that named no account, or None.

    Returns the overlay's ``env`` AND ``account_id``, because the
    spawn seams stamp the picked id on the row's ``launch_account`` ;
    the env alone drops the one fact re-entry needs.

    Advisory in every direction (see :func:`pick_account_id`): a routed spawn
    is never picked for, and the printed receipt keeps a silent re-accounting
    from becoming a billing surprise.
    """
    picked = pick_account_id(role=role, route_env=route_env)
    if picked is None:
        return None
    try:
        from types import SimpleNamespace

        from fno.agents.spawn_overlay_client import account_overlay

        return SimpleNamespace(**account_overlay(picked))
    except Exception as exc:  # noqa: BLE001 - picking is advisory; never block a spawn
        print(f"account: default (pick unavailable: {exc})", file=sys.stderr)
        return None


def pick_account_id(
    *,
    role: Optional[str] = None,
    route_env: Optional[Mapping[str, str]] = None,
) -> Optional[str]:
    """The account id a spawn that named none should launch on, or None.

    THE picking decision, with two consumers: the in-process spawn seams turn it
    into an env overlay, and the runtime seam turns it into an injected
    ``--account`` flag so the Rust client inherits the same choice. One decision,
    so those two can never disagree about which account a worker is billing.

    Advisory in every direction: opt-in via ``accounts.quota.pick_on_launch``,
    and any refusal or failure returns None so the spawn proceeds exactly as it
    does today. A routed spawn is never picked for: it bills the route's
    vendor, not an Anthropic account, so there is no quota to manage.
    """
    if route_env or role is not None:
        return None
    try:
        from fno.adapters.providers.cli import PICK_EXIT_NOT_ARMED, pick_account

        # `--if-armed` so the opt-in is read in exactly ONE place, the same call
        # the Rust loop makes. Checking `pick_on_launch` here as well would be a
        # second answer to one question, which is the shape this whole feature
        # exists to remove.
        verdict = pick_account(if_armed=True)
        if verdict.exit_code == PICK_EXIT_NOT_ARMED:
            return None  # off by default: say nothing, change nothing
        if verdict.account is None:
            print(
                f"account: default (pick unavailable: {verdict.reason})",
                file=sys.stderr,
            )
            return None
        print(
            f"account: {verdict.account} (picked by accounts.quota.pick_on_launch, "
            f"{_picked_headroom_note(verdict.account)})",
            file=sys.stderr,
        )
        return verdict.account
    except Exception as exc:  # noqa: BLE001 - picking is advisory; never block a spawn
        print(f"account: default (pick unavailable: {exc})", file=sys.stderr)
        return None


def note_quota_death(account_env: Optional[Mapping[str, str]], tail: str | None) -> None:
    """Record a cooldown when a worker died with a quota marker in its tail.

    The reactive half of quota survival: a snapshot can be up to
    ``probe_ttl_seconds`` stale, so without this the next pick would hand the
    successor the account that just died. Thin caller over the one
    lock-decision site; launch-time attribution stays here. Best-effort."""
    if not tail:
        return
    try:
        from fno.adapters.providers.loader import effective_active
        from fno.agents.quota_lock import record_quota_lock

        provider_id = _account_id_for_env(account_env) or effective_active()
        if provider_id:
            record_quota_lock(provider_id, tail)
    except Exception:  # noqa: BLE001 - never let a health write break teardown
        pass


def _account_id_for_env(account_env: Optional[Mapping[str, str]]) -> Optional[str]:
    """Which record an overlay pinned, by asking each what its overlay would be.

    Matching on ``config_dir`` alone missed two real shapes: an ``oauth_dir``
    record's overlay names its STAGED dir, not a config_dir, and an api_key
    record's overlay has no directory at all. Both would fall through to
    ``effective_active()`` and record a quota death against the wrong account -
    poisoning an account that is fine while leaving the dead one pickable.
    Comparing the resolved overlay covers every lane, and only runs when a worker
    has already died, so the extra resolutions are not on any hot path.
    """
    if not account_env:
        return None
    try:
        from fno.agents.spawn_overlay_client import spawn_overlay_call

        return spawn_overlay_call({"kind": "account-for-env", "env": dict(account_env)})["account"]
    except Exception:  # noqa: BLE001 - attribution is best-effort
        return None


# ---------------------------------------------------------------------------
# US4-lifecycle: stop / rm / reconcile / attach (write + read commands)
# ---------------------------------------------------------------------------


_DEFAULT_CLAUDE_LOGS_TAIL_TIMEOUT = 10.0


@dataclass(frozen=True)
class ReconcileResult:
    """Return shape for :func:`reconcile_agents`.

    Lists are JSON-friendly: each entry is a dict with ``name``, ``provider``,
    optional ``id`` (short_id or session_id), optional ``reason``. The
    CLI emits this verbatim under ``--json`` (Locked Decision 4 mirror).
    """

    scanned: int
    orphaned: list[dict] = field(default_factory=list)
    recovered: list[dict] = field(default_factory=list)
    skipped: list[dict] = field(default_factory=list)
    errors: list[dict] = field(default_factory=list)
    # Live rows whose null canonical harness_session_id reconcile healed from the
    # harness store. Empty list (not absent) distinguishes "ran, nothing
    # to heal" from "healed": each entry is {name, provider, harness_session_id}.
    backfilled: list[dict] = field(default_factory=list)
    # rows whose structurally impossible mux ref reconcile cleared. Each
    # entry is {name, provider, harness, cleared_mux}; empty list = ran, nothing
    # to clear.
    mux_cleared: list[dict] = field(default_factory=list)


def _resolve_lifecycle_target(
    token: str,
    *,
    registry_path: Optional[Path] = None,
) -> tuple[AgentEntry, Optional[RecipientIdentity]]:
    """Resolve a lifecycle token without discarding address-bound identity.

    A genuine miss falls through to the exact-name lookup so the downstream
    error remains ``agent {name!r} not found in registry``.
    Exact names intentionally follow the current owner of that name under the
    lock; full ids and short handles remain pinned to the selected session.
    Ambiguity and unavailable identity evidence are refusals: falling back could
    make a token that also happens to be a row name select that row and act on
    the wrong session."""
    from fno.agents.registry import AgentResolutionError, resolve_agent

    try:
        resolved = resolve_agent(token, path=registry_path)
        expected_identity = (
            None
            if resolved.matched_by == "name"
            else _recipient_identity_key(resolved.entry)
        )
        return resolved.entry, expected_identity
    except AgentResolutionError as exc:
        if exc.ambiguous or exc.unavailable:
            raise DispatchAskError(
                str(exc),
                exit_code=12 if exc.unavailable else 2,
            ) from exc
        return _resolve_registry_entry(token, registry_path=registry_path), None


def _resolve_registry_entry(name: str, *, registry_path: Optional[Path] = None) -> AgentEntry:
    """Load the registry and return the entry for ``name``.

    Raises :class:`DispatchAskError`(exit_code=2) when the entry is
    missing - the convention for lifecycle commands (AC1-UI / AC2-UI /
    AC7-UI). Raises (exit_code=12) on registry-read failures so the
    operator gets a distinct "registry busted" signal.

    Args:
        name: agent name to look up.
        registry_path: optional override forwarded to ``load_registry``.
            A non-default override locks AND reads against the same file
            (Codex P2 on PR #317: previously the override only reached
            the lock path and the entry read silently fell through to
            the default registry).
    """
    try:
        entries = load_registry(registry_path)
    except (OSError, ValueError, RegistryVersionError) as exc:
        raise DispatchAskError(
            f"registry read failed: {exc}",
            exit_code=12,
        ) from exc
    matches = [entry for entry in entries if entry.name == name]
    if len(matches) > 1:
        candidates = ", ".join(
            f"{entry.harness_session_id or entry.short_id or '-'} ({entry.harness})"
            for entry in matches
        )
        raise DispatchAskError(
            f"registry name {name!r} is ambiguous across {len(matches)} rows: "
            f"{candidates}. Repair the duplicate registry rows before retrying.",
            exit_code=2,
        )
    if matches:
        return matches[0]
    raise DispatchAskError(
        f"agent {name!r} not found in registry",
        exit_code=2,
    )


def reconcile_agents(
    *,
    claude_logs_timeout: float = _DEFAULT_CLAUDE_LOGS_TAIL_TIMEOUT,
    codex_session_index_path: Optional[Path] = None,
) -> ReconcileResult:
    """Walk the registry, sync statuses against provider reality, report.

    Read-mostly: each entry's status flip goes through ``update_registry``'s
    atomic load+filter+write cycle. No per-agent flock (Locked Decision 8):
    concurrent reconcile + ask is safe because ask mutates ``last_message_at``
    via the same atomic cycle and last-writer-wins on the timestamp; the
    status field updated by reconcile is independent.

    For each entry:

    - **claude**: ``claude logs <short_id> --tail 1`` (10s timeout) decides
      reachability. Exit 0 → live; anything else → orphaned.
    - **codex**: presence in ``~/.codex/session_index.jsonl`` decides
      reachability. Missing index → skip with an ``errors`` entry, leave
      status untouched (AC3-EDGE: fresh install must NOT trigger false
      orphan flags).
    - **gemini**: legacy registry rows are warned and skipped because the
      Python provider adapter is retired; new work routes to agy.

    Emits ``reconcile_done`` once at the end with the aggregate counts.
    """
    try:
        entries = load_registry()
    except (OSError, ValueError, RegistryVersionError) as exc:
        raise DispatchAskError(
            f"registry read failed: {exc}",
            exit_code=12,
        ) from exc

    entry_by_name = {entry.name: entry for entry in entries}
    # The PRIMARY index is (harness, harness_session_id): a label can be
    # renamed mid-flight; the pair cannot.
    entry_by_sid = {
        entry.harness_session_id: entry for entry in entries if entry.harness_session_id
    }

    def _entry_for(name: str, session_id: Optional[str] = None) -> Optional["AgentEntry"]:
        """Identity-first: a probed session id answers before the label."""
        if session_id:
            entry = entry_by_sid.get(session_id)
            if entry is not None:
                return entry
        return entry_by_name.get(name)

    def _vendor_for_name(name: str, session_id: Optional[str] = None) -> Optional[str]:
        entry = _entry_for(name, session_id)
        return entry.provider if entry is not None else None

    def _harness_for_name(name: str, session_id: Optional[str] = None) -> Optional[str]:
        entry = _entry_for(name, session_id)
        return entry.harness if entry is not None else None

    orphaned: list[dict] = []
    recovered: list[dict] = []
    skipped: list[dict] = []
    errors: list[dict] = []
    backfilled: list[dict] = []
    # rows whose structurally impossible mux ref was cleared (each
    # entry carries the cleared ref), with one ``mux_ref_cleared`` event each.
    mux_cleared: list[dict] = []
    # name -> (probed short_id, resolved harness_session_id) for a live row
    # whose canonical id never landed. Folded into the SAME batched
    # update_registry write as the status flips, so no new write cycle or lock
    # scope appears. The probed short_id is retained so the write only stamps a row
    # that STILL matches what we probed: a slow reconcile can race a rm + same-name
    # re-register, and stamping by name alone would put the old row's claude uuid
    # onto a replacement (possibly codex/gemini) row and misroute its mail.
    pending_backfill: dict[str, tuple[Optional[str], str]] = {}
    # name -> (pid, mux ref, resolved Codex thread id). Unlike the Claude
    # backfill above, every field is rechecked under the registry write lock:
    # the pane process is the identity authority and a same-name replacement
    # must never inherit its rollout.
    pending_codex_backfill: dict[
        str,
        tuple[Optional[int], Optional[int], dict, int, Optional[int], str],
    ] = {}

    # ``pending_updates`` accumulates per-name status flips across the
    # probe loop; at the end we apply ALL of them via a SINGLE
    # ``update_registry`` call. The dict[str, AgentEntry] shape (vs
    # list[tuple]) makes last-writer-wins explicit when the same name
    # could appear twice (Locked Decision 5; reconcile shouldn't probe
    # the same name twice today, but a future stale-cache or duplicate
    # row would silently collapse here instead of writing twice).
    #
    # Atomicity contract (AC3-ERR / Locked Decision 1): SIGINT mid-loop
    # discards ``pending_updates`` because the propagated KeyboardInterrupt
    # bypasses the post-loop ``update_registry`` call. The on-disk
    # registry mtime never changes.
    pending_updates: dict[str, tuple[AgentEntry, AgentStatus]] = {}

    # a mux ref that names no pane (a default-zero, an empty dict) is a
    # wrong value, and a wrong value is worse than no value -- 49 of 51 live
    # rows carry ``mux: None`` and every reader handles that, while the broken
    # shape made six live workers unreachable in every mux path. Queue the
    # clear for the SAME batched write as the status flips. Keyed by name with
    # the probed ref so the under-lock apply can compare-and-set against a
    # same-name replacement, exactly like the backfills above.
    pending_mux_clear: dict[str, dict] = {}

    # Read codex's session index ONCE outside the loop so a registry with
    # N codex agents only pays the I/O cost once. Mirror the same one-shot
    # capability check for claude so a host without `claude` on PATH does
    # NOT mass-flip every claude row to orphaned (sigma-review C1 finding:
    # the false-orphan storm is the worst kind of silent failure — it
    # rewrites the registry on insufficient evidence).
    from fno.agents.harnesses import codex as codex_mod
    from fno.agents.codex_pane import codex_backfill_stamp_applies

    # Tri-state per-codex-side capability: True (readable + present),
    # False (file missing, fresh install), None (file present but
    # unreadable, a permission or device error). Lumping "unreadable" with
    # "fresh install" would mass-orphan every codex agent on a host
    # with a permission glitch.
    codex_index_state: Optional[str] = None  # "ready" | "missing" | "unreadable"
    known_codex_ids: set[str] = set()
    claude_path_present: Optional[bool] = None
    for entry in entries:
        if entry.harness == "codex" and codex_index_state is None:
            # Probing the index path can raise PermissionError on hosts
            # where the parent directory is unreadable. Without this
            # catch a codex-local permission glitch would abort the
            # entire reconcile_agents call (Codex P1 round-4 finding
            # on PR #315). Treat any stat-time OSError as
            # "unreadable" — same operator outcome as a load-time
            # codex ReachabilityProbeError: route codex agents to errors.
            try:
                index_present = codex_mod.session_index_exists(
                    session_index_path=codex_session_index_path
                )
            except OSError as exc:
                codex_index_state = "unreadable"
                sys.stderr.write(
                    f"WARN: codex session index path unreadable: {exc}; "
                    "codex agents will be skipped (no reachability data)\n"
                )
            else:
                if not index_present:
                    codex_index_state = "missing"
                    missing_path = (
                        codex_session_index_path or codex_mod.default_session_index_path()
                    )
                    sys.stderr.write(
                        f"WARN: codex session index missing at {missing_path}; "
                        "codex agents will be skipped (no reachability data)\n"
                    )
                else:
                    try:
                        known_codex_ids = codex_mod.load_known_session_ids(
                            session_index_path=codex_session_index_path
                        )
                        codex_index_state = "ready"
                    except ReachabilityProbeError as exc:
                        # Catch the lifted base class so any codex-side probe
                        # error routes through the same path. The ``provider``
                        # attribute is "codex" by construction; the reason
                        # carries the underlying OSError detail.
                        codex_index_state = "unreadable"
                        sys.stderr.write(
                            f"WARN: codex session index unreadable: {exc.reason}; "
                            "codex agents will be skipped (no reachability data)\n"
                        )
        if entry.harness == "claude" and claude_path_present is None:
            claude_path_present = is_provider_available("claude")
            if not claude_path_present:
                sys.stderr.write(
                    "WARN: claude CLI not on PATH; claude agents will be "
                    "skipped (no reachability data — statuses will NOT be "
                    "flipped to orphaned)\n"
                )

    for entry in entries:
        new_status: AgentStatus
        if entry.mux is not None and not mux_ref_names_a_pane(entry.mux):
            # Structurally impossible ref: queue the heal, then probe this row
            # as the null-mux row it is about to become. Only an unreconcilable
            # ref is cleared here -- a ref that names a real pane is never touched,
            # alive or dead, and neither is one that merely could not be
            # probed (that distinction belongs to the pane falsifier's
            # False-vs-None split, not to validity).
            pending_mux_clear[entry.name] = entry.mux
        if entry.harness == "gemini":
            sys.stderr.write(
                f"WARN: agent {entry.name!r} references retired provider 'gemini'; "
                "skipping reachability probe (route new work to agy)\n"
            )
            errors.append(
                {
                    "name": entry.name,
                    "provider": entry.provider, "harness": entry.harness,
                    "id": entry.harness_session_id,
                    "reason": "retired-provider",
                }
            )
            continue
        elif entry.harness == "codex":
            # An id-less persistent pane can be healthy while Codex is still
            # creating its rollout. Heal it from the pane's own process tree;
            # the session index is not needed for this correlation. Gated on
            # ref VALIDITY, not truthiness: a ref that names no pane
            # must not send the row down the pane arm.
            if not entry.harness_session_id and mux_ref_names_a_pane(entry.mux):
                if entry.status in _TERMINAL_AGENT_STATUSES:
                    continue

                from fno.agents.mux_spawn import (
                    _codex_session_id_for_pid, _lookup_child_pid, _mux_pane_alive,
                )
                from fno.agents.spawn_gate import _pid_alive, _process_start_time

                pane_state = _mux_pane_alive(entry.mux)
                if pane_state is None:
                    errors.append(
                        {
                            "name": entry.name,
                            "provider": entry.provider, "harness": entry.harness,
                            "id": None,
                            "reason": "mux-pane-liveness-unavailable",
                        }
                    )
                    continue
                probe_pid = entry.pid
                probe_start = entry.pid_start_time
                # An incarnation token is what makes a recorded pid trustworthy:
                # without one `_pid_alive` degrades to bare existence, so a
                # recycled pid could bind a stranger's rollout onto this row. Rows
                # written before this stamping have no token, and they are exactly
                # the population this heal exists to repair - so re-derive the
                # pane's real child and require agreement. `_mux_pane_alive` proves
                # the PANE is up, never that this pid is still its child.
                confirmed = probe_start is not None
                if pane_state is not False and not confirmed:
                    live_pid = _lookup_child_pid(
                        str(entry.mux["session"]),
                        int(entry.mux["pane_id"]),
                        subprocess.run,
                    )
                    if live_pid is not None and probe_pid in (None, live_pid):
                        probe_pid = live_pid
                        probe_start = _process_start_time(probe_pid)
                        confirmed = True
                    elif probe_pid is None:
                        errors.append(
                            {
                                "name": entry.name,
                                "provider": entry.provider, "harness": entry.harness,
                                "id": None,
                                "reason": "codex-pane-pid-pending",
                            }
                        )
                        continue

                pid_state = (
                    _pid_alive(probe_pid, probe_start)
                    if pane_state is True and probe_pid is not None
                    else False
                )
                if pid_state is None:
                    errors.append(
                        {
                            "name": entry.name,
                            "provider": entry.provider, "harness": entry.harness,
                            "id": None,
                            "reason": "codex-process-incarnation-unavailable",
                        }
                    )
                    continue
                if pid_state is True:
                    assert probe_pid is not None
                    # Live, but we could not prove this pid is the pane's child.
                    # Never stamp an identity on that; a DEAD unconfirmed pid still
                    # takes the orphan path below, which binds nothing.
                    if not confirmed:
                        errors.append(
                            {
                                "name": entry.name,
                                "provider": entry.provider, "harness": entry.harness,
                                "id": None,
                                "reason": "codex-pane-pid-unconfirmed",
                            }
                        )
                        continue
                    probe_failed = False
                    try:
                        healed = _codex_session_id_for_pid(probe_pid)
                    except Exception as exc:  # noqa: BLE001 -- stays pending, never guesses
                        # The correlator already absorbs every EXPECTED failure
                        # (gone / access-denied / no rollout open) and returns None
                        # for them, so anything landing here is unexpected. Keep the
                        # row pending, but never render it as the benign "codex has
                        # not opened its rollout yet" case with the cause dropped -
                        # that reads as "still starting" forever.
                        healed = None
                        probe_failed = True
                        sys.stderr.write(
                            f"WARN: codex identity probe failed for {entry.name} "
                            f"(pid {probe_pid}): {exc}\n"
                        )
                    if healed:
                        # Scan the rows AND the ids this same pass already decided
                        # to stamp. Two id-less rows pointing at one pane both see
                        # the other as `harness_session_id=None`, so a rows-only
                        # check clears both and stamps the duplicate identity this
                        # guard exists to prevent.
                        duplicate = any(
                            other.name != entry.name
                            and other.harness_session_id == healed
                            for other in entries
                        ) or any(
                            claimed[-1] == healed
                            for name, claimed in pending_codex_backfill.items()
                            if name != entry.name
                        )
                        if duplicate:
                            if entry.status != "spawning":
                                pending_updates[entry.name] = (entry, "spawning")
                            errors.append(
                                {
                                    "name": entry.name,
                                    "provider": entry.provider, "harness": entry.harness,
                                    "id": None,
                                    "reason": "duplicate-codex-session-id",
                                }
                            )
                        else:
                            pending_codex_backfill[entry.name] = (
                                entry.pid,
                                entry.pid_start_time,
                                dict(entry.mux),
                                probe_pid,
                                probe_start,
                                healed,
                            )
                        continue

                    if entry.status != "spawning":
                        pending_updates[entry.name] = (entry, "spawning")
                    errors.append(
                        {
                            "name": entry.name,
                            "provider": entry.provider, "harness": entry.harness,
                            "id": None,
                            "reason": (
                                "codex-session-probe-failed"
                                if probe_failed
                                else "codex-session-id-pending"
                            ),
                        }
                    )
                    continue

                # The pane process is gone. Preserve terminal rows above; a
                # nonterminal pending/live row follows the existing orphan path.
                new_status = "orphaned"
            elif entry.status in {"failed", "exited", "permanent_dead"}:
                continue
            elif not entry.harness_session_id and entry.status == "orphaned":
                continue
            elif not entry.harness_session_id:
                events.emit(
                    "agent_inconsistent",
                    name=entry.name,
                    provider="codex",
                )
                errors.append(
                    {
                        "name": entry.name,
                        "provider": entry.provider, "harness": entry.harness,
                        "id": None,
                        "reason": "missing-codex-session-id",
                    }
                )
                continue
            elif codex_index_state != "ready":
                # AC3-EDGE: cannot probe codex reachability; report as
                # error but do NOT flip status. The reason discriminator
                # distinguishes "fresh install" (operator action: ignore)
                # from "permission glitch" (operator action: fix perms).
                if codex_index_state == "unreadable":
                    reason = "codex-session-index-unreadable"
                else:
                    reason = "codex-session-index-missing"
                errors.append(
                    {
                        "name": entry.name,
                        "provider": entry.provider, "harness": entry.harness,
                        "id": entry.harness_session_id,
                        "reason": reason,
                    }
                )
                continue
            else:
                reachable = entry.harness_session_id in known_codex_ids
                new_status = "live" if reachable else "orphaned"

        elif entry.harness == "claude":
            # EVERY claude PANE row reconciles from the pane, with or without a
            # session id. A mux row's short_id is deliberately empty (one live ref
            # per row), so the `claude logs` probe below - which is keyed on
            # short_id - can never reach one; without this arm a pane row falls to
            # `missing-claude-short-id`, which only reports and never changes
            # status, so a dead pane holds its name against every future spawn of
            # that name, forever.
            #
            # Gating this on `not harness_session_id` was the earlier shape and it
            # was the AGENTS.md path-uniqueness trap in miniature: it covered the
            # pane only until its worker restamped, and a pane that then died was
            # unreachable by any arm. The pane is the reachability oracle for a
            # pane-hosted row for its whole life, not just before it has an id.
            #
            # Deliberately ahead of the claude-on-PATH guard: this probes the mux
            # and the pid, never the claude CLI, so a host where claude was
            # removed can still retire a provably dead pane. Gated on ref
            # VALIDITY, not truthiness: a ref that names no pane is
            # not a pane row and must not be pane-probed.
            if mux_ref_names_a_pane(entry.mux):
                if entry.status in _TERMINAL_AGENT_STATUSES:
                    continue

                from fno.agents.mux_spawn import _lookup_child_pid, _mux_pane_alive
                from fno.agents.spawn_gate import _pid_alive, _process_start_time

                pane_state = _mux_pane_alive(entry.mux)
                if pane_state is None:
                    errors.append(
                        {
                            "name": entry.name,
                            "provider": entry.provider, "harness": entry.harness,
                            "id": None,
                            "reason": "mux-pane-liveness-unavailable",
                        }
                    )
                    continue
                probe_pid = entry.pid
                probe_start = entry.pid_start_time
                # `_mux_pane_alive` proves the PANE is up, never that the recorded
                # pid is still its child. Without an incarnation token `_pid_alive`
                # degrades to bare existence, so re-derive the pane's real child
                # before trusting a live answer. Unlike the codex arm this never
                # stamps an identity from the pid, so an unconfirmed pid only ever
                # keeps the row waiting - it can never bind a stranger.
                # ALWAYS re-derive the pane's current child, even when the row
                # carries an incarnation token. That token proves only that the
                # stored pid's incarnation is alive; it says nothing about which
                # pane that process now belongs to. A mux restart can hand
                # `(session, pane_id)` to a different child while the original is
                # still running, and then `_mux_pane_alive` and `_pid_alive` are
                # both true about DIFFERENT processes - which preserves the row as
                # live and points delivery at the stranger's pane.
                confirmed = False
                if pane_state is not False:
                    live_pid = _lookup_child_pid(
                        str(entry.mux["session"]),
                        int(entry.mux["pane_id"]),
                        subprocess.run,
                    )
                    if live_pid is not None and probe_pid in (None, live_pid):
                        probe_pid = live_pid
                        # Keep a stored incarnation token: it is stronger than a
                        # fresh read, which cannot see a pid recycled since.
                        if probe_start is None:
                            probe_start = _process_start_time(probe_pid)
                        confirmed = True
                # A live pane with no usable pid is INCONCLUSIVE, never dead.
                # `_lookup_child_pid` is best-effort, so folding its miss into
                # `False` would orphan a healthy worker on absent evidence - and
                # `orphaned` is terminal here while a later restamp only promotes
                # `spawning`, so that mistake is permanent. Stay pending, exactly
                # as the codex arm does with `codex-pane-pid-pending`.
                if pane_state is True and probe_pid is None:
                    errors.append(
                        {
                            "name": entry.name,
                            "provider": entry.provider, "harness": entry.harness,
                            "id": None,
                            "reason": "claude-pane-pid-pending",
                        }
                    )
                    continue
                # An uncorrelated pid may not be this pane's at all. A legacy row
                # carries no incarnation token, so `_pid_alive` degrades to bare
                # existence there; a mux restart can hand `(session, pane_id)` to
                # a different child while the recorded pid has been RECYCLED and
                # is alive. Trusting that keeps the row `live` and points
                # name-based delivery at a stranger. Only a DEAD pane is decided
                # without correlation, since nothing can be bound to it.
                if pane_state is True and not confirmed:
                    errors.append(
                        {
                            "name": entry.name,
                            "provider": entry.provider, "harness": entry.harness,
                            "id": None,
                            "reason": "claude-pane-pid-unconfirmed",
                        }
                    )
                    continue
                pid_state = (
                    _pid_alive(probe_pid, probe_start)
                    if pane_state is True and probe_pid is not None
                    else False
                )
                if pid_state is None:
                    errors.append(
                        {
                            "name": entry.name,
                            "provider": entry.provider, "harness": entry.harness,
                            "id": None,
                            "reason": "claude-process-incarnation-unavailable",
                        }
                    )
                    continue
                if pid_state is True:
                    # The pane is up. Leave the status alone: a row still owed
                    # its restamp is correctly `spawning`, and one that already
                    # got it was promoted to `live` by the restamp itself. Either
                    # way this pass has nothing to correct.
                    continue
                new_status = "orphaned"

            elif not claude_path_present:
                # Mirror the codex-index-missing pattern: when claude is
                # not installed we cannot probe reachability, so we route
                # the entry to `errors` with status untouched. Anything
                # else would mass-flip every claude row to orphaned on a
                # host where claude was removed mid-day.
                errors.append(
                    {
                        "name": entry.name,
                        "provider": entry.provider, "harness": entry.harness,
                        "id": entry.short_id,
                        "reason": "claude-cli-not-on-path",
                    }
                )
                continue
            elif not entry.short_id:
                events.emit(
                    "agent_inconsistent",
                    name=entry.name,
                    provider="claude",
                )
                errors.append(
                    {
                        "name": entry.name,
                        "provider": entry.provider, "harness": entry.harness,
                        "id": None,
                        "reason": "missing-claude-short-id",
                    }
                )
                continue
            else:
                from fno.agents.harnesses import claude as claude_mod

                # Phase 5: MCP-backed claude agents probe via the sidecar
                # instead of `claude logs`. Same tri-state contract:
                # True/False/raise. Socket-only agents (mcp_channel_id is
                # None) keep the legacy claude_logs_reachable path.
                # NOTE: probe_label is assigned BEFORE the probe call so a
                # ReachabilityProbeError from the probe still has the
                # label in scope for the error route.
                probe_label = (
                    "claude-mcp-probe-failed" if entry.mcp_channel_id else "claude-probe-failed"
                )
                try:
                    if entry.mcp_channel_id:
                        reachable = claude_mod.mcp_channel_reachable(entry.mcp_channel_id, timeout=0.25)
                    else:
                        reachable = claude_mod.claude_logs_reachable(
                            entry.short_id, timeout=claude_logs_timeout
                        )
                except ReachabilityProbeError as exc:
                    # Catch the lifted base class (US4-gemini Wave 1.1) so
                    # both the claude-side timeout/OSError probe error and the
                    # Phase 5 ``mcp_channel_disconnected`` probe error are routed
                    # identically. Probe inconclusive -> preserve status,
                    # route to errors with a per-provider reason
                    # discriminator. Mirrors the codex-side
                    # codex-session-index-unreadable treatment so transient
                    # CLI slowness or sidecar I/O hiccups don't mass-orphan
                    # healthy agents (Codex P1 round-5 on PR #315).
                    events.emit(
                        "agent_inconsistent",
                        name=entry.name,
                        provider="claude",
                        reason=exc.reason,
                    )
                    errors.append(
                        {
                            "name": entry.name,
                            "provider": entry.provider, "harness": entry.harness,
                            "id": entry.short_id,
                            "reason": f"{probe_label}: {exc.reason}",
                        }
                    )
                    continue
                new_status = "live" if reachable else "orphaned"

                # US4 heal: a live claude row whose canonical id never landed
                # (the uuid resolution raced at spawn) is unroutable-but-live. Resolve
                # it from claude's own store -- the same jsonl the liveness probe just
                # read -- and fold the write into reconcile's single batched cycle. A
                # miss leaves it null (the durable queue stays the floor); never fatal.
                if reachable and not entry.harness_session_id and entry.short_id:
                    try:
                        healed = claude_mod.resolve_session_uuid(entry.short_id)
                    except Exception:  # noqa: BLE001 — a resolver error is a tolerated miss
                        healed = None
                    if healed:
                        # Queue only. The `backfilled` claim is made AFTER the write,
                        # against the names the write actually stamped: the under-lock
                        # guard can refuse this row (same-name rm + re-register), and
                        # claiming success here reported a heal that never landed.
                        pending_backfill[entry.name] = (entry.short_id, healed)

        else:
            errors.append(
                {
                    "name": entry.name,
                    "provider": entry.provider, "harness": entry.harness,
                    "id": None,
                    "reason": f"unknown-provider-{entry.harness}",
                }
            )
            continue

        if entry.status == new_status:
            continue  # no change; do not write

        # Status drifted — queue the updated entry for the batched
        # single-cycle write at the end of the loop. ``dataclasses.replace``
        # preserves every other field automatically (Gemini medium on
        # PR #317), which is more robust against future AgentEntry
        # schema additions than manual field-by-field reconstruction.
        pending_updates[entry.name] = (entry, new_status)

        change = {
            "name": entry.name,
            "provider": entry.provider, "harness": entry.harness,
            # Codex P2 on PR #317: include gemini_session_id so reconcile
            # records carry an identifier for every provider. Pre-fix
            # gemini agents flipped between live/orphaned with "id": null
            # which rendered as "?" in human output and broke follow-up
            # tooling.
            "id": (entry.short_id or entry.harness_session_id),
        }
        if new_status == "orphaned":
            orphaned.append(change)
        else:
            recovered.append(change)

    # Single atomic write for ALL queued flips (AC3-HP: at most one
    # update_registry call per reconcile). Empty pending_updates
    # short-circuits with no write at all (AC3-UI). On disk-write
    # failure, every queued change moves from orphaned/recovered into
    # errors so the operator sees a single coherent failure rather than
    # a partial split. The all-or-nothing atomicity is enforced by
    # update_registry's own atomic-rename semantics — the closure is
    # pure, so an OSError mid-write leaves the registry untouched.
    if pending_updates or pending_backfill or pending_codex_backfill or pending_mux_clear:

        codex_backfill_applied: set[str] = set()
        status_updates_applied: set[str] = set()
        codex_ids_claimed: set[str] = set()
        claude_backfill_applied: set[str] = set()
        mux_clears_applied: set[str] = set()

        def _apply(current_entries: list[AgentEntry]) -> list[AgentEntry]:
            # Reset per call: update_registry may re-run _apply against a fresh
            # snapshot after lock contention, and carrying a previous attempt's
            # names over would report a row as applied that this attempt skipped.
            codex_backfill_applied.clear()
            status_updates_applied.clear()
            codex_ids_claimed.clear()
            claude_backfill_applied.clear()
            mux_clears_applied.clear()
            # Build the new entries from the CURRENT (under-lock) entries,
            # overriding only the ``status`` field from pending_updates.
            # Pre-fix this returned ``pending_updates.get(e.name, e)`` which
            # substituted the entire snapshot AgentEntry captured at probe
            # time — silently losing any ``last_message_at`` bump that
            # dispatch_ask wrote during the probe loop (US4-gemini handoff:
            # concurrent reconcile + ask data loss).
            out: list[AgentEntry] = []
            for e in current_entries:
                updates: dict = {}
                if e.name in pending_mux_clear:
                    # Compare-and-set against the probed ref: a same-name
                    # re-register during the probe loop may have written a
                    # fresh hosting ref, and this stale heal must never wipe
                    # it. Change NO other field here -- a heal that becomes a
                    # second bug is the exact failure this narrowness prevents.
                    if e.mux == pending_mux_clear[e.name]:
                        updates["mux"] = None
                        mux_clears_applied.add(e.name)
                if e.name in pending_updates:
                    probed, target_status = pending_updates[e.name]
                    if (
                        e.harness == probed.harness
                        and e.pid == probed.pid
                        and e.pid_start_time == probed.pid_start_time
                        and e.mux == probed.mux
                        and e.harness_session_id == probed.harness_session_id
                        and e.short_id == probed.short_id
                        and e.status == probed.status
                    ):
                        updates["status"] = target_status
                        status_updates_applied.add(e.name)
                if e.name in pending_backfill:
                    probed_short, hsid = pending_backfill[e.name]
                    # Only stamp a row that STILL matches the row we probed: a
                    # same-name rm+re-register during the probe loop would put this
                    # claude uuid onto a replacement row (misrouting its mail).
                    if e.harness == "claude" and e.short_id == probed_short:
                        # Canonical wins: set harness_session_id; the legacy
                        # claude uuid is synced from it on the next load's backfill.
                        updates["harness_session_id"] = hsid
                        updates["harness"] = e.harness
                        claude_backfill_applied.add(e.name)
                if e.name in pending_codex_backfill:
                    (
                        expected_pid,
                        expected_start,
                        probed_mux,
                        resolved_pid,
                        resolved_start,
                        hsid,
                    ) = pending_codex_backfill[e.name]
                    # `codex_ids_claimed` carries ids stamped EARLIER IN THIS APPLY,
                    # which `current_entries` cannot show because it is a consistent
                    # pre-update snapshot. Without it two rows racing for one pane
                    # both read "nobody owns this id" and both take it.
                    duplicate = hsid in codex_ids_claimed or any(
                        other.name != e.name
                        and other.harness_session_id == hsid
                        for other in current_entries
                    )
                    # ONE guard, branch inside; the conjuncts are the shared
                    # codex_pane.codex_backfill_stamp_applies helper the spawn-time
                    # late-bind stamp uses, so the two stampers cannot drift.
                    if codex_backfill_stamp_applies(
                        e, e.name, hsid, expected_pid, expected_start, probed_mux
                    ):
                        if duplicate:
                            updates["status"] = "spawning"
                        else:
                            updates["harness_session_id"] = hsid
                            updates["status"] = "live"
                            updates["pid"] = resolved_pid
                            updates["pid_start_time"] = resolved_start
                            codex_ids_claimed.add(hsid)
                            codex_backfill_applied.add(e.name)
                out.append(replace(e, **updates) if updates else e)
            return out

        try:
            update_registry(_apply)
        except (OSError, RegistryVersionError) as exc:
            # Re-classify every queued change as a write failure. Move
            # them out of orphaned/recovered into errors so callers don't
            # see a recovered/orphaned record that never actually committed.
            # A backfill that never committed must not claim it healed either.
            write_error = f"registry-write-failed: {exc}"
            failed_names = set(pending_updates.keys()) | set(pending_codex_backfill.keys())
            for change in list(orphaned):
                if change["name"] in failed_names:
                    orphaned.remove(change)
                    errors.append({**change, "reason": write_error})
            for change in list(recovered):
                if change["name"] in failed_names:
                    recovered.remove(change)
                    errors.append({**change, "reason": write_error})
            for change in list(backfilled):
                backfilled.remove(change)
                errors.append({**change, "id": None, "reason": write_error})
            for name in (*pending_backfill, *pending_codex_backfill, *pending_mux_clear):
                errors.append(
                    {
                        "name": name,
                        "provider": _vendor_for_name(name), "harness": _harness_for_name(name),
                        "id": None,
                        "reason": write_error,
                    }
                )
        else:

            def _record_backfill(
                name: str, hsid: Optional[str], applied: set, raced: str
            ) -> None:
                if name in applied:
                    backfilled.append(
                        {
                            "name": name,
                            "provider": _vendor_for_name(name, hsid), "harness": _harness_for_name(name, hsid),
                            "harness_session_id": hsid,
                        }
                    )
                else:
                    errors.append(
                        {
                            "name": name,
                            "provider": _vendor_for_name(name, hsid), "harness": _harness_for_name(name, hsid),
                            "id": None,
                            "reason": raced,
                        }
                    )

            for name, (_probed_short, hsid) in pending_backfill.items():
                _record_backfill(name, hsid, claude_backfill_applied, "claude-session-id-backfill-raced")
            for name, (_epid, _estart, _mux, _pid, _start, hsid) in pending_codex_backfill.items():
                _record_backfill(name, hsid, codex_backfill_applied, "codex-session-id-backfill-raced")
            raced_updates = set(pending_updates) - status_updates_applied
            for change in list(orphaned):
                if change["name"] in raced_updates:
                    orphaned.remove(change)
            for change in list(recovered):
                if change["name"] in raced_updates:
                    recovered.remove(change)
            for name in raced_updates:
                probed, _target = pending_updates[name]
                errors.append(
                    {
                        "name": name,
                        "provider": probed.provider, "harness": probed.harness,
                        "id": probed.short_id or probed.harness_session_id,
                        "reason": "registry-status-update-raced",
                    }
                )
            for name, probed_mux in pending_mux_clear.items():
                if name in mux_clears_applied:
                    # AC6: the heal is spoken. One event per cleared ref, naming
                    # exactly what was removed, so the heal is auditable from
                    # events.jsonl alone.
                    mux_cleared.append(
                        {
                            "name": name,
                            "provider": _vendor_for_name(name),
                            "harness": _harness_for_name(name),
                            "cleared_mux": probed_mux,
                        }
                    )
                    events.emit(
                        "mux_ref_cleared",
                        name=name,
                        provider=_vendor_for_name(name),
                        harness=_harness_for_name(name),
                        cleared_mux=probed_mux,
                    )
                else:
                    errors.append(
                        {
                            "name": name,
                            "provider": _vendor_for_name(name),
                            "harness": _harness_for_name(name),
                            "id": None,
                            "reason": "mux-ref-clear-raced",
                        }
                    )

    events.emit(
        "reconcile_done",
        scanned=len(entries),
        orphaned=len(orphaned),
        recovered=len(recovered),
        skipped=len(skipped),
        errors=len(errors),
        backfilled=len(backfilled),
        mux_cleared=len(mux_cleared),
    )
    return ReconcileResult(
        scanned=len(entries),
        orphaned=orphaned,
        recovered=recovered,
        skipped=skipped,
        errors=errors,
        backfilled=backfilled,
        mux_cleared=mux_cleared,
    )


# ---------------------------------------------------------------------------
# G2 Task 2.1 — send command (async, durable-first)
# ---------------------------------------------------------------------------

#: Body size cap enforced before any envelope write (AC3-EDGE).
_SEND_MAX_BODY_BYTES = 1024 * 1024  # 1 MiB


def rpc_roundtrip(
    sock_path: Path,
    method: str,
    params: dict,
    *,
    connect_timeout: float = 3.0,
    read_timeout: float = 5.0,
    note=None,
    unreachable: str = "fno-agents daemon unreachable; message queued durable",
) -> Optional[dict]:
    """One JSON-RPC request over a unix socket, the single client for the
    4-byte-LE-u32 + JSON framing of crates/fno-agents/src/protocol.rs.

    Returns the ``result`` field dict on success; None on any transport error
    or ``error`` response. NEVER raises. ``note(message)`` receives each
    diagnostic (None = silent); ``unreachable`` wording is the lane's own.
    """
    import json
    import socket
    import struct

    emit = note if note is not None else (lambda _msg: None)
    payload = json.dumps({"id": 1, "method": method, "params": params}).encode("utf-8")
    if len(payload) > 16 * 1024 * 1024:
        return None  # mirror the outbound MAX_FRAME_BYTES cap (protocol.rs)
    frame = struct.pack("<I", len(payload)) + payload

    sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    try:
        sock.settimeout(connect_timeout)
        try:
            sock.connect(str(sock_path))
        except (FileNotFoundError, ConnectionRefusedError, OSError):
            emit(unreachable)
            return None

        sock.settimeout(read_timeout)
        sock.sendall(frame)

        header = b""
        while len(header) < 4:
            chunk = sock.recv(4 - len(header))
            if not chunk:
                emit("daemon closed connection unexpectedly")
                return None
            header += chunk
        (length,) = struct.unpack_from("<I", header)

        # Guard against absurd lengths (mirrors protocol.rs MAX_FRAME_BYTES).
        if length > 16 * 1024 * 1024:
            emit(f"daemon returned oversized frame ({length} bytes)")
            return None

        data = bytearray()
        while len(data) < length:
            chunk = sock.recv(length - len(data))
            if not chunk:
                emit("daemon closed connection mid-frame")
                return None
            data += chunk

        resp = json.loads(data.decode("utf-8"))
        if not isinstance(resp, dict):
            emit("daemon returned invalid JSON-RPC response shape")
            return None
        if "error" in resp:
            err = resp["error"]
            emit(f"daemon RPC error: {err.get('message', err)}")
            return None
        return resp.get("result")
    except (OSError, ValueError) as exc:
        # ValueError covers a malformed daemon response; print the reason.
        emit(f"socket error: {exc}")
        return None
    finally:
        sock.close()


def agents_home() -> Path:
    """The fno-agents home, resolved as the Rust client does:
    ``$FNO_AGENTS_HOME`` else ``$HOME/.fno/agents``."""
    import os

    env = os.environ.get("FNO_AGENTS_HOME")
    if env:
        return Path(env)
    return Path(os.path.expanduser("~")) / ".fno" / "agents"


def _daemon_rpc(
    method: str,
    params: dict,
    *,
    connect_timeout: float = 3.0,
    read_timeout: float = 5.0,
) -> Optional[dict]:
    """Send one JSON-RPC request to the daemon and return the result dict.

    The supervisor socket is ``supervisor.sock`` inside :func:`agents_home`.
    None on any transport error or ``error`` response (the
    :func:`rpc_roundtrip` contract, exactly one attempt); callers demote to
    durable on any falsy return.
    """
    sock_path = agents_home() / "supervisor.sock"

    def _note(message: str) -> None:
        print(message, file=sys.stderr)

    return rpc_roundtrip(
        sock_path,
        method,
        params,
        connect_timeout=connect_timeout,
        read_timeout=read_timeout,
        note=_note,
    )


# read_timeout exceeds the daemon's per-turn ceiling
# (SWITCHBOARD_TURN_TIMEOUT_MS=120s) plus its 5s grace, so a real reply is never
# cut short and the client never abandons a turn the daemon is still driving.
# The detached background relay continuation (_relay_worker_loop) runs
# off-thread and wants the full ceiling for its own reason: a genuine multi-hop
# stream exchange. The first hop needs the same number for the reason below.
_SWITCHBOARD_READ_TIMEOUT = 130.0
# The FIRST hop's read budget. SYNCHRONOUS: `agent.switchboard_v2` does not
# ack and return; `handle_switchboard` drives B's WHOLE turn before it answers
# (SWITCHBOARD_TURN_TIMEOUT_MS + SWITCHBOARD_DRIVE_GRACE_S). Reading under
# 125s abandons a turn the daemon still drives and injects the SAME body a
# second time under a durable receipt. Capping the daemon instead stamps every
# slow-but-healthy peer orphaned; the wedged-daemon fix belongs daemon-side,
# so the sender pays this ceiling.
_SWITCHBOARD_FIRST_HOP_READ_TIMEOUT = _SWITCHBOARD_READ_TIMEOUT
# A SHORT connect timeout: every claude send now tries the switchboard first, so
# a DOWN/wedged daemon must not tax the common (non-stream) path — it should fail
# the connect fast and demote, rather than burn the 3s default before the
# existing MCP/socket path runs.
_SWITCHBOARD_CONNECT_TIMEOUT = 1.0
# A pre-identity daemon rejects this command before it can act on the message body.
_SWITCHBOARD_RPC_METHOD = "agent.switchboard_v2"


def _load_a2a_settings() -> tuple[bool, int]:
    """Read ``(auto, turn_ceiling)`` from ``config.agents.a2a``.

    A failed / malformed read degrades SAFELY to OBSERVED mode (``auto=False``)
    so a broken settings file never starts an autonomous A<->B relay. The
    ceiling still applies and stays positive.
    """
    try:
        from fno.config import load_settings

        a2a = load_settings().agents.a2a
        return bool(a2a.auto), max(1, int(a2a.turn_ceiling))
    except Exception:
        return (False, 6)


def _wrap_relay_body(cur: str, ctx: "Optional[_MailCtx]") -> str:
    """Wrap a relay hop in the peer's envelope; the hop mints its header id."""
    if ctx is None:
        return cur
    from fno.inbox.store import generate_msg_id
    from fno.mail.envelope import wrap_fno_mail

    return wrap_fno_mail(
        cur,
        from_=ctx.from_,
        harness=ctx.harness,
        node=ctx.node,
        to=ctx.to,
        id=ctx.id or generate_msg_id(),
        from_session=ctx.from_session,
        origin=ctx.origin,
        to_session=ctx.to_session,
    )


def _emit_relay_stopped(
    target: str,
    peer: str,
    turns_completed: int,
    reason: str,
    *,
    error: Optional[BaseException | str] = None,
) -> None:
    data: dict[str, object] = {
        "target": target,
        "peer": peer,
        "turn": turns_completed + 1,
        "turns_completed": turns_completed,
        "reason": reason,
    }
    if error is not None:
        data["error"] = str(error)
        if isinstance(error, BaseException):
            data["error_type"] = type(error).__name__
    _emit_ev(events.KIND_AGENT_RELAY_STOPPED, **data)


def _run_relay_loop(
    to_name: str,
    from_name: str,
    seed: str,
    ceiling: int,
    mail_ctxs: "Optional[dict[str, _MailCtx]]" = None,
    *,
    recipient_identities: "Mapping[str, SwitchboardIdentity]",
) -> int:
    """Drive the bounded A2A relay AFTER the first hop (B already replied
    ``seed``). Alternate driving A then B with each other's reply — the drive IS
    the literal injection into the target — up to ``ceiling`` total turns
    (counting the first hop), stopping with a visible "loop ceiling reached". A
    side that is not a live stream thread ends the relay.

    Returns the total number of turns driven (counting the caller's first hop),
    so a synchronous driver can report the terminal state.
    Existing callers (:func:`_kickoff_background_relay`, the inline fallback)
    ignore the return, so this is additive.

    Pure orchestration over ``_daemon_rpc`` (no forking here), so it is callable
    both inline (tests, fork-unavailable fallback) and from the detached
    background process kicked off by :func:`_kickoff_background_relay`.
    """
    cur = seed
    target, peer = from_name, to_name  # next: drive A (from) with B's reply
    turns = 1  # the first hop (drive B) already happened in the caller
    while turns < ceiling and cur.strip():
        target_identity = recipient_identities.get(target)
        if target_identity is None:
            _emit_relay_stopped(target, peer, turns, "recipient-identity-missing")
            break
        try:
            hop = _daemon_rpc(
                _SWITCHBOARD_RPC_METHOD,
                {
                    "to": target,
                    "from": peer,
                    # Wrap each continuation in the sending peer's <fno_mail> so the
                    # relay turn carries provenance, not just the seed (node).
                    "body": _wrap_relay_body(cur, (mail_ctxs or {}).get(peer)),
                    "mirror": False,
                    "recipient_identity": target_identity,
                },
                connect_timeout=_SWITCHBOARD_CONNECT_TIMEOUT,
                read_timeout=_SWITCHBOARD_READ_TIMEOUT,
            )
        except Exception as exc:
            _emit_relay_stopped(target, peer, turns, "relay-hop-error", error=exc)
            break
        if (
            not isinstance(hop, dict)
            or hop.get("delivered") is not True
            or hop.get("identity_verified") is not True
        ):
            if not isinstance(hop, dict):
                reason = "invalid-response"
            elif hop.get("delivered") is not True:
                reason = str(hop.get("reason") or "not-delivered")
            else:
                reason = "identity-unverified"
            error = hop.get("error") if isinstance(hop, dict) else None
            _emit_relay_stopped(target, peer, turns, reason, error=error)
            break
        turns += 1
        cur = hop.get("reply") or ""
        target, peer = peer, target
    if turns >= ceiling:
        print(
            f"fno-agents switchboard: loop ceiling reached ({ceiling} turns)",
            file=sys.stderr,
        )
    return turns


def _detach_stdio() -> bool:
    """Detach every standard fd, returning false if any still stays inherited."""
    import os

    try:
        devnull = os.open(os.devnull, os.O_RDWR)
    except OSError:
        return False
    detached = True
    for fd in (0, 1, 2):
        try:
            os.dup2(devnull, fd)
        except OSError:
            detached = False
    if devnull > 2:
        try:
            os.close(devnull)  # the dup2'd copies remain; don't leak the original
        except OSError:
            pass
    return detached


def _kickoff_background_relay(
    to_name: str,
    from_name: str,
    seed: str,
    ceiling: int,
    mail_ctxs: "Optional[dict[str, _MailCtx]]" = None,
    *,
    recipient_identities: "Mapping[str, SwitchboardIdentity]",
) -> None:
    """Run the A2A relay in a DETACHED background process so the caller returns
    immediately.

    The relay is autonomous — no human waits on it — so blocking the
    ``fno agents mail send`` caller for up to ``turn_ceiling × 130s`` was pure
    latency. The send's actual delivery (hop 1: B received the message) already
    happened synchronously in :func:`_switchboard_exchange`; this only continues
    the autonomous A<->B exchange. Double-fork + ``setsid`` so the relay outlives
    the short-lived CLI process and reparents to init (no zombie). A fork failure
    stops only this optional continuation and stays visible; hop one is already
    delivered, so running inline would suppress its receipt and make retry unsafe.
    """
    import os

    try:
        pid = os.fork()
    except OSError as exc:
        _emit_relay_stopped(
            from_name,
            to_name,
            1,
            "relay-detach-failed",
            error=exc,
        )
        return
    if pid > 0:
        # Parent: reap the intermediate child (it exits at once) and return.
        try:
            os.waitpid(pid, 0)
        except OSError:
            pass
        return
    # Intermediate child: detach into a new session, fork the grandchild that
    # actually runs the relay, then exit so the grandchild reparents to init.
    try:
        os.setsid()
        try:
            grandchild = os.fork()
        except OSError as exc:
            _emit_relay_stopped(
                from_name,
                to_name,
                1,
                "relay-detach-failed",
                error=exc,
            )
            return
        if grandchild > 0:
            os._exit(0)
        if not _detach_stdio():
            _emit_relay_stopped(
                from_name,
                to_name,
                1,
                "relay-stdio-detach-failed",
            )
            return
        try:
            _run_relay_loop(
                to_name,
                from_name,
                seed,
                ceiling,
                mail_ctxs,
                recipient_identities=recipient_identities,
            )
        except Exception as exc:
            _emit_relay_stopped(
                from_name,
                to_name,
                1,
                "relay-process-error",
                error=exc,
            )
    finally:
        # _exit (not sys.exit) so the child never runs atexit handlers or flushes
        # the parent's buffers a second time.
        os._exit(0)


_A2A_CONFIRM_TIMEOUT_SECONDS = 5.0
_A2A_CONFIG_LOCK_TIMEOUT_SECONDS = 1.0


def _a2a_first_use_gate(
    auto: bool,
    ceiling: int,
    *,
    confirm_timeout_seconds: float = _A2A_CONFIRM_TIMEOUT_SECONDS,
    config_lock_timeout_seconds: float = _A2A_CONFIG_LOCK_TIMEOUT_SECONDS,
) -> bool:
    """First-use confirm for the autonomous a2a relay (US6).

    Returns the EFFECTIVE ``auto`` after gating. Only the autonomous relay
    (``auto=True``) is gated; observed mode (``auto=False``, incl. the
    malformed-config fail-safe) needs no confirm and passes through.

    The first time the relay would fire its first autonomous hop, the user is
    asked once and the answer is persisted (a host marker + the settings value),
    so it never re-asks (AC6-FR). The prompt names the turn ceiling and that the
    relay draws plan credit (AC6-UI).

    Headless / no-TTY (Locked Decision 7 / F4): the relay NEVER inherits
    ``auto:true`` unattended — the conservative fallback (autonomous relay OFF,
    i.e. a single observed hop) applies regardless of the configured default,
    the decision is logged, and the caller is never blocked. The fallback is a
    per-run decision and is NOT persisted, so a later interactive run still asks.
    """
    import os

    # Test seam: relay-logic tests exercise auto=True directly and bypass the
    # confirm. Never set in production.
    if os.environ.get("FNO_A2A_NO_CONFIRM"):
        return auto
    if not auto:
        return False

    from fno import paths

    marker = paths.state_runtime_file("a2a-confirmed", legacy_name=".a2a-confirmed")
    if marker.exists():
        return True  # answered once already; honor the persisted setting.

    interactive = sys.stdin.isatty() and sys.stderr.isatty()
    if not interactive:
        sys.stderr.write(
            "fno-agents a2a: no TTY to confirm autonomous relay; applying the "
            "conservative fallback (autonomous relay OFF, single observed hop). "
            "Run `fno config set config.agents.a2a.auto true` to opt in.\n"
        )
        sys.stderr.flush()
        return False

    sys.stderr.write(
        f"\na2a auto-relay is ON: an A<->B send runs up to {ceiling} autonomous "
        "turns, which draws plan credit.\nKeep auto-relay on? [Y/n] "
    )
    sys.stderr.flush()
    try:
        from fno.time_budget import validate_timeout_budget

        validate_timeout_budget(
            confirm_timeout_seconds,
            label="a2a confirmation",
        )
        ready, _, _ = select.select(
            [sys.stdin],
            [],
            [],
            confirm_timeout_seconds,
        )
        if not ready:
            raise TimeoutError
        raw_answer = sys.stdin.readline()
        if raw_answer == "":
            raise EOFError
        answer = raw_answer.strip().lower()
    except Exception:
        sys.stderr.write(
            "\nfno-agents a2a: confirmation timed out or could not be read; "
            "applying the conservative fallback (autonomous relay OFF, single "
            "observed hop).\n"
        )
        sys.stderr.flush()
        return False
    keep_on = answer in ("", "y", "yes")

    try:
        from fno.rust_binary import resolve_front_binary

        argv = ["config", "set", "config.agents.a2a.auto", "true" if keep_on else "false"]
        # The budget bounds the lock wait only; the run bound covers startup.
        env = {**os.environ, "FNO_CONFIG_LOCK_TIMEOUT": str(config_lock_timeout_seconds)}
        subprocess.run([str(resolve_front_binary() or "fno"), *argv], check=True,
                       capture_output=True, env=env, timeout=config_lock_timeout_seconds + 30)
    except Exception as exc:
        sys.stderr.write(
            f"\nfno-agents a2a: could not persist confirmation ({exc}); "
            "applying the conservative fallback (autonomous relay OFF, single "
            "observed hop).\n"
        )
        sys.stderr.flush()
        return False
    try:
        marker.parent.mkdir(parents=True, exist_ok=True)
        marker.write_text("answered\n", encoding="utf-8")
    except OSError as exc:
        sys.stderr.write(
            f"\nfno-agents a2a: could not write confirmation marker ({exc}); "
            "applying the conservative fallback (autonomous relay OFF, single "
            "observed hop).\n"
        )
        sys.stderr.flush()
        return False
    return keep_on


def _switchboard_exchange(
    to_name: str,
    from_name: str,
    body: str,
    mail_ctxs: "Optional[dict[str, _MailCtx]]" = None,
    *,
    to_identity: SwitchboardIdentity,
    from_identity: "Optional[SwitchboardIdentity]" = None,
) -> Optional[bool]:
    """Drive a stream-json switchboard exchange (Group 2, Tasks 3.1 + 4.1).

    ``mail_ctxs`` (node) maps each endpoint name to its ``<fno_mail>``
    sender context. When set (the mail-send path), every autonomous relay
    continuation is wrapped so later peer turns keep provenance, not just the
    seed. An unwrapped hop passes None, so the raw path stays unchanged.

    Returns ``True`` when the turn(s) were delivered via the switchboard, or
    ``None`` when B is not a live stream thread / the daemon is unreachable (the
    caller then demotes to the MCP/socket path).

    The FIRST hop (drive B with ``body``) is the actual ``send A->B`` delivery and
    runs synchronously so the delivered/demote decision is exact. Its read
    budget is ``_SWITCHBOARD_FIRST_HOP_READ_TIMEOUT``, which must stay above the
    daemon's own drive ceiling; see that constant for why a tighter one delivers
    the message twice. When ``config.agents.a2a.auto`` is True (the default) the
    bounded autonomous relay that follows (drive A with B's reply, then B with
    A's reply, ... up to ``config.agents.a2a.turn_ceiling`` total turns) is
    kicked off in a DETACHED background process and the caller returns
    ``True`` immediately — it no longer blocks for up to
    ``turn_ceiling × 130s``, so the relay hop keeps the full ceiling: nothing
    is waiting on it. When ``auto`` is False, a single OBSERVED hop drives B
    and mirrors B's reply into A's view, with no autonomous relay.
    """
    auto, ceiling = _load_a2a_settings()
    # US6: the first-use confirm gates the first autonomous hop.
    # On a no / headless / unconfirmed gate this downgrades to observed mode, so
    # the hop below runs as a single mirrored hop with no autonomous relay.
    auto = _a2a_first_use_gate(auto, ceiling)
    # First hop: drive B. In observed mode (auto off) ask the daemon to mirror
    # B's reply into A's view; in auto mode the relay's next hop injects it (so
    # mirror=False avoids a double-injection).
    mirror = not auto and from_identity is not None
    params: dict[str, object] = {
        "to": to_name,
        "from": from_name,
        "body": body,
        "mirror": mirror,
        "recipient_identity": to_identity,
    }
    if from_identity is not None:
        params["from_identity"] = from_identity
    sb = _daemon_rpc(
        _SWITCHBOARD_RPC_METHOD,
        params,
        connect_timeout=_SWITCHBOARD_CONNECT_TIMEOUT,
        read_timeout=_SWITCHBOARD_FIRST_HOP_READ_TIMEOUT,
    )
    if (
        sb is None
        or sb.get("delivered") is not True
        or sb.get("identity_verified") is not True
    ):
        return None  # not a live stream thread / daemon down -> caller demotes
    if not auto:
        return True  # observed: one hop, B's reply mirrored into A

    # A2A relay: kick off the remaining alternating hops in the background so the
    # caller is not blocked for the whole exchange. A self-send (from == to) or an
    # empty first reply has no relay to run.
    cur = sb.get("reply") or ""
    if (
        ceiling > 1
        and from_name != to_name
        and cur.strip()
        and from_identity is not None
    ):
        _kickoff_background_relay(
            to_name,
            from_name,
            cur,
            ceiling,
            mail_ctxs,
            recipient_identities={
                to_name: to_identity,
                from_name: from_identity,
            },
        )
    return True


# Subprocess budget for the mail-inject command. It polls the recipient transcript
# for ~10s (40 * 250ms) before reporting not-confirmed; give it headroom.
_MAIL_INJECT_TIMEOUT_S = 60.0

# Liveness-scaled confirm budget (node, change 2). The enqueue record is
# written at submit time, not at turn end, so a healthy busy recipient confirms
# in well under a second -- the default 10s budget already exists only to cover
# recipients the daemon successfully attached to, i.e. ones already proven
# alive. What it currently does wrong is convert "the CR has not landed yet
# because the recipient is deep in a long tool call" into a live-miss, handing
# the message to a queue nobody drains. When the SENDER's own liveness signal
# (`_registered_family1_state`) independently reports the recipient mid-turn,
# raise the poll budget to ~30s (120 * 250ms) so a long tool call has room to
# yield back to the prompt before we give up; any other recipient keeps the
# unscaled default. A live recipient that still has not enqueued within the
# raised budget is genuinely wedged, a different fact from "busy" (change 4
# reports it as such via the reason token). Not a fixed sleep: the poll still
# exits the instant the enqueue lands.
_MAIL_INJECT_LIVENESS_SCALED_ATTEMPTS = 120
_MAIL_INJECT_LIVENESS_SCALED_TIMEOUT_S = 40.0

# Rust mux pane exit code for a guarded send the server refused because the
# recipient pane's turn is not takeable (crates/fno/src/mux_cli.rs
# EXIT_TARGET_NOT_IDLE). The delivery ladder reads it as turn-not-taken -> a
# stalled demotion to the durable floor, never a hosted receipt (US4, LD4).
_MUX_EXIT_TARGET_NOT_IDLE = 15

# The server's PaneClaim refusal for a HELD claim names the holder pid
# (crates/fno/src/server.rs "writer claim held by pid"); every other refusal
# (not claim-eligible, dead pane) is a pane with no writer interlock, which
# keeps the fail-open path. How long a held claim is waited out before the
# send demotes to durable: a concurrent writer's burst is paste + settle
# delay + CR, low single-digit seconds at the 800 ms table value.
_MUX_CLAIM_HELD_MARKER = "held by pid"
_MUX_PANE_CLAIM_WAIT_S = 4.0
# `_run` returns this sentinel when the mux subprocess timed out after the
# request may already have reached the server. It is not a pre-submit refusal:
# retrying the same payload could duplicate a paste whose outcome is unknown.
_MUX_SEND_UNKNOWN = -2

# Re-exported: the envelope identity context moved to its own module, and
# `fno.agents.dispatch` stays the import path callers already use.
from fno.agents.mail_ctx import _MailCtx, _build_mail_ctx  # noqa: E402

# Poll budget for the mux lane's content confirm (node, change 3),
# matched to the claude control.sock lane's default (crates/fno-agents/src/
# mail_inject.rs DEFAULT_ATTEMPTS/DEFAULT_INTERVAL_MS): 40 * 250ms = 10s. Kept
# in parity so neither keystroke lane is structurally more patient than the
# other for the same "did the paste land" question.
_MUX_CONFIRM_ATTEMPTS = 40
_MUX_CONFIRM_INTERVAL_S = 0.25

_CODEX_QUEUE_MARKER = "tab to queue message"
# Substring match, so the bare word subsumes every longer spelling that
# contains it ("queued review", "review queued"); listing those adds nothing.
_CODEX_QUEUED_MARKERS = ("queued",)
# Composer read-back (codex panes): extra submit/queue keys, and the
# settle+read rounds a paste that has not fully rendered gets.
_CODEX_POST_SUBMIT_RESENDS = 2
_CODEX_ABSORB_POLLS = 12


def _pane_recipient_handle(entry: "AgentEntry") -> Optional[str]:
    """The canonical mail handle of a pane's occupant, or None.

    None is not a failure: an unregistered pane has no handle to address, and
    an envelope with no `to` is the honest rendering of that.
    """
    from fno.harness_identity import canonical_handle

    session_id = getattr(entry, "harness_session_id", None) or getattr(
        entry, "session_id", None
    )
    return canonical_handle(session_id) if session_id else None


def _mux_pane_send(
    entry: "AgentEntry",
    text: str,
    *,
    guarded: bool = True,
    sender: Optional[str] = None,
    confirm: bool = False,
    raw: bool = False,
    gate: Optional[bool] = None,
    failure_out: Optional[list[str]] = None,
    source_label: Optional[str] = None,
) -> bool | str:
    """Live-inject to a mux-hosted agent via ``fno mux pane send``.

    ``guarded`` rides the server-side turn-taken interlock (no fno caller opts
    in; the rerun command uses the command directly). ``guarded=False`` holds the
    best-effort writer claim across the text-then-CR burst; a failed send
    fails closed -> durable. ``confirm`` polls the recipient's own transcript
    for the injected content (claude only: other harnesses have no
    ~/.claude/projects transcript) and reports False on any miss, never a
    pass on an unreadable transcript. ``raw`` opts out of the
    enveloped gate for genuine keystrokes; already-wrapped mail passes the
    wrap through untouched. Narrative and node history: docs/backlog-usage
    and the PR bodies that introduced each flag.
    """
    def _record_failure(reason: str) -> None:
        if failure_out is not None:
            failure_out.append(reason)

    mux = entry.mux or {}
    session = mux.get("session")
    pane_id = mux.get("pane_id")
    if not session or pane_id is None:
        _record_failure("pre-submit")
        return False
    # A bus-only recipient never gets a pane paste, same as every other lane.
    if _delivery_policy_refusal(entry, text) == BUS_ONLY_POLICY:
        _record_failure("pre-submit")
        return False
    from fno.agents.harness_map import capabilities_or_undeclared

    harness = getattr(entry, "harness", "") or ""
    # the posture answers submit_keys=[enter] for an undeclared
    # harness, so the pane delivery arm below runs for it unchanged. Its
    # `["unsupported"]` refusal names harness_capabilities.toml, which stays
    # correct for declared harnesses and unreachable for undeclared ones.
    input_caps = capabilities_or_undeclared(harness)
    expected_name = getattr(entry, "name", None)
    expected_fno_id = (
        getattr(entry, "fno_id", None)
        or getattr(entry, "harness_session_id", None)
        or getattr(entry, "session_id", None)
    )
    submit_keys = input_caps["submit_keys"]
    if submit_keys == ["unsupported"]:
        # Name the TABLE and the KEY, not just the layer that did not run. The
        # old wording ("no pinned submit contract") plus the caller's
        # "[mux-send-failed]" receipt read as a broken transport, and two agents
        # plus an operator spent a night on a codex pane before anyone checked
        # whether something had DECLARED the lane unavailable. The transport was
        # never tried.
        print(
            f"mux pane delivery refused: harness {harness!r} declares "
            f"submit_keys = [\"unsupported\"] in "
            f"cli/src/fno/agents/harness_capabilities.toml, so the pane lane is "
            f"refused before it is tried. This is a capability declaration, not "
            f"a transport failure; the message falls back to the durable queue.",
            file=sys.stderr,
        )
        _record_failure("pre-submit")
        return False
    submit_bytes = {
        "enter": "\r", "tab": "\t", "left": "\x1b[D", "right": "\x1b[C",
        "up": "\x1b[A", "down": "\x1b[B", "esc": "\x1b",
    }
    try:
        submit_text = [submit_bytes.get(key, key) for key in submit_keys]
        enter_delay_s = input_caps["send_keys_enter_delay_ms"] / 1000
    except (KeyError, TypeError):
        _record_failure("pre-submit")
        return False
    # Envelope by default (node): every agent-to-agent pane drive carries
    # the same attribution the mail lane produces, and the read-back gate refuses
    # a pane showing an option prompt (a submit there dismisses the payload and
    # selects the highlighted default). `raw=True` is the keystroke opt-out.
    # Already-wrapped text passes the wrap through, so the mail lane's own bodies
    # are byte-unchanged and only gain the gate.
    # `prepare` wraps AND refuses a pane showing a prompt, and a caller can
    # want either without the other: default a2a mail takes both (wrap+gate),
    # a gate=True/raw=True operational payload takes the gate only (a submit
    # into a showing prompt discards it too), and a keystroke answering a
    # prompt takes neither (gating it breaks the caller that needs the prompt).
    # Collapsing the middle case into the last shipped a hold digest into an
    # ungated pane. `gate` defaults to `not raw`, so every caller keeps its
    # behavior and the middle case says `gate=True`.
    if gate is None:
        gate = not raw
    if not raw or gate:
        from fno.mail.pane_transport import PaneSendRefused, prepare

        try:
            text = prepare(
                text,
                session=str(session),
                pane_id=pane_id,
                harness=harness or None,
                sender=sender,
                # The recipient's own handle. Without it the envelope renders
                # with no `to`, and `_addressed_here` answers True for any tag
                # carrying none, which turns the receipt check for this whole
                # lane into a mention check. The entry is right here.
                to=_pane_recipient_handle(entry),
                gate=gate,
                # `wrap=False` for the gate-only case: the body is already what
                # has to land, byte for byte.
                wrap_body=not raw,
                check_identity=bool(expected_name and expected_fno_id),
                expected_name=expected_name,
                expected_fno_id=(
                    str(expected_fno_id) if expected_fno_id is not None else None
                ),
            )
        except PaneSendRefused as exc:
            print(f"mux pane {pane_id} send refused: {exc}", file=sys.stderr)
            _record_failure("pre-submit")
            return False

    # Audit floor: the row this pane write leaves is written by the
    # Rust command itself (`fno mux pane send`), one layer below, so it covers a
    # direct caller too. This lane only DECLARES its provenance (--source) so
    # the floor's row joins the bus record; it must not write a second row of
    # its own, or every pane dispatch reads as two.

    fno_bin = os.environ.get("FNO_BIN") or "fno"
    pane = str(pane_id)

    def _run(args: list[str], stdin_text: Optional[str] = None):
        """Run one ``fno mux pane`` command; return its CompletedProcess, or None
        on spawn failure. A non-zero code's stderr detail is surfaced, never
        swallowed. The process (not a bare code) is the return so the claim
        gate can read the refusal reason without a second wrapper."""
        try:
            proc = subprocess.run(
                [fno_bin, "mux", "pane", *args, "--server", str(session)],
                input=stdin_text,
                capture_output=True,
                text=True,
                timeout=_MAIL_INJECT_TIMEOUT_S,
            )
        except subprocess.TimeoutExpired as exc:
            print(f"fno mux pane {args[0]} failed: {exc}", file=sys.stderr)
            return _MUX_SEND_UNKNOWN
        except OSError as exc:
            print(f"fno mux pane {args[0]} failed: {exc}", file=sys.stderr)
            return None
        if proc.returncode != 0:
            detail = (proc.stderr or "").strip()
            print(
                f"fno mux pane {args[0]} exited {proc.returncode}: {detail}",
                file=sys.stderr,
            )
        return proc

    def _verify_pane_occupant() -> bool:
        """Prove the pane still hosts the row selected by the resolver."""
        expected_name = getattr(entry, "name", None)
        if not expected_name:
            return True
        expected_fno_id = (
            getattr(entry, "fno_id", None)
            or getattr(entry, "harness_session_id", None)
            or getattr(entry, "session_id", None)
        )
        if not expected_fno_id:
            return True
        try:
            proc = subprocess.run(
                [
                    fno_bin,
                    "mux",
                    "pane",
                    "ls",
                    "--json",
                    "--server",
                    str(session),
                ],
                capture_output=True,
                text=True,
                timeout=_MAIL_INJECT_TIMEOUT_S,
            )
        except (OSError, subprocess.TimeoutExpired) as exc:
            print(f"mux pane {pane} identity check failed: {exc}", file=sys.stderr)
            _record_failure("pre-submit")
            return False
        if proc.returncode != 0:
            detail = (proc.stderr or "").strip() or "pane ls returned non-zero"
            print(f"mux pane {pane} identity check refused: {detail}", file=sys.stderr)
            _record_failure("pre-submit")
            return False
        try:
            rows = json.loads(proc.stdout)
        except (TypeError, ValueError) as exc:
            print(f"mux pane {pane} identity check refused: invalid pane ls JSON: {exc}", file=sys.stderr)
            _record_failure("pre-submit")
            return False
        if not isinstance(rows, list):
            print("mux pane identity check refused: pane ls JSON was not a list", file=sys.stderr)
            _record_failure("pre-submit")
            return False
        matches = [row for row in rows if isinstance(row, dict) and row.get("pane_id") == pane_id]
        if len(matches) != 1:
            print(
                f"mux pane {pane} identity check refused: expected one pane row, found {len(matches)}",
                file=sys.stderr,
            )
            _record_failure("pre-submit")
            return False
        row = matches[0]
        actual_fno_id = row.get("fno_id")
        if expected_fno_id is not None and actual_fno_id != expected_fno_id:
            print(
                f"mux pane {pane} identity mismatch: addressed {expected_name} "
                f"({expected_fno_id or '-'}) pane hosts {row.get('name') or '<unknown>'} "
                f"({actual_fno_id or '-'})",
                file=sys.stderr,
            )
            _record_failure("pre-submit")
            return False
        return True

    last_attempt_phase = "pre-submit"

    def _paste_then_submit() -> bool:
        nonlocal last_attempt_phase
        # PaneSend is bytes; --raw because the gate and wrap above already ran
        # (a second pass would re-decide a question this one answered).
        send_args = ["send", pane, "--stdin", "--raw"]
        expected_fno_id = (
            getattr(entry, "fno_id", None)
            or getattr(entry, "harness_session_id", None)
            or getattr(entry, "session_id", None)
        )
        if expected_fno_id:
            send_args.extend(["--fno-id", str(expected_fno_id)])
        if source_label:
            # The floor's audit row joins this dispatch to its bus record by
            # the mail id; declared, never sniffed from the wrapped body.
            send_args.extend(["--source", source_label])
        if guarded:
            send_args.append("--guarded")
        pasted = _run(send_args, stdin_text=text)
        if pasted == _MUX_SEND_UNKNOWN:
            rc = _MUX_SEND_UNKNOWN
        else:
            rc = pasted.returncode if pasted is not None else -1
        if rc != 0:
            if rc == _MUX_SEND_UNKNOWN:
                last_attempt_phase = "unconfirmed"
            if rc == _MUX_EXIT_TARGET_NOT_IDLE:
                # Turn not taken: the recipient is mid-turn, so the paste never
                # landed. Name the stall; the caller demotes to the durable floor.
                print(
                    f"mux pane {pane} stalled: recipient turn not taken; demoting to durable",
                    file=sys.stderr,
                )
            return False
        last_attempt_phase = "submitted"
        # The CR is unguarded: the guarded paste already proved the pane idle, and
        # guarding the submit could strand a pasted-but-unsent prompt.
        time.sleep(enter_delay_s)
        for key in submit_text:
            proc = _send_pane_key(key)
            if proc == _MUX_SEND_UNKNOWN:
                last_attempt_phase = "unconfirmed"
                return False
            if proc is None or proc.returncode != 0:
                return False
        return True

    def _mail_inject_pane_lane(binary) -> bool:
        recipient_id = (
            getattr(entry, "harness_session_id", None)
            or getattr(entry, "session_id", None)
            or session
        )
        argv = [
            str(binary),
            "mail-inject",
            "--session",
            str(recipient_id),
            "--harness",
            "pane",
            "--pane",
            f"{session}:{pane_id}",
            "--harness-row",
            (getattr(entry, "harness", "") or "") or "claude",
            "--enter-delay-ms",
            str(int(enter_delay_s * 1000)),
        ]
        return _run_mail_inject(argv, text, _MAIL_INJECT_TIMEOUT_S, _record_failure)

    def _read_screen() -> Optional[str]:
        proc = _run(["read", pane])
        if (
            proc is None
            or proc == _MUX_SEND_UNKNOWN
            or proc.returncode != 0
        ):
            return None
        return proc.stdout or ""

    def _send_pane_key(key: str):
        """One raw control key at the pane; the caller classifies the command's
        process. A control key is a byte, never a message: always --raw."""
        return _run(
            [
                "send",
                pane,
                "--text",
                key,
                "--raw",
                *(["--fno-id", str(expected_fno_id)] if expected_fno_id else []),
            ]
        )

    def _contains_payload(screen: str) -> bool:
        # Codex may wrap a long slash request across visual lines. Compare the
        # positive content after collapsing whitespace, never on screen growth.
        compact = re.sub(r"\s+", " ", screen).strip()
        expected = re.sub(r"\s+", " ", text).strip()
        return bool(expected) and expected in compact

    def _payload_signature() -> str:
        # One distinctive token of the payload, used to bind status markers to
        # the composer region that echoed THIS request. The same words anywhere
        # else in scrollback (a "queued" from an earlier turn) must not
        # classify this frame. Empty for payloads too short to be distinctive,
        # which fails the caller closed (unconfirmed).
        words = re.sub(r"\s+", " ", text).strip().lower().split(" ")
        longest = max(words, key=len) if words else ""
        return longest if len(longest) >= 4 else ""

    def _marker_near_payload(screen: str, marker: str, window: int = 2) -> bool:
        # The marker counts only within a few lines of a line carrying the
        # payload signature: the composer and its status render adjacent, while
        # scrollback can carry the bare word arbitrarily far away.
        sig = _payload_signature()
        if not sig:
            return False
        lines = [
            re.sub(r"\s+", " ", ln).strip().lower() for ln in screen.splitlines()
        ]
        for i, ln in enumerate(lines):
            if marker not in ln:
                continue
            near = lines[max(0, i - window) : i + window + 1]
            if any(sig in other for other in near):
                return True
        return False

    def _codex_post_submit_outcome() -> bool | str:
        # Composer read-back after the submit key: the CR fires after one
        # settle, so a slow paste absorbs it as a newline and a mid-turn pane
        # takes Enter as steer only; either way the envelope stays resident.
        # The queue affordance beside the payload names the composer as still
        # holding it (Tab moves it); not-yet-rendered polls then one late key;
        # an affordance-less frame gets one deciding key (no-op when landed);
        # an unreadable frame keeps the bytes-written verdict, and anything
        # else resident ends unconfirmed -- never delivered over a composer
        # that may still hold the envelope.
        resends = polls = 0
        seen_payload = hedged = False
        while True:
            time.sleep(enter_delay_s)
            screen = _read_screen()
            polls += 1
            if screen is None:
                return True
            if _contains_payload(screen):
                seen_payload = True
                if any(_marker_near_payload(screen, m) for m in _CODEX_QUEUED_MARKERS):
                    return "queued"
                if _marker_near_payload(screen, _CODEX_QUEUE_MARKER):
                    key = "\t"
                elif not hedged:
                    hedged, key = True, submit_text[0]
                else:
                    return True
            elif seen_payload:
                return True
            elif polls >= _CODEX_ABSORB_POLLS:
                key = submit_text[0]
            else:
                continue
            if resends >= _CODEX_POST_SUBMIT_RESENDS:
                _record_failure("post-submit-unconfirmed")
                return "unconfirmed"
            resends += 1
            proc = _send_pane_key(key)
            if proc is None or proc == _MUX_SEND_UNKNOWN or proc.returncode != 0:
                _record_failure("post-submit-unconfirmed")
                return "unconfirmed"

    if guarded:
        sent = _paste_then_submit()
        if not sent:
            _record_failure(last_attempt_phase)
        return sent

    if not _verify_pane_occupant():
        return False

    # Baseline BEFORE the paste: the confirm scans only lines appended past it.
    # Transcript confirm is claude-only (the resolver reads ~/.claude/projects);
    # other panes keep the bytes-written verdict, whose miss would demote a
    # landed paste to durable and make the recipient drain it twice.
    confirm = confirm and (getattr(entry, "harness", "") or "") == "claude"
    confirm_transcript = _mux_recipient_transcript(entry) if confirm else None
    confirm_baseline: Optional[int] = None
    if confirm_transcript is not None:
        try:
            confirm_baseline = confirm_transcript.stat().st_size
        except OSError:
            confirm_transcript = None

    def _claim_writer() -> tuple[bool, str]:
        """Acquire the pane writer claim; return (ok, refusal detail). The
        detail names the held-claim marker (another writer mid-burst) or the
        server's own refusal reason."""
        proc = _run(["claim", pane, "--pid", str(os.getpid())])
        if proc == _MUX_SEND_UNKNOWN:
            # The claim may have reached the server or another writer may own
            # it; PaneRelease is pane-wide, so confirm with this PID before
            # releasing rather than clear a stranger's claim.
            confirmed = _run(["claim", pane, "--pid", str(os.getpid())])
            if (
                confirmed not in (None, _MUX_SEND_UNKNOWN)
                and confirmed.returncode == 0
            ):
                _run(["release", pane])
            return False, "claim result unknown"
        if proc is None:
            return False, ""
        return proc.returncode == 0, (proc.stderr or "").strip()

    claimed, claim_detail = _claim_writer()
    if not claimed and claim_detail == "claim result unknown":
        print(
            f"mux pane {pane} send demoted to durable: writer claim outcome unknown; "
            "refusing to type",
            file=sys.stderr,
        )
        _record_failure("pre-submit")
        return False
    if not claimed and _MUX_CLAIM_HELD_MARKER in claim_detail:
        # A HELD claim is another writer mid-burst: pasting now interleaves two
        # envelopes under one CR. Wait it out, else demote. Any other refusal
        # falls through to the fail-open send: a pane without the interlock
        # never had serialization to lose.
        deadline = time.monotonic() + _MUX_PANE_CLAIM_WAIT_S
        while (
            not claimed
            and _MUX_CLAIM_HELD_MARKER in claim_detail
            and time.monotonic() < deadline
        ):
            time.sleep(0.25)
            claimed, claim_detail = _claim_writer()
        if not claimed and _MUX_CLAIM_HELD_MARKER in claim_detail:
            print(
                f"mux pane {pane} send demoted to durable: writer claim held "
                f"by another writer past {_MUX_PANE_CLAIM_WAIT_S}s; refusing "
                f"to interleave envelopes",
                file=sys.stderr,
            )
            return False
    try:
        if confirm and not raw:
            # Rust typed lane when a binary resolves; without one the Python
            # lane below carries the same confirm contract.
            from fno import rust_binary as _rb
            binary = _rb.resolve_installed_binary()
            if binary is not None:
                sent = _mail_inject_pane_lane(binary)
                if not sent:
                    _record_failure("pane-lane-not-confirmed")
                return sent
        sent = _paste_then_submit()
        outcome: bool | str = sent
        if sent and (getattr(entry, "harness", "") or "") == "codex":
            # No transcript to confirm against, so the composer is the confirm.
            outcome = _codex_post_submit_outcome()
        elif sent and confirm:
            # Bytes-written alone is Locked-Decision-4 banned as a hosted
            # verdict; confirm by content against the recipient's own
            # transcript, never optimistically on an unreadable one.
            # Strip first: a leading newline would make the first line empty,
            # and an empty marker never confirms (a landed paste read as a miss).
            marker = text.strip().split("\n", 1)[0]
            sent = confirm_transcript is not None and confirm_baseline is not None and (
                _mux_content_confirm(confirm_transcript, marker, confirm_baseline)
            )
            outcome = sent
            if not sent:
                _record_failure("unconfirmed")
        elif not sent:
            _record_failure(last_attempt_phase)
        return outcome
    finally:
        if claimed:
            _run(["release", pane])


#: The one delivery-policy value that forbids prompt-line injection.
#: A DELIVERY-POLICY fact, never a liveness verdict: a bus-only session may be
#: alive and mid-turn, it just belongs on the durable bus. Kept as a literal
#: (compared everywhere) because the registry field is an open
#: ``Optional[str]``; a second value would graduate to an enum then.
BUS_ONLY_POLICY = "bus-only"


def _mux_recipient_transcript(entry: "AgentEntry") -> Optional[Path]:
    """Locate the mux recipient's OWN claude transcript by session uuid, the
    confirm target for :func:`_mux_pane_send`'s ``confirm`` mode (node).

    Reads through :func:`fno.rust_binary.find_transcript`, the native doctor
    op door (`fno doctor --find-transcript`), the same resolver the Rust
    control.sock lane mirrors as `find_transcript` in claude_drive.rs. None
    when the entry carries no resolvable full session uuid, or no matching
    transcript file exists -- both fail the confirm closed, never open.
    """
    from fno.rust_binary import find_transcript

    session_id = entry.harness_session_id or entry.session_id
    if not session_id:
        return None
    return find_transcript(session_id)


def _mux_content_confirm(
    transcript: Path,
    marker: str,
    since_byte: int,
    *,
    attempts: int = _MUX_CONFIRM_ATTEMPTS,
    interval_s: float = _MUX_CONFIRM_INTERVAL_S,
) -> bool:
    """Poll ``transcript`` for ``marker`` in lines appended after ``since_byte``
    (node, change 3): content, not growth, mirroring the claude
    control.sock lane's ``confirm_content_after``/``escaped_marker`` pair
    (``crates/fno-agents/src/mail_inject.rs``) so both keystroke lanes confirm
    delivery the same way. ``marker`` is escaped the same way ``json.dumps``
    would embed it in a JSON string field, since a claude transcript line
    stores the turn's text JSON-encoded -- matching Rust's
    ``serde_json::to_string`` + quote-strip. An empty escaped marker (an empty
    ``text``) never confirms; a submitted turn is recorded verbatim, an unsent
    paste records nothing, so a busy recipient's unrelated transcript growth
    never carries our marker by accident.
    """
    import json

    # ensure_ascii=False to match Rust's `serde_json::to_string`, which leaves
    # non-ASCII literal. With the default the marker's accented or emoji
    # characters become \uXXXX escapes that a claude transcript never carries,
    # and the confirm could never match.
    escaped = json.dumps(marker, ensure_ascii=False)[1:-1]
    if not escaped:
        return False
    needle = escaped.encode("utf-8")
    for _ in range(max(attempts, 1)):
        try:
            with transcript.open("rb") as fh:
                fh.seek(since_byte)
                for raw_line in fh:
                    if needle in raw_line:
                        return True
        except OSError:
            pass
        time.sleep(interval_s)
    return False


def _delivery_policy_refusal(
    target, body: Optional[str] = None, park: bool = False
) -> Optional[str]:
    """:data:`BUS_ONLY_POLICY` when the Rust hold gate holds mail to ``target``;
    ``None`` otherwise. One-call port of the gate body to ``mail_hold.rs``
    ``gate``: the gate owns row resolution through ``hold.addresses``, the clock
    sweep, the own-send/``control:`` passes, and the receipt; a failed gate on a
    stamped row fails closed, ``park`` returns the park receipt, never raises."""
    if target is None:
        return None
    if hasattr(target, "delivery_policy"):
        if getattr(target, "delivery_policy", None) != BUS_ONLY_POLICY:
            return None
        token = getattr(target, "harness_session_id", None) or getattr(target, "name") or ""
    else:
        token = target
        from fno.mail.hold import addresses as hold_addresses
        try:
            stamped = any(
                getattr(entry, "delivery_policy", None) == BUS_ONLY_POLICY
                and target in hold_addresses(entry)
                for entry in load_registry()
            )
        except Exception:  # noqa: BLE001 - a registry read failure never blocks delivery
            return None
        if not stamped:
            return None
    from fno import rust_binary

    binary = rust_binary.resolve_installed_binary()
    if binary is None:
        from fno.mail import hold as hold_mod
        return hold_mod.gate_answer_in_process(token)
    try:
        argv = [str(binary), "mail-hold", "--gate"]
        if park:
            argv.append("--park-on-hold")
        argv += ["--session", token]
        proc = subprocess.run(argv, input=body or "", capture_output=True, text=True, timeout=10)
        out = json.loads(proc.stdout)
        verdict = out.get("verdict")
        if verdict == "deliver":
            return None
        if verdict == "parked":
            return out.get("receipt") or BUS_ONLY_POLICY
        return BUS_ONLY_POLICY
    except Exception:  # noqa: BLE001 - fail closed: never lift a hold we could not read
        return BUS_ONLY_POLICY


def _run_mail_inject(argv: list[str], text: str, timeout: float, _record) -> bool:
    """Run one ``mail-inject`` probe and classify its stdout verdict."""
    started = time.monotonic()
    try:
        proc = subprocess.run(
            argv,
            input=text,
            stdout=subprocess.PIPE,  # stderr inherits: the crate's self-teaching text (refusal help, route hint) reaches the sender
            text=True,
            timeout=timeout,
        )
    except (OSError, subprocess.SubprocessError):
        _record("probe-unavailable")
        return False
    try:
        out = json.loads(proc.stdout.strip())
        delivered = bool(out.get("delivered"))
        _record(str(out.get("reason") or "unknown"))
        if not delivered:
            _record(f"waited-{round(time.monotonic() - started)}s")
        return delivered
    except (ValueError, AttributeError):
        _record("unreadable")
        return False


def _mail_inject_keeper(
    recipient: str,
    text: str,
    *,
    harness: str,
    sender: Optional[str] = None,
    reason_out: Optional[list] = None,
) -> bool:
    """Inject ``text`` into a keeper-hosted lane-B thread via the
    ``fno-agents mail-inject --harness <hosted>`` command.

    ``harness`` is the hosted harness from the recipient's row. It names the
    lane (the Rust side routes a keeper-lane harness to the keeper socket and
    refuses every other name) and the settle-delay table row: the command
    defaults ``--enter-delay-ms`` from that row itself, so this side passes no
    delay and the one fact stays single-sourced. The command resolves the
    recipient's registry row, pastes the envelope as one keeper ``Input``
    frame plus the wire-level CR, and confirms by content in the hosted
    harness's own transcript store. Returns True only on that confirmed
    landing; any miss returns False with the command's reason token in
    ``reason_out`` so the durable demotion names why.

    Unlike ``_mail_inject_claude`` there is no claude fallback for an unknown
    harness: a fallback name would route to lane A's socket, so an unreadable
    row is a refusal, never a wrong-lane guess."""
    from fno import rust_binary

    def _record(reason: str) -> None:
        if reason_out is not None:
            reason_out.append(reason)

    # A bus-only recipient gets no prompt-line paste on any transport.
    if _delivery_policy_refusal(recipient, text) == BUS_ONLY_POLICY:
        _record(BUS_ONLY_POLICY)
        return False

    binary = rust_binary.resolve_installed_binary()
    if binary is None:
        _record("no-binary")
        return False
    argv = [str(binary), "mail-inject", "--session", recipient, "--harness", harness]
    if sender:
        argv += ["--sender", sender]
    return _run_mail_inject(argv, text, _MAIL_INJECT_TIMEOUT_S, _record)


def _mail_inject_claude(
    recipient: str,
    text: str,
    *,
    sender: Optional[str] = None,
    reason_out: Optional[list] = None,
    liveness_scaled: bool = False,
    harness: Optional[str] = None,
    origin: Optional[str] = None,
    self_send: bool = False,
) -> bool:
    """Inject ``text`` into a live claude session over the daemon
    ``control.sock`` via the ``fno-agents mail-inject`` command. Returns True
    only when the command confirms the turn landed in the recipient transcript;
    any miss (binary absent, recipient not on the roster, not confirmed
    within the poll budget) returns False so the caller writes the durable
    fallback. ``reason_out``, when a non-empty list, receives the command's own
    reason token as a side-channel rather than a second return value, so the
    many callers and test mocks that read this as a plain bool are
    unaffected. ``sender`` is the invoking session's mail handle for the
    binary's audit event; only UNWRAPPED lanes need it, since a wrapped
    envelope carries its own ``from`` in the transcript.
    ``liveness_scaled`` passes the raised confirm budget
    (``_MAIL_INJECT_LIVENESS_SCALED_ATTEMPTS``) when the caller's OWN
    liveness signal already reports the recipient mid-turn, so a long tool
    call gets room to yield back before the confirm gives up. ``harness``,
    when the caller holds the recipient's roster row, names the settle-delay
    table row directly; ``None`` resolves it from the roster by session id,
    and a miss falls back to claude, the table's largest delay, so an
    unresolved read waits longer, never less."""
    from fno import rust_binary

    def _record(reason: str) -> None:
        if reason_out is not None:
            reason_out.append(reason)

    # A bus-only recipient gets no prompt-line paste on any lane.
    if _delivery_policy_refusal(recipient, text) == BUS_ONLY_POLICY:
        _record(BUS_ONLY_POLICY)
        return False

    binary = rust_binary.resolve_installed_binary()
    if binary is None:
        _record("no-binary")
        return False
    from fno.agents.harness_map import capabilities

    # The settle delay belongs to the RECIPIENT's harness row, not to a claude
    # constant: the caller's row wins when held (correct for short-id/mcp
    # recipients too), else the roster. A registry error is an unresolved read,
    # never a raised mail path. The constant keeps the fallback out of the
    # `capabilities("claude")` literal the shared-contract sentinel greps for.
    _FALLBACK_DELAY_HARNESS = "claude"
    recipient_harness = harness or _FALLBACK_DELAY_HARNESS
    if harness is None:
        try:
            entry = _roster_entry_for_session(recipient)
        except (OSError, RegistryVersionError):
            entry = None
        if entry is not None:
            recipient_harness = (
                getattr(entry, "harness", "") or _FALLBACK_DELAY_HARNESS
            )
    try:
        enter_delay_ms = capabilities(recipient_harness)["send_keys_enter_delay_ms"]
    except DispatchResolveError:
        enter_delay_ms = capabilities(_FALLBACK_DELAY_HARNESS)["send_keys_enter_delay_ms"]
    argv = [
        str(binary), "mail-inject", "--session", recipient,
        "--enter-delay-ms", str(enter_delay_ms),
    ]
    if harness and harness != "claude":
        argv += ["--harness", harness]
    if sender:
        argv += ["--sender", sender]
    if origin:
        argv += ["--origin", origin]
    if self_send:
        argv.append("--self-send")
    timeout = _MAIL_INJECT_TIMEOUT_S
    if liveness_scaled:
        argv += ["--attempts", str(_MAIL_INJECT_LIVENESS_SCALED_ATTEMPTS)]
        timeout = _MAIL_INJECT_LIVENESS_SCALED_TIMEOUT_S
    return _run_mail_inject(argv, text, timeout, _record)


def _roster_entry_for_session(session_uuid: str) -> Optional["AgentEntry"]:
    """The registry row whose ``harness_session_id`` is ``session_uuid``, or None.

    Best-effort: an unreadable/missing registry returns None, so the settle
    delay falls back to the claude row rather than blocking mail.
    """
    try:
        loaded = load_registry()
        if getattr(loaded, "complete", True) is not True:
            raise RegistryVersionError(
                "registry forward read is incomplete; routed wake cannot be classified"
            )
        return next(
            (e for e in loaded if getattr(e, "harness_session_id", None) == session_uuid),
            None,
        )
    except OSError:
        return None


def _mail_inject_codex(
    thread_id: str,
    text: str,
    *,
    reason_out: Optional[list] = None,
    origin: Optional[str] = None,
) -> bool:
    """Inject ``text`` into a live codex session over the app-server daemon socket
    via the ``fno-agents mail-inject --harness codex`` command (US8, node).

    ``reason_out``, when a non-empty list, receives the live lane's
    cause on a miss -- the same side-channel contract as
    :func:`_mail_inject_claude`, so a bus-only refusal names itself in the
    caller's receipt instead of reading as a generic live-miss."""
    import json

    from fno import rust_binary

    if _delivery_policy_refusal(thread_id, text) == BUS_ONLY_POLICY:
        if reason_out is not None:
            reason_out.append(BUS_ONLY_POLICY)
        return False

    binary = rust_binary.resolve_installed_binary()
    if binary is None:
        return False
    try:
        argv = [str(binary), "mail-inject", "--harness", "codex", "--session", thread_id]
        if origin:
            argv.extend(("--origin", origin))
        proc = subprocess.run(
            argv,
            input=text,
            capture_output=True,
            text=True,
            timeout=_MAIL_INJECT_TIMEOUT_S,
        )
    except (OSError, subprocess.SubprocessError):
        return False
    try:
        receipt = json.loads(proc.stdout.strip())
        if reason_out is not None and receipt.get("reason"):
            reason_out.append(str(receipt["reason"]))
        return bool(receipt.get("delivered"))
    except (ValueError, AttributeError):
        return False


def _deliver_live(
    entry: "AgentEntry",
    body: str,
    from_name: str,
    mail: "Optional[_MailCtx]" = None,
    sender_entry: "Optional[AgentEntry]" = None,
    reason_out: "Optional[list]" = None,
    family1_state: Optional[str] = None,
) -> bool:
    """Attempt a single fire-and-forget live delivery (live-inject-first; the
    caller writes the durable fallback when this returns False -- node).

    ``reason_out`` (node), when a non-empty list, receives the live
    lane's own cause (the claude control.sock vocabulary from
    :func:`_mail_inject_claude`, the codex daemon RPC reason, or a mux token) so
    a durable demotion receipt can name WHY the live lane missed instead of a
    generic live-miss. A side-channel, not a second return value, so callers
    and test mocks that read this as a plain bool are unaffected.

    ``family1_state`` is the caller's already-computed registered-state class
    for ``entry``, passed in so the recipient transcript is not re-read;
    ``"working"`` scales the claude confirm budget.

    When ``mail`` is set the body is wrapped in the paired ``<fno_mail>`` envelope
    so the recipient sees agent-to-agent structure and the delivered turn is
    self-recording (``grep <fno_mail>`` reconstructs a2a history). Every live
    transport below carries the same wrapped turn, ``agy`` mux entries included.

    For claude peers the ``control.sock`` inject via the ``fno-agents
    mail-inject`` command (G1) is the live primitive (the switchboard / MCP fast
    lanes still apply first for stream-json / MCP-routed peers); for
    codex/gemini peers the daemon ``agent.deliver`` RPC carries the envelope,
    and its failure demotes to durable with a stderr notice.
    """
    wrapped = body
    if mail is not None:
        from fno.mail.envelope import wrap_fno_mail

        wrapped = wrap_fno_mail(
            body,
            from_=mail.from_,
            harness=mail.harness,
            node=mail.node,
            to=mail.to,
            id=mail.id,
            from_session=mail.from_session,
            origin=mail.origin, to_session=mail.to_session, subject=mail.subject,
            from_name=sender_entry.name if sender_entry is not None else None,
        )

    # Dual-run dispatch on the row's live ref (4a-G2): a mux-hosted agent gets
    # PaneSend; worker/bg rows keep the legacy lanes below until G4.
    def _record(reason: str) -> None:
        if reason_out is not None:
            reason_out.append(reason)

    # Bus-only bounds EVERY live transport below, the switchboard and
    # daemon-RPC lanes included.
    if _delivery_policy_refusal(entry, wrapped) == BUS_ONLY_POLICY:
        _record(BUS_ONLY_POLICY)
        return False

    if entry.mux:
        # `mail is None` carries operational text or a preframed hold digest.
        #
        # `sender` is passed for the wrapped case too. It costs nothing there
        # (the body is already enveloped, so `prepare` passes it through) and it
        # stops this call site from depending on that staying true.
        for attempt in (1, 2):
            attempt_failure: list[str] = []
            mux_delivered = _mux_pane_send(
                entry,
                wrapped,
                guarded=False,
                confirm=True,
                raw=mail is None,
                # Raw payloads need this gate; a visible prompt can discard them
                # after a successful write.
                gate=True,
                sender=from_name or None,
                failure_out=attempt_failure,
                source_label=(f"mail:{mail.id}" if mail is not None else None),
            )
            # Name the widened failure values, never truthiness: an
            # "unconfirmed" composer frame is not a delivery.
            if mux_delivered not in (False, "unconfirmed"):
                return True
            failure_reason = attempt_failure[0] if attempt_failure else "unknown"
            _record(f"mux-send-failed-attempt-{attempt}:{failure_reason}")
            # A pane ref is still a live route only while the selected registry
            # row remains non-terminal. One retry covers the measured mid-turn
            # inject miss; no loop can turn a stale ref into a storm.
            if (
                attempt == 2
                or failure_reason != "pre-submit"
                or getattr(entry, "exited", False)
                or not entry.mux
            ):
                break
        # Heal after the pane miss: a codex row whose pane is gone
        # but whose thread is loaded in the app-server rebinds to the thread
        # lane, and the wrapped body rides turn/start instead of the durable
        # queue. Every other verdict keeps the durable fallback.
        sid = getattr(entry, "harness_session_id", None)
        if getattr(entry, "harness", None) == "codex" and sid:
            heal_verdict, _heal_reason, _pane = _lane_heal(sid)
            if heal_verdict == "rebound-thread":
                from fno.agents.registry import resolve_agent

                entry = resolve_agent(entry.name).entry
                return _mail_inject_codex(
                    entry.harness_session_id or sid,
                    wrapped,
                    reason_out=reason_out,
                    origin=(mail.origin if mail else None),
                )
        return False

    # Codex hosted thread: the daemon's thread actor drives the turn.
    # A thread row (harness codex, interactive, no short_id, no mux ref) has
    # neither a pane for _mux_pane_send above nor a live route through
    # agent.deliver below - the Rust daemon does not implement deliver for a
    # thread, so the old fallthrough silently demoted every live mail send to
    # the durable queue. The switchboard arm answers delivered on turn
    # ACCEPTANCE (start/steer ack), never a whole-turn wait.
    if _is_codex_thread_entry(entry):
        delivered = _switchboard_exchange(
            entry.name,
            from_name,
            wrapped,
            None,
            to_identity=_switchboard_identity(entry),
            from_identity=None,
        )
        if delivered:
            return True
        _record("codex-thread-switchboard-miss")
        return False

    # Keeper-hosted lane-B thread: the row has neither a pane above
    # nor a lane-A socket below - its keeper unix socket IS the live
    # transport. The same mail-inject command drives it with --harness naming the
    # hosted harness (which owns the settle delay and the confirm store); the
    # command resolves the row by harness session id, pastes the envelope as one
    # Input frame plus the wire-level CR, and confirms by content in the
    # hosted harness's transcript store. Any miss demotes with the command's own
    # reason token (no-keeper-listener / not-confirmed / not-injectable), so
    # the durable receipt names the cause instead of a bare live-miss.
    from fno.agents.harness_map import thread_lane_or_none

    if (
        entry.harness_session_id
        and entry.messaging_socket_path
        and "mux/threads/" in entry.messaging_socket_path
        and thread_lane_or_none(entry.harness) == "keeper"
    ):
        return _mail_inject_keeper(
            entry.harness_session_id,
            wrapped,
            harness=entry.harness,
            reason_out=reason_out,
        )

    # Route key is the canonical harness; opencode uses the shared attach writer.
    route_harness = entry.harness
    if route_harness not in ("claude", "opencode"):
        # Route codex/gemini through the daemon deliver RPC (now <fno_mail>-wrapped).
        result = _daemon_rpc(
            "agent.deliver",
            {
                "name": entry.name,
                "body": wrapped,
                "from_name": from_name,
            },
        )
        if result is None:
            # _daemon_rpc already printed to stderr.
            return False
        if result.get("delivered") is True:
            return True
        # delivered=false: print the demotion reason to stderr.
        reason = str(result.get("reason") or "unknown")
        print(
            f"fno-agents deliver demoted: {reason}; message queued durable",
            file=sys.stderr,
        )
        _record(reason)
        return False

    # Group 2 (Task 3.1): both-endpoints-live switchboard fast lane. When B is a
    # held stream-json thread the daemon drives a turn against it and (the A2A
    # default, Task 4.1 gates it by config) mirrors B's reply back into A. The
    # daemon is authoritative: it probes B's worker socket, so a claude peer that
    # is NOT a live stream thread returns delivered=false / "not-a-live-stream-
    # thread" and we fall through to the MCP/socket path below. This is purely
    # additive — today's behavior is unchanged whenever the lane does not apply
    # (demote, daemon-unreachable=None, or any non-delivered result).
    #
    # The exchange (single observed hop, or the bounded A2A relay when
    # config.agents.a2a.auto is on) is in _switchboard_exchange. It returns True
    # when delivered via the switchboard, or None to demote to the MCP/socket
    # path below (B not a live stream thread, or daemon unreachable).
    # node: provenance for the autonomous relay continuations. The sender's
    # ctx wraps A's turns; the recipient's ctx (from/to swapped) wraps B's. None
    # when there is no mail envelope, leaving the relay raw (an unwrapped hop
    # never reaches _deliver_live, unaffected).
    relay_ctxs = None
    if mail is not None:
        relay_ctxs = {from_name: mail}
        # Only wrap the recipient's relay turns when it has a resolvable short id;
        # otherwise leave that side raw rather than emit <fno_mail from=""> (codex
        # peer P2). mail.to is the recipient short resolved in dispatch_send.
        if mail.to:
            # The recipient ctx wraps B's replies, injected into A, so the
            # reader rank reads A's session (mail.from_session).
            relay_ctxs[entry.name] = _MailCtx(
                from_=mail.to,
                harness=entry.harness,
                model="unknown",
                to=mail.from_,
                from_session=entry.harness_session_id or None,
                origin="peer",
                to_session=mail.from_session,
            )
    if _switchboard_exchange(
        entry.name,
        from_name,
        wrapped,
        relay_ctxs,
        to_identity=_switchboard_identity(entry),
        from_identity=(
            _switchboard_identity(sender_entry) if sender_entry is not None else None
        ),
    ):
        return True

    # Live inject over control.sock: the SOLE claude live lane since the
    # PTY worker.sock and MCP-channel lanes retired. The mail-inject command
    # resolves the handle via ClaudeRoster (uuid or 8-hex short id) and
    # confirms transcript growth before reporting delivered; mcp_channel_id
    # (minted 1:1 from short_id) stays the fallback for a cleared short_id.
    recipient = entry.harness_session_id or entry.short_id or entry.mcp_channel_id
    if not recipient:
        _record("no-recipient")
        return False
    return _mail_inject_claude(
        recipient,
        wrapped,
        reason_out=reason_out,
        liveness_scaled=family1_state == "working",
        # the row is in hand; pass its harness so the settle delay
        # reads the recipient's table row without a registry re-read (and
        # stays correct when `recipient` is a short_id or mcp_channel_id).
        harness=getattr(entry, "harness", None) or None,
    )


def _registered_family1_state(entry: "AgentEntry") -> str:
    """Classify a registered recipient without trusting its stored lifecycle."""
    from types import SimpleNamespace

    from fno.agents.session_truth import resolve_session_truth

    session_id = (
        entry.harness_session_id
        or entry.cc_session_id
        or entry.session_id
        or entry.short_id
    )
    known = SimpleNamespace(
        agent=entry.harness,
        session_id=session_id,
        cwd=entry.cwd,
    )
    result = resolve_session_truth(
        entry.name, resolve=lambda _handle: (known, [])
    )
    return str(result.get("state") or "unknown")


def _queue_durable_fallback(
    entry: "AgentEntry",
    message: str,
    from_name: str,
    entries: "list[AgentEntry]",
    *,
    msg_id: Optional[str] = None,
    reason: Optional[str] = None,
    mail_ctx: "Optional[_MailCtx]" = None,
    owner: Optional[str] = None,
    origin: Optional[str] = None,
    subject: Optional[str] = None,
) -> "tuple[str, str]":
    """Write the <fno_mail> envelope to the durable bus.

    Returns ``(msg_id, durable_recipient)``. The recipient rides back so a
    caller's receipt line names the handle this actually wrote to, without
    re-deriving it from an Optional field the refusal below already narrowed.

    ``mail_ctx`` is the envelope the caller already built. Pass it whenever one
    exists: rebuilding it here would re-run ``resolve_self_model``, a transcript
    scan, and a live send that then falls back would stamp the durable copy with
    a model resolved separately from the one the live turn carried.

    Raises DispatchAskError(12) when the row has no harness_session_id, or
    when the bus write fails; both messages say that no durable envelope
    was written, so a caller cannot mistake the failure for a receipt.
    """
    from fno.inbox.store import DurableOwner, generate_msg_id, write_new_thread
    from fno.mail.envelope import wrap_fno_mail

    durable_recipient = (
        canonical_handle(entry.harness_session_id)
        if entry.harness_session_id
        else None
    )
    if durable_recipient is None:
        events.emit(
            "agent_send_failed",
            stage="durable-address",
            name=entry.name,
            msg_id=msg_id,
            reason="missing_harness_session_id",
            caller_reason=reason,
        )
        raise DispatchAskError(
            f"cannot queue durable mail for {entry.name!r}: registry row has "
            "no full harness session id; no durable envelope was written",
            exit_code=12,
        )

    msg_id = msg_id or generate_msg_id()
    if mail_ctx is None:
        from_harness, from_session = _stamped_sender_provenance(
            _resolve_sender_entry(entries, from_name), from_name
        )
        mail_ctx = _build_mail_ctx(
            from_name,
            from_session,
            from_harness,
            # Never the short_id fallback the caller-built ctx uses: the
            # refusal above already proved durable_recipient is not None here.
            to=durable_recipient,
            id=msg_id,
            origin=origin,
            to_session=entry.harness_session_id,
            subject=subject,
        )
    else:
        # The envelope body and the thread row must name the same sender.
        from_harness = mail_ctx.harness
        from_session = mail_ctx.from_session
    # No to_session: a durable body is read whenever the recipient next drains.
    durable_body = wrap_fno_mail(
        message,
        from_=mail_ctx.from_,
        harness=mail_ctx.harness,
        node=mail_ctx.node,
        to=mail_ctx.to,
        id=mail_ctx.id,
        from_session=mail_ctx.from_session,
        origin=mail_ctx.origin,
        subject=mail_ctx.subject,
    )
    from fno import rust_binary

    # Count the raw body, not the wire wrapper: Rule 7 and the rolling
    # budget read the same string, so the row must too.
    try:
        write_new_thread(
            recipient=durable_recipient,
            sender=mail_ctx.from_,
            kind="send",
            body=durable_body,
            msg_id=msg_id,
            to_kind="session",
            to_harness=entry.harness,
            from_harness=from_harness,
            from_session=from_session,
            from_model=mail_ctx.model,  # the tag never renders the model
            owner=owner or DurableOwner.WAKE_DAEMON.value,
            origin=mail_ctx.origin,
            word_count=rust_binary.style_word_count(message),
            subject=mail_ctx.subject,
        )
    except (OSError, ValueError, RuntimeError) as exc:
        events.emit(
            "agent_send_failed",
            stage="envelope-write",
            name=entry.name,
            msg_id=msg_id,
            caller_reason=reason,
        )
        # The clause has to appear, and appear once. The bus-lock timeout this
        # most often wraps already ends with it, so appending unconditionally
        # stuttered "...; no durable envelope was written; no durable envelope
        # was written" at the one moment the reader is deciding whether to
        # re-send.
        detail = str(exc)
        tail = "" if _NO_ENVELOPE_CLAUSE in detail else f"; {_NO_ENVELOPE_CLAUSE}"
        raise DispatchAskError(
            f"durable envelope write failed: {detail}{tail}",
            exit_code=12,
        ) from exc
    return msg_id, durable_recipient


def _stamp_after_delivery(
    name: str,
    identity: "RecipientIdentity",
    delivery: str,
    msg_id: str,
    *,
    registry_path: "Path",
    registry_lock_timeout: float,
) -> None:
    """Bump `last_message_at` (and `status` for a hosted send) after delivery.

    Delivery is already complete when this runs, so a failure here cannot make
    the send retryable, but it must stay visible. Shared by the normal path and
    the lock-timeout queue so a durable success is booked the same way in both.
    """
    try:

        def _stamp(entries_list: "list[AgentEntry]") -> "list[AgentEntry]":
            out = []
            for e in entries_list:
                if e.name == name:
                    updates: dict = {"last_message_at": _utc_now_iso()}
                    if delivery == "hosted":
                        updates["status"] = "live"
                    out.append(replace(e, **updates))
                else:
                    out.append(e)
            return out

        decline_reason: list[str] = []
        stamp_written = _update_registry_if_recipient_unchanged(
            name,
            identity,
            _stamp,
            registry_path=registry_path,
            registry_lock_timeout=registry_lock_timeout,
            decline_reason=decline_reason,
        )
        if not stamp_written:
            row_removed = decline_reason and decline_reason[0] == "row_removed"
            reason_text = "row removed entirely" if row_removed else "recipient identity changed"
            print(
                f"registry stamp failed after {delivery} delivery for "
                f"{name!r}: {reason_text}; delivery succeeded; "
                "do not retry",
                file=sys.stderr,
            )
            try:
                events.emit(
                    "agent_send_failed",
                    stage="registry-write",
                    name=name,
                    msg_id=msg_id,
                    delivery=delivery,
                    reason="row_removed" if row_removed else "recipient_identity_changed",
                    error=f"{reason_text} after delivery",
                    error_type="RecipientRowRemoved" if row_removed else "RecipientIdentityChanged",
                )
            except (OSError, ValueError):
                pass  # stderr already carries the non-retryable degradation
    except (OSError, ValueError, RegistryVersionError) as exc:
        print(
            f"registry stamp failed after {delivery} delivery for "
            f"{name!r}: {exc}; delivery succeeded; do not retry",
            file=sys.stderr,
        )
        try:
            events.emit(
                "agent_send_failed",
                stage="registry-write",
                name=name,
                msg_id=msg_id,
                delivery=delivery,
                error=str(exc),
                error_type=type(exc).__name__,
            )
        except (OSError, ValueError):
            pass  # stderr already carries the non-retryable degradation


def _reserve_send_budget(
    *,
    sender: str,
    recipient: str,
    message: str,
    msg_id: str,
):
    """Reserve the control lane's ledger before any outward send side effect.

    An ordinary body reserves nothing (rule 7 is its only word gate), so this
    returns None and callers release unconditionally.
    """
    from fno import rust_binary
    from fno.mail import budget

    if not budget.is_control(message):
        return None
    try:
        words = rust_binary.style_word_count_checked(message)
        return budget.reserve_control(
            sender=sender, recipient=recipient, words=words, msg_id=msg_id
        )
    except budget.BudgetRefused as exc:
        raise DispatchAskError(
            f"refused: control word budget for {exc.pair}: {exc.marker()}",
            exit_code=1,
        ) from exc
    except (budget.BudgetUnavailable, budget.BudgetCountUnavailable) as exc:
        raise DispatchAskError(f"refused: {exc}", exit_code=1) from exc


def dispatch_send(
    name: str,
    message: str,
    provider: Optional[str],
    cwd: "Path",
    lock_timeout: float = _DEFAULT_LOCK_TIMEOUT,
    from_name: str = _FROM_NAME_DEFAULT,
    *,
    registry_stamp_timeout_seconds: float = 1.0,
    origin: Optional[str] = None,
    subject: Optional[str] = None,
) -> "DispatchSendResult":
    """Dispatch an async ``send`` to an already-registered agent.

    Live-inject-first (node): live delivery is attempted FIRST and the
    durable inbox envelope is written ONLY when the recipient is not
    live-reachable or the live inject does not confirm. A confirmed live
    (``hosted``) send is self-recording in the transcript and is NOT also queued;
    its bus row is audit-only, while the durable bus remains the offline fallback
    tier. Both the live turn and the bus record carry the same ``<fno_mail>``
    envelope. The steps run in the numbered order the body's inline comments
    carry; the lock-timeout retry lands durable (exit 0) and only sustained
    contention exits 11 with nothing written.

    Raises:
        DispatchAskError: every documented failure mode.  send never
            creates agents; unknown names get exit 16 identical to ask.
    """
    # 1. Input validation (reuses ask's _validate_inputs). `name` here is a
    # target to resolve: a bare short-id is the canonical mailbox handle, so it
    # must reach registry lookup (and then handle resolution on a miss) rather
    # than being refused as a badly-shaped name.
    _validate_inputs(
        name=name, message=message, from_name=from_name, name_is_address=True
    )

    from fno import paths
    from fno.time_budget import validate_timeout_budget

    validate_timeout_budget(
        registry_stamp_timeout_seconds,
        label="post-delivery registry stamp",
    )

    # 2. Body size cap (exit 2 BEFORE any write).
    if len(message.encode("utf-8")) > _SEND_MAX_BODY_BYTES:
        raise DispatchAskError(
            f"message body exceeds maximum size "
            f"({_SEND_MAX_BODY_BYTES // 1024 // 1024} MiB); "
            f"got {len(message.encode('utf-8'))} bytes",
            exit_code=2,
        )

    registry_path = paths.agents_registry_path()
    requested_name = name
    # One discovery sweep per send; a lock wait clears it (a new owner may appear).
    discovered: dict[str, list] = {}

    def _load_and_resolve_target(
        expected_identity: Optional[RecipientIdentity] = None,
    ) -> tuple[list[AgentEntry], AgentEntry]:
        try:
            entries = load_registry(registry_path)
        except (OSError, ValueError, RegistryVersionError) as exc:
            events.emit(
                "agent_send_failed",
                stage="registry-read",
                name=requested_name,
            )
            raise DispatchAskError(
                f"registry read failed: {exc}",
                exit_code=12,
            ) from exc
        try:
            existing = resolve_registered_agent_across_sources(
                entries, requested_name
            ).entry
            resolved_identity = _recipient_identity_key(existing)
            if (
                expected_identity is not None
                and resolved_identity != expected_identity
            ):
                raise DispatchAskError(
                    f"agent address {requested_name!r} changed from "
                    f"{expected_identity[0]!r} to {existing.name!r} while acquiring "
                    "its lock; recipient identity changed from "
                    f"{expected_identity!r} to {resolved_identity!r}; retry the send",
                    exit_code=2,
                )
            from fno.agents.discover import discovery_address_matches

            registry_id = resolved_identity[2]
            registry_key = (resolved_identity[1], registry_id)
            if "matches" not in discovered:
                discovered["matches"] = discovery_address_matches(
                    requested_name, registry_path=registry_path
                )
            live_foreign = {
                (session.agent, session_identity_key(session.session_id)): session
                for session in discovered["matches"]
                if (session.agent, session_identity_key(session.session_id)) != registry_key
            }
            if live_foreign:
                candidates = [
                    f"{registry_id or existing.name} "
                    f"({existing.harness}, registry name={existing.name})",
                    *(
                        f"{session.session_id} ({session.agent}, live discovery)"
                        for session in sorted(
                            live_foreign.values(),
                            key=lambda value: (value.agent, value.session_id),
                        )
                    ),
                ]
                raise AgentResolutionError(
                    f"token {requested_name!r} is ambiguous across "
                    f"{len(candidates)} sessions: {', '.join(candidates)}. "
                    "Disambiguate with the full session id.",
                    ambiguous=True,
                )
        except AgentResolutionError as exc:
            events.emit(
                "agent_send_failed",
                stage=(
                    "ambiguous-address"
                    if exc.ambiguous
                    else "identity-unavailable"
                    if exc.unavailable
                    else "unknown-name"
                ),
                name=requested_name,
            )
            if exc.ambiguous:
                raise DispatchAskError(str(exc), exit_code=2) from exc
            if exc.unavailable:
                raise DispatchAskError(str(exc), exit_code=12) from exc
            raise DispatchAskError(
                f"unknown agent {requested_name!r}; spawn it first: "
                f"fno agents spawn {requested_name} --harness <harness>",
                exit_code=UNKNOWN_AGENT_EXIT_CODE,
            ) from exc
        return entries, existing

    # Resolve before locking so every address form serializes on the registry
    # primary key. The second resolution under that lock closes the read/lock
    # race and refuses if the address changed owners while we waited.
    _, initial = _load_and_resolve_target()
    canonical_name = initial.name
    canonical_identity = _recipient_identity_key(initial)

    def _on_wait() -> None:
        discovered.clear()
        print(f"Waiting for agent {canonical_name!r} lock...", file=sys.stderr, flush=True)

    # 3. Per-agent flock. Confirmed (node, change 6): this `with` block
    # spans the ENTIRE rest of the send, including the live-delivery attempt
    # below -- not only the registry read/identity-check mutation it exists to
    # guard (the "second resolution under that lock closes the read/lock race"
    # comment a few lines down). A live-delivery attempt at a busy recipient
    # can now take up to `_MAIL_INJECT_LIVENESS_SCALED_TIMEOUT_S` (40s, change
    # 2's own budget raise) or the switchboard's 130s relay ceiling, so a
    # second sender addressing the SAME busy recipient serializes behind the
    # whole first attempt rather than just the identity check -- the specimen
    # measured delivering this plan's own report ("Waiting for agent
    # 'lead-footnote-g3' lock..." with no progress until killed). Narrowing
    # it to the registry mutation alone is a separate, larger restructuring.
    try:
        with hold_agent_lock(
            canonical_name,
            registry_path,
            timeout=lock_timeout,
            on_wait=_on_wait,
        ):
            entries, existing = _load_and_resolve_target(canonical_identity)
            if existing.name != canonical_name:
                raise DispatchAskError(
                    f"agent address {requested_name!r} changed from "
                    f"{canonical_name!r} to {existing.name!r} while acquiring "
                    "its lock; retry the send",
                    exit_code=2,
                )
            selected_identity = _recipient_identity_key(existing)
            # Live routing and events use the registry primary key, never the
            # caller's alias. Durable routing below binds to this selected
            # session's canonical handle so later name reuse cannot inherit it.
            name = canonical_name

            # 4b. Provider mismatch check (shared select_provider gate).
            try:
                select_provider(name=name, requested_provider=provider)
            except ProviderMismatchError as exc:
                raise DispatchAskError(str(exc), exit_code=2) from exc
            except ValueError as exc:
                raise DispatchAskError(str(exc), exit_code=2) from exc
            except (OSError, RegistryVersionError) as exc:
                events.emit(
                    "agent_send_failed",
                    stage="registry-read",
                    name=name,
                )
                raise DispatchAskError(
                    f"registry read failed: {exc}",
                    exit_code=12,
                ) from exc

            # 4c. Capture sender provenance for the <fno_mail> envelope and the
            # durable fallback record (node). Sender identity is
            # best-effort: an unregistered caller leaves from_session None and
            # exclusion falls back to the always-present from_ name. from_model is
            # NOT set on the durable envelope (AgentEntry has no model field; we do
            # not fabricate one -- LD11 forward-compat).
            from fno.inbox.store import generate_msg_id

            sender_entry = _resolve_sender_entry(entries, from_name)
            self_proof = _proven_self_sender(from_name)
            if self_proof[1] is not None and (
                sender_entry is None
                or getattr(sender_entry, "harness_session_id", None) != self_proof[1]
            ):
                # The self proof owns from_name but the registry row resolved
                # against it names a different session (a same-bucket codex
                # sibling): the envelope keeps the proven self, and the row
                # must not reach the switchboard lane as from_identity - the
                # daemon would attribute the stranger. Floor it to unproven,
                # which is also what the old exact-name lookup left here.
                sender_entry = None
            from_harness, from_session = _stamped_sender_provenance(
                sender_entry, from_name, self_proof
            )
            # A `fno agents mail send <name>` is always directed -> stamp the selected
            # session's canonical handle as the envelope `to`. A transport short
            # id is retained only for hosted delivery when the legacy row has no
            # full session id; such a row cannot safely receive durable mail.
            # Mint the id BEFORE building the ctx so the SAME id rides the live
            # inject, the durable fallback, AND the durable thread record (US1 /
            # Locked Decision 8: _deliver_live carries it).
            msg_id = generate_msg_id()
            durable_recipient = (
                canonical_handle(existing.harness_session_id)
                if existing.harness_session_id
                else None
            )
            budget_recipient = durable_recipient or existing.short_id or canonical_name
            mail_ctx = _build_mail_ctx(
                from_name,
                from_session,
                from_harness,
                to=(durable_recipient or existing.short_id or None),
                id=msg_id,
                origin=origin,
                to_session=existing.harness_session_id,
                subject=subject,
            )
            reservation = _reserve_send_budget(
                sender=mail_ctx.from_,
                recipient=budget_recipient,
                message=message,
                msg_id=msg_id,
            )

            live_attempted = False
            from fno.inbox.store import DurableOwner, classify_durable_owner

            durable_owner = DurableOwner.WAKE_DAEMON.value

            def _write_durable() -> None:
                """Write the durable FALLBACK envelope: the pending-queue for an
                offline recipient, or the recovery record when a live inject did
                not land. The jsonl bus is the fallback tier now, not a peer to the
                live path (node). Drain-on-wake semantics are unchanged.

                Delegates to :func:`_queue_durable_fallback`, the same helper the
                lock-timeout handler below uses, so a message queued from inside
                the lock and one queued after failing to acquire it share one
                envelope-construction and error path."""
                try:
                    _queue_durable_fallback(
                        existing,
                        message,
                        from_name,
                        entries,
                        msg_id=msg_id,
                        mail_ctx=mail_ctx,
                        owner=durable_owner,
                        subject=subject,
                    )
                except Exception:
                    if not live_attempted:
                        from fno.mail import budget

                        budget.release(reservation)
                    raise

            # 4d/4e. Live-inject-first, durable fallback. The context stash ensures
            # started/done share one request_id + caller attribution (mirrors the
            # dispatch_ask pattern introduced in PR #457).
            ctx_for_dispatch = build_context(
                to_name=name,
                to_provider=existing.harness,
                to_session_id=getattr(existing, "harness_session_id", None),
                transport="direct-cli",
                from_name_override=from_name,
            )
            ctx_token = _DISPATCH_CTX.set(ctx_for_dispatch)

            try:
                _emit_ev(
                    "agent_send_started",
                    name=name,
                    provider=existing.harness,
                    msg_id=msg_id,
                )

                family1_state = _registered_family1_state(existing)
                family1_live = family1_state in {"working", "watching", "your-move"}
                durable_owner = classify_durable_owner(
                    param_forced=False,
                    recipient_live=family1_live,
                    recipient_resumable=not family1_live,
                ).value
                # Unknown is hands-off, not dead. A claude row off any pane is always
                # tried: mail-inject reads the daemon roster first, and the roster decides.
                family1_attemptable = family1_live or family1_state == "unknown" or (
                    existing.harness == "claude" and not existing.mux)

                # W3 write-ahead: a recipient we will not attempt live is asleep, so a
                # sender crash before it wakes must not lose the message; its durable
                # placeholder writes first. The live lane stages its row before the inject
                # (the bus row is the body's only home): a duplicate beats a drop.
                if durable_recipient is not None and not family1_attemptable:
                    _write_durable()
                delivery = "durable"
                demotion_notice: Optional[str] = None
                live_miss_reason: Optional[str] = None

                _live_delivered = False
                _live_reason: list = []
                # A skipped live lane names the reading that vetoed it. Bus-only outranks both.
                _bus_only = _delivery_policy_refusal(existing) == BUS_ONLY_POLICY
                if not family1_attemptable:
                    live_miss_reason = f"transcript-{family1_state}"
                if family1_attemptable:
                    from fno import rust_binary
                    from fno.mail.envelope import wrap_fno_mail

                    record: dict[str, Any] = dict(
                        msg_id=msg_id, sender=mail_ctx.from_,
                        recipient=durable_recipient or existing.short_id,
                        body=wrap_fno_mail(
                            message, from_=mail_ctx.from_, harness=mail_ctx.harness,
                            node=mail_ctx.node, to=mail_ctx.to, id=mail_ctx.id,
                            from_session=mail_ctx.from_session,
                            to_session=mail_ctx.to_session, subject=mail_ctx.subject,
                            from_name=sender_entry.name if sender_entry is not None else None,
                        ),
                        from_harness=from_harness, to_harness=existing.harness,
                        to_session=mail_ctx.to_session, from_session=from_session,
                        from_model=mail_ctx.model, to_kind="session",
                        word_count=rust_binary.style_word_count(message),
                        subject=mail_ctx.subject,
                    )
                    bus_live = str(paths.bus_dir() / "messages.jsonl")
                    staged_fields = {k: v for k, v in record.items() if v is not None}
                    # Stage before the inject: a staging failure demotes to the durable floor; an mcp-only row stages nothing.
                    try:
                        if record["recipient"]:
                            rust_binary.chats_command(["stage", bus_live], staged_fields)
                    except Exception as exc:  # noqa: BLE001 - demote to durable
                        print(f"body row not staged; skipping live delivery: {exc}", file=sys.stderr)
                        live_miss_reason = "stage-failed"
                    else:
                        live_attempted = True
                        _live_delivered = _deliver_live(
                            existing,
                            message,
                            from_name,
                            mail_ctx,
                            sender_entry=sender_entry,
                            reason_out=_live_reason,
                            family1_state=family1_state,
                        )
                        if _live_delivered:
                            delivery = "hosted"
                            try:
                                rust_binary.chats_command(
                                    ["stage", bus_live, "--delivery", "hosted"], staged_fields,
                                )
                            except Exception as exc:  # noqa: BLE001 - delivery already succeeded
                                print(f"delivery succeeded; receipt row failed; the staged bus row holds the body; do not retry: {exc}", file=sys.stderr)
                        else:
                            live_miss_reason = ";".join(_live_reason) if _live_reason else None
                            if live_miss_reason and existing.mux:
                                live_miss_reason = f"{live_miss_reason};owner={durable_owner}"
                if _bus_only:
                    live_miss_reason = BUS_ONLY_POLICY
                if not _live_delivered and (durable_recipient is None or family1_attemptable):
                    # Live-first fallback: an attemptable recipient whose live
                    # attempt missed, or a recipient with no durable address
                    # (which raises durable-address exit 12 inside _write_durable).
                    _write_durable()

                if delivery == "durable" and family1_live:
                    why = f" ({live_miss_reason})" if live_miss_reason else ""
                    demotion_notice = (
                        f"live delivery failed for {name!r}{why}; message queued durable ({msg_id})"
                    )

                _emit_ev(
                    "agent_send_done", name=name, provider=existing.harness,
                    msg_id=msg_id, delivery=delivery, reason=live_miss_reason,
                )
            finally:
                _DISPATCH_CTX.reset(ctx_token)

            if demotion_notice:
                print(demotion_notice, file=sys.stderr)

            # 4f. Bump registry stamps (shared with the lock-timeout queue).
            _stamp_after_delivery(
                name,
                selected_identity,
                delivery,
                msg_id,
                registry_path=registry_path,
                registry_lock_timeout=registry_stamp_timeout_seconds,
            )

            return DispatchSendResult(
                msg_id=msg_id, delivery=delivery, reason=live_miss_reason,
                durable_owner=durable_owner if delivery == "durable" else None,
                to=durable_recipient or existing.short_id or None,
                to_harness=existing.harness,
                to_session=existing.harness_session_id or None,
            )

    except AgentLockTimeout as exc:
        # INVARIANT: this handler guards the whole `with` body, not only the
        # acquire, and the body ends in a durable queue returning exit 0. Safe
        # ONLY because no callee inside takes a per-agent flock (deliver,
        # switchboard, pane send, family-1 state, durable fallback, stamp).
        # Add a nested acquire and a confirmed hosted delivery lands here
        # DOUBLE-QUEUED with a receipt for one that already arrived. Narrow
        # this `try` to the acquire before adding one.
        #
        # A durable write needs a VERIFIED recipient and only the lock gives
        # one: an unlocked re-read can hit a same-name reclaim that has not
        # committed its row, where "identity unchanged" passes vacuously and
        # the queue strands the message in the dead session's mailbox. So the
        # queue takes the lock too, on a grace window that asks "did the
        # holder just finish?" rather than waiting again.
        #
        # The locked path rebinds `name` to the registry primary key before it
        # emits or stamps anything; this path never entered that block, so it
        # must do the same: a stamp keyed to the caller's alias matches no row
        # and reports "recipient identity changed" over a send that succeeded.
        name = canonical_name
        grace_seconds = _queue_grace_seconds(exc.timeout)
        # Reassigned under the lock once the row is resolved: a bus-only row
        # queues by design, not by deferral, and must not wear this lane's
        # reason. Declared here because the receipt is built after the block.
        queue_reason = LOCK_TIMEOUT_REASON
        try:
            with hold_agent_lock(
                canonical_name,
                registry_path,
                timeout=grace_seconds,
                # The grace window makes a default send block for 42s, not 30.
                # Without the callback the extra 12s is silent, so the command
                # reads as hung right after it announced it was waiting.
                on_wait=_on_wait,
            ):
                # Every resolution that feeds a write happens here, under the
                # lock, through the same call the normal path uses. An owner
                # change now REFUSES (exit 2) rather than guessing which
                # session the caller meant.
                timeout_entries, timeout_entry = _load_and_resolve_target(
                    canonical_identity
                )

                # Provider mismatch refuses BEFORE the queue. The locked path
                # checks this and exits 2 without delivering, so lock
                # contention must not turn a refused send into a delivered one.
                try:
                    select_provider(
                        name=timeout_entry.name, requested_provider=provider
                    )
                except ProviderMismatchError as mismatch:
                    raise DispatchAskError(str(mismatch), exit_code=2) from mismatch
                except ValueError as bad_provider:
                    # An unknown --harness is a usage error on the locked path
                    # too. Lock timing must not change which exception the CLI
                    # sees.
                    raise DispatchAskError(
                        str(bad_provider), exit_code=2
                    ) from bad_provider
                except (OSError, RegistryVersionError) as unreadable:
                    events.emit("agent_send_failed", stage="registry-read", name=name)
                    raise DispatchAskError(
                        f"registry read failed: {unreadable}",
                        exit_code=12,
                    ) from unreadable

                # A queued message is a delivered outcome, so it books like
                # one: started/done and the registry stamp all fire, exactly as
                # the normal durable path does. Emitting only agent_send_failed
                # here left a successful send reading as a failure in the event
                # trail and left last_message_at stale, which agent display and
                # project anycast rank on.
                #
                # All of it stays INSIDE the lock, like the normal path: a stamp
                # written after release can lose a same-name reclaim race and
                # print "recipient identity changed" over a send that succeeded.
                ctx_for_timeout = build_context(
                    to_name=name,
                    to_provider=timeout_entry.harness,
                    to_session_id=getattr(timeout_entry, "harness_session_id", None),
                    transport="direct-cli",
                    from_name_override=from_name,
                )
                # A legacy row with no full harness session id has no durable
                # ADDRESS: hosted delivery is its only lane, and this path
                # never ran one. Queuing anyway raises exit 12 "cannot queue
                # durable mail ... no full harness session id", which reads as
                # a permanent registry defect and stops the caller dead. The
                # truth is narrower and recoverable, and it is what exit 11
                # said before this lane existed: the lock was busy, live is
                # the only lane this row has, retry.
                if not timeout_entry.harness_session_id:
                    events.emit(
                        "agent_send_failed",
                        stage="durable-address",
                        name=name,
                        reason="missing_harness_session_id",
                        caller_reason=LOCK_TIMEOUT_REASON,
                    )
                    raise DispatchAskError(
                        f"live delivery to {name!r} was not attempted (its "
                        f"agent lock was busy for {exc.timeout}s, and this "
                        "queue holds it now), and its registry row carries no "
                        "full harness session id, so it has no durable "
                        f"address; {_NO_ENVELOPE_CLAUSE}; retry the send",
                        exit_code=11,
                    ) from exc

                # It does NOT retry the live lane from here, and that is a
                # decision rather than an omission - five review passes have
                # read it as one, so it is written down. The block holds a
                # verified row and could inject. It does not, because an
                # inject here runs to the same 40s budget WHILE HOLDING this
                # flock, on a send that has already waited out one holder: it
                # would push a routine send past 80s and make the next sender
                # time out on us, which is the contention that caused this bug.
                # The sender is not stranded either way - the message is
                # durable and the receipt names withdraw-then-resend. Moving
                # live delivery in here belongs with narrowing the flock, and
                # is tracked with it.
                #
                # A bus-only row's queue is its DESIGNED lane, not a deferral.
                # Reporting it as a lock timeout hands the sender the recovery
                # ladder - withdraw, then retry live - for a message that is
                # correctly queued, against a row whose own policy forbids the
                # live retry. The normal path branches here; a grace-path queue
                # that skipped the check mirrored this arm's own wrong-cause
                # defect onto a different row.
                queue_reason = (
                    BUS_ONLY_POLICY
                    if _delivery_policy_refusal(timeout_entry) == BUS_ONLY_POLICY
                    else LOCK_TIMEOUT_REASON
                )

                # Mint the id before the started event so started and done name
                # the same message, as they do on the normal path.
                from fno.inbox.store import generate_msg_id

                msg_id = generate_msg_id()
                from_harness, from_session = _stamped_sender_provenance(
                    _resolve_sender_entry(timeout_entries, from_name), from_name
                )
                timeout_recipient = canonical_handle(
                    timeout_entry.harness_session_id
                )
                timeout_mail_ctx = _build_mail_ctx(
                    from_name,
                    from_session,
                    from_harness,
                    to=timeout_recipient,
                    id=msg_id,
                    origin=origin,
                    subject=subject,
                )
                reservation = _reserve_send_budget(
                    sender=timeout_mail_ctx.from_,
                    recipient=timeout_recipient,
                    message=message,
                    msg_id=msg_id,
                )
                ctx_token = _DISPATCH_CTX.set(ctx_for_timeout)
                try:
                    _emit_ev(
                        "agent_send_started",
                        name=name,
                        provider=timeout_entry.harness,
                        msg_id=msg_id,
                    )
                    try:
                        msg_id, _durable_to = _queue_durable_fallback(
                            timeout_entry,
                            message,
                            from_name,
                            timeout_entries,
                            msg_id=msg_id,
                            reason=queue_reason,
                            mail_ctx=timeout_mail_ctx,
                            subject=subject,
                        )
                    except Exception:
                        from fno.mail import budget

                        budget.release(reservation)
                        raise
                    _emit_ev(
                        "agent_send_done",
                        name=name,
                        provider=timeout_entry.harness,
                        msg_id=msg_id,
                        delivery="durable",
                        reason=queue_reason,
                    )
                finally:
                    _DISPATCH_CTX.reset(ctx_token)

                _stamp_after_delivery(
                    name,
                    _recipient_identity_key(timeout_entry),
                    "durable",
                    msg_id,
                    registry_path=registry_path,
                    registry_lock_timeout=registry_stamp_timeout_seconds,
                )
        except AgentLockTimeout as still_held:
            # Sustained contention: no verified recipient, so nothing is
            # written. Loud and nonzero beats a message delivered to whoever
            # used to own the name. The requirement was "queue durable OR exit
            # nonzero and say so"; this is the second arm.
            events.emit(
                "agent_send_failed",
                stage="lock-timeout",
                name=name,
                reason="unverified_recipient",
            )
            # Name the holder that blocked the QUEUE, not the one that blocked
            # the first acquire. They differ whenever the original holder
            # released and a third process took the lock inside the grace
            # window, and reporting the first one points at a process that no
            # longer owns anything. Both waits are named for the same reason:
            # the caller blocked for lock_timeout AND the grace window, so the
            # first alone understates the block and reads as a tuning knob
            # that did not do what it says.
            raise DispatchAskError(
                f"timed out waiting for agent {exc.name!r} lock "
                f"(timeout={exc.timeout}s + {grace_seconds}s queue grace)"
                f"{still_held.holder_note()}; "
                "recipient identity could not be verified, so no durable "
                "envelope was written; retry the send",
                exit_code=11,
            ) from still_held

        # The message is queued, so this is a durable SUCCESS, not a failure.
        # cmd_send's stdout contract is one receipt line and exit 0 for every
        # durable outcome; a nonzero exit here would both break that and make
        # a retry-on-failure caller enqueue the same message twice. The live
        # lane's cause rides back as the receipt's reason, and the holder goes
        # to stderr the way a live-miss demotion notice does.
        # Not for a bus-only row: its queue is the designed destination, so a
        # "live delivery deferred" notice would report a miss that never was.
        if queue_reason == LOCK_TIMEOUT_REASON:
            # Past tense throughout, and deliberately. Reaching this line
            # proves the holder RELEASED, because the grace acquire only wins
            # once it does, so a present-tense "lock busy ... held by pid P"
            # names an owner that no longer owns it - and `_pid_is_alive`
            # cannot refuse that pid, which is usually still running. The
            # exit-11 arm below reads `still_held` for the same reason.
            print(
                f"live delivery deferred for {name!r}: "
                f"lock was busy for {exc.timeout}s{exc.holder_note(past=True)}",
                file=sys.stderr,
            )
        return DispatchSendResult(
            msg_id=msg_id,
            delivery="durable",
            reason=queue_reason,
            to=timeout_recipient or timeout_entry.short_id or None,
            to_harness=timeout_entry.harness,
            to_session=timeout_entry.harness_session_id or None,
        )


# ---------------------------------------------------------------------------
# Project-destination addressing (anycast) - Group 3 Task 3.3 (US6)
# ---------------------------------------------------------------------------
# Project/cwd is demoted from address to resolver. `send --to-project X` (and
# `ask --to-project`) resolves over the registry: cwd->project mapping plus the
# config.inbox.peers `project:` hint. Rule: exactly one live peer -> deliver
# live; none -> durable queue to project X; many -> error listing the live
# candidates unless `--any` breaks the tie (most recent last_message_at wins,
# lexicographic registry name as the final tiebreak). One log underneath.

AMBIGUOUS_PROJECT_EXIT_CODE = 17


@dataclass
class ProjectResolution:
    """Outcome of resolving a project name to a delivery target.

    Exactly one of three outcomes holds: live (``recipient`` set), durable
    (``durable``), or ambiguous (``ambiguous``). ``__post_init__`` enforces the
    mutual exclusivity so an illegal combination (e.g. a recipient AND
    ambiguous) fails loudly at construction rather than silently mis-routing.
    """

    recipient: Optional[str]  # the single live peer to deliver to, else None
    live_candidates: list[str]  # all live peer names in the project (sorted)
    durable: bool  # True when no live peer -> durable queue
    ambiguous: bool  # True when >1 live peer and no --any

    def __post_init__(self) -> None:
        active = (self.recipient is not None) + self.durable + self.ambiguous
        if active != 1:
            raise ValueError(
                "ProjectResolution must encode exactly one outcome; got "
                f"recipient={self.recipient!r}, durable={self.durable}, "
                f"ambiguous={self.ambiguous}"
            )
        if self.ambiguous and len(self.live_candidates) < 2:
            raise ValueError("ambiguous resolution requires >=2 live candidates")


def _entry_projects(entry: "AgentEntry", peer_projects: dict[str, str]) -> set[str]:
    """Return every project a registry entry serves.

    The registry cwd->project mapping is authoritative; the
    `config.inbox.peers.<name>.project` hint only ADDS an association, it never
    replaces the cwd mapping. So an entry serves BOTH its cwd-resolved project
    and any hinted project: a stale or extra hint can never hide a live peer
    from its actual cwd project. Returns the (possibly empty) set of project
    names this entry is a candidate for.
    """
    projects: set[str] = set()
    if entry.cwd:
        try:
            from fno.inbox.store import (
                ProjectIdentificationError,
                resolve_project,
            )

            projects.add(resolve_project(Path(entry.cwd)))
        except ProjectIdentificationError:
            pass
        except Exception:  # noqa: BLE001 - a bad cwd must not abort resolution
            pass
    hinted = peer_projects.get(entry.name)
    if hinted:
        projects.add(hinted)
    return projects


def resolve_to_project(
    project: str,
    *,
    any_: bool = False,
    registry_path: "Optional[Path]" = None,
) -> ProjectResolution:
    """Resolve a destination project to a single delivery target.

    Registry cwd->project mapping is authoritative; the `config.inbox.peers`
    `project:` hint only adds associations and degrades to {} (never raises)
    on a missing/malformed config, so resolution always works off the registry
    alone (AC6-FR).
    """
    # A malformed block reads as {} natively; this guard catches a failed run.
    try:
        from fno.rust_binary import resolve_binary
        argv = [str(resolve_binary()), "compaction", "inbox-peers", "--json"]
        proc = subprocess.run(argv, capture_output=True, text=True, check=True)
        sys.stderr.write(proc.stderr)
        peer_projects = json.loads(proc.stdout)["projects"]
    except Exception as exc:  # noqa: BLE001 - the hint is best-effort; never fatal
        print(
            f"warning: --to-project peer hint unavailable ({type(exc).__name__}: "
            f"{exc}); resolving over the registry cwd mapping alone",
            file=sys.stderr,
        )
        peer_projects = {}

    try:
        entries = load_registry(registry_path) if registry_path else load_registry()
    except (OSError, ValueError, RegistryVersionError) as exc:
        raise DispatchAskError(f"registry read failed: {exc}", exit_code=12) from exc

    # Candidate = any entry that serves this project (cwd mapping OR hint),
    # deduped by name.
    candidates: dict[str, "AgentEntry"] = {}
    for e in entries:
        if project in _entry_projects(e, peer_projects):
            candidates[e.name] = e

    live = [e for e in candidates.values() if e.status == "live"]
    live_names = sorted(e.name for e in live)

    if not live:
        return ProjectResolution(recipient=None, live_candidates=[], durable=True, ambiguous=False)
    if len(live) == 1:
        return ProjectResolution(
            recipient=live[0].name,
            live_candidates=live_names,
            durable=False,
            ambiguous=False,
        )
    if not any_:
        return ProjectResolution(
            recipient=None,
            live_candidates=live_names,
            durable=False,
            ambiguous=True,
        )
    # --any tiebreak: most recent last_message_at, then lexicographic name.
    max_ts = max((e.last_message_at or "") for e in live)
    tied = sorted(
        (e for e in live if (e.last_message_at or "") == max_ts),
        key=lambda e: e.name,
    )
    return ProjectResolution(
        recipient=tied[0].name,
        live_candidates=live_names,
        durable=False,
        ambiguous=False,
    )


def dispatch_send_to_project(
    project: str,
    message: str,
    *,
    provider: Optional[str] = None,
    cwd: "Path",
    from_name: str = _FROM_NAME_DEFAULT,
    any_: bool = False,
    lock_timeout: float = _DEFAULT_LOCK_TIMEOUT,
    origin: Optional[str] = None,
    subject: Optional[str] = None,
) -> "DispatchSendResult":
    """Async send addressed to a project (anycast over the registry).

    One live peer -> live send to it (records the resolved recipient). None ->
    durable queue addressed to the project (picked up at that project's next
    drain). Many -> error listing the live candidates, delivering to none,
    unless ``any_`` breaks the tie deterministically.
    """
    # Validate message + from_name (the project name is validated by the
    # resolver / store recipient check, not the agent-name rule).
    _validate_inputs(name="placeholder", message=message, from_name=from_name)

    # Body size cap BEFORE any write, matching dispatch_send (both delivery
    # paths - live by-name and durable-to-project - share the ceiling).
    if len(message.encode("utf-8")) > _SEND_MAX_BODY_BYTES:
        raise DispatchAskError(
            f"message body exceeds maximum size "
            f"({_SEND_MAX_BODY_BYTES // 1024 // 1024} MiB); "
            f"got {len(message.encode('utf-8'))} bytes",
            exit_code=2,
        )

    res = resolve_to_project(project, any_=any_)

    if res.ambiguous:
        listing = ", ".join(res.live_candidates)
        raise DispatchAskError(
            f"--to-project {project!r} is ambiguous: {len(res.live_candidates)} "
            f"live peers ({listing}); pass --any to break the tie or address one "
            f"by name. Delivered to none.",
            exit_code=AMBIGUOUS_PROJECT_EXIT_CODE,
        )

    if res.recipient is not None:
        # Exactly one live peer (or --any winner): deliver live by name.
        result = dispatch_send(
            name=res.recipient,
            message=message,
            provider=provider,
            cwd=cwd,
            lock_timeout=lock_timeout,
            from_name=from_name,
            origin=origin,
            subject=subject,
        )
        return replace(result, recipient=res.recipient, to_project=project)

    # No live peer: durable queue addressed to the project itself. The envelope
    # (and bus mirror) record to == project (to_kind=project); the next drain in
    # that project picks it up, EXCLUDING the sender (Group 1). The
    # sender identity is best-effort - exclusion falls back to the from_ name.
    from fno.inbox.store import DurableOwner, generate_msg_id, write_new_thread

    from_session = from_harness = None
    try:
        from fno.agents.registry import load_registry as _load_reg

        _se = next((e for e in _load_reg() if e.name == from_name), None)
        if _se is not None:
            from_harness = _se.harness
            from_session = (
                getattr(_se, "harness_session_id", None)
                or getattr(_se, "short_id", None)
            )
    except Exception:  # noqa: BLE001 - sender identity is best-effort
        pass

    from fno import rust_binary
    from fno.mail import budget

    msg_id = generate_msg_id()
    reservation = _reserve_send_budget(
        sender=from_name,
        recipient=project,
        message=message,
        msg_id=msg_id,
    )

    try:
        sent_id = write_new_thread(
            recipient=project,
            sender=from_name,
            kind="send",
            body=message,
            msg_id=msg_id,
            to_kind="project",
            from_session=from_session,
            from_harness=from_harness,
            word_count=rust_binary.style_word_count(message),
            # US6: an explicit --to-project note deliberately chose the durable
            # project-inbox lane; the project's own drain owns it.
            owner=DurableOwner.INBOX_DRAIN.value,
            origin=origin,
            subject=subject,
        )
    except (OSError, ValueError, RuntimeError) as exc:
        budget.release(reservation)
        events.emit("agent_send_failed", stage="durable-write", name=project)
        raise DispatchAskError(
            f"durable envelope write failed for project {project!r}: {exc}",
            exit_code=12,
        ) from exc

    _emit_ev(
        "agent_send_done",
        name=project,
        provider=provider or "",
        msg_id=sent_id,
        delivery="durable",
    )
    return DispatchSendResult(
        msg_id=sent_id,
        delivery="durable",
        recipient=None,
        to_project=project,
        durable_owner=DurableOwner.INBOX_DRAIN.value,
    )
