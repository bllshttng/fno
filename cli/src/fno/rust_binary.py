"""Locate the bundled ``fno-agents`` binary.

Pure filesystem lookup: stdlib only, no ``fno`` imports at all, which is why it
sits at the platform layer rather than under ``fno.agents``. Its callers span
every layer (``fno do phase kill-check``, ``fno doctor``, ``fno agents restart``, the
post-merge ledger finalizer, the relay, the agents runtime), and the ones below
the runtime were paying an upward import for what is four ``os.access`` checks.

Resolution order, widest first:

1. ``$FNO_AGENTS_BIN`` -- an explicit operator override.
2. The wheel-bundled binary at ``<package>/_bin/``.
3. The binary installed next to the running launcher.
4. ``PATH``.
5. A ``cargo build --release`` artifact (dev checkouts only, and only from
   :func:`resolve_binary`).
"""

from __future__ import annotations

import os
import shutil
import sys
from functools import lru_cache
from pathlib import Path
from typing import Any, Optional, Sequence

BINARY_NAME = "fno-agents.exe" if os.name == "nt" else "fno-agents"

#: Operator override. The shell shims (scripts/run-target-loop.sh, the stop
#: hooks) have always honored this, so the Python resolver honors it too rather
#: than making the same export mean two different things depending on which half
#: of the toolchain reads it. Both "binary not found" messages in
#: ``fno.agents.rust_runtime`` name it, so the remedy is discoverable from the
#: failure itself.
BINARY_ENV = "FNO_AGENTS_BIN"


def _env_binary() -> Optional[Path]:
    """The explicit ``$FNO_AGENTS_BIN`` override, when it names a runnable file.

    An unset, empty, or non-executable value falls through to the search rather
    than failing: the shims treat it the same way, and a stale export must not
    make an otherwise-installed binary unreachable.
    """
    raw = (os.environ.get(BINARY_ENV) or "").strip()
    if not raw:
        return None
    candidate = Path(raw).expanduser()
    return candidate if candidate.is_file() and os.access(candidate, os.X_OK) else None


def _bundled_binary() -> Optional[Path]:
    """The wheel-bundled binary at ``<package>/_bin/fno-agents`` (W6 Wave 3)."""
    bundled = Path(__file__).resolve().parent / "_bin" / BINARY_NAME
    return bundled if bundled.is_file() and os.access(bundled, os.X_OK) else None


def _sibling_binary() -> Optional[Path]:
    """The binary installed next to the running launcher (the wheel scripts dir).

    pip installs both the ``fno`` console script and the bundled ``fno-agents``
    wheel-script into the same bin/ (Scripts/ on Windows). When ``fno`` is invoked
    by absolute path without that dir on ``PATH`` (common in CI / cron wrappers),
    ``shutil.which`` misses the binary even though it sits right beside the
    launcher; this finder catches that case (codex P2 on PR #351).
    """
    launcher = sys.argv[0] if sys.argv else ""
    if not launcher:
        return None
    sibling = Path(launcher).resolve().parent / BINARY_NAME
    return sibling if sibling.is_file() and os.access(sibling, os.X_OK) else None


def _path_binary() -> Optional[Path]:
    """The binary as resolved on ``PATH`` (``cargo install`` / GH release / wheel script)."""
    found = shutil.which(BINARY_NAME)
    return Path(found) if found else None


def newest_runnable(candidates: Sequence[Path]) -> Optional[Path]:
    """The newest runnable candidate, or None when none is runnable."""
    runnable = [c for c in candidates if c.is_file() and os.access(c, os.X_OK)]
    return max(runnable, key=lambda c: c.stat().st_mtime_ns, default=None)


def _cargo_dev_binary() -> Optional[Path]:
    """Dev fallback: a ``cargo build`` artifact under the repo tree.

    ``__file__`` is ``cli/src/fno/rust_binary.py`` so the repo root is
    ``parents[3]``. Checks both a crate-local ``target/`` and a workspace
    ``target/`` so it works whether or not a workspace is introduced later.
    The newest artifact wins so a fresh build is never shadowed by a stale
    one, but a debug build counts too: the CI smoke lanes build debug and
    strip ``FNO_*`` env, so this finder is the only reader left for the
    footprint door there.
    """
    here = Path(__file__).resolve()
    try:
        repo_root = here.parents[3]
    except IndexError:  # installed shallower than a dev checkout
        return None
    # Only meaningful in a development checkout. When the package is installed
    # into site-packages, parents[3] is some unrelated ancestor; refuse to
    # traverse it so we never return a coincidental wrong binary.
    if not (repo_root / "Cargo.toml").exists() and not (repo_root / "crates").is_dir():
        return None
    candidates = (
        repo_root / "crates" / "fno-agents" / "target" / "release" / BINARY_NAME,
        repo_root / "target" / "release" / BINARY_NAME,
        repo_root / "crates" / "fno-agents" / "target" / "debug" / BINARY_NAME,
        repo_root / "target" / "debug" / BINARY_NAME,
    )
    return newest_runnable(candidates)


def _front_binary() -> Optional[Path]:
    """The checkout-scoped front binary CI and the doctor tooling export
    (``$FNO_AGENTS_FRONT``, from the smoke-setup action). Sits behind PATH so
    a stale export never outranks a fresh install, but ahead of the cargo dev
    walk so a lane that built the binary and exported only the FRONT name
    stays readable - the footprint door now resolves through this finder, and
    a resolver blind to FRONT refuses every dispatch on those lanes.
    """
    raw = (os.environ.get("FNO_AGENTS_FRONT") or "").strip()
    if not raw:
        return None
    candidate = Path(raw).expanduser()
    return candidate if candidate.is_file() and os.access(candidate, os.X_OK) else None


def resolve_binary() -> Optional[Path]:
    """Locate ``fno-agents``: env override -> bundled -> sibling -> PATH -> FRONT -> cargo dev.

    Bundled beats PATH so a ``pip install fno`` wheel is self-contained even when
    a different (older) ``fno-agents`` happens to be on PATH. The launcher-sibling
    lookup sits ahead of PATH so an abs-path ``fno`` invocation still resolves the
    co-installed binary. ``$FNO_AGENTS_BIN`` outranks all of them, because an
    operator who set it meant it.
    """
    for finder in (
        _env_binary,
        _bundled_binary,
        _sibling_binary,
        _path_binary,
        _front_binary,
        _cargo_dev_binary,
    ):
        found = finder()
        if found is not None:
            return found
    return None


