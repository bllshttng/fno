"""fno agents mail: durable polled mailbox CLI.

One namespace over the jsonl-canon bus log. Publish appends a durable envelope;
consume is a per-recipient cursor scan over the log, and the chats store
records every message. No per-message file is written.
This replaces the retired ``inbox`` namespace and the messaging half of
``agents`` (whose send/inbox/ack commands moved here).

Commands:
    send           - publish a message to a peer or project (durable-first)
    unread         - list bus messages addressed to me past my cursor
    ack            - advance my read cursor
    reply          - answer a message by id; name-lane -> back to its sender
    view           - render the jsonl bus as an inbox projection

Call shape (one rule, because the commands used to disagree):
    The message BODY is uniform across ``send`` and ``reply``. Both take it
    positionally, or via ``--body``, or via ``--body-file`` -- exactly one of the
    three, with two-at-once an explicit refusal rather than a precedence rule.

    ``reply`` was flag-only until 2026-08-10, so the positional form that the
    skills taught (and that ``send`` accepts) exited 2 while click echoed the
    body back. An echoed body resembles a delivery receipt closely enough that
    two agents independently concluded the command was broken, and one of them
    reported mail as sent that had never left. Keep the two commands symmetric: a
    surface whose call shape depends on which command you happen to be calling is
    the defect, and correcting the docs alone leaves it live for the next doc
    written from memory.

Exit codes:
    0  success
    1  user error (invalid input, deprecated kind, typo in recipient)
    2  runtime error
"""
from __future__ import annotations

import os
import subprocess
import sys
from pathlib import Path
from typing import Optional

import typer

from fno import paths


mail_app = typer.Typer(
    help="Durable polled mailbox: send/unread/ack/reply/view.",
    no_args_is_help=True,
)


@mail_app.command("reply", context_settings={"allow_extra_args": True, "ignore_unknown_options": True})
def mail_reply_forward(ctx: typer.Context) -> None:
    """Reply to a message, routed by the answered message's lane.

    The command lives in the Rust mail front now; Python owns transport only,
    the same split as the team forward. The cross-host branch runs inside the
    native leaf, before local routing. Flags relay verbatim and are refused
    there if unknown.
    """
    _forward_mail(["reply", *ctx.args], label="fno agents mail reply")


# The markdown inbox these read is gone. Each stub stays one release so a
# caller learns the chats-store replacement, then the stubs retire.
_RETIRED_READERS = {
    "list": "fno agents mail unread [-n NAME] [--json]",
    "drain": "fno agents mail drain-self",
    "status": "fno agents mail unread",
    "triage": "fno agents mail show <id>",
}


def _retired_reader(name: str, replacement: str) -> None:
    @mail_app.command(
        name,
        context_settings={"allow_extra_args": True, "ignore_unknown_options": True},
        help=f"Retired: the markdown inbox is gone. Use {replacement}.",
    )
    def _stub(ctx: typer.Context) -> None:
        typer.echo(
            f"fno agents mail {name} is retired: the markdown inbox is gone. "
            f"Use {replacement}.",
            err=True,
        )
        raise typer.Exit(1)


for _name, _replacement in _RETIRED_READERS.items():
    _retired_reader(_name, _replacement)


