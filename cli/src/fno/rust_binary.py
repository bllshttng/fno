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

import json as _json
import os
import shutil
import subprocess
import sys

from functools import lru_cache
from pathlib import Path
from typing import Any, Optional, Sequence

BINARY_NAME = "fno-agents.exe" if os.name == "nt" else "fno-agents"

#: Operator override. The shell shims (scripts/run-target-loop.sh, the stop
#: hooks) have always honored this, so the Python resolver honors it too rather
#: than making the same export mean two different things depending on which half
#: of the toolchain reads it.
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
    command: str, args: Sequence[str] = (), *, timeout: Optional[float] = 60,
    binary: Optional[Path] = None,
    input_text: Optional[str] = None,
) -> tuple[Optional[str], Any]:
    """Run one direct ``fno-agents`` client command and parse its JSON stdout.

    ``input_text`` feeds the child's stdin when given, so a large payload
    rides stdin instead of an argv entry (the OS per-argument cap would
    otherwise become an undocumented body cap).

    Returns ``(error, parsed)``: ``error`` is None on success; a missing
    binary, non-zero exit, timeout, or unparseable stdout yields a short error
    text and a None payload. Callers keep the failure shape theirs (refuse
    closed, raise, or exit) - this seam only standardizes the door.

    ``binary`` pre-resolves the door; the dev build outranks the stale
    installed copy (the rule :func:`command_call` states).
    """
    import json
    import subprocess

    binary = binary or resolve_binary()
    if binary is None:
        return ("fno-agents binary not found", None)
    try:
        proc = subprocess.run(
            [str(binary), command, *args], capture_output=True, text=True, timeout=timeout,
            input=input_text,
        )
    except subprocess.TimeoutExpired:
        bound = f"{timeout:.1f}s" if timeout is not None else "the caller's bound"
        return (f"timed out after {bound}", None)
    except OSError as exc:
        return (str(exc)[:200], None)
    if proc.returncode != 0:
        return ((proc.stderr or "command failed").strip()[:200], None)
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


def style_receipt_advisory(text: str) -> Optional[dict]:
    """The style receipt for an advisory read: None on a door failure.

    ``word_cap`` rides the receipt (the configured ``style.word_cap.encounter``
    or the model's 80 default), so an advisory learns cap and count in one
    spawn instead of its own config read.
    """
    err, receipt = call_binary_json("style-check", ["--stdin", "--json"], input_text=text)
    if err or not isinstance(receipt, dict):
        return None
    return receipt


def style_word_count(text: str) -> int:
    """Masked word count through the style door; 0 when the door fails.

    The counting sites are advisory (a ledger row, a long-note nudge), so a
    door failure counts zero rather than blocking the send.
    """
    receipt = style_receipt_advisory(text) or {}
    return int(receipt.get("word_count") or 0)


def settings_read(detect: Optional[str] = None) -> Optional[dict]:
    """One settings fact-set read through the door; None when it fails.

    The receipt carries the node-id knobs (``prefix``, ``hex_width``), the
    config ``candidates``, the work-map ``projects`` pairs, and the optional
    cwd -> project ``project`` answer. Callers are fail-open resolvers, so a
    door failure reads as None and the caller keeps its legacy default.
    """
    import json

    request = json.dumps({"detect": detect}) if detect is not None else "{}"
    err, receipt = call_binary_json("state", ["settings-read"], input_text=request)
    if err or not isinstance(receipt, dict):
        return None
    return receipt


def plan_schema(payload: dict) -> Optional[dict]:
    """One plan-frontmatter validation read through the door; None on failure.

    ``payload`` carries the op (``validate-frontmatter`` or ``gates``) with
    its arguments. YAML dates are flattened to ISO strings so the payload is
    JSON-safe, and a door failure reads as None: the caller keeps its own
    failure posture.
    """
    import datetime
    import json

    def _plain(value):
        if isinstance(value, (datetime.datetime, datetime.date)):
            return value.isoformat()
        if isinstance(value, dict):
            return {k: _plain(v) for k, v in value.items()}
        if isinstance(value, (list, tuple)):
            return [_plain(v) for v in value]
        return value

    # The dev build outranks the installed copy: a validating surface must
    # answer with its own checkout's schema, never a stale binary that lacks
    # the op (the rule command_call states for the same reason).
    err, receipt = call_binary_json(
        "state",
        ["plan-schema"],
        binary=find_dev_binary() or resolve_binary(),
        input_text=json.dumps(_plain(payload), default=str),
    )
    if err or not isinstance(receipt, dict):
        return None
    return receipt


class MailShapeError(RuntimeError):
    """The Rust mail-shape classifier failed or returned malformed output."""