def call_binary_json(
    verb: str, args: Sequence[str] = (), *, timeout: Optional[float] = 60,
    binary: Optional[Path] = None,
    input_text: Optional[str] = None,
) -> tuple[Optional[str], Any]:
    """Run one direct ``fno-agents`` client verb and parse its JSON stdout.

    ``input_text`` feeds the child's stdin when given, so a large payload
    rides stdin instead of an argv entry (the OS per-argument cap would
    otherwise become an undocumented body cap).

    Returns ``(error, parsed)``: ``error`` is None on success; a missing
    binary, non-zero exit, timeout, or unparseable stdout yields a short error
    text and a None payload. Callers keep the failure shape theirs (refuse
    closed, raise, or exit) - this seam only standardizes the door.

    ``binary`` pre-resolves the door; the dev build outranks the stale
    installed copy (the rule :func:`verb_call` states).
    """
    import json
    import subprocess

    binary = binary or resolve_binary()
    if binary is None:
        return ("fno-agents binary not found", None)
    try:
        proc = subprocess.run(
            [str(binary), verb, *args], capture_output=True, text=True, timeout=timeout,
            input=input_text,
        )
    except subprocess.TimeoutExpired:
        bound = f"{timeout:.1f}s" if timeout is not None else "the caller's bound"
        return (f"timed out after {bound}", None)
    except OSError as exc:
        return (str(exc)[:200], None)
    if proc.returncode != 0:
        return ((proc.stderr or "verb failed").strip()[:200], None)
    try:
        return (None, json.loads(proc.stdout or "null"))
    except ValueError:
        return ("unreadable JSON receipt", None)


def style_receipt(
    text: str, surface: str, word_cap: Optional[int] = None
) -> tuple[Optional[str], Any]:
    """One style-gate read through the door: ``(error, receipt)``.

    The body rides stdin, so the OS per-argument cap never becomes an
    undocumented body cap. The receipt carries ``exception``, ``word_count``,
    ``violations`` and ``report``. An error tuple means the gate could not
    run, and the caller keeps its own failure posture.
    """
    argv = ["--surface", surface, "--stdin", "--json"]
    if word_cap is not None:
        argv += ["--word-cap", str(word_cap)]
    return call_binary_json("style-check", argv, input_text=text)


def style_word_count(text: str) -> int:
    """Masked word count through the style door; 0 when the door fails.

    The counting sites are advisory (a ledger row, a long-note nudge), so a
    door failure counts zero rather than blocking the send.
    """
    err, receipt = call_binary_json("style-check", ["--stdin", "--json"], input_text=text)
    if err or not isinstance(receipt, dict):
        return 0
    return int(receipt.get("word_count") or 0)


def style_word_count_checked(text: str) -> int:
    """Masked word count for an ENFORCING site: raises
    ``budget.BudgetCountUnavailable`` on a door failure instead of answering a
    silent zero (a zero would read as unlimited traffic under the control
    lane's rolling cap). Imported lazily: budget.py never imports this module,
    so there is no cycle.
    """
    from fno.mail.budget import BudgetCountUnavailable

    err, receipt = call_binary_json("style-check", ["--stdin", "--json"], input_text=text)
    if err or not isinstance(receipt, dict):
        raise BudgetCountUnavailable(f"style count unavailable: {err or 'no receipt'}")
    return int(receipt.get("word_count") or 0)


def mint_fno_id() -> str:
    """Footnote's one session-id mint lives in Rust: `fno-agents state mint-id`.

    Raises VerbUnavailable when the binary is missing or answers nothing; no
    Python-side fallback exists, by the rule that keeps the mint single.
    """
    import subprocess

    binary = resolve_binary()
    if binary is None:
        raise VerbUnavailable("fno-agents binary not found; run `fno doctor update`")
    proc = subprocess.run(
        [str(binary), "state", "mint-id"], capture_output=True, text=True, timeout=10
    )
    minted = proc.stdout.strip()
    if proc.returncode != 0 or not minted:
        raise VerbUnavailable(
            (proc.stderr or "fno-agents state mint-id answered nothing").strip()[:200]
        )
    return minted


@lru_cache(maxsize=1)
def delivered_terminals() -> frozenset:
    """The delivered-terminal vocabulary, owned by Rust
    (`TerminationReason::is_delivered` in loopcheck.rs) and served through one
    fail-closed `fno-agents terminals` subprocess per process, like the roster.

    The set counts a run DELIVERED for telemetry: DonePRGreen, DoneAdvisory,
    DoneDelivery, DoneBatched. DoneAwaitingMerge and DoneUnreviewed are
    deliberately absent (complete-but-unmerged and waiting are not landed).
    Raises VerbUnavailable when the binary is missing or answers nothing; no
    Python-side fallback exists, by the rule that keeps the vocabulary single.
    """
    error, payload = call_binary_json(
        "terminals", timeout=15, binary=find_dev_binary() or resolve_binary()
    )
    if error is not None:
        raise VerbUnavailable(
            "the delivered-terminal vocabulary lives in the fno-agents binary"
            " (loopcheck.rs TerminationReason::is_delivered) and the read"
            f" failed: {error}; run `fno doctor update --rust` or set FNO_AGENTS_BIN"
        )
    names = payload.get("delivered") if isinstance(payload, dict) else None
    if not names or not all(isinstance(n, str) and n for n in names):
        raise VerbUnavailable(
            f"fno-agents terminals answered no usable delivered list: {payload!r}"[:200]
        )
    return frozenset(names)


def resolve_installed_binary() -> Optional[Path]:
    """Locate an *installed* ``fno-agents``, deliberately excluding the cargo dev target.

    The ``auto`` (default) runtime uses this narrower set so a *development*
    checkout -- where only ``crates/fno-agents/target/release`` exists -- stays on
    the Python dispatch by default, and the in-process test suite never execs the
    binary. A dev who wants Rust opts in explicitly with ``FNO_AGENTS_RUNTIME=rust``,
    which routes through the full :func:`resolve_binary` (cargo dev included).

    Deliberately does NOT consult ``$FNO_AGENTS_BIN``: this function decides the
    *default* runtime, and a stale export pointing at a dev build must not
    silently flip an install onto the Rust path. ``FNO_AGENTS_RUNTIME=rust`` is
    the opt-in, and it routes through :func:`resolve_binary`, which does.
    """
    for finder in (_bundled_binary, _sibling_binary, _path_binary):
        found = finder()
        if found is not None:
            return found
    return None


def find_dev_binary() -> Optional[Path]:
    """The ``@requires_rust`` marker's detector: a binary built in THIS checkout.

    Debug or release under ``crates/fno-agents/target/``, and nothing else. No
    installed, wheel-bundled, or ``$FNO_AGENTS_BIN`` copy may answer for it:
    the tests behind the marker assert against this checkout's build, and the
    smoke-pytest CI shard deletes exactly these two files so they skip there
    instead of running against whatever else happens to be installed. The
    single source for a fact three test files used to copy by hand.
    """
    here = Path(__file__).resolve()
    try:
        repo_root = here.parents[3]
    except IndexError:
        return None
    if not (repo_root / "crates" / "fno-agents").is_dir():
        return None
    base = repo_root / "crates" / "fno-agents" / "target"
    return newest_runnable([base / p / BINARY_NAME for p in ("release", "debug")])


class VerbUnavailable(RuntimeError):
    """The fno-agents binary is missing, failed, or answered malformed JSON."""

    # Structured exit code for the non-zero-exit failure; None on the other
    # paths (binary missing, OSError/timeout, bad JSON). The gh budget's admit
    # gate reads it: a negative code or 137 is a signalled reader, which is a
    # machine in distress, not a ledger that cannot answer.
    returncode: Optional[int] = None


