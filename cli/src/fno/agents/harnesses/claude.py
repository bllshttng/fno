"""fno.agents.harnesses.claude — Claude --bg adapter for US1 dispatch.

Surface:

- :func:`bg_create` — invoke ``claude --bg --name <name> <message>``, parse
  the supervisor short-id from stdout, return :class:`ProviderResult`.
- :func:`parse_short_id` — pure regex extractor for the documented stdout
  shape, used both by ``bg_create`` and by callers that already have the
  raw stdout in hand.
- :class:`ProviderParseError` — raised when stdout does not match the
  ``^backgrounded · ([0-9a-f]{8}) · `` contract.
- :class:`ProviderSubprocessError` — raised when ``claude --bg`` exits
  non-zero, preserving the verbatim stderr and exit code for the caller
  to surface (AC1-FR subprocess non-zero).

Locked Decision 6: the short-id contract is the regex above. Any
``claude`` release that changes the format fails closed at this layer
with a diagnostic carrying the first 200 chars of stdout; the registry
write never runs.

Argv-overflow (AC1-EDGE 300KB → stdin): the implementation routes the
message via ``subprocess.run(input=msg)`` when the rendered argv would
exceed 200KB. The exact stdin-trigger argv shape is version-dependent
in real ``claude``; the integration tests use a fake script that reads
from stdin unconditionally, so the substrate is verified independent of
real CLI drift. When ``claude`` changes its stdin convention, only the
``_ARGV_OVERFLOW_THRESHOLD`` branch needs to follow.
"""

from __future__ import annotations

import json
import os
import re
import socket
import subprocess
import sys
import time
from pathlib import Path
from typing import Any, Literal, Mapping, Optional, Sequence

from fno.agents.harnesses._claude_session_registry import (
    resolve_session_uuid,  # noqa: F401 - re-export: dispatch reconcile reads it here
    )
from fno.agents.harnesses.base import ProviderResult, ReachabilityProbeError
from fno.agents.writable_dirs import worker_writable_dirs

OrphanReason = Literal[
    "not-found",
    "socket-null",
    "liveness-failed",
    "roster-live-inject-failed",
    "truth-live-inject-failed",
]

# Locked Decision 6: 8 lowercase hex chars after "backgrounded · ".
_SHORT_ID_PATTERN = re.compile(r"^backgrounded · ([0-9a-f]{8}) · ")

# claude >= 2.1.191 colorizes the short-id in the --bg receipt even over a
# non-TTY pipe (`backgrounded · \x1b[36m<id>\x1b[39m · <name>`), which the
# anchored hex pattern above cannot match through. Strip CSI/SGR escapes
# before matching so the parser is robust to color drift across versions.
_ANSI_ESCAPE = re.compile(r"\x1b\[[0-9;]*m")

# How many leading chars of stdout to include in parse-failure diagnostics.
_STDOUT_HEAD_LIMIT = 200

# Indirection so unit tests can monkeypatch the subprocess call without
# touching the real subprocess module's API surface. Lookup happens via
# module globals at call time so ``monkeypatch.setattr(claude_mod,
# "_subprocess_run", fake)`` works.
_subprocess_run = subprocess.run

# Claude Code force-ends a turn after N consecutive Stop-hook blocks (default
# 9). fno's loop blocks repeatedly by design, so on the unraised harness a
# sustained block-and-retry - the US4 gh-read-error arm during a GitHub outage,
# or any loop that needs >9 fires to reach done() - is truncated at 9, before
# fno's own NoProgress fingerprint backstop or budget ceiling can bind. We raise
# it for fno-spawned workers. 50 is ~5.5x the default, comfortably above the
# NoProgress backstop (3 unattended / 5 attended), and finite so a genuinely
# wedged rapid-block loop still dies quickly instead of running until budget
# exhausts. Attended sessions (the operator's own claude) are not fno-spawned,
# so they keep the harness default unless the operator sets the var themselves.
CLAUDE_CODE_STOP_HOOK_BLOCK_CAP_DEFAULT = "50"
CLAUDE_CODE_STOP_HOOK_BLOCK_CAP_ENV = "CLAUDE_CODE_STOP_HOOK_BLOCK_CAP"


def claude_stop_hook_block_cap() -> str:
    """The Stop-hook block cap to inject for a fno-spawned claude worker.

    Honors an explicit operator setting in the environment; otherwise returns
    the fno default. Both spawn substrates (pane and bg) read this so the value
    lives in one place."""
    return os.environ.get(CLAUDE_CODE_STOP_HOOK_BLOCK_CAP_ENV) or CLAUDE_CODE_STOP_HOOK_BLOCK_CAP_DEFAULT


class ProviderParseError(RuntimeError):
    """Raised when claude --bg stdout does not match the short-id contract.

    Carries the first :data:`_STDOUT_HEAD_LIMIT` chars of stdout so the
    caller can dump them to stderr for AC1-FR diagnostics.
    """

    def __init__(self, stdout_head: str) -> None:
        super().__init__(
            f"unable to parse short-id from claude --bg output: "
            f"first {len(stdout_head)} chars: {stdout_head!r}"
        )
        self.stdout_head = stdout_head


class ProviderSubprocessError(RuntimeError):
    """Raised when ``claude --bg`` exits non-zero, times out, or is missing.

    Preserves the verbatim stderr and exit code so the dispatcher can
    surface them unwrapped (AC1-FR subprocess non-zero / auth-quota).
    """

    def __init__(
        self, exit_code: int, stderr: str, *, summary: Optional[str] = None
    ) -> None:
        # `summary` is for a refusal raised BEFORE claude is invoked. The default
        # names a process exit, and a caller that never started one would report
        # the wrong layer: a reader chasing "claude --bg exited 2" goes looking
        # at claude, its auth, and its quota, for a decision this side made.
        super().__init__(summary or f"claude --bg exited {exit_code}: {stderr!r}")
        self.exit_code = exit_code
        self.stderr = stderr


