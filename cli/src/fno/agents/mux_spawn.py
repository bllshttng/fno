"""Mux-pane spawn back half (4a-G2): host an agent's PTY as a mux pane.

``fno agents spawn`` (the closable pane lane) lands here: the
front half - name validation, provider selection, per-agent flock, collision
check, role routing, billing guard - is the same machinery the daemon/bg paths
use; only the HOSTING call differs. Instead of the fno-agents daemon spawning
a PTY worker, this subprocesses ``fno mux pane run --server <s> --cwd <cwd>
-- env <mesh env> <provider argv>`` (the G1 script API), parses the
machine-readable pane id off stdout, and writes the registry row with the
``mux: {session, pane_id}`` ref (create-after-spawn: a failed spawn writes NO
row, and there is never a silent daemon-PTY fallback - AC1-ERR).

The mux server itself sets ``FNO_SERVER`` (and the legacy ``FNO_SESSION``)
with ``FNO_PANE`` in the pane child env
(crates/fno pty.rs); the mesh identity (``FNO_AGENT_SELF``/``FNO_AGENT_HARNESS``)
rides an ``env(1)`` wrapper because ``pane run`` carries argv, not env.

Interactive argv per provider mirrors the Rust daemon providers
(crates/fno-agents/src/provider.rs) - the subscription-billed interactive
forms, never ``-p``/``--print`` (D2 billing guard, re-checked here before any
pane exists).
"""

from __future__ import annotations

import functools
import json
import os
import re
import subprocess
from pathlib import Path
from typing import Callable, Optional, Sequence

from fno.agents.dispatch_errors import DispatchAskError
from fno.agents.harness_map import DispatchResolveError, normalize_command
from fno.config._dispatch_commands import is_command_seed
from fno.agents.writable_dirs import (
    add_dir_tokens,
    strip_remote_add_dirs,
    worker_writable_dirs,
)
from fno.agents.model_routing import DEFAULT_SECONDARY_MODEL


#: Bound on the `pane run` / `pane ls` subprocesses. `pane run` includes a
#: possible server self-spawn + squad git resolve (~2s worst case), so this is
#: generous next to reality, tight next to a wedged mux.
_MUX_SUBPROCESS_TIMEOUT_S = 30

#: Per-harness spelling for a model selected by the route table. The route
#: table owns the model choice; this map only adapts its provider/model id to a
#: harness that requires the provider prefix in its argv.
_PER_HARNESS_MODEL_SPELLING = {
    "opencode": f"zai-coding-plan/{DEFAULT_SECONDARY_MODEL}",
}


def _fno_bin() -> str:
    """The `fno` front-door binary (the Rust mux owner). ``FNO_BIN`` overrides
    for tests and non-PATH installs."""
    return os.environ.get("FNO_BIN") or "fno"


# Moved to fno.agents.mux_server (file budget); re-exported here.


def permission_pane_tokens(provider: str, mode: str, role_level: Optional[int] = None) -> list[str]:
    """Map a ``--permission-mode`` value to provider-native pane argv tokens.

    Fail-closed (Locked Decision 1): an unmappable (provider, value) pair raises
    before any spawn - permissions are a trust boundary, never a silent
    downgrade. The vocabulary lives in Rust (crates/fno-agents/src/codex_posture.rs),
    so this is a transport bridge: one subprocess round-trip.
    agy ``skip`` maps to ``[]`` because its argv already carries
    ``--dangerously-skip-permissions`` unconditionally. An empty mode with a
    ``role_level`` asks the owner's role-aware pane default (a promoted pane
    launches full access); an empty mode without one is refused by the owner
    (the same literal, one source)."""
    from fno.rust_binary import command_call
    from fno.rust_binary import CommandUnavailable

    payload: dict = {"provider": provider, "mode": mode}
    if role_level is not None:
        payload["role_level"] = role_level
    try:
        answer = command_call("permission-tokens", payload, CommandUnavailable)
    except CommandUnavailable as exc:
        raise DispatchAskError(
            f"the permission vocabulary owner (fno-agents) is unavailable: {exc}",
            exit_code=2,
        ) from exc
    if answer.get("refusal"):
        raise DispatchAskError(answer["refusal"], exit_code=2)
    return [str(token) for token in answer.get("tokens") or []]