def verb_call(
    verb: "str | list[str]",
    payload: dict,
    unavailable: type = VerbUnavailable,
    *,
    timeout: float = 30,
    passthrough_stderr: bool = False,
) -> dict:
    """One subprocess round-trip with the fno-agents binary: JSON payload in,
    parsed JSON answer out. The dev checkout's own build outranks any stale
    installed copy. Raises the caller's ``unavailable`` exception - a named
    refusal, never a silent fallback.

    ``timeout`` defaults to the 30s a pure local resolver needs. A verb that
    makes its own network round trips must raise it, or the door reports the
    owner unreachable for a decision that was merely still running - and an
    unread authorization refuses the merge.

    ``passthrough_stderr`` leaves the child's stderr attached to this process
    instead of capturing it: a gate that queues for minutes streams its
    ``spawn queued: ...`` prose live, so an operator watching the spawn sees
    the wait instead of silence.
    """
    import json
    import os
    import subprocess

    binary = find_dev_binary() or resolve_binary()
    if binary is None:
        raise unavailable(
            "the fno-agents binary was not found; reinstall fno,"
            " run `fno doctor update --rust`, or set FNO_AGENTS_BIN"
        )
    verb_display = verb if isinstance(verb, str) else " ".join(verb)
    try:
        proc = subprocess.run(
            [str(binary), *(verb if isinstance(verb, list) else [verb])],
            input=json.dumps(payload),
            stdout=subprocess.PIPE,
            stderr=None if passthrough_stderr else subprocess.PIPE,
            text=True,
            timeout=timeout,
        )
    except (OSError, subprocess.TimeoutExpired) as exc:
        raise unavailable(f"fno-agents {verb_display} failed: {exc}") from exc
    if proc.returncode != 0:
        # With passthrough_stderr the child owns the real stderr (None here),
        # so name where it went instead of crashing on strip().
        detail = f"fno-agents {verb_display} exited {proc.returncode}"
        if passthrough_stderr:
            detail += " (its stderr went to your terminal)"
        else:
            detail += f": {proc.stderr.strip()[:200]}"
        raised = unavailable(detail)
        raised.returncode = proc.returncode
        raise raised
    try:
        return json.loads(proc.stdout)
    except ValueError as exc:
        raise unavailable(f"fno-agents {verb_display} bad output: {exc}") from exc
    finally:
        if os.environ.get("FNO_ROUTE_SLOT_DEBUG"):
            print(json.dumps({"payload": payload}), flush=True)


def chats_verb(args: "list[str]", payload: dict) -> dict:
    return verb_call(["chats", *args], payload, timeout=30)


def resolve_front_binary() -> Optional[Path]:
    """The native ``fno`` front binary: this checkout's build, then ``PATH``."""
    import shutil

    root = Path(__file__).resolve().parents[3] / "crates" / "fno" / "target"
    for profile in ("debug", "release"):
        if (root / profile / "fno").exists():
            return root / profile / "fno"
    return Path(found) if (found := shutil.which("fno")) else None

class FrontTimeout(VerbUnavailable):
    """The law door gave no answer in time. A retry meets the same wait."""


#: The law door's default bound. The read is machine-relative: measured 18s
#: wall for 2.7s user at load 176 (2026-10-06) and 61s at load 80
#: (2026-10-09), so the old fixed 60s wedged blueprint validation on a loaded
#: machine. Raise the knob there; an explicit ``timeout=`` argument outranks
#: it, and junk falls back to 60.
LAW_DOOR_TIMEOUT_ENV = "FNO_LAW_DOOR_TIMEOUT_SECONDS"


def _law_door_timeout() -> float:
    raw = os.environ.get(LAW_DOOR_TIMEOUT_ENV, "").strip()
    try:
        return float(raw) if raw else 60.0
    except ValueError:
        return 60.0


def call_front_json(payload: dict, *, timeout: Optional[float] = None) -> dict:
    """One round-trip with the front's law door, fail-closed like verb_call."""
    import json
    import subprocess

    if timeout is None:
        timeout = _law_door_timeout()

    binary = resolve_front_binary()
    if binary is None:
        raise VerbUnavailable("the native fno binary was not found")
    try:
        done = subprocess.run([str(binary), "inbox", "law", "match"],
            input=json.dumps(payload), capture_output=True, text=True, timeout=timeout)
    except subprocess.TimeoutExpired:
        # The read needs about 3s of CPU. Measured 2026-10-06 at load 176: 18s
        # wall for 2.7s user, so a timeout means the machine had no CPU to give.
        try:
            load = f"{os.getloadavg()[0]:.0f} on {os.cpu_count()} cores"
        except OSError:
            load = "unreadable"
        raise FrontTimeout(
            f"`fno inbox law match` gave no answer in {timeout:g}s (machine load {load})"
        ) from None
    if done.returncode != 0:
        raise VerbUnavailable(f"fno inbox law match exited {done.returncode}: {done.stderr.strip()}")
    return json.loads(done.stdout)


# ---------------------------------------------------------------------------
# Claim transport client + sweep instruments.
#
# Moved verbatim out of the ``fno.claims`` package (core.py, io.py, types.py,
# roster.py, cli.py) so the agents family can talk to the native claim store
# without importing the package: the package is scheduled for deletion by the
# claims-port deletion wave, and this module is the one Python seam every
# family keeps. The bodies are unchanged; only their home moved. Rust owns the
# protocol (crates/fno-agents/src/claims.rs, claim_cli/); this section is
# transport and instrumentation only. Module-level imports stay stdlib-only:
# every ``fno.*`` import below is function-local, exactly as in the originals.
# ---------------------------------------------------------------------------

from enum import Enum
from types import MappingProxyType
from typing import Mapping, NamedTuple
from urllib.parse import quote as _url_quote
import re
import subprocess

CLAIMS_DIRNAME = ".fno/claims"

# Env var naming the base dir under which GLOBAL (node-level) claims live.
# Node ids are global (like ~/.fno/graph.json), so a node:<id> lock must
# coordinate across worktrees rather than land in a worktree-local cwd. Shell
# call sites that manage node claims set this to "$HOME" so the claim lands at
# ~/.fno/claims.
CLAIMS_ROOT_ENV = "FNO_CLAIMS_ROOT"

# Raw key cap. The on-disk filename is `quote(key, safe="") + ".lock"`,
# which can expand non-ASCII / reserved chars up to ~3x. We validate
# the encoded filename length separately at acquire-time against
# MAX_ENCODED_FILENAME_BYTES, so MAX_KEY_LENGTH only needs to keep raw
# keys human-readable and prevent obvious denial-of-service strings.
MAX_KEY_LENGTH = 256

# Filesystem hard cap. ext4, HFS+, APFS all have 255-byte filename limits.
# acquire_claim rejects any key that would encode past this bound.
MAX_ENCODED_FILENAME_BYTES = 240  # 240 + ".lock" suffix = 245 bytes
MIN_TTL_MS = 60_000        # 1 minute
MAX_TTL_MS = 86_400_000    # 24 hours