def mail_shape(texts: list) -> list[dict]:
    """Per text: framing, msg_id, ids, holds_tag, envelope_block,
    legacy_tags, header_turns, relay_parse."""
    binary = find_dev_binary() or resolve_binary() or "fno-agents"
    try:
        result = subprocess.run(
            [str(binary), "mail-envelope", "--classify"],
            input=_json.dumps(texts),
            capture_output=True,
            text=True,
            timeout=5,
        )
    except subprocess.TimeoutExpired as exc:
        raise MailShapeError("mail-envelope --classify timed out after 5s") from exc
    if result.returncode:
        raise MailShapeError(result.stderr.strip() or "mail-envelope --classify failed")
    try:
        parsed = _json.loads(result.stdout)
    except _json.JSONDecodeError as exc:
        raise MailShapeError(f"malformed classifier output: {exc}") from None
    if not isinstance(parsed, list) or len(parsed) != len(texts):
        raise MailShapeError(f"classifier returned {len(parsed)} items for {len(texts)} texts")
    return parsed


def mint_fno_id() -> str:
    """Footnote's one session-id mint lives in Rust: `fno-agents state mint-id`.

    Raises CommandUnavailable when the binary is missing or answers nothing; no
    Python-side fallback exists, by the rule that keeps the mint single.
    """
    import subprocess

    binary = resolve_binary()
    if binary is None:
        raise CommandUnavailable("fno-agents binary not found; run `fno doctor update`")
    proc = subprocess.run(
        [str(binary), "state", "mint-id"], capture_output=True, text=True, timeout=10
    )
    minted = proc.stdout.strip()
    if proc.returncode != 0 or not minted:
        raise CommandUnavailable(
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
    Raises CommandUnavailable when the binary is missing or answers nothing; no
    Python-side fallback exists, by the rule that keeps the vocabulary single.
    """
    error, payload = call_binary_json(
        "terminals", timeout=15, binary=find_dev_binary() or resolve_binary()
    )
    if error is not None:
        raise CommandUnavailable(
            "the delivered-terminal vocabulary lives in the fno-agents binary"
            " (loopcheck.rs TerminationReason::is_delivered) and the read"
            f" failed: {error}; run `fno doctor update --rust` or set FNO_AGENTS_BIN"
        )
    names = payload.get("delivered") if isinstance(payload, dict) else None
    if not names or not all(isinstance(n, str) and n for n in names):
        raise CommandUnavailable(
            f"fno-agents terminals answered no usable delivered list: {payload!r}"[:200]
        )
    return frozenset(names)


def sidecar_get(id: str) -> dict:
    """One id's sidecar fields through the native tracker door.

    Answers a plain dict (sparse: nulls dropped, the list fields defaulted
    to ``[]``). A missing row or file answers the empty shape (``id`` plus
    the list defaults) - the "no sidecar yet" case, never an error. A file
    that fails the sidecar validation raises, the way the old model's
    ValidationError did. The backend is the door's own resolution; the
    guard helper lives in ``fno.graph.api``.
    """
    out = command_call(["backlog", "get"], {"tracker": "sidecar-get", "id": id})
    if out.get("error"):
        raise CommandUnavailable(f"sidecar read refused: {out['error']}")
    return out["sidecar"]


def sidecar_all() -> dict:
    """Every id's sidecar fields in one native read (the scan primitive).

    Graph mode projects the store's rows (archived residents excluded,
    invalid projections skipped); an external backend decodes the per-id
    files. Values are plain JSON - ``sc.get("cwd")`` style access.
    """
    return command_call(["backlog", "get"], {"tracker": "sidecar-all"})["sidecars"]


def sidecar_set(id: str, fields: dict) -> Path:
    """Merge ``fields`` into the id's sidecar under the selected backend's
    own store: the graph row under the store's optimistic write cycle, the
    per-id file otherwise. Returns the physical path written. A key outside
    the sidecar set refuses - the tracker owns it.
    """
    out = command_call(["backlog", "get"], {"tracker": "sidecar-set", "id": id, "fields": fields})
    if out.get("error"):
        raise CommandUnavailable(f"sidecar write refused: {out['error']}")
    return Path(out["saved"])


def resolve_installed_binary() -> Optional[Path]:
    """Locate an *installed* ``fno-agents``, deliberately excluding the cargo dev target.

    A caller that acts on the answer uses this narrower set, so a *development*
    checkout -- where only ``crates/fno-agents/target/release`` exists -- changes
    no behavior. :func:`resolve_binary` is the full order, cargo dev included.

    Deliberately does NOT consult ``$FNO_AGENTS_BIN``: a stale export pointing at
    a dev build must not silently stand in for the installed binary.
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


class CommandUnavailable(RuntimeError):
    """The fno-agents binary is missing, failed, or answered malformed JSON."""

    # Structured exit code for the non-zero-exit failure; None on the other
    # paths (binary missing, OSError/timeout, bad JSON). The gh budget's admit
    # gate reads it: a negative code or 137 is a signalled reader, which is a
    # machine in distress, not a ledger that cannot answer.
    returncode: Optional[int] = None


