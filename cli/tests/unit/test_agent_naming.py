"""The `fno agents name` bridge contract (what the shell dispatchers branch on).

The name vocabulary, mint, and parse live in crates/fno-agents/src/naming.rs
and carry their own tests there.
"""


def _run_name(*args):
    from typer.testing import CliRunner

    from fno.cli import app

    return CliRunner().invoke(app, ["agents", "name", *args])


def test_bridge_prints_the_name_and_exits_zero():
    res = _run_name("target", "x-3218", "--slug", "Path Consolidation: Wave 0")
    assert res.exit_code == 0
    assert res.stdout.strip() == "target-3218-path-consolidation-wave-0"


def test_bridge_dispatch_form_refusals():
    assert _run_name("", "x-1", "--source", "ab").exit_code == 2  # --source needs --command
    assert _run_name("legacy", "x-1", "--command", "t").exit_code == 2  # not both forms
    # An unknown word refuses (exit 3), never defaulting to t; a work-command
    # word maps through the vocabulary owner.
    assert _run_name("", "x-1", "--command", "impeccable").exit_code == 3
    word = _run_name("", "x-1", "--command", "target")
    assert word.exit_code == 0 and word.stdout.strip() == "t-1"
    assert _run_name("", "x-1", "--source", "zz", "--command", "t").exit_code == 3


def test_bridge_refusal_exit_code_is_three_not_two():
    """The load-bearing constant: 3 is the refusal, 2 is Click's usage error.

    Both shell dispatchers branch on 3 alone. If this collapses to 2, an `fno`
    too old to know this command (Click answers "no such command" with 2) reads as
    "node unrepresentable" and refuses every dispatch instead of degrading to
    the fallback assembly. Asserting the exact code is the only thing that
    catches that: the downstream receipt lines are indistinguishable, because
    the degraded path has its own over-64 refusal that prints a similar message.
    """
    from fno.agents.cli import NAME_REFUSED_EXIT

    assert NAME_REFUSED_EXIT == 3
    assert NAME_REFUSED_EXIT != 2, "2 is Click's usage/unknown-command exit"

    res = _run_name("target", "n-" + "z" * 70)
    assert res.exit_code == NAME_REFUSED_EXIT
    assert res.exit_code == 3