@mail_app.command("pane-prepare", hidden=True)
def cmd_pane_prepare_forward(
    session: Optional[str] = typer.Option(
        None, "--session-id", help="mux session name."
    ),
    session_legacy: Optional[str] = typer.Option(
        None, "--session", hidden=True, help="[DEPRECATED] alias for --session-id."
    ),
    pane: int = typer.Option(..., "--pane", help="mux pane id."),
    harness: Optional[str] = typer.Option(
        None, "--harness",
        help="Harness hosting the pane (default: resolved from the agent registry).",
    ),
    style_exception: Optional[str] = typer.Option(
        None, "--style-exception",
        help="Reasoned one-send exception to the style gates on enveloped "
        "prose (a --raw send never enters them).",
    ),
) -> None:
    """Forward the pane-send renderer to the bundled fno-agents binary.

    The renderer's body lives in the Rust mail front now (``fno-agents mail
    pane-prepare``); Python owns the command surface only, the same split as
    the hold forward. The Rust ``fno mux pane send`` command calls that binary
    directly and never lands here.
    """
    argv = ["pane-prepare", "--pane", str(pane)]
    # Both session spellings ride verbatim when given; the renderer owns the
    # alias merge and its deprecation warning, so this leaf stays argument-silent.
    if session is not None:
        argv.extend(["--session-id", session])
    if session_legacy is not None:
        argv.extend(["--session", session_legacy])
    if harness is not None:
        argv.extend(["--harness", harness])
    if style_exception is not None:
        argv.extend(["--style-exception", style_exception])
    _forward_mail_pane_prepare(argv)


# Publish + cursor-consume (relocated from `fno agents`, Move B)
# `fno agents mail send` is the durable-first publish (the envelope lands on the bus
# log before any live delivery is attempted). `fno agents mail unread`/`ack` are the
# cursor-based consume over that log: unread lists messages addressed to me
# after my cursor; ack advances it. These supersede the retired agents
# send/inbox/ack and inbox unread/ack commands (the one messaging namespace).


_RAW_SELF_TOKEN = object()



def _self_recipient(
    token: str,
    *,
    resolved_session_id: object = _RAW_SELF_TOKEN,
    full_only: bool = False,
) -> Optional[str]:
    """Canonical own address after resolution, or on a clean raw-token miss."""
    from fno.harness_identity import (
        canonical_handle,
        current_session_id,
        session_handle_tier,
    )

    own = current_session_id() or ""
    if not own:
        return None
    candidate = token if resolved_session_id is _RAW_SELF_TOKEN else resolved_session_id
    if not isinstance(candidate, str):
        return None
    tier = session_handle_tier(candidate, own)
    if (
        tier is None
        or (resolved_session_id is not _RAW_SELF_TOKEN and tier != 0)
        or (full_only and tier != 0)
    ):
        return None
    return canonical_handle(own)


def _is_self_send(recipient: Optional[str]) -> bool:
    """True when an already-addressed token is this ambient session."""
    return bool(recipient and _self_recipient(recipient))


# Send-time human escalation for a question, per (sender, recipient). A burst
# re-nudges every window rather than once forever (marker refreshed only on an
# actual escalation, so the window runs from the last nudge, not the first send).
# Send-time human escalation moved to fno.mail.receipts (file-budget: this
# module is shrink-only); import it there.


