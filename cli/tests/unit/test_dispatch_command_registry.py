"""Tests for the dispatch command registry (node x-c8e4).

`dispatch.allowed_commands` is a bare string list: it cannot answer which
harnesses a command exists on, how it is spelled there, what it requires, or what
a completion proves. `dispatch.command_registry` carries those as a
`DispatchCommandDescriptor`, mirroring `review.reviewer_registry` field for
field. The load-bearing invariant is the same as the reviewer table's: a
registry entry never redefines a shipped command, and the resolver renders a
registry command's spelling VERBATIM - the fno-namespace normalizer would mint a
phantom `$fno:sec:audit`-shaped skill on codex.
"""
from __future__ import annotations

import pytest

from fno.config import DispatchCommandDescriptor, resolvable_commands

REG = {
    "/security-audit": DispatchCommandDescriptor(
        invocation="/sec:audit",
        invocations={"claude": "/sec:audit", "codex": "$sec:audit"},
    )
}


# ── 1.1: config parse + collision rule ──────────────────────────────────


def test_resolvable_commands_canonicalizes_keys_and_drops_builtins():
    reg = {
        "security-audit": DispatchCommandDescriptor(invocation="/sec:audit"),
        "/target": DispatchCommandDescriptor(invocation="/evil"),
        "fno:think": DispatchCommandDescriptor(invocation="/evil"),
    }
    out = resolvable_commands(reg, ["/target", "/think", "/blueprint"])
    assert set(out) == {"/security-audit"}
    assert out["/security-audit"].asserts == "invocation"


# ── 1.2: the shared skill-presence probe ────────────────────────────────
#
# The probe itself is native now: its three outcomes are pinned by the
# review_capability goldens and Rust unit tests, so only the dispatch
# caller's WIRING (derive the first token, pass --name/--harness/--context,
# raise on `unavailable`) stays under test here, door-stubbed.


def _stub_skill_probe(monkeypatch: pytest.MonkeyPatch, answer: dict) -> list:
    calls: list = []

    def _fake(command, payload, *args, **kwargs):
        calls.append((command, payload))
        if isinstance(answer, Exception):
            raise answer
        return answer

    monkeypatch.setattr("fno.rust_binary.command_call", _fake)
    return calls


PROBE_ABSENT = {
    "status": "unavailable",
    "reason": (
        "skill 'sec-audit' resolves in none of the roots searched "
        "(<roots>); install it there or change config.dispatch.command_registry"
    ),
}


# ── 1.3: resolve, refuse, render ────────────────────────────────────────


def test_pre_rename_node_key_still_reads_but_config_keys_do_not():
    from fno.config import DispatchBlock
    from fno.graph.types import Node

    block = DispatchBlock.model_validate(
        {"allowed_verbs": ["/x"], "verb_registry": {"/x": {"invocation": "/x"}}}  # verb-wording: allow
    )
    assert block.allowed_commands == ["/target", "/think", "/blueprint"]
    assert block.command_registry == {}
    node = Node.model_validate({"id": "x-1", "dispatch_verb": "/think"})  # verb-wording: allow
    assert node.dispatch_command == "/think"
    assert "dispatch_verb" not in node.model_dump()  # verb-wording: allow