class ClaimState(str, Enum):
    """Classification of a key's current state.

    - free: no claim file exists
    - live: claim exists and holder process is verifiably alive
    - suspect: TTL unexpired but holder not provably alive (dead/replaced pid).
      The respawned-worker case: TTL still protects the slot, so acquire/
      dispatch treat it like `live` (never steal); only TTL expiry (-> stale)
      frees it.
    - stale: claim exists but holder is dead or expired (recoverable)
    - corrupted: claim file present but cannot be parsed
    """

    FREE = "free"
    LIVE = "live"
    SUSPECT = "suspect"
    STALE = "stale"
    CORRUPTED = "corrupted"


class ClaimVerdictUnavailable(RuntimeError):
    """The native claim decision could not be reached."""


class ClaimVerdictError(RuntimeError):
    """The native claim decision returned an invalid or failed response."""


class ClaimHeldByOther(Exception):
    """A live claim is held by a different holder."""

    def __init__(self, holder: str, pid: Optional[int], host: str, key: str) -> None:
        self.holder = holder
        self.pid = pid
        self.host = host
        self.key = key
        super().__init__(f"claim {key!r} held by {holder} (pid={pid}, host={host})")


class HolderMismatch(Exception):
    """release/refresh called with a different holder than the existing claim."""

    def __init__(self, expected: str, actual: str, key: str) -> None:
        self.expected = expected
        self.actual = actual
        self.key = key
        super().__init__(f"claim {key!r}: holder mismatch (expected {expected!r}, got {actual!r})")


class ClaimValidationError(ValueError):
    """Inputs to a verb failed validation (ttl out of range, key too long, ...)."""


class ClaimContended(Exception):
    """acquire_claim/refresh_claim gave up after the native retry budget
    contention retries on the same key's recovery mutex.

    A distinct type from ClaimHeldByOther (a live claim is held by someone
    else) even though a caller usually treats both the same way ("can't have
    this one right now, retry later") - the recovery mutex being contended
    says nothing about who, if anyone, ends up holding the claim. Callers
    that only catch this narrow type cannot accidentally reclassify an
    unrelated RuntimeError raised deeper in the call stack (pydantic,
    resolve_self_identity, serialize_claim, ...) as contention.

    Callers of acquire_claim/refresh_claim should catch this ALONGSIDE
    ClaimHeldByOther, not instead of it - an except clause naming only one of
    the two lets the other escape uncaught.
    """


# Every acquire_claim/refresh_claim caller that treats "someone else has this
# right now" and "the recovery mutex is too busy to tell" the same way
# ("can't have this one right now, retry/skip") should catch this tuple
# instead of hand-rolling `except (ClaimHeldByOther, ClaimContended):` plus a
# restated comment at each site. A caller that needs to read `.holder`/`.pid`/
# `.host` (ClaimHeldByOther-only attributes) still needs its own separate
# `except ClaimHeldByOther as exc:` block ahead of this one.
CLAIM_UNAVAILABLE = (
    ClaimHeldByOther,
    ClaimContended,
    ClaimVerdictError,
    ClaimVerdictUnavailable,
)


class ForceReleaseOutcome(NamedTuple):
    """What a force-release found at the path; archived=False means nothing was there."""

    path: Path
    archived: bool
    previous_holder: Optional[str]


#: The dispatcher-minted handover holder: the suffix is the launched WORKER's
#: name, and the record's own `session_id` is the dispatcher's, not the
#: worker's. Mirrored by `HANDOVER_HOLDER_PREFIX` in the Rust client entry.
HANDOVER_HOLDER_PREFIX = "spawn-handover:"

_TTL_PATTERN = re.compile(r"^\s*(\d+)\s*([smh]?)\s*$", re.IGNORECASE)


def parse_ttl_ms(value: Optional[str]) -> Optional[int]:
    """Convert "1h" / "30m" / "3600s" / "5000" into milliseconds.

    Empty string and None return None (caller decides default).
    Plain digits are interpreted as seconds, matching ``sleep`` convention.

    The one ttl parser: `fno.claims.cli._parse_ttl` wraps this so its CLI
    flags keep raising typer.BadParameter.
    """
    if value is None or value == "":
        return None
    m = _TTL_PATTERN.match(value)
    if not m:
        raise ValueError(f"invalid TTL format: {value!r} (use '30m', '1h', '3600s')")
    n = int(m.group(1))
    unit = m.group(2).lower()
    if unit == "s" or unit == "":
        return n * 1000
    if unit == "m":
        return n * 60_000
    if unit == "h":
        return n * 3_600_000
    raise ValueError(f"unknown TTL unit: {unit!r}")


def global_claims_root() -> Path:
    """Base dir for GLOBAL (node-level) claims.

    ``claims_dir(global_claims_root())`` is ``~/.fno/claims`` by default,
    a sibling of the global ``~/.fno/graph.json``. Honors
    ``$FNO_CLAIMS_ROOT`` so tests (and any path-config override) can
    redirect it; falls back to the user's home directory.
    """
    override = os.environ.get(CLAIMS_ROOT_ENV)
    return Path(override) if override else Path.home()


def claims_dir(root: Path | None = None) -> Path:
    """Return the claims directory under the given root.

    Cross-worktree coordination state. Resolution order for the base dir:

      1. explicit ``root`` argument (per-root claims, e.g. walker singleton),
      2. ``$FNO_CLAIMS_ROOT`` env var (global node claims),
      3. the repo's space (keyed on the canonical root), so claims from every
         worktree land in one directory and no claims dir lives in a checkout.
    """
    if root is not None:
        base: Path = root
    else:
        override = os.environ.get(CLAIMS_ROOT_ENV)
        if override:
            return Path(override) / CLAIMS_DIRNAME
        from fno.paths import space_dir

        # fno-owned state: <space>/claims directly, no nested .fno segment.
        return space_dir() / "claims"
    return base / CLAIMS_DIRNAME


def _configured_claim_root() -> Optional[Path]:
    value = os.environ.get("FNO_CLAIMS_ROOT", "").strip()
    return Path(value) if value else None


def native_root_flags(root: Optional[Path]) -> list[str]:
    return ["--root", str(root)] if root is not None else []


def claim_op(operation: str, key: str, flags: list[str]) -> dict[str, Any]:
    """Run one native claim operation and decode its JSON reply."""
    import json

    binary = resolve_binary()
    if binary is None:
        raise ClaimVerdictUnavailable(
            "fno-agents claim unavailable: set FNO_AGENTS_BIN or reinstall fno"
        )
    command = [str(binary), "claim", operation]
    # The root op's key IS the question, and an empty key is a legal input
    # (a colon-less key routes repo-local); list/reap pass key="" to mean no
    # key at all, so the falsy skip stays for them.
    if key or operation == "root":
        command.append(key)
    command.extend(flags)
    command.append("--json")
    try:
        result = subprocess.Popen(command, stdout=subprocess.PIPE,
                                  stderr=subprocess.PIPE, text=True)
        stdout, stderr = result.communicate()
    except OSError as exc:
        raise ClaimVerdictUnavailable(f"fno-agents claim could not run: {exc}") from exc
    try:
        payload = json.loads(stdout) if stdout.strip() else {}
    except ValueError as exc:
        raise ClaimVerdictError(f"fno-agents claim returned invalid JSON: {exc}") from exc
    if result.returncode == 1 and payload.get("outcome") == "held_by_other":
        raise ClaimHeldByOther(
            str(payload.get("holder") or "unknown"),
            payload.get("pid"),
            str(payload.get("host") or "unknown"),
            key,
        )
    # A corrupted row is an answer, not a failure: status prints its verdict
    # and exits 2 so shell callers notice.
    if operation == "status" and isinstance(payload, dict) and payload.get("state") == "corrupted":
        return payload
    if result.returncode != 0:
        detail = stderr.strip() or stdout.strip() or f"exit {result.returncode}"
        raise ClaimVerdictError(f"fno-agents claim {operation} failed: {detail}")
    if isinstance(payload, list) and operation == "list":
        return {"rows": payload}
    if not isinstance(payload, dict):
        raise ClaimVerdictError("fno-agents claim returned a non-object JSON value")
    return payload