_CURSOR_NATIVE_WORKTREE_FLAGS = frozenset({"-w", "--worktree", "--worktree-base"})


def refuse_cursor_native_worktree_tokens(passthrough: Optional[Sequence[str]]) -> None:
    """Refuse Cursor's native worktree controls at the passthrough boundary."""
    for token in passthrough or ():
        flag = token.split("=", 1)[0]
        if flag in _CURSOR_NATIVE_WORKTREE_FLAGS:
            raise DispatchAskError(
                f"cursor-agent passthrough refuses native worktree flag {flag!r}; "
                "fno owns the worker worktree",
                exit_code=2,
            )


#: The minimum codex CLI version that ships --dangerously-bypass-hook-trust.
#: An older codex's clap parser rejects an unrecognized flag outright, so the
#: flag must never be emitted for a version below this - and an OLDER codex
#: predates the hook-trust modal it exists to clear, so omitting it there is
#: not a regression, just a no-op.
_CODEX_HOOK_TRUST_FLAG_MIN_VERSION = (0, 148, 0)


@functools.lru_cache(maxsize=1)
def _codex_cli_version() -> Optional[tuple]:
    """The installed ``codex`` CLI's (major, minor, patch), memoized once per
    process so a version check never costs more than one subprocess call no
    matter how many panes this process spawns. None on any miss (missing
    binary, timeout, unparseable output) - treated as "assume incompatible"
    by every caller, never as "assume compatible"."""
    if (pinned := os.environ.get("FNO_CODEX_VERSION")) is not None:
        found = re.search(r"(\d+)\.(\d+)\.(\d+)", pinned)
        return tuple(int(part) for part in found.groups()) if found else None
    try:
        proc = subprocess.run(
            ["codex", "--version"], capture_output=True, text=True, timeout=5
        )
    except (OSError, subprocess.SubprocessError):
        return None
    if proc.returncode != 0:
        return None
    match = re.search(r"(\d+)\.(\d+)\.(\d+)", (proc.stdout or "") + (proc.stderr or ""))
    if not match:
        return None
    return tuple(int(part) for part in match.groups())


def codex_hook_trust_args() -> list[str]:
    """Trust fno's own plugin hooks on every launched codex; empty on an old codex."""
    too_old = (_codex_cli_version() or (0, 0, 0)) < _CODEX_HOOK_TRUST_FLAG_MIN_VERSION
    return [] if too_old else ["--dangerously-bypass-hook-trust"]


