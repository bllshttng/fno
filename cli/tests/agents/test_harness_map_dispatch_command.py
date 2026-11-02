"""Per-harness dispatch_command resolution (x-567d).

Each harness resolves to the right worker command: a native skill invocation
where one is verified (claude/agy ``/target``, opencode ``/fno:target``, codex
``$fno:target``). gemini is deprecated (successor: agy) and its dispatch lane is
a loud refusal - no prose brief (x-de43).
"""
from __future__ import annotations

import pytest

from fno.agents.harness_map import (
    DispatchResolveError,
    normalize_command,
)


# --- autonomous pane capabilities ----------------------------------------- #


# --- the normalizer (x-a5e4) ------------------------------------------------ #


def test_normalize_command_is_command_agnostic_for_codex():
    # ANY footnote /command -> $fno:command on codex, not just /target.
    assert normalize_command("/fno:ship pr create", "codex") == "$fno:ship pr create"


# ---------------------------------------------------------------------------
# x-8e59: the builtin rung reads config.dispatch.auto_merge
#
# x-4391 shipped the key to 2 of 3 dispatch paths. The builtin here was the
# deaf one, so an operator who set the key still got `/target --no-merge <id>`,
# a manifest frozen at auto_merge_approved: false, and a refused `fno do pr merge`.
# The two callers that honored it did so by passing an explicit command around
# the builtin rather than fixing it.
# ---------------------------------------------------------------------------


# ---------------------------------------------------------------------------
# x-ebd2: the lifecycle rung. Law d-834b6ff1: difficulty decides at intake
# (no plan, no status to read); the plan's rung decides at re-dispatch. The
# stored dispatch_command is audit input, never truth.
# ---------------------------------------------------------------------------


def _stub_door(monkeypatch, impl):
    """Stub the keeper client: ``impl(method, params)`` answers or raises."""
    import fno.graph.store as store

    class _FakeClient:
        @staticmethod
        def request(method, params):
            assert method == "effective_command"
            return impl(method, params)

    monkeypatch.setattr(store, "_client_for", lambda path: _FakeClient())


def _refusal_text(rung):
    return (
        f"dispatch command cannot be derived for node x-abcd: plan rung "
        f"{rung!r} with difficulty '' answers no lifecycle rung"
    )


def assert_refusal_names_subject_and_cites_no_node(exc: DispatchResolveError) -> None:
    # A lead reads the refusal cold: the failing node must be the first
    # identifier in the sentence, and a trailing citation invites a misread
    # (the node that shipped this assertion went to fix the cited node).
    message = str(exc)
    assert "for node x-abcd" in message
    assert message.index("for node x-abcd") < message.index("plan rung")
    assert "(x-" not in message


def test_normalize_command_gemini_refused():
    with pytest.raises(DispatchResolveError, match="agy"):
        normalize_command("/target --no-merge {id}", "gemini")