def _validate_inputs(
    key: str,
    holder: str,
    ttl_ms: Optional[int],
    *,
    pid: Optional[int] = None,
    pid_unavailable: bool = False,
) -> None:
    if not key:
        raise ClaimValidationError("key must be non-empty")
    if len(key) > MAX_KEY_LENGTH:
        raise ClaimValidationError(f"key length {len(key)} exceeds MAX_KEY_LENGTH={MAX_KEY_LENGTH}")
    # Raw length passing MAX_KEY_LENGTH does not guarantee the encoded
    # filename fits the filesystem's 255-byte name limit. Check the
    # URL-encoded form explicitly: keys with many reserved characters
    # (slashes, colons) expand up to 3x.
    encoded_len = len(_url_quote(key, safe="").encode("utf-8"))
    if encoded_len > MAX_ENCODED_FILENAME_BYTES:
        raise ClaimValidationError(
            f"URL-encoded key length {encoded_len} exceeds "
            f"MAX_ENCODED_FILENAME_BYTES={MAX_ENCODED_FILENAME_BYTES}"
        )
    if not holder:
        raise ClaimValidationError("holder must be non-empty")
    if pid_unavailable and ttl_ms is None:
        raise ClaimValidationError("pid_unavailable requires a TTL claim")
    if pid_unavailable and pid is not None:
        raise ClaimValidationError("--pid and --pid-unavailable are mutually exclusive")
    if pid is not None and pid <= 0:
        raise ClaimValidationError("pid must be positive")
    if ttl_ms is not None and not (MIN_TTL_MS <= ttl_ms <= MAX_TTL_MS):
        raise ClaimValidationError(f"ttl_ms={ttl_ms} out of range [{MIN_TTL_MS}, {MAX_TTL_MS}]")


def native_acquire(
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
) -> dict[str, Any]:
    """Take one claim natively; the payload-level acquire.

    Returns the receipt payload; raises ClaimHeldByOther (with `.holder`/
    `.pid`/`.host`) when a live claim blocks, per `claim_op`. The Claim-typed
    wrapper lives in ``fno.claims.core`` until the package deletion moves the
    last importers.
    """
    _validate_inputs(key, holder, ttl_ms, pid=pid, pid_unavailable=pid_unavailable)
    native_root = root or _configured_claim_root()
    flags = ["--holder", holder]
    if ttl_ms is not None:
        flags.extend(("--ttl-ms", str(ttl_ms)))
    if reason is not None:
        flags.extend(("--reason", reason))
    if metadata is not None:
        import json

        flags.extend(("--metadata", json.dumps(metadata, separators=(",", ":"))))
    if not pid_unavailable:
        flags.extend(("--pid", str(pid if pid is not None else os.getpid())))
    if pid_unavailable:
        flags.append("--pid-unavailable")
    for flag, value in [("--host", host), ("--harness", harness), ("--pid-provenance", pid_provenance), ("--session-id", harness_session_id)]:
        if value is not None:
            flags.extend((flag, str(value)))
    flags.extend(native_root_flags(native_root))
    return claim_op("acquire", key, flags)


def native_release(
    key: str,
    holder: str,
    *,
    strict: bool = False,
    root: Optional[Path] = None,
) -> Optional[dict[str, Any]]:
    """Drop one claim natively; the payload-level release.

    Returns the `--with-claim` receipt payload, or None when the store
    answers not-released. `strict` reads the claim first and raises
    HolderMismatch on a foreign holder.
    """
    if not key or not holder:
        raise ClaimValidationError("key and holder must be non-empty")
    native_root = root or _configured_claim_root()
    prior_payload = claim_op("status", key, native_root_flags(native_root)) if strict else {}
    if prior_payload.get("state") not in {None, "free"} and prior_payload.get("holder"):
        if prior_payload["holder"] != holder and strict:
            raise HolderMismatch(holder, prior_payload["holder"], key)
    # --with-claim: the wave-2 leaf port moved the release surface into
    # claim_cli; the operator --json receipt is the frozen two-field shape,
    # and the engine leg asks for the removed record so this parse is
    # unchanged.
    receipt = claim_op(
        "release", key, ["--holder", holder, "--with-claim", *native_root_flags(native_root)]
    )
    return receipt if receipt.get("released") is True else None


def _unrouted_key_verdict(key: str) -> Optional[dict[str, Any]]:
    """A bare well-formed node id names no store, and ``free`` there reads as
    safe-to-dispatch. Both legs answer the same unknown."""
    if not key or ":" in key:
        return None
    from fno.graph._constants import is_wellformed_node_id

    if not is_wellformed_node_id(key):
        return None
    return {
        "key": key,
        "state": "unknown",
        "basis": "key-unrouted",
        "detail": f"{key!r} has no claim prefix; node claims are keyed node:{key}",
    }


def claim_status_payload(key: str, *, root: Optional[Path] = None) -> dict[str, Any]:
    """Inspect one key natively; the payload-level status read."""
    unrouted = _unrouted_key_verdict(key)
    if unrouted is not None:
        return unrouted
    if not key:
        raise ClaimValidationError("key must be non-empty")
    return claim_op("status", key, native_root_flags(root or _configured_claim_root()))


def native_force_release(
    key: str,
    reason: str,
    *,
    root: Optional[Path] = None,
    holding_recovery_lock: bool = False,
    expected_claim: Optional[dict[str, Any]] = None,
) -> ForceReleaseOutcome:
    """Administrative override release; the payload-level force-release.

    ``expected_claim`` is the previously read record payload (a native sweep
    row round-trips: serde ignores the row's computed fields), arming the
    compare-and-release so a claim that changed between read and release is
    refused instead of stolen.
    """
    if not key:
        raise ClaimValidationError("key must be non-empty")
    if not reason:
        raise ClaimValidationError("reason must be non-empty for force-release")
    import json

    flags = ["--reason", reason]
    if holding_recovery_lock:
        flags.append("--holding-recovery-lock")
    if expected_claim is not None:
        flags.extend(("--expected-claim", json.dumps(expected_claim)))
    payload = claim_op(
        "force-release", key,
        [*native_root_flags(root or _configured_claim_root()), *flags],
    )
    return ForceReleaseOutcome(
        path=Path(str(payload.get("path") or "")),
        archived=bool(payload.get("archived")),
        previous_holder=payload.get("previous_holder"),
    )


