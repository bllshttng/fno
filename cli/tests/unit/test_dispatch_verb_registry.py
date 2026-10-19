"""Tests for the dispatch verb registry (node x-c8e4).

`dispatch.allowed_verbs` is a bare string list: it cannot answer which
harnesses a verb exists on, how it is spelled there, what it requires, or what
a completion proves. `dispatch.verb_registry` carries those as a
`DispatchVerbDescriptor`, mirroring `review.reviewer_registry` field for
field. The load-bearing invariant is the same as the reviewer table's: a
registry entry never redefines a shipped verb, and the resolver renders a
registry verb's spelling VERBATIM - the fno-namespace normalizer would mint a
phantom `$fno:sec:audit`-shaped skill on codex.
"""
from __future__ import annotations

import pytest

from fno.agents.harness_map import DispatchResolveError, resolve_dispatch
from fno.config import DispatchVerbDescriptor, resolvable_verbs

REG = {
    "/security-audit": DispatchVerbDescriptor(
        invocation="/sec:audit",
        invocations={"claude": "/sec:audit", "codex": "$sec:audit"},
    )
}


# ── 1.1: config parse + collision rule ──────────────────────────────────


def test_resolvable_verbs_canonicalizes_keys_and_drops_builtins():
    reg = {
        "security-audit": DispatchVerbDescriptor(invocation="/sec:audit"),
        "/target": DispatchVerbDescriptor(invocation="/evil"),
        "fno:think": DispatchVerbDescriptor(invocation="/evil"),
    }
    out = resolvable_verbs(reg, ["/target", "/think", "/blueprint"])
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

    def _fake(verb, payload, *args, **kwargs):
        calls.append((verb, payload))
        if isinstance(answer, Exception):
            raise answer
        return answer

    monkeypatch.setattr("fno.rust_binary.verb_call", _fake)
    return calls


PROBE_ABSENT = {
    "status": "unavailable",
    "reason": (
        "skill 'sec-audit' resolves in none of the roots searched "
        "(<roots>); install it there or change config.dispatch.verb_registry"
    ),
}


# ── 1.3: resolve, refuse, render ────────────────────────────────────────


def test_registry_verb_renders_native_spelling():
    out = resolve_dispatch(
        harness="codex",
        verb="/security-audit",
        node_id="x-1234",
        dispatch_cfg={"verb_registry": REG},
    )
    assert out["command"] == "$sec:audit x-1234"
    assert "fno:" not in out["command"]
    assert any(
        "registry-verb(/security-audit, asserts=invocation)" in d
        for d in out["decision"]
    )


def test_registry_verb_undeclared_harness_refuses():
    with pytest.raises(DispatchResolveError) as exc:
        resolve_dispatch(
            harness="opencode",
            verb="/security-audit",
            node_id="x-1",
            dispatch_cfg={"verb_registry": REG},
        )
    msg = str(exc.value)
    assert "opencode" in msg
    assert "claude, codex" in msg


def test_registry_verb_missing_skill_refuses_naming_roots(monkeypatch: pytest.MonkeyPatch):
    # A readable-but-empty root makes the probe's answer unavailable (with no
    # root at all it would be unverifiable, which proceeds by design); the
    # stub hands the caller that answer so the refusal wiring stays pinned.
    _stub_skill_probe(monkeypatch, PROBE_ABSENT)
    cfg = {
        "verb_registry": {
            "/security-audit": DispatchVerbDescriptor(
                invocation="/sec-audit", requires="skill"
            )
        }
    }
    with pytest.raises(DispatchResolveError) as exc:
        resolve_dispatch(
            harness="claude", verb="/security-audit", node_id="x-1",
            dispatch_cfg=cfg,
        )
    msg = str(exc.value)
    assert "roots searched" in msg
    assert "config.dispatch.verb_registry" in msg


def test_verb_in_neither_surface_names_both():
    cfg = {
        "verb_registry": {"/other": DispatchVerbDescriptor(invocation="/other")}
    }
    with pytest.raises(DispatchResolveError) as exc:
        resolve_dispatch(
            harness="claude", verb="/nope", node_id="x-1", dispatch_cfg=cfg
        )
    msg = str(exc.value)
    assert "neither the allowlist" in msg
    assert "config.dispatch.verb_registry" in msg
    assert "/other" in msg