@mail_app.command("send")
def mail_send_forward(
    name: str | None = typer.Argument(
        None,
        help=(
            "Agent name, short-id, or full session id. A codex head-8 is a "
            "65.536-second UUIDv7 bucket, not random, and is refused against "
            "a codex row; claude ids are UUIDv4, so either form works."
        ),
    ),
    message: str | None = typer.Argument(
        None, help="Message to send (async, fire-and-forget)."
    ),
    harness: str | None = typer.Option(
        None, "--harness", "-H",
        help="claude | codex | gemini (optional; used for mismatch check).",
    ),
    _provider_tombstone: str | None = typer.Option(
        None, "--provider", hidden=True,
        help="Retired: the harness axis is --harness/-H.",
    ),
    cwd: str | None = typer.Option(
        None, "--cwd", "-c", help="Working directory context."
    ),
    from_name: str | None = typer.Option(
        None, "--from-name",
        help="XML-safe sender. Unset: session handle or 'fno' for agents; project for inbox notes.",
    ),
    origin: str | None = typer.Option(
        None,
        "--origin",
        hidden=True,
        help="Stamped machine mail origin: operator, peer, scheduler, or recovery.",
    ),
    from_self: bool = typer.Option(
        False, "--from-self",
        help=(
            "Stamp the sender with this session's own canonical mail handle "
            "(the reply handle `fno whoami` shows). Fails loud (exit 2) with "
            "no ambient harness identity."
        ),
    ),
    to_project: str | None = typer.Option(
        None, "--to-project",
        help=(
            "Anycast: deliver to whoever works on this project (live if exactly "
            "one peer, durable queue if none). Use instead of <name>."
        ),
    ),
    to_lead: str | None = typer.Option(
        None, "--to-lead",
        help=(
            "Anycast over the role: deliver to whoever holds this scope "
            "RIGHT NOW, resolved at send time; no live holder queues nothing."
        ),
    ),
    any_live: bool = typer.Option(
        False, "--any",
        help="With --to-project, break a multi-live-peer tie (most recent activity wins).",
    ),
    kind: str | None = typer.Option(
        None, "--kind", "-k",
        help=(
            "Inbox kind (heads-up | question | fyi); pair with --to-project. "
            "Omit for a plain agent-to-agent send."
        ),
    ),
    reply_to: str | None = typer.Option(
        None, "--reply-to",
        help="With --kind: msg-id being replied to (appends to the existing thread).",
    ),
    persist: str | None = typer.Option(
        None, "--persist",
        help="With --kind fyi: 'memory' writes a recipient memory file.",
    ),
    body: str | None = typer.Option(
        None, "--body", "-b",
        help="Message body in ANY mode (alternative to the positional arg).",
    ),
    body_file: Path | None = typer.Option(
        None, "--body-file",
        help="Read the body from a file ('-' = stdin); binds in every mode.",
    ),
    ref_pr: int | None = typer.Option(
        None, "--ref-pr", help="With --kind: PR number reference for triage."
    ),
    ref_node: str | None = typer.Option(
        None, "--ref-node", help="With --kind: graph node id reference."
    ),
    ref_gate: str | None = typer.Option(
        None, "--ref-gate", help="With --kind: named gate/milestone reference."
    ),
    json_out: bool = typer.Option(
        False, "--json", "-J",
        help="With --kind: print {msg_id, thread_path, appended} as JSON.",
    ),
    raw: bool = typer.Option(
        False, "--raw",
        help=(
            "Inject the payload UNWRAPPED at the recipient's prompt line: one "
            "line, typed verbatim - a command only: it must start with / or $. "
            "A status report or any authored message goes WRAPPED (drop --raw) "
            "so its sender stays visible. A payload starting /fno: or $fno: is "
            "an fno command, and the lane rewrites the marker to the form of the "
            "receiving harness. A showing prompt is answered with `fno agents "
            "ask`. Never queues durable. An actor OTHER than the model must "
            "supply the trigger; self-injection is barred unless --to-self. "
            "Mechanics and the reviewer-off-the-author rationale: "
            "docs/architecture/review-lanes.md."
        ),
    ),
    check: bool = typer.Option(
        False, "--check",
        help=(
            "With --raw: report whether an injection path EXISTS and inject "
            "nothing: 'injectable: <lane>' (exit 0), 'not-injectable' (1), "
            "'unmeasurable' (3), malformed payload (2). Reports a PATH only."
        ),
    ),
    to_self: bool = typer.Option(
        False, "--to-self",
        help=(
            "Address this session as the recipient (no <id> needed); with "
            "--raw the envelope is stripped so a slash command parses at your "
            "own prompt line."
        ),
    ),
    force: bool = typer.Option(
        False, "--force", "-F",
        help=(
            "Deliver over the PANE transport: type the wrapped body at the "
            "recipient's prompt, keeping every mail semantic. The receipt says "
            "`typed (pane <id>)`, NEVER `delivered` - bytes written to a PTY "
            "is not action. It reads the pane first and refuses a showing "
            "prompt."
        ),
    ),
    style_exception: str | None = typer.Option(
        None, "--style-exception",
        help="Bypass the style check for this body with a stated reason.",
    ),
) -> None:
    """Forward the wrapped send command to the Rust leaf.

    The send body lives in the Rust mail front now; Python owns --help and
    this shell. The relay's byte-verbatim forward lands here for a plain
    name it cannot bind, and this execs the native command.
    """
    import subprocess

    from fno._subprocess_util import propagate_returncode
    from fno.rust_binary import resolve_binary

    # Typer parsed the invocation, so rebuild the tail from the params:
    # sys.argv belongs to the driving process (pytest under CliRunner) and
    # carries no "send" to slice on. The Rust parser is order-insensitive.
    argv: list[str] = []
    if name is not None:
        argv.append(name)
    if harness is not None:
        argv += ["--harness", harness]
    if _provider_tombstone is not None:
        argv += ["--provider", _provider_tombstone]
    if cwd is not None:
        argv += ["--cwd", cwd]
    if from_name is not None:
        argv += ["--from-name", from_name]
    if origin is not None:
        argv += ["--origin", origin]
    if from_self:
        argv.append("--from-self")
    if to_project is not None:
        argv += ["--to-project", to_project]
    if to_lead is not None:
        argv += ["--to-lead", to_lead]
    if any_live:
        argv.append("--any")
    if kind is not None:
        argv += ["--kind", kind]
    if reply_to is not None:
        argv += ["--reply-to", reply_to]
    if persist is not None:
        argv += ["--persist", persist]
    if body is not None:
        argv += ["--body", body]
    if body_file is not None:
        argv += ["--body-file", str(body_file)]
    if ref_pr is not None:
        argv += ["--ref-pr", str(ref_pr)]
    if ref_node is not None:
        argv += ["--ref-node", ref_node]
    if ref_gate is not None:
        argv += ["--ref-gate", ref_gate]
    if json_out:
        argv.append("--json")
    if raw:
        argv.append("--raw")
    if check:
        argv.append("--check")
    if to_self:
        argv.append("--to-self")
    if force:
        argv.append("--force")
    if style_exception is not None:
        argv += ["--style-exception", style_exception]
    if message is not None:
        # End-of-options fence: the body's first line is data, never a flag.
        argv.append("--")
        argv.append(message)
    binary = resolve_binary()
    if binary is None:
        typer.echo(
            "fno mail send: the fno-agents binary was not found. "
            "It ships in the `pip install fno` wheel and with the plugin; "
            "reinstall fno or run `fno doctor update --rust`, or set "
            "FNO_AGENTS_BIN to its path.",
            err=True,
        )
        raise typer.Exit(code=127)
    result = subprocess.run(
        [str(binary), "mail", "send", *argv],
        capture_output=True,
        text=True,
        check=False,
    )
    if result.stdout:
        typer.echo(result.stdout.rstrip("\n"))
    if result.stderr:
        typer.echo(result.stderr.rstrip("\n"), err=True)
    raise typer.Exit(code=propagate_returncode(result.returncode))