def native_claims_root(key: str) -> Optional[Path]:
    """The claims root the native leg resolves ``key`` against, or None for a
    repo-local key. The ONE routing read for callers that need the PATH (the
    prefix list lives once, in the Rust
    ``claims_root.rs``); the lockfile operations themselves never needed it.
    Raises :class:`ClaimVerdictUnavailable` when the fno-agents binary is
    missing, like every native claim call."""
    payload = claim_op("root", key, [])
    root = payload.get("root")
    return Path(str(root)) if root else None


# --- Sweep instruments: the roster-backed settlement and abandonment probes.
#
# The Python half of the reap decision. The native sweep verdict
# (`claim_op("sweep", ...)` rows) carries `provably_dead`/`bucket`, and these
# instruments refine it with evidence that lives outside the claim store: the
# fleet roster, the transcript tails, the mux pane listing, the graph's
# terminal nodes. Their Rust twins live in crates/fno-agents/src/claim_cli/
# reap.rs; until the sweep callers go native, this is the one home.

#: Seconds the roster cross-check may spend shelling out to the harness.
#: A status read is interactive, so it must answer late-but-honestly
#: ("roster not consulted") rather than hang a lead mid-decision.
_ROSTER_CROSSCHECK_TIMEOUT_S = 10.0


class RosterReading(NamedTuple):
    consulted: bool
    rows_scanned: int
    workers_by_node: dict
    reason: str = ""
    rows_by_session: Mapping = MappingProxyType({})
    rows_unresolved: int = 0
    unresolved_rows: tuple = ()
    unmeasurable_by_node: Mapping = MappingProxyType({})

    def workers_on(self, node_id: str) -> list:
        return self.workers_by_node.get(node_id, [])

    def row_for_session(self, session_id: str):
        return self.rows_by_session.get(session_id)


def read_roster(
    timeout: float = 10.0, require_live_probe: bool = True
) -> RosterReading:
    """Read the fleet once and index it by resolved node id.

    ``require_live_probe``: claim-status liveness refuses when the probe
    never ran; the worked overlay passes False (the registry-only view
    still carries attribution).
    """
    try:
        from fno.agents.watchdog import fleet_rows

        rows, warnings = fleet_rows(timeout=timeout)
    except Exception as exc:  # noqa: BLE001 - any failure must degrade loudly
        return RosterReading(False, 0, {}, f"{type(exc).__name__}: {exc}")

    from fno.agents.watchdog import ADVISORY_WARNING_PREFIX, UNMEASURABLE_ROW_PREFIX

    # The harnesses' degraded-probe wording, as a literal: an import would be a layering edge.
    registry_only_mark = "falling back to registry-only view"
    unmeasurable: dict = {}
    blocking = []
    degraded_reason = ""
    for w in warnings:
        idx = w.find(UNMEASURABLE_ROW_PREFIX)
        if idx == -1:
            degraded_probe = (not require_live_probe) and (registry_only_mark in w)
            if not w.startswith(ADVISORY_WARNING_PREFIX) and not degraded_probe:
                blocking.append(w)
            elif degraded_probe and not degraded_reason:
                # Tolerated, not silent: the caller learns the probe degraded,
                # so a registry-only answer never masquerades as a full one.
                degraded_reason = w
            continue
        fields = dict(
            tok.split("=", 1)
            for tok in w[idx + len(UNMEASURABLE_ROW_PREFIX):].split()
            if "=" in tok
        )
        if fields.get("node"):
            unmeasurable.setdefault(fields["node"], []).append(
                fields.get("name") or "unknown"
            )

    if blocking:
        return RosterReading(False, 0, {}, blocking[0])

    index: dict = {}
    by_session: dict = {}
    unresolved: list[dict] = []
    for r in rows:
        entry = {
            "name": r.name,
            "state": r.state,
            "cwd": r.cwd,
            "row_id": str(r.row_id or ""),
            "pid": getattr(r, "pid", None),
            "pid_start_time": getattr(r, "pid_start_time", None),
            "mux": getattr(r, "mux", None),
            "stopped_at": getattr(r, "stopped_at", None),
        }
        if r.node:
            index.setdefault(r.node, []).append(entry)
        else:
            unresolved.append(entry)
        if r.row_id:
            by_session[str(r.row_id)] = entry
    return RosterReading(True, len(rows), index, degraded_reason, by_session, len(unresolved), tuple(unresolved), unmeasurable)


def _finished_row_states() -> frozenset:
    from fno.agents.watchdog import _TERMINAL_STATES, _WAKE_STATES

    return _TERMINAL_STATES - _WAKE_STATES


def _node_settlement(reading: Optional[RosterReading] = None):
    """The closure-shaped reading ``sweep_verdict`` runs FIRST on a node claim
    : is this claim's own node still the holder's workplace?

    Two positive findings, both proven by FINDING things, never by failing to:

      * The claim's node is terminal in the graph (done/superseded) and the
        holder cannot be proven alive. The closure release should have
        dropped the claim already; one that outlived its node (pre-fix
        leaks, a closer that crashed mid-release) protects nothing once its
        lease is spent or its pid is gone. A LIVE holder keeps the claim
        until its own lease ends: measured 2026-09-05, this arm reaped four
        unexpired, live-pid claims on closed nodes (twice,
        -node) on every sweep, killing active loop-check leases and
        opening the dup-PR window each time.
      * The lease is EXPIRED and the holder's roster row resolves to a
        DIFFERENT node. An expired lease is the holder's own statement that
        it stopped renewing; a row on another node is where it went. An
        unexpired lease is never settled away from a live holder.

    Everything else answers None, and None keeps: an unreadable graph, an
    unconsulted roster, an absent row (not-found is not gone), a row whose
    node did not resolve, an unparseable holder, a handover launch window.
    Mirrors ``_abandonment_probe``: never raises, instruments read lazily and
    cached across the sweep, one roster reading shareable from a caller that
    already took it outside whatever lock it holds.
    """
    cache: dict = {}

    def _terminal_ids():
        if "terminal" not in cache:
            try:
                from fno.tracker import active_backend_name

                if active_backend_name() != "graph":
                    # graph.json is not the store under an external tracker
                    # backend; a terminal reading taken from it would be a
                    # wrong answer, and unknown keeps.
                    cache["terminal"] = None
                    return None
                from fno.graph.statuses import is_terminal_entry
                from fno.graph import api as graph_api
                from fno.paths import graph_json

                # is_terminal_entry, not a bare completed_at test: read_graph
                # does not run the recompute migration, so a legacy
                # "deferred:<ts>" row still carries deferral inside
                # completed_at, and deferral is a returnable rung.
                cache["terminal"] = frozenset(
                    e.get("id")
                    for e in (
                        n.model_dump(by_alias=True)
                        for n in graph_api.nodes(include_archived=True, path=graph_json()).nodes
                    )
                    if is_terminal_entry(e)
                )
            except Exception:  # noqa: BLE001 - an unreadable graph proves nothing
                cache["terminal"] = None
        return cache["terminal"]

    def _probe(claim, native_verdict=None) -> Optional[bool]:
        node_id = claim.key[len("node:") :]
        terminal = _terminal_ids()
        if terminal is not None and node_id in terminal:
            # Closure settles a holder that cannot be proven alive, never one
            # that can. Measured 2026-09-05 on four premature reaps (
            # twice, -node): every one was an UNEXPIRED lease
            # with a live recorded pid on a node the graph had closed, reaped
            # by this arm on each sweep - a live session's loop-check lease
            # (the follow-up-work shape) died with it, and every reap opened
            # the dup-PR window the claim exists to close. An expired lease
            # still settles here, as does a dead pid: the holder named its
            # own end or the pid table ends it, and a closure release that
            # crashed mid-way is exactly the leak this arm heals.
            if native_verdict is not None and (
                native_verdict.get("expired") is not True
                and native_verdict.get("bucket") != "suspect"
            ):
                # Not expired and not dead-pid: the holder is proven live
                # (bucket live) or unprobeable (offhost - unknown keeps, the
                # same asymmetry the suspect buckets teach).
                return None
            return True
        if native_verdict is None or native_verdict.get("expired") is not True:
            return None
        if claim.holder.startswith(HANDOVER_HOLDER_PREFIX):
            # A launch window, never a settled abandonment: same reasoning as
            # the suspect probe below.
            return None
        session_id = _holder_session_id(claim.holder)
        if not session_id:
            return None
        if "reading" not in cache:
            cache["reading"] = (
                reading
                if reading is not None
                else read_roster(timeout=_ROSTER_CROSSCHECK_TIMEOUT_S)
            )
        seen: RosterReading = cache["reading"]
        if not seen.consulted:
            return None
        if seen.row_for_session(session_id) is None:
            # Not found is not gone. See _abandonment_probe.
            return None
        # The roster indexes rows BY node; invert it for this session. A row
        # present but under no node (never ran target init) says the holder
        # is alive but nothing about THIS claim.
        holder_node = next(
            (
                nid
                for nid, rows in seen.workers_by_node.items()
                for r in rows
                if r.get("row_id") == session_id
            ),
            None,
        )
        if holder_node is None:
            return None
        # A different node is positive abandonment; the same node falls
        # through to liveness, which keeps it.
        return True if holder_node != node_id else None

    return _probe