def parse_short_id(stdout: str) -> str:
    """Extract the 8-hex short-id from claude --bg's stdout.

    Inspects only the first line. Raises :class:`ProviderParseError`
    with the first :data:`_STDOUT_HEAD_LIMIT` chars of stdout if the
    contract is not matched.
    """
    if not stdout:
        raise ProviderParseError(stdout_head="")

    first_line = _ANSI_ESCAPE.sub("", stdout.split("\n", 1)[0])
    match = _SHORT_ID_PATTERN.match(first_line)
    if match is None:
        raise ProviderParseError(stdout_head=stdout[:_STDOUT_HEAD_LIMIT])
    return match.group(1)


def _tier3_tokens(
    add_dir: Optional[str] = None,
    agent: Optional[str] = None,
    tools: Optional[str] = None,
    deny_tools: Optional[str] = None,
    computed_dirs: "Sequence[str]" = (),
) -> list[str]:
    """x-aaaa: claude's own spellings for the Tier-3 passthrough flags, in a fixed
    order (--add-dir/--agent/--allowedTools/--disallowedTools), skipping empty.
    claude-only (the fail-closed for other providers lives at the CLI/client
    guard); kept identical to the Rust HarnessFlags::push_onto for argv parity.

    ``computed_dirs`` is fno's own writable-directory set. It rides the SAME
    decision the pane funnel uses (:func:`fno.agents.writable_dirs.add_dir_tokens`)
    rather than a second copy of it here: a claude worker with no grant for the
    state root fails to write the claim store exactly as a codex one does. The
    repository's ``.claude/settings.json`` carries no permissions, so a fresh
    user's claude worker has no ``permissions.additionalDirectories`` to mask it."""
    from fno.agents.writable_dirs import add_dir_tokens

    def _unreachable(flag: str) -> object:  # pragma: no cover - claude maps every cell
        raise AssertionError(f"claude has no {flag} mapping")

    # Order matches the Rust HarnessFlags::push_onto exactly: the operator's
    # own --add-dir, then --agent/--allowedTools/--disallowedTools, then the
    # computed set last. The parity claim in this docstring is only true if the
    # INTERLEAVING matches too, and nothing catches a drift here - the Rust test
    # filters to --add-dir pairs and the Python parity assertions pass cwd=None,
    # so the computed half is empty on both sides of every existing comparison.
    out: list[str] = add_dir_tokens(
        "claude", add_dir, (), unsupported=_unreachable
    )
    for flag, value in (
        ("--agent", agent),
        ("--allowedTools", tools),
        ("--disallowedTools", deny_tools),
    ):
        if value:
            out += [flag, value]
    out += add_dir_tokens("claude", None, computed_dirs, unsupported=_unreachable)
    return out


def _claude_project_slug(cwd: Path) -> str:
    """Claude's projects-dir slug for a launch cwd.

    Delegates to the canonical encoder in provenance.resolver (confirmed
    against the real ~/.claude layout): only '/' and '.' become '-', every
    other char (including '_') is preserved. A local regex that folded all
    non-alphanumerics would diverge on underscore-bearing paths and point the
    receipt at a directory that does not exist.
    """
    from fno.provenance.resolver import _slug

    return _slug(str(cwd))


def _claude_projects_dir(env: Mapping[str, str]) -> Path:
    """The projects root claude writes transcripts under for this spawn.

    An ``--account`` spawn relocates the whole config tree via
    ``CLAUDE_CONFIG_DIR`` (see account_env), and claude writes ``projects/``
    beneath that relocated root, not ``~/.claude/projects``. Deriving the
    locator from the effective env keeps the receipt pointing where the
    transcript will actually land instead of a dir it will never appear in.
    """
    cfg = env.get("CLAUDE_CONFIG_DIR")
    root = Path(cfg) if cfg else Path.home() / ".claude"
    return root / "projects"


def _resolved_request_model(
    model: Optional[str], env: Mapping[str, str]
) -> Optional[str]:
    """The model claude will actually serve, best-effort from argv + env.

    An explicit argv model pin (``claude --model``) wins over a role's
    ``ANTHROPIC_MODEL`` env (the provider contract: the pin is more specific),
    but a tier alias still remaps to its ``ANTHROPIC_DEFAULT_<ALIAS>_MODEL``
    when set. When the argv model is None the model comes from
    ``ANTHROPIC_MODEL`` if the spawn was routed that way: cli.py's
    ``--provider/--model`` path clears the argv token and routes via env, so
    treating a missing argv model as "no model" would misreport a routed spawn.
    """
    if model:
        alias = str(model).strip().lower()
        return env.get(f"ANTHROPIC_DEFAULT_{alias.upper()}_MODEL") or model
    return env.get("ANTHROPIC_MODEL")