@mail_app.command("team", context_settings={"allow_extra_args": True, "ignore_unknown_options": True})
def team_forward(ctx: typer.Context) -> None:
    """Announce one body to a fleet scope as ONE bus line.

    The body lives in the Rust mail front now; Python owns transport only,
    the same split as the hold forward. The body checks (forged envelope,
    brevity cap, style) and the announce writer run on the native leg.
    Announcement flags belong to the Rust writer and relay verbatim:
    `--subject=S` (supersede key; the = spelling keeps click from parking
    the value in the positional body), `--expires=45m|24h|7d` (standing
    window, default 24h, max 7d), `--urgent`. Any unrecognized flag is
    passed through the same way and refused there, so this shim adds no
    Python flag surface.
    """
    _forward_mail(["team", *ctx.args], label="fno mail team")


@mail_app.command("unread")
def cmd_unread(
    name: str = typer.Option(
        "fno", "--name", "-n",
        help="Whose inbox to read (registry name or project).",
    ),
    json_out: bool = typer.Option(
        False, "--json", "-J", help="Emit JSON regardless of TTY."
    ),
) -> None:
    """Show unread bus messages addressed to <name> (cursor-filtered).

    "My inbox" is a cursor-bounded scan of the one global bus log filtered to
    ``to == name``: only messages after the consumer's cursor are shown,
    regardless of which provider sent them. ``fno agents mail ack`` advances the
    cursor. JSON when stdout is not a TTY or ``--json`` is passed. This leaf
    forwards to the native fno-agents reader and relays its streams.
    """
    _forward_unread(name, json_out)