def _transcript_says_finished(session_id: str, cwd: str) -> bool:
    """Has this session gone quiet with a tail that is not engaged?

    ``finished_with_the_tree`` is the one question the occupancy tally and the
    reap predicate both ask, so asking it here cannot drift from either. Every
    unreadable input answers False, and False keeps the claim.
    """
    try:
        import time

        from fno.agents.watchdog import (
            QUIET_AFTER_S,
            finished_with_the_tree,
            harness_for_session,
            tail_facts,
        )

        return finished_with_the_tree(
            tail_facts(session_id, cwd, agent=harness_for_session(session_id)),
            time.time(),
            QUIET_AFTER_S,
        )
    except Exception:  # noqa: BLE001 - an unreadable transcript proves nothing
        return False


def _holder_session_id(holder: str) -> Optional[str]:
    """The session id inside a claim holder, via the canonical parser.

    One holder vocabulary, owned by ``fno.agents.truth_status``. A foreign
    holder shape returns None and condemns nothing.
    """
    try:
        from fno.agents.truth_status import _session_from_holder

        return _session_from_holder(holder)
    except Exception:  # noqa: BLE001 - an unparseable holder proves nothing
        return None


def _mux_pane_absent_for(worker: str, node_id: str = "", runner=None) -> Optional[bool]:
    """True when the mux enumerated its panes and none plausibly hosts WORKER.

    The evidence a handover claim's death leaves outside the claim: the
    spawner launched the worker into a mux pane. Presence is matched against
    EVERY identity a spawned worker's pane can carry, because a miss on any
    one of them reaps a LIVE worker's claim (the ``reaped_a_live_worker``
    disaster): the pane's ``fno_id`` is the SESSION uuid, not the worker
    name (mux_spawn stamps ``fno_id=stored_session_uuid or name``), the OSC
    ``title`` is whatever the pane's shell set, and the load-bearing join is
    the worktree: dispatch names the worker's worktree after the NODE id, so
    ``basename(pane.cwd) == node_id`` is the normal live-launch marker, with
    a worker-named tree still possible from older spawns. Follows
    ``_pane_absent_from_listing``'s empty-is-ambiguous rule: ``pane ls``
    prints ``[]`` both for a session with no panes and for an unreachable
    socket, so only a NON-EMPTY listing somewhere proves the instrument ran
    and absence from it is a finding. True = positively absent, False =
    present, None = cannot tell (and None keeps the claim).

    Residual, named: a ``worktree.policy = never`` project launches the
    worker at its repo root, where no pane identity names the worker - there
    this helper can read absence while a live worker runs, so the handover
    early-reap arm must not be the only thing standing between that worker
    and a steal (its TTL expiry still governs, and the pid check still
    refuses while the spawner lives).
    """
    import json as _json
    import subprocess as _subprocess

    if runner is None:
        runner = _subprocess.run

    def _mux(*args: str):
        import os

        fno_bin = os.environ.get("FNO_BIN") or "fno"

        try:
            return runner([fno_bin, *args], capture_output=True, text=True, timeout=10)
        except Exception:  # noqa: BLE001 - a probe never fails a sweep
            return None

    ls = _mux("mux", "ls", "--json")
    if ls is None or getattr(ls, "returncode", 1) != 0:
        return None
    try:
        sessions = _json.loads(ls.stdout or "")
    except (ValueError, TypeError):
        return None
    if not isinstance(sessions, list):
        return None
    names = {worker, node_id} - {""}
    saw_content = False
    for row in sessions:
        if not isinstance(row, dict) or row.get("state") != "live":
            continue
        session = row.get("session")
        if session is None or session == "":
            return None
        if "panes" not in row:
            return None
        try:
            n_panes = int(row["panes"])
        except (TypeError, ValueError):
            return None
        if not n_panes:
            # A zero-pane session would only contribute an
            # ambiguous []; the probe never raises on a malformed row.
            continue
        panes = _mux("mux", "pane", "ls", "--server", str(session), "--json")
        if panes is None or getattr(panes, "returncode", 1) != 0:
            return None
        try:
            listing = _json.loads(panes.stdout or "")
        except (ValueError, TypeError):
            return None
        if not isinstance(listing, list) or not listing:
            return None
        saw_content = True
        for pane in listing:
            if not isinstance(pane, dict):
                continue
            cwd_name = str(pane.get("cwd") or "").rstrip("/").rsplit("/", 1)[-1]
            if pane.get("fno_id") in names or pane.get("title") in names or cwd_name in names:
                return False
    return True if saw_content else None


def _claim_worktree_cwd(claim) -> Optional[str]:
    """The worktree path a claim's writer stamped into metadata, if any.

    The transcript fallback needs a cwd to find the session's tree; the
    roster row (the usual source) is absent on exactly the paths that need
    the fallback, so the claim itself carries it when its writer could.
    Accepts a Claim object OR a plain dict row (list_cmd's shape) - a bare
    `getattr(a_dict, "metadata", None)` silently returns None for every
    dict, so the two shapes need this one branch to share an implementation.
    """
    meta = claim.get("metadata") if isinstance(claim, dict) else getattr(claim, "metadata", None)
    meta = meta or {}
    for key in ("worktree", "cwd"):
        value = meta.get(key)
        if isinstance(value, str) and value:
            return value
    return None