def build_pane_argv(
    provider: str,
    message: str,
    cwd: Path,
    session_uuid: Optional[str],
    name: Optional[str] = None,
    passthrough: Optional[Sequence[str]] = None,
    computed_dirs: Optional[Sequence[str]] = None,
    role_level: Optional[int] = None,
) -> list[str]:
    """The interactive PANE argv for ``provider`` - the bare-TUI form a mux
    pane hosts. This is DISTINCT from each provider's Rust ``create_argv``
    (crates/fno-agents/src/provider.rs), which builds the HEADLESS one-shot
    form for the `--substrate headless` lane; the two intentionally differ
    (e.g. opencode: bare ``opencode --prompt`` here vs ``opencode run
    --dangerously-skip-permissions`` there, NOT the stale docs' ``--auto``); no parity contract.

    ``name`` is the worker's registry name, forwarded to claude's ``--name``
    (its session DISPLAY name). Parity with the bg lane, which has always
    hardcoded it (``claude_ask.rs`` ``build_argv``). Without it claude falls
    back to a name inherited from the launching session's lineage, so every
    pane worker on one box shows the SAME string in any session list that reads
    it - N distinct workers collapse onto one row and the list cannot route.
    Only claude is wired: the other pane arms have no verified equivalent flag,
    and guessing one fails the spawn rather than degrading.

    ``passthrough`` : tokens after a ``--`` on the spawn command line,
    spliced INSIDE the arm - upstream of the composed-argv refusals in
    :func:`dispatch_spawn_pane` - so they inherit the same guards fno's own
    flags pass through, rather than appending past them. Absent/empty composes
    a byte-identical argv."""
    message = normalize_command(message, provider) if is_command_seed(message) else message

    from fno.agents.harness_map import is_declared, render_session_argv

    # An undeclared harness has no resume contract to render an identity from,
    # and none is invented: the generic arm below is the bare binary. Any
    # session pinning is a per-vendor flag shape this lane refuses to guess.
    # A DECLARED harness whose interactive_create lane is unsupported (zcode:
    # its TUI is unbuildable on the measured install) is in the same boat for
    # pane purposes - no pane identity exists - and falls through every arm
    # to the refusal below, matching the readable-but-argvless contract.
    identity = [provider]
    if is_declared(provider):
        try:
            identity = render_session_argv(provider, "interactive_create", session_uuid)
        except DispatchResolveError:
            pass

    # resolve the Tier-3 passthrough tokens once, up front, so an
    # unmappable (provider, flag) cell fails closed BEFORE any provider arm builds
    # an argv. Supported cells return the tokens; every arm splices them in below.
    # fno's own state directories: the one cell that survives the axis
    # retirement. Computed here, at the ONE pane funnel, so the arms below
    # cannot disagree about which spawn gets the grant.
    dirs = worker_writable_dirs(cwd) if computed_dirs is None else computed_dirs
    grant = add_dir_tokens(
        provider,
        None,
        dirs,
        unsupported=lambda flag: (_ for _ in ()).throw(
            DispatchAskError(f"{flag} is not supported for harness {provider!r}", exit_code=2)
        ),
    )
    if provider == "claude":
        # `claude --session-id <uuid> [message]`: the pinned session id makes
        # the transcript discoverable and keys the inside-leg reports
        # (handle_report matches claude_session_uuid).
        argv = identity
        if name:
            argv += ["--name", name]
        argv += grant
        if passthrough:
            argv += list(passthrough)
        if message:
            # The seed rides behind `--` so a leading-flag seed ("--model x
            # ...") reaches claude as the prompt positional instead of dying
            # in its flag parser (verified against the real CLI).
            argv += ["--", message]
        return argv
    if provider == "codex":
        # `codex [OPTIONS] [PROMPT]` with no subcommand is the interactive CLI.
        argv = [*identity, "-C", str(cwd)]
        # The bounded default is the thread lane's own CodexPosture::bounded()
        # pair, routed through the Rust owner so pane and thread cannot
        # disagree about what "bounded" means. The mode axis is retired; the
        # lane default is the only answer left. A promoted pane (a lead rank
        # on the spawn) launches at the role-aware default instead - full
        # access, because a lead must write the state root; the
        # owner answers both through one vocabulary.
        argv += permission_pane_tokens(
            "codex",
            "" if role_level is not None else "workspace-write:never",
            role_level=role_level,
        )
        # Codex 0.148 parks a pane on a `Hooks need review` modal for a new or
        # changed hook, and no posture clears it. Sandbox and approval stay.
        argv += codex_hook_trust_args()
        # Any sandboxed posture (including --full-auto and an explicit
        # <sandbox>:<approval>) inherits codex's read-only .git carveout and
        # cannot commit without the grant. Only the two bypass postures skip it.
        from fno.agents.harnesses.codex import git_writable_args, plan_writable_args

        # The bounded posture never carries bypass, so the carveouts always
        # ride now.
        argv += git_writable_args(cwd)
        argv += plan_writable_args(cwd)
        argv += grant
        if passthrough:
            argv += list(passthrough)
        argv = strip_remote_add_dirs(argv)
        if message:
            # Same fence as the claude arm: clap itself prescribes `--` ("to
            # pass ... as a value, use '-- ...'"), so a leading-flag seed is
            # the PROMPT, not a flag.
            argv += ["--", message]
        return argv
    if provider == "gemini":
        # `-i` executes the prompt then stays interactive; --skip-trust avoids
        # the workspace-trust modal blocking the TUI.
        argv = [*identity, "--skip-trust"]
        argv += grant
        # Permission tokens are computed before the passthrough splice so the
        # duplicate-flag refusal sees them (they append after the -i pair, past
        # the splice position). Output order is unchanged. The emitted set also
        # carries the -i flag itself (it rides after the splice) and BOTH
        # permission spellings: the axis is always materialized here under one
        # of the two names, so an alias-shaped passthrough (--yolo against an
        # --approval-mode arm or the reverse) is a named refusal, never a
        # silent last-wins.
        argv += ["--approval-mode", "default"]
        if passthrough:
            argv += list(passthrough)
        if message:
            # argv-fence: exempt (gemini CLI deprecated 2026-07-27; the -i
            # value form is pinned by tests and left as-is).
            argv += ["-i", message]
        return argv
    if provider == "agy":
        # agy (Antigravity) interactive pane (US1). Posture tokens are the
        # Rust owner's answer (agy_launch.keeper_posture, lane pane): the lane
        # default is the never-prompt bypass. This LANE mints no session id;
        # `-p`/`--print` is agy's HEADLESS form and must NOT be used for a
        # pane. The readiness gate submits the seed.
        from fno.agents.spawn_axes_client import keeper_posture

        argv = ["agy", *keeper_posture("agy", "pane")]
        argv += grant
        if passthrough:
            argv += list(passthrough)
        return argv
    if provider == "opencode":
        # Bare `opencode` is the TUI; `opencode run` is the HEADLESS
        # form and must not be pane-hosted. The positional is a PROJECT PATH,
        # not a prompt, so the message rides --prompt (argv pinned from
        # opencode source, packages/opencode/src/cli/cmd/tui.ts). --auto is
        # the never-prompt lane (visible spelling of the hidden
        # --yolo/--dangerously-skip-permissions aliases); non-yolo keeps
        # opencode's default permission prompting for the answer queue.
        argv = identity
        if message:
            # Equal-form binds the value even when it is flag-shaped; yargs
            # misparses the split form there (probed: usage error on a
            # leading-flag value), so never spell this "--prompt", message.
            argv += [f"--prompt={message}"]
        # opencode expects the provider/model form. An explicit --model wins,
        # else the per-harness default table (opencode is the only entry);
        # inject nothing if the table has no entry for this provider.
        _default_model = _PER_HARNESS_MODEL_SPELLING.get(provider)
        if _default_model:
            argv += ["--model", _default_model]
        argv += grant
        if passthrough:
            argv += list(passthrough)
        return argv
    if provider == "pi":
        # pi's WATCHING lane: the plain TUI, never `--mode rpc` (mutually
        # exclusive per process). The pinned `--session-id` JOINS a live rpc
        # session (measured 2026-08-28); a not-yet-existing id is a CREATE,
        # serialised by `fno.agents.harnesses.pi.create_decision`. The ONE pi
        # route owner (keeper and pane share it). No permission popups, so
        # `yolo` maps to nothing.
        from fno.agents.spawn_axes_client import spawn_axes_call

        # The one pi route owner (keeper and pane share it); the axes that
        # fed it are retired, so the ask carries the ambient route only.
        route = spawn_axes_call(
            {"pi_route": {"model": "", "effort": "", "tools": "", "deny_tools": ""}}
        )
        argv = [*identity, *(str(t) for t in route["tokens"])]
        argv += grant
        if passthrough:
            argv += list(passthrough)
        if message:
            # argv-fence: exempt. Probed on pi 0.84.2: `pi -- --version` PRINTS
            # the version, so `--` is not an end-of-options marker here and a
            # fence would be decoration. The seed is a positional and pi offers
            # no equal-form. fno's DRIVING lane is rpc, where the message rides
            # a JSON string field that no parser can read as a flag.
            argv += [message]
        return argv
    if provider == "cursor-agent":
        # cursor-agent's stream-json mode is output-only, so fno drives it
        # through a hosted pty: the pane, or the keeper on the thread lane.
        # create-chat mints the UUID first; the declared form joins that chat.
        # --trust rides the DECLARED form (a second one is a duplicate, not a
        # stronger one) and is NOT the bypass: an untrusted cwd refuses with
        # Workspace Trust Required. -w/--worktree is never passed; the
        # worktree is fno's.
        argv = identity
        argv += grant
        refuse_cursor_native_worktree_tokens(passthrough)
        if passthrough:
            argv += list(passthrough)
        return argv
    if provider == "footnote":
        # The TUI takes one launch spec, not a session flag; the Rust owner
        # resolves the endpoint and writes the 0600 file. The model rides the
        # fence (`-- -m <provider>/<model>`), read the way the headless lane's
        # `fenced_model` reads it, and goes into the spec, never the TUI argv.
        from fno.agents.spawn_axes_client import footnote_pane_spec

        fenced = list(passthrough or [])
        model = None
        for i, tok in enumerate(fenced):
            if tok in ("-m", "--model") and i + 1 < len(fenced):
                model = fenced[i + 1]
                break
            if tok.startswith("--model="):
                model = tok.split("=", 1)[1]
                break
        return [*identity, footnote_pane_spec(str(cwd), model, None, session_uuid)]
    if provider == "grok":
        # grok's pane hosts the plain TUI; the declared form carries only the
        # caller-assigned `--session-id`. The bypass, model and effort axes
        # append here so pane and keeper compose the same argv. grok maps no
        # --add-dir and its permission vocabulary is claude's.
        argv = identity
        argv += grant
        if passthrough:
            argv += list(passthrough)
        return argv
    if not is_declared(provider):
        # The undeclared lane: fno is the VIEWPORT, nothing more. No identity
        # flag, no model, no effort, no permission mapping and no tier3 state
        # grant - every one of those is a per-vendor spelling, and inventing one
        # is the guess this lane exists to refuse. Whatever the binary needs
        # rides the operator's own `-- <flags>` (passthrough).
        #
        # The seed is deliberately NOT in argv: `seed_rode_in_argv` then answers
        # False and `_submit_spawn_seed` types it after readiness, the same path
        # agy already takes. Nothing new is written for it.
        argv = [provider]
        if passthrough:
            argv += list(passthrough)
        return argv
    try:
        render_session_argv(provider, "headless_create", session_uuid)
        hint = "; --substrate headless runs it as a one-shot"
    except DispatchResolveError:
        hint = ""
    raise DispatchAskError(f"provider {provider!r} has no interactive pane form{hint}", exit_code=2)


