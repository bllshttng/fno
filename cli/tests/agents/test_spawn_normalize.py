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


# --- AC2-HP / AC3-HP / AC3-EDGE: -r widening ---------------------------------

def test_ac2_hp_short_form_revival_normalizes_fully():
    out = _norm(["spawn", "--name", "foo", "-r", SHORT, "thread"])
    assert out == ["spawn", "--name", "foo", "--resume", UUID, "--substrate", "thread"]


# --- AC1-ERR / AC2-ERR / refusals --------------------------------------------

def test_ac1_err_unresolvable_short_id_exits_2():
    with pytest.raises(SystemExit) as e:
        _norm(["spawn", "--name", "foo", "-r", "deadbeef"], resolver=lambda s: None)
    assert e.value.code == 2


# --- non-substrate trailing positional stays a message ------------------------


# --- --argv payload boundary (codex finding) ---------------------------------

def test_argv_payload_resume_flag_is_not_scanned():
    # A --resume inside the provider payload must pass through untouched: it is
    # the child command's flag, not an fno do resume request (no --substrate bg added).
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


def test_trailing_substrate_word_still_leaves_one_positional():
    # `spawn "<prompt>" bg` is the canonical shape: pass 1 lifts the substrate
    # word off the tail, so the prompt is the only positional left.
    out = _norm(["spawn", "review this diff", "bg"])
    assert out[1] == "--name" and "-" in out[2]
    assert out[3] == "review this diff"
    assert out[4:] == ["--substrate", "bg"]


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


def test_off_pane_substrate_with_uncarried_tokens_demotes_to_pane():
    # A thread lane carries only the spellings its contract row maps, so an
    # unmapped fenced token demotes the spawn to the pane at the seam - the
    # one front door both runtimes share - instead of refusing on the
    # substrate or silently dropping the token on the Rust lane.
    err = io.StringIO()
    out = _norm(
        [
            "spawn", "--name", "w", "-H", "codex",
            "--substrate", "bg", "hi", "--", "--verbose", "x",
        ],
        stderr=err,
    )
    assert out[out.index("--substrate") + 1] == "pane"
    assert "no carrier for --verbose" in err.getvalue()


def test_claude_bg_carries_fenced_tokens():
    # claude's bg lane takes the whole harness argv, so its row carries ["*"]:
    # fenced tokens pass through untouched, on any substrate.
    err = io.StringIO()
    out = _norm(
        ["spawn", "--name", "w", "--substrate", "bg", "hi", "--", "--verbose", "x"],
        stderr=err,
    )
    assert out[out.index("--substrate") + 1] == "bg"
    assert "--verbose" in out
    assert "no carrier" not in err.getvalue()


def test_implied_thread_substrate_splices_before_the_fence():
    # --resume plus a fenced seed: the implied `--substrate thread` lands in
    # the fno head, never past the fence where click would read it as
    # passthrough positionals and lose the lane.
    out = _norm(["spawn", "-r", UUID, "--", "--flag seed"])
    assert out.index("--substrate") < out.index("--")
    assert out[out.index("--substrate") + 1] == "thread"


# --- x-b80d: a node-driven spawn mints t-<node>-<slug>-<model> ----------------

def test_node_spawn_mints_provenance_name():
    # The registry row's NAME is the only node carrier, so the mint must put
    # the node and slug there: t-x919-sentinel-glm.
    out = _norm(
        ["spawn", "--node", "x919", "--slug", "sentinel-arms", "--model", "glm-5.2", "go"]
    )
    assert out[1:3] == ["--name", "t-x919-sentinel-glm"]
    assert out[3:] == ["--node", "x919", "--slug", "sentinel-arms", "--model", "glm-5.2", "go"]


def test_node_spawn_re_spawn_suffixes_the_taken_name():
    # The mint is deterministic; a name already held by a live worker gets a
    # numeric suffix instead of a dispatcher refusal (a re-spawn on one node
    # is the two-worker implement+review shape).
    argv = ["spawn", "--node", "x919", "--slug", "arms", "--model", "glm-5.2", "go"]
    first = _norm(argv, existing_names=set())
    assert first[2] == "t-x919-arms-glm"
    second = _norm(argv, existing_names={"t-x919-arms-glm"})
    assert second[2] == "t-x919-arms-glm-2"
    third = _norm(argv, existing_names={"t-x919-arms-glm", "t-x919-arms-glm-2"})
    assert third[2] == "t-x919-arms-glm-3"


def test_graph_read_failure_falls_back_to_adjective_noun(monkeypatch):
    # A spawn must never die on a naming lookup: the graph read raising falls
    # back to the plain adjective-noun mint, not a refusal, not a t-<node>
    # name built on an unresolved identity.
    import fno.graph.store as gl

    def _raise(*args):
        raise RuntimeError("graph unreadable")

    monkeypatch.setattr(gl, "read_nodes_by_ids", _raise)
    out = _norm(["spawn", "--node", "x919", "--slug", "arms", "go"])
    from fno.agents import spawn_defaults as sd

    assert out[2] in {f"{a}-{n}" for a in sd._SLUG_ADJ for n in sd._SLUG_NOUN}


def test_graph_read_supplies_the_slug_when_flag_absent(monkeypatch):
    # --slug omitted: the best-effort graph read fills id + slug, and a slug
    # input normalizes to the canonical id.
    import fno.graph.store as gl

    monkeypatch.setattr(
        gl,
        "read_nodes_by_ids",
        lambda path, tokens: {"entries": [{"id": "x-919abcd", "slug": "sentinel-arms", "plan_path": None}], "missing": []},
    )
    out = _norm(["spawn", "--node", "x-919abcd", "--model", "glm-5.2", "go"])
    assert out[2] == "t-919abcd-sentinel-glm"