def _forward_unread(name: str, json_out: bool) -> None:
    from fno._subprocess_util import propagate_returncode
    from fno.rust_binary import resolve_binary

    binary = resolve_binary()
    if binary is None:
        typer.echo(
            "fno agents mail unread: the fno-agents binary was not found; reinstall fno "
            "or run `fno doctor update --rust`, or set FNO_AGENTS_BIN to its path.",
            err=True,
        )
        raise typer.Exit(code=127)
    # The binary's own bus ladder skips the settings file, so pin FNO_BUS_DIR.
    env = dict(os.environ, FNO_BUS_DIR=str(paths.bus_dir()))
    # A terminal inherits stdio; a captured stdout (pipe, tests) is relayed.
    tty = sys.stdout is not None and sys.stdout.isatty()
    result = subprocess.run(
        [str(binary), "mail", "unread", "--name", name, *(["--json"] if json_out else [])],
        env=env, capture_output=not tty, text=True, check=False,
    )
    if not tty:
        sys.stdout.write(result.stdout)
        sys.stderr.write(result.stderr)
    raise typer.Exit(code=propagate_returncode(result.returncode))


@mail_app.command("sent")
def cmd_sent(
    unclaimed_only: bool = typer.Option(
        False, "--unclaimed", "-u",
        help="Only mail still past its recipient's cursor AND older than "
             "config.inbox.unclaimed_ttl - the ones the nag counts.",
    ),
    from_name: Optional[str] = typer.Option(
        None, "--from-name",
        help="List mail sent under this label instead of the ambient handle "
             "(the same label `mail send --from-name` stamps).",
    ),
    json_out: bool = typer.Option(
        False, "--json", "-J", help="Emit JSON regardless of TTY."
    ),
) -> None:
    """List mail THIS session sent, with claim state.

    The outbound half of ``fno agents mail unread``. It exists because the sender could
    previously see only a tally: ``mail status`` said "sent unclaimed: 1" and the
    every-prompt nudge repeated it, with no way to learn which message, to whom,
    or how old - so a strand was something you were told about hourly and could
    do nothing about.

    ``--unclaimed`` applies exactly the predicate the nag applies, so what this
    prints is what that line is counting, never a differently-scoped set.

    ``landed`` proves the text reached the recipient's own transcript, never
    that the agent read or acted on it -- that fourth state isn't observable.
    """
    # The body lives in the Rust mail front now; Python forwards, the same
    # split as the hold commands.
    argv = ["sent"]
    if unclaimed_only:
        argv.append("--unclaimed")
    if from_name is not None:
        argv += ["--from-name", from_name]
    if json_out:
        argv.append("--json")
    _forward_mail(argv, label="fno mail sent")


@mail_app.command("withdraw")
def cmd_withdraw(
    msg_id: str = typer.Argument(..., help="Message id to retract."),
    from_name: Optional[str] = typer.Option(
        None, "--from-name",
        help="Prove ownership as this label instead of the ambient handle "
             "(needed for mail sent with `mail send --from-name`).",
    ),
) -> None:
    """Retract a message you sent that the recipient has not picked up.

    The bus log is append-only and read state is a per-consumer cursor, so a
    withdrawal is neither a delete nor a cursor move. It cannot delete a line,
    and it must not advance the RECIPIENT's cursor: a cursor is a last-seen
    position rather than a per-message flag, so moving it would mark every other
    message queued for that recipient as seen - trading one strand for many.

    So this appends a tombstone naming the message, and every reader that
    decides delivery skips the pair.

    Refuses when the message is already past the recipient's cursor. They have
    read it; a tombstone then would only hide it from you.
    """
    _forward_mail(
        [
            "withdraw",
            msg_id,
            *(["--from-name", from_name] if from_name is not None else []),
        ],
        label="fno mail withdraw",
    )