def _run_mux(
    args: list[str],
    runner: Callable[..., "subprocess.CompletedProcess[str]"],
    env: Optional[dict[str, str]] = None,
    timeout: Optional[float] = None,
) -> "subprocess.CompletedProcess[str]":
    try:
        return runner(
            [_fno_bin(), *args],
            capture_output=True,
            text=True,
            timeout=_MUX_SUBPROCESS_TIMEOUT_S if timeout is None else timeout,
            **({"env": env} if env is not None else {}),
        )
    except FileNotFoundError as exc:
        raise DispatchAskError(
            f"the '{_fno_bin()}' binary was not found on PATH; the pane "
            "substrate is hosted by the fno mux (set FNO_BIN or install fno)",
            exit_code=127,
        ) from exc
    except OSError as exc:
        # A binary that exists but can't run (bad permissions, wrong arch,
        # partial install) is the same "can't reach the mux" case as a
        # missing binary, not a reason to lose the inline path.
        raise DispatchAskError(
            f"could not run '{_fno_bin()}': {exc}",
            exit_code=127,
        ) from exc
    except subprocess.TimeoutExpired as exc:
        # The EFFECTIVE timeout, not the module default: the binding-loop probes
        # pass a 2s bound, so naming 30s here would be a diagnostics lie.
        effective = _MUX_SUBPROCESS_TIMEOUT_S if timeout is None else timeout
        raise DispatchAskError(
            f"fno mux did not answer within {effective}s ({' '.join(args[:3])}...)",
            exit_code=1,
        ) from exc