def _handover_pane_probe_blocked(claim) -> bool:
    """Return True when pane identity cannot prove an in-place worker dead.

    ``worktree.policy = never`` launches the worker at the repository root, so
    its pane cwd cannot carry the worker identity. An unreadable policy is the
    same safety answer: keep the claim until its TTL rather than reap on pane
    absence.
    """
    try:
        from fno.worktree_paths import resolve_worktree_policy

        cwd = _claim_worktree_cwd(claim)
        repo_root = Path(cwd) if cwd else Path.cwd()
        return resolve_worktree_policy(repo_root).policy == "never"
    except Exception:  # noqa: BLE001 - an unreadable policy proves nothing
        return True


def _abandonment_probe(reading: Optional[RosterReading] = None):
    """The roster-backed SUSPECT abandonment probe (agents surface).

    Returns a callable answering ``True`` (proven abandoned), ``False`` (the
    holder is still working) or ``None`` (unproven, so the claim is kept). One
    roster READING is taken lazily and shared across every claim in the sweep.

    ABANDONMENT IS PROVEN BY FINDING THE HOLDER, NEVER BY FAILING TO FIND IT.
    The probe resolves the claim's own holder session id and requires its roster
    row to exist and read terminal. An absent row answers ``None``.

    That asymmetry is the whole safety argument, and an earlier version of this
    function got it wrong in the exact way the third pitfalls entry describes.
    It asked "does any row resolve to this node", read the empty answer as
    abandonment, and defended it with a scanned-row count. But a row count
    validates the INSTRUMENT, never the TARGET. ``fleet_rows`` enumerates
    ``claude agents --json --all`` and drops interactive rows, so a codex worker,
    an opencode worker, and any hand-started session are invisible to it BY
    CONSTRUCTION. A forty-row scan that cannot represent the holder at all would
    have read as forty rows of proof, and reaping on it archives a live worker's
    claim - disaster from the other side, which the
    ``reaped_a_live_worker`` kill criterion exists to stop.

    So the roster's coverage gap now costs a missed reap rather than a wrongly
    archived claim. That is the correct direction to fail: an unreaped claim
    expires on its own TTL, and a wrongly reaped one hands a live worker's node
    to a second worker.

    THREE additions feed it evidence that lives outside the claim (the lead's
    2026-08-23 specimens, both of which needed a manual --force release):

    1. A handover holder is no longer unconditionally unprovable. Its launch
       window is observable: the mux pane the spawner created is ABSENT from
       every live listing AND the recorded pid is dead. Both absences together
       are the window over - a positive finding, never a failed lookup.
    2. A roster reading that degraded (``roster not consulted``) is retried
       ONCE before the sweep answers, so a transient ``claude not on PATH``
       does not strand the whole pass.
    3. A dead recorded pid with a parseable session id continues to the
       transcript check even without a roster row, when the claim carries a
       worktree path to find the tree with. A finished tree is a positive
       finding.
    """
    cache: dict = {}
    # One token per None answer, folded into kept_suspect_unprobed_by.
    reasons: dict = {}

    def _reading() -> RosterReading:
        """Take (and cache) the shared reading; retry once on a degraded read."""
        if "reading" not in cache:
            first = (
                reading
                if reading is not None
                else read_roster(timeout=_ROSTER_CROSSCHECK_TIMEOUT_S)
            )
            if not first.consulted and reading is None:
                # One retry, never a loop: a degraded read is usually a
                # transient spawn failure, and a second degradation is a real
                # one the sweep should report rather than paper over.
                cache["reading"] = read_roster(timeout=_ROSTER_CROSSCHECK_TIMEOUT_S)
            else:
                cache["reading"] = first
        return cache["reading"]

    def _probe(claim, native_verdict=None) -> Optional[bool]:
        native = native_verdict
        if native is None:
            reasons[claim.key] = "native-verdict-missing"
            return None

        if claim.holder.startswith(HANDOVER_HOLDER_PREFIX):
            # The launch window, not an abandoned session - but the window is
            # observable from outside: the pane the spawner created and the
            # spawner's own pid. BOTH absent is the window over (a positive
            # finding); a pane that still hosts the worker, a live spawner
            # pid, or a mux that cannot answer all keep the claim. The pid
            # check is local and cheap, so it runs before the pane subprocess;
            # the pane answer is cached per worker for the sweep's lifetime,
            # like the shared roster reading.
            if native is not None and native.get("state") == ClaimState.LIVE.value:
                reasons[claim.key] = "handover-live"
                return None
            if _handover_pane_probe_blocked(claim):
                reasons[claim.key] = "handover-pane-probe-blocked"
                return None
            worker = claim.holder[len(HANDOVER_HOLDER_PREFIX) :]
            node_id = claim.key[len("node:") :] if claim.key.startswith("node:") else ""
            pane_key = f"pane_absent:{worker}"
            if pane_key not in cache:
                cache[pane_key] = _mux_pane_absent_for(worker, node_id)
            if cache[pane_key] is not True:
                reasons[claim.key] = "handover-pane-not-absent"
                return None
            return True
        session_id = _holder_session_id(claim.holder)
        if not session_id:
            # A holder shape this lane cannot parse names no session to look up.
            reasons[claim.key] = "holder-session-unparsed"
            return None
        seen: RosterReading = _reading()
        if not seen.consulted:
            reasons[claim.key] = "roster-read-degraded"
            return None
        row = seen.row_for_session(session_id)
        if row is None:
            # Not found is not gone - but with the holder's own pid dead, one
            # more positive instrument remains: the transcript. It needs only
            # a cwd to find the tree, and the claim carries one when its
            # writer stamped it. A finished tree is abandonment proven by
            # FINDING the end, never by failing to find the worker.
            if native is not None and native.get("state") == ClaimState.LIVE.value:
                reasons[claim.key] = "row-absent-native-live"
                return None
            cwd = _claim_worktree_cwd(claim)
            if not cwd:
                reasons[claim.key] = "row-absent-no-cwd"
                return None
            if not _transcript_says_finished(session_id, cwd):
                reasons[claim.key] = "row-absent-transcript-unfinished"
                return None
            return True
        if row.get("state") not in _finished_row_states():
            return False
        # The row state alone does NOT authorize a reap. `_TERMINAL_STATES`
        # carries its own warning: the roster called a WORKING session done on
        # 2026-08-15, and occupancy stopped keying on it for exactly that
        # reason. Reaping on a field known to lie in this direction archives a
        # live worker's claim, which is the `reaped_a_live_worker` kill
        # criterion. So the row narrows the candidates and the transcript
        # decides, which is the instrument the watchdog itself trusts.
        if not _transcript_says_finished(session_id, row.get("cwd") or ""):
            reasons[claim.key] = "row-transcript-unfinished"
            return False
        return True

    setattr(_probe, "reasons", reasons)
    return _probe