def _emit_headless_receipt(
    *, name: Optional[str], cwd: Path, model: Optional[str], env: Mapping[str, str]
) -> None:
    """Write one compact JSON receipt line to stderr and flush it, before the
    blocking call.

    stderr (not stdout): headless stdout IS claude's reply stream, and a
    structured consumer like pr_watch runs ``json.loads`` on the full stdout
    under ``--output-format json``, so a receipt line there corrupts the
    envelope. A progress receipt belongs on stderr; stdout stays the clean
    reply channel.

    Promises only what is knowable at this synchronous pre-block boundary:
    substrate, name, the effective cwd, the resolved request model, and the
    transcript locator directory. It deliberately does NOT name an exact
    transcript file, session UUID, or PID - none exist yet, and inventing them
    would repeat the receipt-can-lie failure the receipt exists to close.
    """
    receipt = {
        "substrate": "headless",
        "name": name,
        "cwd": str(cwd),
        "model": _resolved_request_model(model, env),
        "transcript_dir": f"{_claude_projects_dir(env) / _claude_project_slug(cwd)}/",
        "lifecycle": "ephemeral",
        "roster": "unbound",
    }
    sys.stderr.write(json.dumps(receipt) + "\n")
    sys.stderr.flush()


def headless_create(
    message: str,
    cwd: Path,
    timeout: Optional[int] = None,
    model: Optional[str] = None,
    permission_mode: Optional[str] = None,
    effort: Optional[str] = None,
    add_dir: Optional[str] = None,
    agent: Optional[str] = None,
    tools: Optional[str] = None,
    deny_tools: Optional[str] = None,
    output_format: Optional[str] = None,
    account_env: Optional[Mapping[str, str]] = None,
    route_env: Optional[Mapping[str, str]] = None,
    name: Optional[str] = None,
    passthrough: Optional[Sequence[str]] = None,
) -> ProviderResult:
    """Run a one-shot ``claude -p`` without creating a background session.

    Emits one compact JSON receipt line to stdout and flushes it before the
    blocking ``claude -p`` call, so a watcher sees the worker started without
    waiting for the one-shot to return. ``name`` labels that receipt; it does
    not reach the claude argv.
    """
    if route_env:
        from fno.agents.model_routing import resolve_spawn_route

        route_env = resolve_spawn_route(None, route_env)
    argv = ["claude", "-p"]
    # apply an explicit --route via --settings, same as bg_create. A
    # direct `claude -p` subprocess would inherit route env too, but --settings
    # keeps the two lanes identical and is the mechanism that survives the bg
    # daemon fork, so the route behaves the same whichever substrate the caller
    # picked. An --account spawn gets its auth/model scrub via the same channel
    # for the same reason (and for parity with bg); the env scrub still runs as
    # the floor below.
    from fno.agents.model_routing import route_settings_path_for

    settings_path = route_settings_path_for(route_env, account_env)
    if settings_path:
        argv += ["--settings", settings_path]
    if permission_mode:
        argv += ["--permission-mode", permission_mode]
    else:
        argv.append("--dangerously-skip-permissions")
    if model:
        argv += ["--model", model]
    if effort:
        argv += ["--effort", effort]
    # x-aaaa: Tier-3 passthrough, same order as the Rust headless builder.
    argv += _tier3_tokens(
        add_dir, agent, tools, deny_tools, computed_dirs=worker_writable_dirs(cwd)
    )
    if output_format:
        argv += ["--output-format", output_format]
    if passthrough:
        from fno.agents.mux_spawn import refuse_passthrough_collision

        refuse_passthrough_collision(passthrough, argv)
        argv = [*argv, *passthrough]
    # Behind `--` like every other claude seed: a leading-flag seed must be
    # the prompt positional, not a claude flag.
    argv += ["--", message or "hello"]
    # A one-shot `claude -p` inherits the parent env, so an ambient
    # ANTHROPIC_API_KEY / CLAUDE_CODE_OAUTH_TOKEN would override BOTH a per-spawn
    # account overlay AND a routed --settings token (codex P1):
    # claude prefers an env credential over the settings file, so a routed
    # headless spawn from a logged-in shell would authenticate with the primary
    # Claude account instead of the selected provider (routing failure / wrong
    # billing). The credentials compose scrubs the inherited auth vars
    # whenever we route or pin an account and applies the same scrub/account/
    # route precedence as every other seam, belt-and-suspenders with
    # --settings. Without either overlay, inherit the parent env untouched.
    spawn_env: Optional[dict[str, str]] = None
    # Identity scrubbing is NOT done here: worker_environment() below is the
    # floor every adapter's child env crosses, so an adapter cannot
    # decline it and the next adapter cannot miss it. The env is constructed
    # only when an overlay or an incoherent model env demands one; a coherent,
    # overlay-free call still passes None and inherits byte-identically (there
    # is a test on that) - the floor's scrub is a no-op on a marker-free env,
    # and the caller-side comparison constructs the scrubbed env when a marker
    # was present.
    from fno.agents.model_routing import (
        incoherent_model_env,
        overlay_restores_model_env,
        scrub_incoherent_model_env_and_notify,
        unrouted_model_keys,
    )

    _incoherent = incoherent_model_env()
    # A stamp with no route behind it is the same class of stale inherited
    # marker as an incoherent model env, and it earns an env for the same
    # reason: leaving it makes the child report a provider it is not billing.
    from fno.agents.model_routing import ROUTE_PROVIDER_ENV, inherited_route_stamp

    _stale_stamp = bool(inherited_route_stamp()) and not route_env
    # An unrouted child carries no model claim: build the env (rather than
    # bare-inheriting os.environ) whenever the shell holds one, so the scrub
    # below can clear it.
    _restores_model = overlay_restores_model_env(account_env, route_env)
    _unrouted_claims = () if _restores_model else unrouted_model_keys(os.environ)
    if account_env or route_env or _incoherent or _stale_stamp or _unrouted_claims:
        spawn_env = dict(os.environ)
        if _stale_stamp:
            spawn_env.pop(ROUTE_PROVIDER_ENV, None)
        # Strip before the overlay below so a real route still wins.
        scrub_incoherent_model_env_and_notify(
            spawn_env, routed=_restores_model
        )
        if account_env or route_env:
            from fno.agents.spawn_overlay_client import compose_credentials

            spawn_env = compose_credentials(account_env, route_env, spawn_env)["env"]
    from fno.harness_identity import worker_environment

    proxy_env = worker_environment(spawn_env or os.environ)
    if spawn_env is not None or proxy_env != dict(os.environ):
        spawn_env = proxy_env
    from fno.harness_identity import stamp_child_harness_identity

    spawn_env = stamp_child_harness_identity(
        spawn_env, "claude", agent_self=name, substrate="headless"
    )
    started = time.monotonic()
    # Pass env ONLY when set: no --account must inherit the parent env by
    # omitting the kwarg entirely (byte-identical to a bare subprocess.run).
    run_kwargs: dict[str, Any] = {
        "cwd": str(cwd),
        "capture_output": True,
        "text": True,
        "timeout": timeout,
    }
    if spawn_env is not None:
        run_kwargs["env"] = spawn_env
    # Emit the spawn receipt BEFORE the synchronous blocking claude -p. A
    # one-shot holds this process until claude -p returns (potentially long),
    # so without an up-front line a watcher sees nothing and may assume the
    # worker never started. Names the effective cwd + transcript locator so a
    # canonical-dir liveness check does not miss a worktree-rooted transcript.
    _emit_headless_receipt(
        name=name,
        cwd=cwd,
        model=model,
        env=spawn_env if spawn_env is not None else os.environ,
    )
    try:
        result = _subprocess_run(argv, **run_kwargs)
    except subprocess.TimeoutExpired as exc:
        raise ProviderSubprocessError(124, f"claude -p timed out after {exc.timeout}s") from exc
    except FileNotFoundError as exc:
        raise ProviderSubprocessError(127, f"claude CLI not found: {exc}") from exc
    if result.returncode != 0:
        raise ProviderSubprocessError(result.returncode, result.stderr or "")
    return ProviderResult(
        exit_code=0,
        stdout=result.stdout or "",
        stderr=result.stderr or "",
        duration_ms=int((time.monotonic() - started) * 1000),
    )


