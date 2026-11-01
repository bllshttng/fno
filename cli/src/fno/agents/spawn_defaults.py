"""Config-sourced spawn defaults, injected argv-level at the dispatch seam.

Every `fno agents spawn` passes this seam before the Rust/Python routing
fork (Locked Decision 9). Per field: explicit CLI flag > lane >
`agents.profiles.<command>` (per-harness overlays inside) > `agents.defaults` >
built-in. A bare scalar `model` is scoped to the config `provider`'s harness,
never the ambient one; an explicit `-m/--model` always wins; a resolved
`--role` lane owns the model.
"""
from __future__ import annotations

import re
import sys
from typing import IO, Callable, List, Mapping, Optional, Sequence, Set, Tuple

# Flags that consume the FOLLOWING token. Scanning for our three flags skips a
# value flag's value so a value that looks like `--model` / `--effort` can never
# masquerade as one of ours. Mirrors client.rs VALUE_FLAGS + the short aliases
# typer exposes on the spawn command.
_VALUE_FLAGS = frozenset(
    {
        "--provider", "-P", "--harness", "-H", "--model", "-m", "--effort",
        "--from", "--cwd", "-c",
        "--message", "--session-id", "--cc-session-id", "--channel-id", "--status",
        "--from-name", "--timeout", "-t", "--mode", "--substrate", "--as", "--permission-mode",
        "--output-format", "--monitor", "--tab",
        "--node",
    }
)

# The harness axis (the CLI binary) is --harness/-H only: --provider/-P names the
# model VENDOR, a different axis that never picks a binary, so it must not feed
# the provider-aware default scan.
_HARNESS_FLAGS = ("--harness", "-H")
_MODEL_FLAGS = ("--model", "-m")
_EFFORT_FLAGS = ("--effort",)


def _scan(args: Sequence[str]) -> Tuple[bool, Optional[str], bool, bool]:
    """One pass over a spawn argv (command already stripped by the caller).

    Returns ``(harness_present, harness_value, model_present, effort_present)``.
    The scanned flag is ``-H/--harness``: this pass reads the HARNESS axis and
    never touches ``-P/--provider``, which the caller reads separately.
    Handles both `--flag value` and `--flag=value`; stops at the `--argv`
    payload boundary and at a bare `--` passthrough fence (: fenced
    tokens are the harness's own flags, never fno's); skips a value flag's value.
    """
    harness_present = model_present = effort_present = False
    harness_value: Optional[str] = None
    it = iter(args)
    for a in it:
        if a == "--argv" or a == "--":
            break
        key, eq, val = a.partition("=")
        if key in _HARNESS_FLAGS:
            harness_present = True
            harness_value = val if eq else next(it, None)
        elif key in _MODEL_FLAGS:
            model_present = True
            if not eq:
                next(it, None)
        elif key in _EFFORT_FLAGS:
            effort_present = True
            if not eq:
                next(it, None)
        elif key in _VALUE_FLAGS and not eq:
            next(it, None)  # skip this flag's value so it can't be misread
    return harness_present, harness_value, model_present, effort_present


# --------------------------------------------------------------------------- #
# Spawn argv normalization: three ergonomic cuts, one argv->argv pass.
#
# Runs at the front door (inside compose_spawn_argv, BEFORE config injection
# and BEFORE the runtime route/fork), so by the time either runtime parser sees
# the argv it is canonical: an explicit NAME, long-form `--resume <full-uuid>`,
# long-form `--substrate <s>`. Neither parser learns a new vocabulary.
# --------------------------------------------------------------------------- #