def command_call(
    command: "str | list[str]",
    payload: dict,
    unavailable: type = CommandUnavailable,
    *,
    timeout: float = 30,
    passthrough_stderr: bool = False,
) -> dict:
    """One subprocess round-trip with the fno-agents binary: JSON payload in,
    parsed JSON answer out. The dev checkout's own build outranks any stale
    installed copy. Raises the caller's ``unavailable`` exception - a named
    refusal, never a silent fallback.

    ``timeout`` defaults to the 30s a pure local resolver needs. A command that
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
    command_display = command if isinstance(command, str) else " ".join(command)
    try:
        proc = subprocess.run(
            [str(binary), *(command if isinstance(command, list) else [command])],
            input=json.dumps(payload),
            stdout=subprocess.PIPE,
            stderr=None if passthrough_stderr else subprocess.PIPE,
            text=True,
            timeout=timeout,
        )
    except (OSError, subprocess.TimeoutExpired) as exc:
        raise unavailable(f"fno-agents {command_display} failed: {exc}") from exc
    if proc.returncode != 0:
        # With passthrough_stderr the child owns the real stderr (None here),
        # so name where it went instead of crashing on strip().
        detail = f"fno-agents {command_display} exited {proc.returncode}"
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
        raise unavailable(f"fno-agents {command_display} bad output: {exc}") from exc
    finally:
        if os.environ.get("FNO_ROUTE_SLOT_DEBUG"):
            print(json.dumps({"payload": payload}), flush=True)


def chats_command(args: "list[str]", payload: dict) -> dict:
    return command_call(["chats", *args], payload, timeout=30)


def resolve_front_binary() -> Optional[Path]:
    """The native ``fno`` front binary: this checkout's build, then ``PATH``."""
    import shutil

    root = Path(__file__).resolve().parents[3] / "crates" / "fno" / "target"
    for profile in ("debug", "release"):
        if (root / profile / "fno").exists():
            return root / profile / "fno"
    return Path(found) if (found := shutil.which("fno")) else None


def _update_signpost() -> None:
    """The no-front-door spelling: the updater is native to the Rust front
    door, so this leaf only names the one command that installs it."""
    # Lazy: this module is imported by crates-side integration tests whose
    # interpreter has no typer, and this leaf is its only user.
    import typer

    typer.echo(
        "fno: the updater is native to the Rust front door; install it from the source checkout with "
        "`cargo install --locked --path <src>/crates/fno`, then re-run `fno doctor update`."
    )
    raise typer.Exit(code=2)

class FrontTimeout(CommandUnavailable):
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
    """One round-trip with the front's law door, fail-closed like command_call."""
    import json
    import subprocess

    if timeout is None:
        timeout = _law_door_timeout()

    binary = resolve_front_binary()
    if binary is None:
        raise CommandUnavailable("the native fno binary was not found")
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
        raise CommandUnavailable(f"fno inbox law match exited {done.returncode}: {done.stderr.strip()}")
    return json.loads(done.stdout)


def find_transcript(session_id: str) -> Optional[Path]:
    """The native doctor op door's transcript-by-uuid answer; None when absent.

    The deleted Python leg's own walk and the door share one resolver, so the
    confirm target for `_mux_recipient_transcript` reads through the same
    surface the self-diagnostic uses: a missing binary, non-zero exit,
    timeout, or a null path all fail the confirm closed.
    """
    import json
    import subprocess

    binary = resolve_front_binary()
    if binary is None:
        return None
    try:
        done = subprocess.run(
            [str(binary), "doctor", "--find-transcript", session_id],
            capture_output=True,
            text=True,
            timeout=5,
        )
    except (OSError, subprocess.SubprocessError):
        return None
    if done.returncode != 0:
        return None
    try:
        payload = json.loads(done.stdout)
    except ValueError:
        return None
    found = payload.get("path")
    return Path(found) if isinstance(found, str) and found else None


def notify_operator(title: str, body: str, pointer: str = "") -> tuple[int, str]:
    """One operator notice through the native front door: ``(code, err)``.

    The dispatch (mux clients, OS toast, sink lane) answers in-process in the
    Rust command; this Python seam only carries the argv, so the notify decision
    table has one owner. ``err`` is the command's one-line degrade message when
    the code is non-zero, else empty.
    """
    import subprocess

    binary = resolve_front_binary()
    if binary is None:
        return 1, "fno inbox notify: the native fno binary was not found"
    argv = [str(binary), "inbox", "notify"]
    if pointer:
        argv += ["--pointer", pointer]
    argv += ["--", title, body]
    try:
        done = subprocess.run(argv, capture_output=True, text=True, timeout=15.0)
    except subprocess.TimeoutExpired:
        return 1, "fno inbox notify: no answer in 15s"
    except OSError as exc:
        return 1, str(exc)[:200]
    return done.returncode, (done.stderr or "").strip()