# ===========================================================================
# US2 — follow-up via messaging socket
# ===========================================================================
#
# Reverse-engineered from claude 2.1.143 (functions BG8/CE7/Ag5/IE7); see
# the design doc and _claude_session_registry for surface-level docs. The
# adapter is intentionally a thin wrapper: a future MCP-channel-server
# replacement (US6) swaps this module and leaves dispatch.py untouched.

# Connect timeout for the 250 ms liveness probe. Short enough to fail fast
# when the messaging socket is dead, long enough to absorb the kernel's
# AF_UNIX setup. Tunable for tests but never exposed on the CLI.
_LIVENESS_PROBE_TIMEOUT_SEC = 0.25

# Socket timeout for the actual send. 5s mirrors claude's documented
# read deadline on the receive side (function CE7).
_SEND_SOCKET_TIMEOUT_SEC = 5.0


class ProviderOrphanError(RuntimeError):
    """Raised when the target bg session can't be reached for follow-up.

    ``reason`` is one of ``"not-found"`` (no sessions/<pid>.json entry
    matches the short-id), ``"socket-null"`` (matched entry has
    ``messagingSocketPath: null``; session is suspended),
    ``"liveness-failed"`` (socket miss confirmed by transcript truth), or a
    ``*-live-inject-failed`` routing gap that must never stamp the row orphaned.
    The dispatch layer maps every reason to exit code 13.
    """

    def __init__(self, *, reason: OrphanReason, short_id: str, detail: str = "") -> None:
        super().__init__(
            f"agent short-id {short_id!r} is not reachable (reason: {reason})"
            + (f": {detail}" if detail else "")
        )
        self.reason: OrphanReason = reason
        self.short_id = short_id
        self.detail = detail


class ProviderSocketError(RuntimeError):
    """Raised when the AF_UNIX send fails (connect/write/close error).

    The underlying ``OSError`` is preserved as ``__cause__`` so the
    dispatcher can include the verbatim error message in stderr.
    """

    def __init__(self, message: str) -> None:
        super().__init__(f"messaging socket error: {message}")


class ProviderTimeoutError(RuntimeError):
    """Raised when state.json fails to transition past baseline in time.

    Carries ``elapsed_sec`` so the dispatcher can format the AC-required
    `message sent but no reply within <N>s` stderr line.
    """

    def __init__(self, *, elapsed_sec: float, short_id: str = "") -> None:
        super().__init__(
            f"timed out waiting for reply after {elapsed_sec:.1f}s"
            + (f" (short_id={short_id})" if short_id else "")
        )
        self.elapsed_sec = elapsed_sec
        self.short_id = short_id


def liveness_probe(sock_path: str) -> bool:
    """Return ``True`` iff a 250 ms connect to ``sock_path`` succeeds.

    Catches every ``OSError`` (ENOENT, ECONNREFUSED, timeout). The
    probe immediately closes the socket; it does not write or read.
    """
    sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    sock.settimeout(_LIVENESS_PROBE_TIMEOUT_SEC)
    try:
        try:
            sock.connect(sock_path)
        except OSError:
            return False
        return True
    finally:
        try:
            sock.close()
        except OSError:
            pass


# ---------------------------------------------------------------------------
# US3 — read-path additions: claude_agents_json() + logs()
# ---------------------------------------------------------------------------

# Locked Decision 1 — 3-second shellout timeout for the live-status probe.
_AGENTS_JSON_TIMEOUT_DEFAULT = 3.0

# Locked Decision 3 — 500ms polling cadence for --follow without an
# inotify/fsevents dep. Matches US2's state.json polling pattern.