#: `fno mux pane wait` exit code when the pane's child exited mid-wait
#: (crates/fno EXIT_WAIT_EXITED). The readiness gate treats it as launch failure.
_WAIT_EXITED = 12


def _pane_absent_from_listing(
    mux: dict, runner, timeout: Optional[float] = None
) -> Optional[bool]:
    """True when the mux SUCCESSFULLY enumerated its panes and ours is not there.

    This is the reaped-pane case, and it is the normal one: when a pane's child
    exits the mux drops the pane outright (``close_pane``), so a later
    ``pane wait`` finds no pane to watch and answers with the generic
    ``EXIT_ERROR`` (1) that also covers io failures, version skew, and server
    errors. Exit 12 only ever fires when the child dies while a watcher is
    already subscribed, which a ``--timeout 0`` probe almost never is. Without
    this fallback the death branch would be near-unreachable and a corpse would
    sit out the whole window and land as `spawning` - the original bug.

    The positive control is a NON-EMPTY listing, and the empty case is the whole
    reason: `fno mux pane ls --json` deliberately prints `[]` and exits 0 when the
    session socket is refused or absent (crates/fno/src/mux_cli.rs, the
    `is_ls && no_server` branch). So exit 0 plus parseable JSON does NOT prove
    the instrument ran - "I could not reach the session" and "the session has no
    panes" are the same bytes. Only a listing that names at least one OTHER pane
    proves the server answered with real content; absence from that is the mux
    saying our pane is gone.

    An empty list therefore returns None, not True. That costs death detection
    when the dying pane was the session's last, and it is the right trade: this
    helper's False is the condemn signal for reconcile and
    ``reachability.pane_falsifier``, so a wrong False marks a LIVE worker exited
    whenever the mux socket is briefly unreachable. Failing to unknown loses a
    retry; failing to condemned loses a worker.
    """
    try:
        proc = _run_mux(
            ["mux", "pane", "ls", "--server", str(mux["session"]), "--json"],
            runner,
            timeout=timeout,
        )
    except Exception:  # noqa: BLE001 -- a probe never fails a spawn
        return None
    if getattr(proc, "returncode", 1) != 0:
        return None
    try:
        rows = json.loads(proc.stdout or "")
    except (ValueError, TypeError):
        return None
    if not isinstance(rows, list) or not rows:
        # Empty is indistinguishable from an unreachable session (see docstring).
        return None
    return not any(
        isinstance(r, dict) and r.get("pane_id") == mux["pane_id"] for r in rows
    )