# Flags on `spawn` that consume the following token. Needed to tell a flag's
# VALUE apart from a positional when scanning for the NAME / substrate token. A
# missing entry would misread that flag's value as a positional (e.g. a Rust-path
# `--message bg` mis-parsed as a substrate token), so this unions the shared
# `_VALUE_FLAGS` (--message, --session-id, --from, --status, ...) with the
# spawn-only value options.
_SPAWN_VALUE_FLAGS = _VALUE_FLAGS | frozenset(
    {
        "--role", "--resume", "-r", "--add-dir", "--agent", "--tools",
        "--deny-tools", "--workspace", "--squad", "-s", "--split", "-x", "--tab",
        "--pane",
        "--node", "--node-reason", "--slug", "--plan", "--name", "--recorded-provider",
        # --route/--account/--promote were absent, so their VALUES read as positionals:
        # a nameless `spawn --route zai,glm-5.2` registered an agent named "zai,glm-5.2".
        # Kept in lockstep with cmd_spawn (test_spawn_value_flags_cover_every_value_option).
        "--route", "--account", "--promote", "--dispatch-account",
        "--take-over",
        # --at's value (current|<pane>) must not read as a positional.
        "--at",
        # --portal's index is a value, never a prompt word.
        "--portal",
        # the sessions-row phase names a phase, not a prompt word.
        "--session-phase",
        # The join call site's per-worker policy file: its PATH is a value,
        # never a prompt word.
        "--sandbox-write-policy",
        # A retry budget is a duration, never a prompt word.
        "--wait",
        # A prompt-file PATH is never a prompt word.
        "--prompt-file",
        # The dispatch-next porcelain's pinned lane name is a value.
        "--mux-session",
    }
)


# Tokens that pin the substrate explicitly (a positional substrate word conflicts
# with any of these -> exit 2). `-o/--once` means headless; `--substrate
# headless` is the long form. `-H` selects the harness and `-P` the vendor, so
# both are value flags here. `--headless`/`-p` retired with change 6 (--print
# took the flag budget; `--substrate headless` is the spelling).
_EXPLICIT_SUBSTRATE_BOOLS = ("-o", "--once")

_UUID_RE = re.compile(r"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$")
def _has_explicit_substrate(toks: Sequence[str]) -> Optional[str]:
    """Return the substrate value if pinned by an explicit flag, else None.

    Stops at the ``--argv`` payload boundary and at a bare ``--`` seed fence
    like the other spawn scans: fenced tokens are prompt text, never flags.
    """
    it = iter(toks)
    for t in it:
        if t in ("--argv", "--"):
            break
        if t in _EXPLICIT_SUBSTRATE_BOOLS:
            return "headless"
        if t in ("--substrate", "--as"):
            return next(it, "")
        if t.startswith(("--substrate=", "--as=")):
            return t.split("=", 1)[1]
    return None


def _positional_indices(toks: Sequence[str]) -> List[int]:
    """Indices of positional tokens (NAME, MESSAGE), skipping flags + their values.

    Stops at a bare ``--`` fence: the first fenced token still lands in the
    MESSAGE (click fills positionals in order), and the rest are the
    provider passthrough - provider tokens, never prompt positionals to refuse.
    """
    idxs: List[int] = []
    i = 0
    n = len(toks)
    while i < n:
        t = toks[i]
        if t == "--argv" or t == "--":
            break
        if t.startswith("-"):
            if "=" not in t and t in _SPAWN_VALUE_FLAGS:
                i += 2  # skip the flag and its value
                continue
            i += 1
            continue
        idxs.append(i)
        i += 1
    return idxs




#: Harness flags a spawn accepts before the fence as shorthand for the same
#: flag behind it. Each takes one value.
_FENCE_SHORTHAND = {
    "--model": "--model",
    "-m": "--model",
    "--effort": "--effort",
    "--permission-mode": "--permission-mode",
    "--add-dir": "--add-dir",
    "--agent": "--agent",
    "--tools": "--tools",
}


def _fence_model_shorthand(toks: List[str], err: IO[str]) -> List[str]:
    """Move ``--model``/``-m``, ``--effort``, ``--permission-mode``, ``--add-dir``,
    ``--agent`` and ``--tools`` from the head behind the ``--`` fence.

    Spawn allows every harness flag, and these are the ones an operator types
    most, so the head spelling is shorthand for the harness's own flag.
    A prompt-less spawn keeps an explicit empty MESSAGE, or the first fenced
    token would become the prompt.
    """
    cut = toks.index("--") if "--" in toks else len(toks)
    kept: List[str] = []
    moved: List[str] = []
    it = iter(toks[:cut])
    for tok in it:
        flag, eq, value = tok.partition("=")
        if tok in _FENCE_SHORTHAND:
            moved += [_FENCE_SHORTHAND[tok], next(it, "")]
        elif eq and flag.startswith("--") and flag in _FENCE_SHORTHAND:
            moved += [_FENCE_SHORTHAND[flag], value]
        else:
            kept.append(tok)
    if not moved:
        return toks
    if not all(moved[1::2]):
        print(
            f"fno agents spawn: {' / '.join(sorted(set(moved[0::2])))} needs a value; "
            "no worker launched",
            file=err,
        )
        raise SystemExit(2)
    if not _positional_indices(kept):
        kept.append("")
    return [*kept, "--", *moved, *toks[cut + 1 :]]