# The OUTPUT vocabulary: what this parser promises its callers, regardless of
# which spelling the binary used. A resolved value outside this set surfaces a
# forensic warning so vocabulary drift is loud, not silent; it still passes
# through unchanged so consumers that already adapted aren't blocked on us.
KNOWN_LIVE_STATUSES = frozenset({"Working", "Needs input", "Idle", "Done"})

NOT_BLOCKED_STATUSES_LOWER = frozenset(
    s.lower() for s in KNOWN_LIVE_STATUSES if s != "Needs input"
)

# The INPUT vocabulary: every spelling observed from a real binary, mapped onto
# the output set. Widening the output set instead (accepting "blocked" as its
# own value) is what let raw lowercase leak to every consumer, so `Needs input`
# was never produced and downstream Title-case comparisons silently never
# matched. Keeping the two sets separate is what keeps a NEW value loud.
_LIVE_STATUS_INPUT = {
    "working": "Working",
    "busy": "Working",
    "blocked": "Needs input",
    "needs input": "Needs input",
    "idle": "Idle",
    "done": "Done",
    "failed": "Done",
    # claude emits this routinely for a terminal row with no live
    # turn. Liveness is the axis this vocabulary carries, and a stopped
    # session has none - same as a failed one above. Outcome (why it
    # stopped) is a different axis this vocabulary does not carry; do not
    # widen this mapping to answer that question.
    "stopped": "Done",
    # The registry's own spellings fold through the same map, so both row
    # producers (claude's roster and fno's registry) read one watchdog
    # vocabulary: `live`/`spawning`/`restarting` are alive states, `ready` is
    # alive with no turn in flight, the same axis as Idle.
    "live": "Working",
    "spawning": "Working",
    "restarting": "Working",
    "ready": "Idle",
}

# Field aliases for the row schema. claude's `agents --json` emits `id` and
# `state`; earlier versions emitted `short_id` and `status`, which is what this
# parser originally read. Accepting both keeps one code path working across
# versions - and the aliasing is why a schema change is a WARNING here rather
# than a silent empty map: reading only the old names dropped every row, so the
# live_status enrichment returned {} on every call while the unit tests, whose
# fixtures used the old shape, stayed green.
#
# Status alias ORDER is load-bearing, and it is the reverse of the short-id
# order. claude 2.1.220 emits BOTH keys on the rows that carry a live pid, with
# different vocabularies and disagreeing values (`state: working` alongside
# `status: idle`). `state` wins because it is the supervisor field the binary's
# own header renders and it is present on ~94% of rows, where `status` appears
# only on the handful with a live pid. Preferring `status` resolved an actively
# working session to Idle, which read.py then replaced with a stale
# truth-status fallback - the render-Idle-while-working bug.
_SHORT_ID_KEYS = ("short_id", "id")
_STATUS_KEYS = ("state", "status")


def _normalize_live_status(value: Any) -> Any:
    """Map one input spelling onto the output vocabulary; pass anything else."""
    if not isinstance(value, str):
        return value
    return _LIVE_STATUS_INPUT.get(value.strip().lower(), value)


def _alias_value(
    row: dict, keys: tuple[str, ...], valid, normalize=None, recognized=None
) -> tuple[Any, Optional[str]]:
    """Resolve one field across its schema aliases. Returns (value, warning).

    Each alias is validated INDEPENDENTLY and only valid values are considered,
    so a malformed value under one spelling cannot mask a good value under
    another. Taking the first merely-present key would let a row carrying a
    junk ``short_id`` alongside a valid ``id`` resolve to the junk and be
    dropped - the same silent-drop shape this aliasing exists to prevent.

    When two aliases both hold valid but DIFFERENT values there is no way to
    tell which the producer meant, so the first value in ``keys`` order is
    returned with a warning rather than a silent pick - UNLESS ``recognized``
    is given and every found value passes it (AC6/AC8): ``state``
    already wins by the fixed ``keys`` order regardless, so a disagreement
    between two individually-understood values is precedence working as
    documented, not drift. Passing ``recognized`` in (rather than making this
    function itself status-aware) keeps the short-id call site, which never
    passes it, on its original always-warn-on-difference behavior.

    ``normalize`` is applied per alias BEFORE the comparison, so two spellings
    of the same meaning (``state: working`` / ``status: busy``) agree instead of
    warning on every live row, and only a real disagreement is reported.
    The warning still quotes the RAW wire values: an operator reading it is
    debugging vocabulary drift against a real binary, and the normalized form
    is this parser's own output vocabulary, not anything the binary emitted.
    """
    raw = [(key, row.get(key)) for key in keys if valid(row.get(key))]
    found = [(key, normalize(value)) for key, value in raw] if normalize else raw
    if not found:
        return None, None
    value = found[0][1]
    # Compared by equality, not via a set: a status alias is accepted as any
    # non-None JSON value, and hashing a dict/list one would raise TypeError
    # out of a function contracted to be best-effort.
    if any(other != value for _, other in found[1:]):
        if recognized is not None and all(recognized(v) for _, v in found):
            return value, None
        pairs = ", ".join(f"{k}={v!r}" for k, v in raw)
        return value, f"conflicting values across aliases ({pairs}); used {value!r}"
    return value, None


def _valid_short_id(value: Any) -> bool:
    return isinstance(value, str) and bool(value)


def _valid_status(value: Any) -> bool:
    return value is not None


