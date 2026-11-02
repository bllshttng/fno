"""Thin binding to the fno-agents binary's name commands (vocabulary owner:
crates/fno-agents/src/naming.rs). Argv marshalling and result parsing only."""

import json
import subprocess
from functools import lru_cache


MAX_LEN = 64

class AgentNameError(ValueError):
    pass

class BridgeUsageError(ValueError):
    pass


def _run(command, args, stdin=None):
    from fno import rust_binary

    binary = rust_binary.resolve_binary()
    if binary is None:
        raise AgentNameError("no fno-agents binary found; run `fno doctor update`")
    proc = subprocess.run(
        [str(binary), command, *args], input=stdin, capture_output=True, text=True,
        timeout=30,
    )
    if proc.returncode:
        msg = (proc.stderr or "binary failed").strip().removeprefix("error: ")
        if command != "name-mint" or proc.returncode == 3:
            raise AgentNameError(msg)
        raise BridgeUsageError(msg)
    return proc.stdout

def _mint(*args):
    lines = _run("name-mint", list(args)).strip().splitlines()
    if not lines or not lines[-1]:
        raise AgentNameError("name mint produced no name")
    return lines[-1]

@lru_cache(maxsize=1)
def _codes():
    # A garbage payload (an rc-0 stub, a wrapped binary with a banner) reads as
    # the stale-binary refusal every caller already guards.
    try:
        raw = json.loads(_run("name-codes", ["--json"]))
    except json.JSONDecodeError as exc:
        raise AgentNameError(f"name-codes payload unparsable: the fno-agents binary is stale ({exc})")
    return (frozenset(raw["sources"]), frozenset(raw["verbs"]), dict(raw["word_codes"]),  # verb-wording: allow
            tuple((r["site"], r["source"], r["verb"]) for r in raw["provenance"]))  # verb-wording: allow


def dispatch_commands():
    return _codes()[1]


def _flags(*pairs):
    out = []
    for flag, value in pairs:
        if (value or "").strip():
            out += [flag, value or ""]
    return out

def agent_name(prefix, node_id, *, slug=None, qualifier=None, discriminator=None, model=None):
    # The binary reads empty-everything as usage (exit 2); in-process producers
    # keep the naming-error contract, so this one refusal stays local.
    if not (prefix or "").strip() and not (node_id or "").strip():
        raise AgentNameError("an agent name needs a prefix or a node id; both were empty")
    return _mint(prefix, node_id, *_flags(("--slug", slug), ("--qualifier", qualifier),
                                          ("--discriminator", discriminator), ("--model", model)))


def dispatch_agent_name(source, command, identity, *, slug=None, qualifier=None, discriminator=None, model=None):
    # An identity longer than the runtime contract can never fit: refuse in
    # process, before any subprocess is spent on a guaranteed refusal.
    if len((identity or "").strip()) > MAX_LEN:
        raise AgentNameError(
            f"required agent-name identity is {len((identity or '').strip())} chars, "
            f"over the {MAX_LEN}-char runtime limit"
        )
    if (command or "").strip() not in dispatch_commands():
        raise AgentNameError(f"unknown dispatch command {command!r}")
    return _mint(*(_opt_pos(source, "--source")), "--command", command, identity,
                 *_flags(("--slug", slug), ("--qualifier", qualifier), ("--discriminator", discriminator),
                         ("--model", model)))


def _opt_pos(value, flag):
    return [flag, value] if (value or "").strip() else []
