"""x-f76e: spawn argv normalization (autogen name, -r/--resume widening,
positional substrate token). Pure argv -> argv, covered against the plan ACs."""
from __future__ import annotations

import io

import pytest

from fno.agents.spawn_defaults import normalize_spawn_args

UUID = "6501096a-1111-2222-3333-444455556666"
SHORT = "6501096a"


def _norm(args, **kw):
    kw.setdefault("existing_names", set())
    kw.setdefault("resolver", lambda s: UUID if s == SHORT else None)
    kw.setdefault("stderr", io.StringIO())
    return normalize_spawn_args(args, **kw)


# --- Pass-through / non-spawn -------------------------------------------------


# --- AC1-HP / AC2-EDGE: autogen name -----------------------------------------

def test_nameless_prompt_names_the_worker_from_its_words():
    out = _norm(["spawn", "fix the login bug today"])
    assert out[1:3] == ["--name", "t-fix-the-login-bug"]


def test_nameless_spawn_with_no_prompt_refuses_by_name():
    # A worker name comes from the node slug or the prompt (law
    # d-9e0ac19d); with neither there is nothing to mint, so refuse instead
    # of inventing one.
    err = io.StringIO()
    with pytest.raises(SystemExit) as e:
        _norm(["spawn"], stderr=err)
    assert e.value.code == 2
    assert "no node slug or prompt" in err.getvalue()
    # A succession hand-off names nobody: the settle draws the name, and a
    # minted slug would arrive as an explicit proposal and decline the draw.
    assert "--name" not in _norm(["spawn", "--hand-off", "--promote", "lead"])


def test_prompt_name_collision_suffixes_not_refuses():
    first = _norm(["spawn", "fix the login bug today"])[2]
    out = _norm(["spawn", "fix the login bug today"], existing_names={first})
    assert out[2] == "t-fix-the-login-bug-2"


# --- --argv payload boundary (codex finding) ---------------------------------

def test_argv_payload_resume_flag_is_not_scanned():
    # A --resume inside the provider payload must pass through untouched: it is
    # the child command's flag, never an fno spawn flag.
    argv = ["spawn", "--name", "w", "--argv", "--", "tool", "--resume", "deadbeef"]
    assert _norm(argv) == argv


# --- x-6de8: the name axis moved off the positional --------------------------


def test_two_positionals_are_refused_not_guessed():
    # The old grammar was `spawn <name> <message>`. Accepting two positionals
    # would silently register an agent named after the prompt, so refuse and
    # name the fix instead.
    err = io.StringIO()
    with pytest.raises(SystemExit) as exc:
        _norm(["spawn", "worker", "/target x-1234"], stderr=err)
    assert exc.value.code == 2
    assert "one positional" in err.getvalue()
    assert "--name" in err.getvalue()


# --- x-1caa: the `--` passthrough boundary at the seam ------------------------

def test_passthrough_fence_tokens_are_not_prompt_positionals():
    # `hi -- --name reviewer`: one prompt positional; the fenced tokens are the
    # provider's passthrough, not a second/third positional to refuse. The mint
    # still fires (a passthrough --name is the provider's flag, not fno's).
    out = _norm(["spawn", "hi", "--", "--name", "reviewer"])
    assert out[1] == "--name" and out[2] != "reviewer"
    assert out[3] == "hi"
    assert out[4:] == ["--", "--name", "reviewer"]


# --- a node-driven or /target spawn mints t-<short slug> --------------------

def _stub_worker_name(monkeypatch, answer):
    import fno.agents.naming as naming

    calls = []

    def fake(node=None, *, seed=None, slug=None):
        calls.append((node, seed, slug))
        if isinstance(answer, Exception):
            raise answer
        return answer

    monkeypatch.setattr(naming, "worker_name", fake)
    return calls


def test_target_seed_spawn_is_named_for_its_node_and_suffixes_a_taken_name(monkeypatch):
    # The seed names the node; no model token rides the name, and a name a
    # live worker holds gets a numeric suffix instead of a refusal.
    calls = _stub_worker_name(monkeypatch, "t-visual-approval-gate")
    argv = ["spawn", "/fno:target x-f850"]
    assert _norm(argv)[1:3] == ["--name", "t-visual-approval-gate"]
    assert calls[0] == (None, "/fno:target x-f850", None)
    taken = {"t-visual-approval-gate", "t-visual-approval-gate-2"}
    assert _norm(argv, existing_names=taken)[2] == "t-visual-approval-gate-3"


def test_node_flag_and_slug_reach_the_mint(monkeypatch):
    calls = _stub_worker_name(monkeypatch, "t-sentinel-arms")
    out = _norm(["spawn", "--node", "x919", "--slug", "sentinel-arms", "go"])
    assert out[1:3] == ["--name", "t-sentinel-arms"]
    assert calls == [("x919", "go", "sentinel-arms")]


def test_mint_failure_refuses_by_name(monkeypatch):
    # A lookup failure is a named refusal, never an invented name.
    from fno.agents.naming import AgentNameError

    _stub_worker_name(monkeypatch, AgentNameError("no node slug"))
    err = io.StringIO()
    with pytest.raises(SystemExit) as e:
        _norm(["spawn", "--node", "x919", "go"], stderr=err)
    assert e.value.code == 2
    assert "no node slug or prompt" in err.getvalue()


def test_head_model_flag_is_shorthand_for_the_fenced_harness_flag():
    # Spawn allows every harness flag: `-m` before the fence moves behind it,
    # where the harness reads it, and a prompt-less spawn keeps MESSAGE empty.
    out = _norm(["spawn", "--name", "w", "-m", "sonnet", "go", "--", "--verbose"])
    assert out == ["spawn", "--name", "w", "go", "--", "--model", "sonnet", "--verbose"]
    bare = _norm(["spawn", "--name", "w", "--model=haiku"])
    assert bare == ["spawn", "--name", "w", "", "--", "--model", "haiku"]


def test_head_effort_and_permission_mode_ride_the_fence_with_the_model():
    out = _norm(
        ["spawn", "--name", "w", "--effort", "high", "--permission-mode=bypassPermissions", "go"]
    )
    assert out == [
        "spawn", "--name", "w", "go", "--",
        "--effort", "high", "--permission-mode", "bypassPermissions",
    ]
