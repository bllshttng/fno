"""x-f76e: spawn argv normalization (autogen name, -r/--resume widening,
positional substrate token). Pure argv -> argv, covered against the plan ACs."""
from __future__ import annotations

import io
import random

import pytest

from fno.agents.spawn_defaults import normalize_spawn_args

UUID = "6501096a-1111-2222-3333-444455556666"
SHORT = "6501096a"


def _norm(args, **kw):
    kw.setdefault("existing_names", set())
    kw.setdefault("resolver", lambda s: UUID if s == SHORT else None)
    kw.setdefault("rng", random.Random(0))
    kw.setdefault("stderr", io.StringIO())
    return normalize_spawn_args(args, **kw)


# --- Pass-through / non-spawn -------------------------------------------------


# --- AC1-HP / AC2-EDGE: autogen name -----------------------------------------

def test_ac1_hp_nameless_spawn_mints_slug():
    out = _norm(["spawn"])
    assert out == ["spawn", "--name", out[2]]
    assert "-" in out[2]  # adjective-noun slug
    # A succession hand-off names nobody: the settle draws the name, and a
    # minted slug would arrive as an explicit proposal and decline the draw.
    assert "--name" not in _norm(["spawn", "--hand-off", "--promote", "lead"])


def test_autogen_avoids_registry_collision():
    rng = random.Random(0)
    first = normalize_spawn_args(["spawn"], existing_names=set(), rng=random.Random(0))[2]
    # Seed the same rng path but mark the first pick taken -> must differ.
    out = normalize_spawn_args(["spawn"], existing_names={first}, rng=rng)
    assert out[2] != first


def test_autogen_exhaustion_exits_2():
    # Every possible slug taken -> bounded exit 2, never an infinite loop.
    from fno.agents import spawn_defaults as sd

    everything = {f"{a}-{n}" for a in sd._SLUG_ADJ for n in sd._SLUG_NOUN}
    err = io.StringIO()
    with pytest.raises(SystemExit) as e:
        normalize_spawn_args(
            ["spawn"], existing_names=everything, rng=random.Random(1), stderr=err
        )
    assert e.value.code == 2
    assert "free auto-name" in err.getvalue()


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


def test_spawn_value_flags_cover_every_cmd_spawn_value_option():
    """The normalizer's value-flag table is a hand-maintained mirror of
    cmd_spawn's option list; a missing entry silently turns that flag's value
    into a positional. Pin the two together so adding an option to cmd_spawn
    without teaching the scanner fails here instead of in the field."""
    import ast
    import inspect

    from fno.agents import cli as agents_cli
    from fno.agents.spawn_defaults import _SPAWN_VALUE_FLAGS

    tree = ast.parse(inspect.getsource(agents_cli.cmd_spawn.__wrapped__
                                       if hasattr(agents_cli.cmd_spawn, "__wrapped__")
                                       else agents_cli.cmd_spawn))
    fn = tree.body[0]
    missing = []
    defaults = fn.args.defaults
    for arg, default in zip(fn.args.args[len(fn.args.args) - len(defaults):], defaults):
        if not isinstance(default, ast.Call) or getattr(default.func, "attr", "") != "Option":
            continue
        annotation = ast.unparse(arg.annotation) if arg.annotation else ""
        if annotation.strip() == "bool":
            continue  # boolean flags consume no value
        for spelling in (e.value for e in default.args[1:] if isinstance(e, ast.Constant)):
            if spelling not in _SPAWN_VALUE_FLAGS:
                missing.append(f"{arg.arg} ({spelling})")
    assert not missing, (
        "cmd_spawn value options absent from _SPAWN_VALUE_FLAGS (their values "
        f"would be read as positionals): {', '.join(missing)}"
    )


def test_existing_pane_is_removed_only_from_the_fno_spawn_head():
    from fno.agents.spawn_defaults import extract_existing_pane

    args, pane = extract_existing_pane(
        ["spawn", "--name", "worker", "--pane=19", "work", "--", "--pane", "7"]
    )

    assert pane == 19
    assert args == ["spawn", "--name", "worker", "work", "--", "--pane", "7"]


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


def test_mint_failure_falls_back_to_adjective_noun(monkeypatch):
    # A spawn must never die on a naming lookup.
    from fno.agents import spawn_defaults as sd
    from fno.agents.naming import AgentNameError

    _stub_worker_name(monkeypatch, AgentNameError("no node slug"))
    out = _norm(["spawn", "--node", "x919", "go"])
    assert out[2] in {f"{a}-{n}" for a in sd._SLUG_ADJ for n in sd._SLUG_NOUN}


def test_head_model_flag_is_shorthand_for_the_fenced_harness_flag():
    # Spawn allows every harness flag: `-m` before the fence moves behind it,
    # where the harness reads it, and a prompt-less spawn keeps MESSAGE empty.
    out = _norm(["spawn", "--name", "w", "-m", "sonnet", "go", "--", "--verbose"])
    assert out == ["spawn", "--name", "w", "go", "--", "--model", "sonnet", "--verbose"]
    bare = _norm(["spawn", "--name", "w", "--model=haiku"])
    assert bare == ["spawn", "--name", "w", "", "--", "--model", "haiku"]