@mail_app.command("ack")
def cmd_bus_ack(
    msg_id: str = typer.Argument(..., help="Message id to acknowledge up through."),
    name: str = typer.Option(
        "fno", "--name", "-n", help="Whose read cursor to advance."
    ),
) -> None:
    """Advance a read cursor to ``msg_id`` (marks everything up to it seen).

    The id is the positional; the cursor's owner is ``--name``/``-n``, NOT a
    second positional::

        fno agents mail ack msg-a1b2c3 --name <handle>

    Spelled out because the old one-liner read as if ``<name>`` came first and
    cost a reader two failed invocations before they resorted to ``--help``.
    """
    _forward_mail(["ack", msg_id, "--name", name], label="fno mail ack")


def _forward_mail(argv: list[str], label: str = "fno mail hold") -> None:
    """Forward one mail-front command to the bundled fno-agents binary.

    The hold, team, sent, withdraw, ack, and drain-self commands' bodies live in
    the Rust
    mail front now; Python owns transport only, the same split as the claims
    forward.
    """
    import subprocess

    from fno._subprocess_util import propagate_returncode
    from fno.rust_binary import find_dev_binary, resolve_binary

    binary = find_dev_binary() or resolve_binary()
    if binary is None:
        typer.echo(
            f"{label}: the fno-agents binary was not found. "
            "It ships in the `pip install fno` wheel and with the plugin; "
            "reinstall fno or run `fno doctor update --rust`, or set "
            "FNO_AGENTS_BIN to its path.",
            err=True,
        )
        raise typer.Exit(code=127)
    # Captured and re-emitted through typer: the leaf's output IS the
    # operator surface, and a CliRunner-hosted caller (the tests) must see it.
    result = subprocess.run(
        [str(binary), "mail", *argv],
        capture_output=True,
        text=True,
        check=False,
    )
    if result.stdout:
        typer.echo(result.stdout.rstrip("\n"))
    if result.stderr:
        typer.echo(result.stderr.rstrip("\n"), err=True)
    raise typer.Exit(code=propagate_returncode(result.returncode))


def _forward_mail_pane_prepare(argv: list[str]) -> None:
    """Forward the pane-send renderer to the bundled fno-agents binary.

    Same split as the hold forward with one deliberate difference: stdout is
    written byte-exact, because the ``fno mux pane send`` caller types these
    bytes at the pane verbatim and an appended newline becomes a submitted
    blank line. The hold commands print for an operator, where the extra newline
    is nothing.
    """
    import subprocess

    from fno._subprocess_util import propagate_returncode
    from fno.rust_binary import resolve_binary

    binary = resolve_binary()
    if binary is None:
        typer.echo(
            "fno mail pane-prepare: the fno-agents binary was not found. "
            "It ships in the `pip install fno` wheel and with the plugin; "
            "reinstall fno or run `fno doctor update --rust`, or set "
            "FNO_AGENTS_BIN to its path.",
            err=True,
        )
        raise typer.Exit(code=127)
    # Byte mode on purpose: text=True enables universal newlines, which
    # rewrites the envelope's CRLF line endings and breaks the byte-exact
    # contract the docstring states.
    result = subprocess.run(
        [str(binary), "mail", *argv],
        capture_output=True,
        check=False,
    )
    if result.stdout:
        sys.stdout.buffer.write(result.stdout)
    if result.stderr:
        typer.echo(result.stderr.decode(errors="replace").rstrip("\n"), err=True)
    raise typer.Exit(code=propagate_returncode(result.returncode))