def _head_flag_value(head: Sequence[str], flags: Tuple[str, ...]) -> Optional[str]:
    """Read one flag's value from a pre-fence head (``--flag v``, ``--flag=v``,
    ``-f v``, ``-f v`` attached). Skips every other value flag's value so it
    cannot misread one, mirroring ``_scan``. First occurrence wins."""
    it = iter(head)
    for a in it:
        if a in ("--argv", "--"):
            break
        key, eq, val = a.partition("=")
        if key in flags:
            return val if eq else next(it, None) or None
        # Short attached form: -m<value> (no long-flag starts with a single dash).
        if (
            not eq
            and len(a) > 2
            and not a.startswith("--")
            and a[:2] in flags
        ):
            return a[2:]
        if key in _SPAWN_VALUE_FLAGS and not eq:
            next(it, None)
    return None


def _mint_node_name(
    node: Optional[str], seed: Optional[str], slug_flag: Optional[str], existing: Set[str]
) -> Optional[str]:
    """The ``agents.naming.worker`` name of a node-driven or ``/target`` spawn.

    The binary renders it (naming.rs ``render_worker_name``). A name a live worker
    holds gets a ``-2``..``-5`` suffix, so a re-spawn on one node is never a
    refusal. Any failure returns ``None`` and the caller refuses with a named
    line: a spawn with no node slug and no prompt has nothing to name a worker
    from, and it never launches under an invented one.
    """
    from fno.agents.naming import AgentNameError, BridgeUsageError, worker_name

    if not (node or seed):
        return None
    try:
        name = worker_name(node, seed=seed, slug=slug_flag)
    except (AgentNameError, BridgeUsageError):
        return None
    for n in range(1, 6):
        candidate = name if n == 1 else f"{name}-{n}"
        if candidate not in existing:
            return candidate
    return None


def _read_registry_rows() -> list[object]:
    """Registry rows shared by name minting and profile-lane selection."""
    try:
        from fno.agents.registry import load_registry

        return list(load_registry())
    except Exception:
        return []