def _row_label(row: Any, index: int, space: str) -> str:
    """Name a row in a warning by something that survives filtering.

    A bare position stopped being a usable pointer once these two enumerators
    walked different lists: `claude_agents_rows` filters interactive rows out,
    so position 0 here is position 2 there, and a drift warning whose whole
    job is aiming an investigation at one record aimed it at another. The
    session id is the row's own identity and moves with it.

    ``space`` names which list the index counts in, for the malformed rows
    that carry no identity at all. Two callers here walk different lists, so
    a single fixed phrase would be wrong at one of them - which is the bug
    this helper was added to fix, one function up.
    """
    if isinstance(row, dict):
        for key in ("sessionId", "id", "short_id", "name"):
            value = row.get(key)
            if isinstance(value, str) and value.strip():
                return f"row {value.strip()}"
    return f"unidentifiable row at position {index} of the {space}"


def claude_agents_rows(
    timeout: float = _AGENTS_JSON_TIMEOUT_DEFAULT,
) -> tuple[list[dict], list[str]]:
    """Shell out to ``claude agents --json --all`` and return the RAW agent rows.

    The one enumeration every fleet consumer reads: ``--all`` is load-bearing
    (without it stopped/completed rows are invisible - 57 of 133 on a real
    fleet - and every sweep reading the narrower view reports success over
    half the fleet). The operator's own interactive rows are skipped by design
    (``kind == "interactive"``); everything else passes through RAW, so a
    consumer that needs claude's own ``state``/``sessionId``/``cwd`` fields
    gets them unmangled while :func:`claude_agents_json` normalizes the subset
    it wants. Best-effort: every failure mode returns ``([], warnings)``.
    """
    # `claude agents --json` alone omits stopped/completed rows; `--all` is
    # the fix; there is no narrower flag that keeps only live rows visible,
    # and no caller here wants that narrower view anyway.
    argv = ["claude", "agents", "--json", "--all"]

    try:
        result = _subprocess_run(
            argv,
            capture_output=True,
            text=True,
            timeout=timeout,
        )
    except subprocess.TimeoutExpired:
        return [], [
            f"claude agents --json timed out after {timeout}s; "
            "live_status unavailable, falling back to registry-only view"
        ]
    except FileNotFoundError:
        return [], [
            "claude agents --json: claude binary not found on PATH; "
            "live_status unavailable, falling back to registry-only view"
        ]
    except OSError as exc:
        return [], [
            f"claude agents --json raised OSError: {exc}; "
            "live_status unavailable, falling back to registry-only view"
        ]

    if result.returncode != 0:
        head = (result.stderr or result.stdout or "").strip()[:200]
        return [], [
            f"claude agents --json exited non-zero ({result.returncode}); "
            f"stderr: {head!r}; falling back to registry-only view"
        ]

    try:
        parsed = json.loads(result.stdout or "{}")
    except json.JSONDecodeError as exc:
        return [], [
            f"claude agents --json parse failure: {exc}; falling back to registry-only view"
        ]

    # Claude's `agents --json` output shape isn't formally pinned: some
    # versions return ``{"agents": [...]}``, others return the bare
    # ``[...]`` array (per the CLI docs). Treat both as valid; the
    # legacy ``{"agents": ...}`` wrapper is unwrapped and a bare list
    # is used directly. Anything else degrades to the warned-fallback
    # path instead of crashing with ``AttributeError`` on ``.get()``.
    rows: Any
    if isinstance(parsed, list):
        rows = parsed
    elif isinstance(parsed, dict):
        rows = parsed.get("agents")
        if not isinstance(rows, list):
            return [], [
                "claude agents --json response missing 'agents' array; "
                "falling back to registry-only view"
            ]
    else:
        return [], [
            f"claude agents --json response has unexpected shape "
            f"({type(parsed).__name__}); falling back to registry-only view"
        ]

    # The operator's own interactive sessions ride in the same array and
    # carry no id BY DESIGN. They are not agents and never were, so they are
    # skipped silently: warning about each one turns a healthy invocation
    # into a wall of "no usable short id", which is how an operator learns
    # to ignore the warnings below. `kind` is the discriminator rather than
    # "did the row have an id", because a drift that renamed BOTH the id and
    # status keys still says `background` and must still be caught.
    #
    # `kind` is itself a drift surface, and this match is exact: a rename to
    # `session_type` or a recase to `Interactive` makes the skip fail OPEN,
    # and every interactive row falls back through to the "no usable short
    # id" path. That degrades to the wall of warnings this skip removed -
    # loud, not silent, which is the direction a diagnostic should fail.
    out: list[dict] = []
    warnings: list[str] = []
    for index, row in enumerate(rows):
        if not isinstance(row, dict):
            warnings.append(f"claude agents --json {_row_label(row, index, 'raw array')} is not "
                f"an object; skipped")
            continue
        if row.get("kind") == "interactive":
            continue
        out.append(row)
    return out, warnings