def cmd_hold(
    minutes: int = typer.Option(
        None,
        "--minutes",
        "-m",
        help="Idle minutes before the hold lifts by itself. The "
        "quiet window restarts every prompt and ends at 2x the requested window.",
    ),
    for_minutes: int = typer.Option(
        None,
        "--for",
        help="Wall-clock minutes before the hold lifts. The deadline never moves.",
    ),
    off: bool = typer.Option(
        False, "--off", help="Lift the hold now and deliver what it held."
    ),
    status: bool = typer.Option(
        False, "--status", help="Report the current hold without changing it."
    ),
) -> None:
    """Busy mode: hold this session's incoming mail, and drain it on a timer.

    While the hold is on, mail addressed to this session never pastes into the
    prompt line. It queues durable and the sender gets a receipt saying so.
    Either clock DELIVERS without a new prompt, so a hold whose only drain
    trigger is the operator cannot stall.
    """
    argv = ["hold"]
    if minutes is not None:
        argv += ["--minutes", str(minutes)]
    if for_minutes is not None:
        argv += ["--for", str(for_minutes)]
    if off:
        argv.append("--off")
    if status:
        argv.append("--status")
    _forward_mail(argv)


def cmd_hold_release(
    handle: str = typer.Option(..., "--handle", help="The held session's handle."),
    poll_s: int = typer.Option(
        15, "--poll-s", hidden=True, help="Seconds between clock re-reads."
    ),
) -> None:
    """Sleep until ``handle``'s hold expires, then release it.

    Re-reads the clock on every wake rather than sleeping once to the original
    deadline, so an idle re-arm (the operator typed again) extends the hold
    instead of being overrun by a timer that already committed to a time.

    Exits quietly when the clock disappears or turns permanent: both mean
    someone else took the hold off, and a second release would be a no-op that
    still emitted a release event.
    """
    _forward_mail(["hold-release", "--handle", handle, "--poll-s", str(poll_s)])


mail_app.command("hold")(cmd_hold)
mail_app.command("hold-release", hidden=True)(cmd_hold_release)


def cmd_seed_provenance_front() -> None:
    """Forward the hidden seed-provenance command to the bundled fno-agents binary.

    The render body lives in the Rust mail front now; Python owns transport
    only, the same split as the hold forward. The launcher
    (hooks/spawn-seed-provenance-session-start.sh) exports the FNO_SEED_PROV_*
    sidecar, which owns the source=startup gate and the hook JSON.

    Exit 1 with no output when there is nothing to attribute: a hand-started
    session, an operator spawn, or an unusable sidecar. The hook stays silent
    on that, because a session with no peer sender has nothing to attribute.
    """
    import subprocess

    from fno._subprocess_util import propagate_returncode
    from fno.rust_binary import find_dev_binary, resolve_binary

    binary = find_dev_binary() or resolve_binary()
    if binary is None:
        typer.echo(
            "fno mail seed-provenance: the fno-agents binary was not found. "
            "It ships in the `pip install fno` wheel and with the plugin; "
            "reinstall fno or run `fno doctor update --rust`, or set "
            "FNO_AGENTS_BIN to its path.",
            err=True,
        )
        raise typer.Exit(code=127)
    result = subprocess.run(
        [str(binary), "mail", "seed-provenance"],
        capture_output=True,
        text=True,
        check=False,
    )
    if result.stdout:
        typer.echo(result.stdout.rstrip("\n"))
    if result.stderr:
        typer.echo(result.stderr.rstrip("\n"), err=True)
    raise typer.Exit(code=propagate_returncode(result.returncode))


mail_app.command("seed-provenance", hidden=True)(cmd_seed_provenance_front)


@mail_app.command("drain-self", hidden=True)
def cmd_drain_self_forward(
    json_out: bool = typer.Option(
        False, "--json", "-J", help="Emit JSON regardless of TTY."
    ),
) -> None:
    """Drain THIS session's own cross-harness inbox and mark it seen (US5).

    Forward-only: the body lives in the Rust mail front, on the same mount as
    the hold commands. Python owns transport only. The behavior contract is
    characterized byte-for-byte by the Rust parity suite frozen from this
    leaf's goldens (crates/fno-agents/tests/mail_drain_self_parity.rs).
    """
    argv = ["drain-self"]
    if json_out:
        argv.append("--json")
    _forward_mail(argv, label="fno agents mail drain-self")