def normalize_spawn_args(
    args: Sequence[str],
    *,
    resolver: Optional[Callable[[str], Optional[str]]] = None,
    existing_names: Optional[Set[str]] = None,
    stderr: Optional[IO[str]] = None,
) -> List[str]:
    """Canonicalize a ``spawn`` argv (command at index 0); pure argv -> argv.

    The single positional is the MESSAGE; the name rides ``--name`` and is
    minted (``t-`` plus words from the node slug or the prompt) when
    omitted. A second positional -> exit 2.

    Non-``spawn`` commands and ``spawn --help`` pass through unchanged. Read-only
    (registry names, session resolver); writes no state.
    """
    out = list(args)
    if not out or out[0] != "spawn":
        return out
    for a in out[1:]:
        if a == "--argv":
            break
        if a in ("-h", "--help"):
            return out

    err = stderr if stderr is not None else sys.stderr
    # Split off the `--argv` provider payload: every pass operates on the fno-arg
    # HEAD only, and derived flags are appended before the payload, so a payload
    # token (e.g. the child command's own `--resume`) is never scanned or rewritten.
    body = out[1:]
    if "--argv" in body:
        cut = body.index("--argv")
        toks, payload = body[:cut], body[cut:]
    else:
        toks, payload = body, []

    if not payload:
        toks = _fence_model_shorthand(toks, err)
    fence = next((i for i, t in enumerate(toks) if t == "--"), None)

    # Pass 3: the NAME axis. `spawn` takes ONE positional and it is the MESSAGE;
    # the agent name is a handle the caller rarely picks, so it is minted unless
    # --name says otherwise. Canonicalize to `--name <n> <message>` so both
    # parsers read the same shape. A second positional is refused rather than
    # guessed at: under the old `<name> <message>` grammar it would silently
    # register an agent named after the prompt. The mint probe scans only the
    # pre-fence head: a passthrough `--name` is the PROVIDER's flag (claude's
    # session display name) and must not suppress fno's own name mint.
    head = toks if fence is None else toks[:fence]
    # A succession hand-off names nobody: the settle draws the successor's
    # name from the machine pool, and a minted slug would arrive as an
    # explicit proposal and decline the documented no-name draw.
    hand_off = any(t == "--hand-off" or t.startswith("--hand-off=") for t in head)
    if not hand_off and not any(t == "--name" or t.startswith("--name=") for t in head):
        names = (
            existing_names
            if existing_names is not None
            else {str(getattr(e, "name", "")) for e in _read_registry_rows()}
        )
        # A node-driven or `/target <node>` spawn is named for its node's
        # slug; the row's `node` and `model` fields carry the rest.
        seeds = [toks[i] for i in _positional_indices(head)]
        minted = _mint_node_name(
            _head_flag_value(head, ("--node",)),
            seeds[0] if seeds else None,
            _head_flag_value(head, ("--slug",)),
            names,
        )
        if minted is None:
            print(
                "fno agents spawn: no node slug or prompt to name the worker "
                "from; pass --name",
                file=err,
            )
            raise SystemExit(2)
        toks = ["--name", minted, *toks]
    extra = _positional_indices(toks)
    if len(extra) > 1:
        print(
            "fno agents spawn: takes one positional (the prompt); got "
            f"{len(extra)} ({', '.join(repr(toks[i]) for i in extra)}). The agent "
            "name moved to --name, so pass `--name <n> \"<prompt>\"`.",
            file=err,
        )
        raise SystemExit(2)

    return ["spawn", *toks, *payload]


# --------------------------------------------------------------------------- #
# Per-command profile resolution: a pure string rule over the seed's first
# token selects `config.agents.profiles.<command>`, layered over `agents.defaults`.
# No content-based inference of any kind - only an explicit leading slash-command.
# --------------------------------------------------------------------------- #

def _seed_slot(toks: Sequence[str]) -> Optional[tuple[int, str]]:
    """Where the MESSAGE seed lives: ``(index, form)`` into ``toks``, or
    None. One scan so the reader and the seam rewrite agree on the slot.
    Same rules as :func:`_seed_of`: the ``--argv`` boundary ends the head,
    and a bare ``--`` fence wins only in the legacy no-message idiom
    ."""
    i = 0
    while i < len(toks):
        t = toks[i]
        if t == "--argv":
            break
        if t == "--":
            head_pos = _positional_indices(toks[:i])
            if head_pos:
                return head_pos[0], "positional"
            if i + 1 < len(toks):
                return i + 1, "fenced"
            return None
        if t == "--message":
            if i + 1 < len(toks):
                return i + 1, "message"
            return None
        if t.startswith("--message="):
            return i, "message_eq"
        i += 1
    pos = _positional_indices(toks)
    if pos:
        return pos[0], "positional"
    return None


def _seed_of(toks: Sequence[str]) -> Optional[str]:
    """The MESSAGE seed: the ``--message`` value, else the sole positional
    (the name rides ``--name``); a thin reader over :func:`_seed_slot`, so
    reading and rewriting share one scan."""
    slot = _seed_slot(toks)
    if slot is None:
        return None
    i, form = slot
    text = toks[i]
    if form == "message_eq":
        return text.split("=", 1)[1]
    return text


def _role_of(toks: Sequence[str]) -> Optional[str]:
    """The ``--role`` value if present, else None. Stops at the ``--argv``
    payload boundary like the other spawn scans."""
    toks = list(toks)
    for i, t in _spawn_tokens(toks):
        if t == "--role":
            return toks[i + 1] if i + 1 < len(toks) else None
        if t.startswith("--role="):
            return t.split("=", 1)[1]
    return None