def claude_agents_json(
    timeout: float = _AGENTS_JSON_TIMEOUT_DEFAULT,
) -> tuple[dict[str, dict], list[str]]:
    """Shell out to ``claude agents --json --all`` and return a short-id map.

    Raw enumeration is :func:`claude_agents_rows` (``--all`` included, so
    stopped/completed rows survive into this map too); this wrapper normalizes
    each row onto the ``live_status`` axis. Best-effort. Returns
    ``({}, warnings)`` on every failure mode so the caller can fall back to
    registry-only data (AC1-FR). Failure modes that map to a non-empty
    warning list:

    - ``FileNotFoundError`` — ``claude`` binary missing from PATH.
    - ``subprocess.TimeoutExpired`` — exceeded the per-call timeout.
    - non-zero exit code — claude itself reported an error.
    - ``json.JSONDecodeError`` — stdout was not parseable JSON.
    - structural drift — a record carrying none of the accepted short-id
      keys is dropped with a forensic warning naming the keys tried
      (AC3-FR).

    The success shape:

    ::

        {
            "<short_id>": {"live_status": "Working" | "Needs input" | "Idle" | "Done"},
            ...
        }

    Per Locked Decision 1, the source of live-status truth is claude's
    supervisor view — replicating that state in fno would be duplicate
    truth. We translate claude's field name to our orthogonal
    ``live_status`` axis (Locked Decision 6). That field name is versioned:
    claude emits ``id``/``state`` today and emitted ``short_id``/``status``
    earlier, so both spellings are accepted (``_SHORT_ID_KEYS`` /
    ``_STATUS_KEYS``) rather than pinning one and silently dropping every
    row on the other. Values are normalized onto the output vocabulary above
    (``_LIVE_STATUS_INPUT``), so a caller reads one set of names no matter
    which spelling the binary used; an unmapped value passes through with a
    drift warning.
    """
    rows, warnings = claude_agents_rows(timeout=timeout)
    out_map: dict[str, dict] = {}
    agent_rows = len(rows)
    # both drift families below are collected per DISTINCT shape while
    # iterating and emitted once each after the loop, rather than once per
    # row. A value drift is loud once no matter how many rows carry it
    # (AC7-EDGE); a genuine fleet can carry the same unmapped value or the
    # same alias disagreement on dozens of rows, and a line per row buries the
    # one fact ("stopped is unmapped") under noise that scales with fleet
    # size instead of with distinct causes.
    unrecognized_status_counts: dict[str, int] = {}
    conflict_message_counts: dict[str, int] = {}

    def _status_recognized(v: Any) -> bool:
        return isinstance(v, str) and v in KNOWN_LIVE_STATUSES

    for index, row in enumerate(rows):
        short_id, id_warning = _alias_value(row, _SHORT_ID_KEYS, _valid_short_id)
        if short_id is None:
            warnings.append(
                f"claude agents --json {_row_label(row, index, 'non-interactive rows')} has no usable short id "
                f"under any of {list(_SHORT_ID_KEYS)}; skipped"
            )
            continue
        if id_warning:
            warnings.append(
                f"claude agents --json {_row_label(row, index, 'non-interactive rows')} short id {id_warning}"
            )
        # `recognized` scopes the alias-conflict suppression to THIS call site
        # only (AC6/AC8/AC9): a disagreement between two values that
        # both already resolve to a known live-status is precedence working
        # as documented (`state` wins - unchanged), not drift, so it stays
        # quiet. A disagreement touching a genuinely unknown value still
        # warns; the short-id call above passes no `recognized` and keeps its
        # original always-warn-on-difference behavior untouched.
        live_status, status_warning = _alias_value(
            row,
            _STATUS_KEYS,
            _valid_status,
            normalize=_normalize_live_status,
            recognized=_status_recognized,
        )
        if status_warning:
            conflict_message_counts[status_warning] = (
                conflict_message_counts.get(status_warning, 0) + 1
            )
        # The isinstance guard short-circuits before the frozenset lookup: a
        # non-string status (dict/list under drift) is itself unrecognized, and
        # hashing it here would raise TypeError out of a best-effort probe.
        if live_status is not None and (
            not isinstance(live_status, str) or live_status not in KNOWN_LIVE_STATUSES
        ):
            key = repr(live_status)
            unrecognized_status_counts[key] = unrecognized_status_counts.get(key, 0) + 1
        out_map[short_id] = {"live_status": live_status}
    for value_repr, count in unrecognized_status_counts.items():
        rows_word = "row" if count == 1 else "rows"
        warnings.append(
            f"claude agents --json {count} {rows_word} with unrecognized status="
            f"{value_repr} (expected one of {sorted(KNOWN_LIVE_STATUSES)}); "
            "passing through unchanged"
        )
    for message, count in conflict_message_counts.items():
        rows_word = "row" if count == 1 else "rows"
        warnings.append(f"claude agents --json {count} {rows_word} status {message}")
    # A per-row warning is quiet enough that a schema change dropping EVERY row
    # shipped unnoticed once already: 42 warnings into a list most callers never
    # print, and an empty map that reads exactly like "no agents running". A
    # total drop is a different claim from a partial one, so it says so once,
    # over the agent rows counted above.
    # The tail names the SHORT ID, not live_status: every row that resolves a
    # short id lands in out_map regardless of what its status field held, so an
    # empty map can only mean short-id resolution failed on all of them. Naming
    # live_status as the failure would point a drift investigation at the one
    # axis that was never reached.
    if agent_rows and not out_map:
        warnings.append(
            f"claude agents --json: 0 of {agent_rows} agent rows parsed; "
            "schema drift? no usable short id on any of them, "
            "so live_status is unavailable for every agent"
        )
    return out_map, warnings


# ---------------------------------------------------------------------------
# US4-lifecycle shellouts: claude stop / rm / attach + reconcile reachability
# probe. Each helper is a thin wrapper around ``claude <command> <short_id>``
# with a timeout. They live here so the fno ``dispatch`` layer can stay
# provider-agnostic and so the per-provider quirks (capture vs inherit-stdio
# for the interactive attach command) sit alongside the ``claude_agents_json``
# adapter.
# ---------------------------------------------------------------------------