def _mux_pane_alive(
    mux: dict, runner=subprocess.run, timeout: Optional[float] = None
) -> Optional[bool]:
    """Return exact pane liveness, or ``None`` when the mux cannot answer.

    ``timeout`` defaults to the shared module bound. The binding loop passes a
    tight one; reconcile and ``reachability.pane_falsifier`` must NOT inherit it,
    because a mux answering in 3s under load would turn into "unavailable" there
    and skip a heal that used to succeed. Tight bounds belong to the caller that
    needs them, not baked into shared code.
    """
    try:
        proc = _run_mux(
            [
                "mux", "pane", "wait", "--server", str(mux["session"]),
                str(mux["pane_id"]), "--timeout", "0",
            ],
            runner,
            timeout=timeout,
        )
    except Exception:  # noqa: BLE001 -- see below: a probe never fails a spawn
        # Deliberately as broad as _read_pane_tail's. This runs at the same
        # point, after the pane exists and under the registry lock, and its whole
        # contract is "None = the mux could not answer" - so an unhandled
        # PermissionError or a fork failure escaping here would orphan a live
        # pane with no registry row, which is a worse outcome than every reason
        # the probe might have failed. A narrow catch held the liveness probe to
        # a weaker standard than the tail read it sits next to.
        return None
    if proc.returncode == _WAIT_EXITED:
        return False
    if proc.returncode in {0, 11}:
        return True
    # Every other code is ambiguous by construction (EXIT_ERROR = 1 covers a
    # dead pane AND io failure AND version skew AND server error), so it is not
    # a death signal on its own. Ask the authoritative enumeration instead.
    absent = _pane_absent_from_listing(mux, runner, timeout=timeout)
    if absent is None:
        return None
    return not absent