def _has_permission_mode(toks: Sequence[str]) -> bool:
    """Whether the permission control is pinned, up to the ``--argv`` boundary
    and a bare ``--`` fence (: a fenced ``--permission-mode`` is the
    provider's flag, not fno's, and must not suppress a config default).
    ``--yolo``/``-Y`` count: they are the same knob as ``--permission-mode`` and
    are mutually exclusive with it downstream, so a config value injected
    alongside an explicit ``--yolo`` would exit 2 (explicit intent must win)."""
    for t in toks:
        if t == "--argv" or t == "--":
            break
        if (
            t in ("--permission-mode", "--yolo", "-Y")
            or t.startswith("--permission-mode=")
        ):
            return True
    return False


def _spawn_tokens(toks: Sequence[str]):
    """Yield ``(index, token)`` from ``toks`` up to the ``--argv`` boundary and
    any bare ``--`` seed fence (fenced tokens are prompt text), skipping any
    token that is another value-flag's consumed VALUE - so a literal occurrence
    of one flag can never be misread out of a different flag's value (e.g.
    ``--session-id --route`` names a session id of ``--route``, not a
    ``--route`` flag)."""
    it = enumerate(toks)
    for i, t in it:
        if t in ("--argv", "--"):
            break
        yield i, t
        if t in _SPAWN_VALUE_FLAGS and "=" not in t:
            next(it, None)


def _flag_present(toks: Sequence[str], flag: str) -> bool:
    """Whether a value ``flag`` appears as ``--flag`` or ``--flag=...`` in toks,
    up to the ``--argv`` payload boundary."""
    for _, t in _spawn_tokens(toks):
        if t == flag or t.startswith(flag + "="):
            return True
    return False


def _flag_value(toks: Sequence[str], *flags: str) -> Optional[str]:
    """Value of the first of ``flags`` found as ``--flag value``,
    ``--flag=value``, or - for a 2-char short flag like ``-P`` - the glued
    ``-Pvalue`` form typer/click also accepts, else None. Stops at the
    ``--argv`` payload boundary."""
    toks = list(toks)
    for i, t in _spawn_tokens(toks):
        for f in flags:
            if t == f:
                return toks[i + 1] if i + 1 < len(toks) else None
            if t.startswith(f + "="):
                return t.split("=", 1)[1]
            if len(f) == 2 and f[1] != "-" and t != f and t.startswith(f):
                return t[len(f):]
    return None


# A model string's implied vendor, by prefix or tier word. A pure string
# opinion and never a routing input: the warning it drives is advisory, because
# the pairing is legal and --model is deliberate passthrough (cli.py).
def resolve_lane_vendor(
    argv: Sequence[str],
    env: Optional[Mapping[str, str]] = None,
    *,
    harness: Optional[str] = None,
) -> Optional[str]:
    """The model vendor a final spawn argv bills: route > provider > harness.
    The vocabulary and the judgment live in the spawn-overlay command; this shim
    resolves the harness-side inputs (explicit arg, then -H, then dispatch
    inference from env) and reads the answer."""
    from fno.agents.spawn_overlay_client import (
        SpawnOverlayUnavailable,
        spawn_overlay_call,
    )

    toks = [str(t) for t in (list(argv)[1:] if argv else [])]
    env_harness = None
    if not (harness and str(harness).strip()):
        harness = _flag_value(toks, "--harness", "-H")
    if not (harness and str(harness).strip()):
        try:
            from fno.rust_binary import command_call

            answer = command_call(
                "route-slot",
                {"mode": "resolve", "explicit": None, "env": dict(env or {})},
            )
            env_harness = answer.get("harness")
        except Exception:
            env_harness = "claude"
    try:
        return spawn_overlay_call(
            {
                "kind": "lane-vendor",
                "argv_tail": toks,
                "argv_head": argv[0] if argv else None,
                "harness": harness,
                "env_harness": env_harness,
            }
        ).get("vendor")
    except SpawnOverlayUnavailable:
        # No binary: no vendor opinion (the documented no-opinion answer).
        return None


# --------------------------------------------------------------------------- #
# The CLI's substrate posture gates, moved here from cmd_spawn (file budget):
# the value gate + bg alias, and the monitor gate.