def claude_logs_reachable(short_id: str, *, timeout: float = 10.0) -> bool:
    """Cheap supervisor-reachability probe used by ``reconcile``.

    Invokes ``claude logs <short_id> --tail 1`` with output suppressed
    and decides three states:

    - Exit 0 → return ``True`` (supervisor reachable; flip to live).
    - Exit non-zero → return ``False`` (supervisor lost the session;
      flip to orphaned).
    - Timeout / OSError / FileNotFoundError → raise
      :class:`ReachabilityProbeError` (probe failed, preserve status;
      reconcile routes to errors with a reason discriminator).

    Locked Decision 9 caps the probe at 10s — longer than the
    supervisor's typical startup, short enough that a degenerate
    registry of 100+ entries does not freeze the operator's shell.

    Args:
        short_id: 8-hex claude short-id.
        timeout: wall-clock seconds (default 10).

    Raises:
        ReachabilityProbeError: probe could not produce a definitive
            answer; caller should preserve status. ``provider`` is
            ``"claude"`` by construction so reconcile routes the error
            with a per-provider reason discriminator.
    """
    try:
        result = _subprocess_run(
            ["claude", "logs", short_id, "--tail", "1"],
            capture_output=True,
            text=True,
            timeout=timeout,
        )
    except subprocess.TimeoutExpired as exc:
        raise ReachabilityProbeError(provider="claude", reason=f"timeout after {timeout}s") from exc
    except FileNotFoundError as exc:
        # The caller's is_provider_available check should make this
        # unreachable, but defensive-catch keeps the contract explicit.
        raise ReachabilityProbeError(provider="claude", reason="claude vanished mid-probe") from exc
    except OSError as exc:
        raise ReachabilityProbeError(provider="claude", reason=str(exc)) from exc
    return result.returncode == 0


# =====================================================================
# Task 1.2 — single-writer session claim guard (stream-json host lane)
# =====================================================================
#
# Before the daemon respawns an idle claude session into the stream-json
# host lane (`claude -p --resume <uuid> ...`), fno must enforce
# single-writer: `claude --resume` does NOT itself guard against a live
# duplicate (only `--continue` skips live sessions), so two writers on one
# transcript would corrupt it. Two guards, in order:
#   1. Liveness: refuse if the bg session's supervisor is reachable (a human
#      interactive TUI / another writer is using it right now).
#   2. Atomic claim: acquire `fno agents claim session:<uuid>` (O_CREAT|O_EXCL) so two
#      concurrent adopts cannot both respawn the same transcript.
# Reuses the existing `fno agents claim` primitive (no new substrate). Session claims
# are host-global (a session is host-wide, not project-scoped), so they live
# under `global_claims_root()` (~/.fno/claims).


# =====================================================================
# MCP channel reachability probe
# =====================================================================
#
# Locked Decision 9 specifies ``mcp_channel_id`` as a server-generated
# UUIDv4. The value is populated from ``claude_short_id`` (1:1) so the
# sidecar can route by its native session id without an additional
# id-translation layer. The ask transport that used to ride this channel
# was ported to the Rust runtime; the probe survives for the dispatcher's
# liveness scan.


def _mcp_status_argv(timeout: float) -> list[str]:
    """Argv for the native sidecar status door: the fno-agents-worker
    binary beside the resolved fno-agents binary, else PATH. The probe's
    one subprocess (the claims-forward shape); the caller's budget rides
    through as --timeout-secs so a wedged sidecar cannot stretch a
    250 ms probe into five seconds.
    """
    from fno import rust_binary

    argv_tail = ["--mcp-exec", "status", "--timeout-secs", repr(timeout)]
    agents_bin = rust_binary.resolve_binary()
    if agents_bin is not None:
        worker = agents_bin.parent / "fno-agents-worker"
        if worker.is_file() and os.access(worker, os.X_OK):
            return [str(worker)] + argv_tail
    return ["fno-agents-worker", *argv_tail]


def mcp_channel_reachable(
    mcp_channel_id: str,
    *,
    timeout: float = 0.25,
) -> bool:
    """Tri-state MCP-channel reachability probe.

    - Returns ``True`` when the sidecar reports the channel as registered
      (reconcile flips to ``"live"``).
    - Returns ``False`` when the sidecar is alive AND reports no such
      channel id — the session is definitively orphaned from the MCP
      side (reconcile flips to ``"orphaned"``).
    - Raises :class:`ReachabilityProbeError` with ``provider="claude"``
      and ``reason="mcp_channel_disconnected"`` when the sidecar itself
      is unreachable (transient I/O, server not listening, parse
      failure). Reconcile MUST preserve status in this case.

    Default ``timeout`` is 250ms — fast enough to keep follow-up latency
    bounded; the sidecar is local, so any healthy response returns
    sub-50ms in practice.

    Spec: LD10 — ``ReachabilityProbeError`` is the lifted base class
    from US4-gemini Wave 1.1; ``reason="mcp_channel_disconnected"`` is
    the discriminator for this probe.

    The probe rides the native sidecar door (`fno-agents-worker
    --mcp-exec status`): exit 0 parses the status JSON, exit 1 (the
    sidecar unreachable) raises the probe error, so the tri-state
    contract holds with no Python sidecar client left.
    """
    import subprocess

    completed = subprocess.run(
        _mcp_status_argv(timeout),
        capture_output=True,
        text=True,
        timeout=timeout + 5.0,
    )
    if completed.returncode != 0:
        raise ReachabilityProbeError(
            provider="claude",
            reason="mcp_channel_disconnected",
        )
    try:
        resp = json.loads(completed.stdout)
    except json.JSONDecodeError as exc:
        raise ReachabilityProbeError(
            provider="claude",
            reason="mcp_channel_disconnected",
        ) from exc

    channels = resp.get("channels") or []
    if not isinstance(channels, list):
        # Garbled sidecar response — treat as inconclusive, not orphaned.
        raise ReachabilityProbeError(
            provider="claude",
            reason="mcp_channel_disconnected",
        )

    for entry in channels:
        if not isinstance(entry, dict):
            continue
        if entry.get("session_id") == mcp_channel_id:
            return True
    return False
