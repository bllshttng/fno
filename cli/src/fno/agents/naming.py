"""Thin binding to the fno-agents binary's name commands (vocabulary owner:
crates/fno-agents/src/naming.rs). Argv marshalling and result parsing only."""

import json
import os
import re
import subprocess
from collections import namedtuple
from functools import lru_cache

from fno.config._dispatch_commands import parse_command_token

MAX_LEN = 64
SLUG_CAP = 30

class AgentNameError(ValueError):
    pass

class BridgeUsageError(ValueError):
    pass

DispatchName = namedtuple("DispatchName", "name source command node tail")

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

def dispatch_sources():
    return _codes()[0]

def dispatch_commands():
    return _codes()[1]

def accepted_command_words():
    """The work-command words the name mint resolves (naming-codes.yaml)."""
    return sorted(_codes()[2])

def provenance_rows():
    return _codes()[3]

def slug_component(raw, cap=SLUG_CAP):
    return re.sub(r"-+", "-", re.sub(r"[^a-z0-9-]", "-", (raw or "").lower())).strip("-")[:cap].rstrip("-")

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

def command_code_for(word):
    w = (word or "").strip()
    parsed = parse_command_token(w) if w else None
    v = (parsed[0] if parsed else w) or "target"
    code = _codes()[2].get(v)
    if not code:
        raise AgentNameError(f"unknown dispatch command {word!r}")
    return code

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

def worker_name(node=None, *, seed=None, slug=None):
    """``t-<short slug>``, a target worker's default name; an empty node reads a ``/target`` seed."""
    return _mint("--worker", node or "", *_flags(("--seed", seed), ("--slug", slug)))

def mint_or_none(source, command, identity, **kwargs):
    """The mint for degradable seams: a stale/missing binary reads as None."""
    try:
        return dispatch_agent_name(source, command, identity, **kwargs)
    except (AgentNameError, BridgeUsageError):
        return None

def _opt_pos(value, flag):
    return [flag, value] if (value or "").strip() else []

def parse_many(names):
    if not names:
        return []
    out = []
    for line in _run("name-parse", [], "\n".join(names)).splitlines():
        try:
            row = json.loads(line)
        except json.JSONDecodeError:
            # A garbage row reads as the stale-binary refusal callers guard.
            raise AgentNameError("name-parse produced an unparsable row: the fno-agents binary is stale")
        out.append(None if row.get("verb") is None else DispatchName(
            row["name"], row.get("source"), row["verb"], row.get("node"), row.get("tail") or ""))
    _enrich_hex_nodes(out)
    return out


@lru_cache(maxsize=8)
def _graph_hex_map(graph_path: str):
    """hex suffix -> full node id for one graph file, keyed on the declared
    state root. One read per file per process; a hex with no unique
    `<prefix>-<hex>` id maps to "" and stays bare (never an invented id)."""
    try:
        from fno.graph.load import load_graph

        rows_all = load_graph()
    except Exception:  # noqa: BLE001 - a graph read failure leaves hex bare
        return {}
    hex_map: dict = {}
    for entry in rows_all:
        node_id = entry.get("id") if isinstance(entry, dict) else None
        if not node_id or "-" not in node_id:
            continue
        prefix, _, hex_part = node_id.rpartition("-")
        if not prefix or not hex_part:
            continue
        if hex_part not in hex_map:
            hex_map[hex_part] = node_id
        elif hex_map[hex_part] != node_id:
            hex_map[hex_part] = ""  # ambiguous: keep bare hex
    return hex_map


def _enrich_hex_nodes(rows):
    """Re-attach full node ids to bare-hex parse results.

    The mint emits the node hex without its prefix; the graph is the only
    place that knows the prefix. A hex with no unique `<prefix>-<hex>` id
    stays bare (never an invented id).
    """
    wanted = {
        row.node
        for row in rows
        if row is not None and row.node and re.fullmatch(r"[0-9a-f]+", row.node)
    }
    if not wanted:
        return
    try:
        from fno import paths as _paths

        key = str(_paths.graph_json())
    except Exception:  # noqa: BLE001 - an unresolved graph path leaves hex bare
        return
    hex_map = _graph_hex_map(key)
    for i, row in enumerate(rows):
        if row is None or not row.node or row.node not in wanted:
            continue
        resolved = hex_map.get(row.node)
        if resolved:
            rows[i] = row._replace(node=resolved)

def parse_dispatch_agent_name(name):
    return parse_many([name])[0] if name else None

def parse_node_ids(names):
    """Batch node extraction: one name-parse subprocess for the whole list,
    never one per row (the ``agents list`` join reads every row)."""
    keys = list(names)
    try:
        rows = parse_many([name or "" for name in keys]) if keys else []
    except AgentNameError:
        # Stale/missing binary: every name reads as no node, no exception.
        return {name: None for name in keys}
    return {name: (row.node if row is not None and row.node else None)
            for name, row in zip(keys, rows)}
