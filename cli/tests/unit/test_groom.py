"""The groom skill brief's lever contract (`skills/groom/SKILL.md`).

The pass itself is native (`crates/fno-agents/src/backlog/groom.rs`); the
skill the worker loads stays here, bound to the real CLI.
"""
from __future__ import annotations

from pathlib import Path

import pytest

SKILL = Path(__file__).resolve().parents[3] / "skills" / "groom" / "SKILL.md"


# (command, flags the brief teaches for it). Kept together so the two tests
# below cannot drift: one pins the brief's text, the other pins the real CLI.
LEVERS = [
    ("supersede", ("--replaces", "--reason")),
    ("defer", ("--reason",)),
    ("undefer", ()),
    ("update", ("--priority",)),
    # rank left this table when it became the operator's pin: the verb refuses
    # a groom worker, so teaching it here would teach a refusal. The vote it
    # was replaced by is the lever below.
    ("encounter", ("--evidence",)),
    ("idea", ()),
    ("intake", ()),
    ("update", ("--blocked-by",)),
]


@pytest.mark.parametrize("command,flags", LEVERS)
def test_skill_names_every_allowed_lever(command, flags):
    text = SKILL.read_text()
    assert f"fno backlog {command}" in text
    for flag in flags:
        assert flag in text


@pytest.mark.parametrize("command,flags", LEVERS)
def test_brief_levers_exist_on_the_real_cli(command, flags):
    """The brief must not teach a flag the CLI does not have.

    A worker follows this brief literally, so a wrong signature fails at exit 2
    on the lever rather than anywhere visible. Substring checks on the doc alone
    cannot catch that - this binds it to the actual command.
    """
    import click
    import typer.main

    from fno.graph.cli import cli as graph_cli

    root = typer.main.get_command(graph_cli)
    sub = root.get_command(click.Context(root), command)
    if sub is None:
        # Retired python leaves answer natively; bind the lever to the
        # binary's own help surface instead.
        from tests._native_door import run_native

        code, out, err = run_native("backlog", command, "--help")
        assert code == 0, f"`fno backlog {command}` does not exist natively"
        missing = {f for f in flags if f not in out + err}
        assert not missing, f"`fno backlog {command}` has no {sorted(missing)}"
        return

    available: set[str] = set()
    for param in sub.params:
        available.update(param.opts)
    missing = set(flags) - available
    assert not missing, f"`fno backlog {command}` has no {sorted(missing)}"


def test_skill_carries_the_auto_convene_and_report_contract():
    text = SKILL.read_text()
    assert "Auto-convene" in text
    assert "fno agents mail send" in text
    assert "Net mint rate" in text


def test_skill_routes_questions_to_the_deferred_pile_not_idea():
    """The triage pile IS `deferred` + a reason, so a question must defer.

    An `idea`-status node does not appear in the pile, so routing questions
    there would silently drop them from the surface grooming reports on.
    """
    text = SKILL.read_text()
    assert 'fno backlog defer <id> --reason "question:' in text
    assert "an idea-status node is not in the pile" in text.lower()


def test_skill_forbids_direct_state_edits():
    text = SKILL.read_text()
    assert "graph.db" in text and "Never" in text
    for forbidden in ("jq -i", "sed -i"):
        assert forbidden in text, "the brief must name the direct-edit paths it forbids"


def test_skill_reads_fresh_proposals_and_reports_the_mechanical_line():
    # The pass-to-pass interface is the live verb, not a file: a digest is what
    # went stale for ten days unnoticed.
    text = SKILL.read_text()
    assert "fno backlog maintain" in text
    assert "Mechanical" in text
    assert "groom-digest" not in text, "the digest is retired; the skill must not resurrect it"
