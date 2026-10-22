"""Unit tests for `fno do target init` and the `fno do state init` redirect.

Change 3 of the worktree-binding plan (backlog ab-02e44aa6): a discoverable
bootstrap command that records input/plan_path + the owner_cwd binding and
refuses to write a stub, plus a redirect on the substitution-prone
`fno do state init` bare bootstrap.
"""
from __future__ import annotations
from tests.fixtures.graph_seed import seed_graph

import json
from types import SimpleNamespace

import pytest
import typer
from typer.testing import CliRunner

from fno.cli import app
from fno import target_cli

runner = CliRunner()


def _retro_dispatch_node(tmp_path, *, source_pr=555, evidence=None):
    details = (
        f"Source: PR #{source_pr}, https://github.com/o/r/pull/{source_pr}#discussion_r123\n"
        f"<!-- retro-triage source_pr={source_pr} finding_hash=deadbeef -->"
    )
    node = {"id": "x-retro", "details": details, "cwd": str(tmp_path)}
    if evidence is not None:
        node["evidence"] = evidence
    return node


def test_retro_dispatch_preflight_tier1_refuses_and_supersedes(tmp_path, capsys):
    node = _retro_dispatch_node(tmp_path)
    closed = []

    def scan(entries, **kwargs):
        assert kwargs["include_planned"] is True
        return [SimpleNamespace(signal="resolved/outdated thread")]

    with pytest.raises(typer.Exit) as exc:
        target_cli._retro_dispatch_preflight(
            node,
            scan_fn=scan,
            supersede_fn=lambda node_id, reason: closed.append((node_id, reason)) or True,
        )
    assert exc.value.exit_code == 3
    assert closed and closed[0][0] == "x-retro"
    assert "source PR #555" in capsys.readouterr().err


def test_retro_dispatch_preflight_tier2_scopes_and_warns_on_overlap(tmp_path, capsys):
    node = _retro_dispatch_node(
        tmp_path, evidence={"items": {"git:merged-region:cli/src/fno/target_cli.py": "region"}}
    )
    calls = []

    def gh(args):
        calls.append(args)
        if args[:2] == ["pr", "view"]:
            return 0, json.dumps({"mergedAt": "2026-07-01T00:00:00Z"}), ""
        return 0, json.dumps([
            {
                "number": 568,
                "title": "selector fix",
                "mergedAt": "2026-07-02T00:00:00Z",
                "files": [{"path": "cli/src/fno/target_cli.py"}],
            }
        ]), ""

    target_cli._retro_dispatch_preflight(
        node,
        scan_fn=lambda entries, **kwargs: [],
        gh_runner=gh,
        unattended=True,
    )
    output = capsys.readouterr().err
    assert "Tier 2 sibling PR #568" in output
    assert "cli/src/fno/target_cli.py" in output
    assert all(args[args.index("--repo") + 1] == "o/r" for args in calls)















def test_state_init_redirects_target_bootstrap():
    """AC (redirect): bare `state init` (default type=target) -> non-zero redirect."""
    result = runner.invoke(app, ["do", "state", "init"])
    assert result.exit_code == 2
    assert "fno do target init" in result.output


def test_check_dispatch_hold_is_wired_for_direct_shell_bootstrap(tmp_path, monkeypatch):
    _held_graph(tmp_path, monkeypatch)
    monkeypatch.setenv("TARGET_INPUT", "x-5a5c")
    result = runner.invoke(app, ["do", "target", "check-dispatch-hold"])
    assert result.exit_code == 9, result.output
    assert "lead:119e3c52" in result.output

    from pathlib import Path as _P
    from fno.paths import resolve_plugin_script

    text = _P(resolve_plugin_script("hooks/helpers/init-target-state.sh")).read_text()
    assert "fno do target check-dispatch-hold" in text
    assert '"$_DH_RC" -eq 9' in text


def _unreadable_graph(tmp_path, monkeypatch):
    """graph.json resolves to a directory: open() raises, nothing parses."""
    gp = tmp_path / "graph-unreadable"
    gp.mkdir()
    monkeypatch.setattr("fno.paths.graph_json", lambda: gp)
    return gp


def test_check_dispatch_hold_refuses_named_node_when_graph_unreadable(tmp_path, monkeypatch):
    _unreadable_graph(tmp_path, monkeypatch)
    monkeypatch.setenv("TARGET_INPUT", "x-5a5c")
    result = runner.invoke(app, ["do", "target", "check-dispatch-hold"])
    assert result.exit_code == 9, result.output
    assert "backlog graph is unreadable" in result.output
    assert "refusing to assume unheld" in result.output


def _contained_graph(tmp_path, monkeypatch, *, owner="x-6320"):
    """A graph with one delivery unit and one node contained in it.

    The owner binds a real plan file. A bound, resolving plan is one of the two
    scope denominators, so the denominator gate at init must see it resolve; a
    placeholder path gets emptied at back-fill and the node reads as plan-less.
    """
    plan = tmp_path / "one.md"
    plan.write_text("# plan\n", encoding="utf-8")
    plan_path = str(plan)
    gp = tmp_path / "graph.json"
    seed_graph(gp, {"entries": [
        {"id": owner, "plan_path": plan_path, "status": "ready"},
        {"id": "x-261c", "plan_path": plan_path, "status": "ready",
         "contained_in": owner},
    ]})
    monkeypatch.setattr("fno.paths.graph_json", lambda: gp)
    return gp


def _held_graph(tmp_path, monkeypatch):
    plan = tmp_path / "held.md"
    plan.write_text(
        "---\nstatus: ready\ndispatch_hold:\n"
        "  reason: Blocking review finding is unresolved\n"
        "  release_when: The finding is fixed and re-reviewed\n"
        "  review_on: 2099-08-20\n"
        "  set_by: lead:119e3c52\n---\n",
        encoding="utf-8",
    )
    gp = tmp_path / "graph-held.json"
    seed_graph(gp, {"entries": [
        {"id": "x-5a5c", "status": "ready", "plan_path": str(plan)},
        {"id": "x-1a2b", "status": "ready", "contained_in": "x-5a5c"},
    ]})
    monkeypatch.setattr("fno.paths.graph_json", lambda: gp)
    return gp


def test_shared_plan_path_resolves_to_the_delivery_unit(tmp_path, monkeypatch):
    """A plan held by a unit and its contained children is legal, not ambiguous.

    Returning None for the normal contained shape let `--plan-path <shared>`
    sail past the containment redirect AND the retro dedup gate: both read this
    one resolver, so the miss was doubled.
    """
    gp = _contained_graph(tmp_path, monkeypatch)
    from fno.graph.store import read_graph_strict

    plan_path = read_graph_strict(gp)[0]["plan_path"]
    node = target_cli._resolve_dispatch_node(None, plan_path)
    assert node is not None and node["id"] == "x-6320"


def test_two_uncontained_holders_stay_ambiguous(tmp_path, monkeypatch):
    """Narrowing to the unit must not start guessing between real rivals.

    Change 1.1 refuses to CREATE this state; a graph that predates it must not
    have one of the two picked silently.
    """
    gp = tmp_path / "graph.json"
    seed_graph(gp, {"entries": [
        {"id": "x-6320", "plan_path": "/p/one.md", "status": "ready"},
        {"id": "x-8a4f", "plan_path": "/p/one.md", "status": "ready"},
    ]})
    monkeypatch.setattr("fno.paths.graph_json", lambda: gp)
    assert target_cli._resolve_dispatch_node(None, "/p/one.md") is None




def test_check_contained_refuses_with_the_shell_gates_own_code(tmp_path, monkeypatch):
    """rc 9, not 2: a stale fno exits 2 as a Click "No such command".

    Treating 2 as a refusal would hard-refuse every direct bootstrap the moment
    the installed CLI fell behind source - the same reasoning check-review-gate
    documents for its own code.
    """
    _contained_graph(tmp_path, monkeypatch)
    monkeypatch.setenv("TARGET_INPUT", "x-261c")
    result = runner.invoke(app, ["do", "target", "check-contained"])
    assert result.exit_code == 9, result.output
    assert "x-6320" in result.output


def test_init_script_wires_the_containment_gate():
    """The command is only a guard if the documented direct path actually calls it.

    A refusal nothing invokes is the decorative-guard failure this whole task is
    about, so assert the wiring, not just the command.
    """
    from pathlib import Path as _P

    from fno.paths import resolve_plugin_script

    script = _P(resolve_plugin_script("hooks/helpers/init-target-state.sh"))
    text = script.read_text(encoding="utf-8")
    assert "fno do target check-contained" in text
    # rc 9 refuses; anything else must fall through rather than brick bootstrap.
    assert '"$_CT_RC" -eq 9' in text




def test_check_contained_says_so_when_the_store_is_unavailable(tmp_path,
                                                               monkeypatch):
    """A lost check that still exits 0 reads as "checked, and fine".

    The keeper gate is what makes this check trustworthy, so swallowing its
    failure silently is the same degrade this gate has already been fixed for
    twice. Still fail-open - it says the check DID NOT RUN and names why.
    """
    import fno.graph.store as gs

    _contained_graph(tmp_path, monkeypatch)

    def boom(*a, **k):
        raise gs.StoreUnavailable("unreachable", "no store for you")

    monkeypatch.setattr(target_cli, "_resolve_dispatch_node", boom)
    monkeypatch.setenv("TARGET_INPUT", "x-261c")
    result = runner.invoke(app, ["do", "target", "check-contained"])
    assert result.exit_code == 0, result.output
    assert "DID NOT RUN" in result.output
    assert "no store for you" in result.output


def test_resolve_owned_identity_command_proven_claude_wins_foreign_codex_refused(
    tmp_path, monkeypatch
) -> None:
    """AC1-HP + AC3-ERR at the command: the proven claude marker wins and the
    foreign codex marker is refused by the walk (it contradicts the prover),
    with no registry collision consulted - a session with no stamp resolves by
    elimination and contradiction only, because colliding there read a
    hand-started joined session's OWN registered row as contention and refused
    every role grantor. Collision reporting lives on the stamped branches.

    Proven in Python so it does not depend on the PATH-resolved `fno` carrying
    the command - the bash hook test's CI limitation, since the PR's own CI runs a
    `fno` that predates the command. The prover is pinned to claude (a real claude
    process's view) so the claude marker is proven and wins. CI-robust: no real
    process tree.
    """
    from fno.agents.registry import register_existing_session
    from fno.paths_testing import use_tmpdir

    use_tmpdir(monkeypatch, tmp_path)
    foreign = "019fc87d-ddff-7c90-926a-6bdd7ebb186c"
    register_existing_session(
        provider="codex", session_id=foreign, cwd="/x"
    )
    # Pin the prover to claude (what a real claude session's process tree sees),
    # so the test does not depend on the runner's actual harness.
    monkeypatch.setattr(
        "fno.claims.session_pid.resolve_session_harness",
        lambda from_pid=None: "claude",
    )
    monkeypatch.setenv("CODEX_THREAD_ID", foreign)
    monkeypatch.setenv("CLAUDE_CODE_SESSION_ID", "aaaa1111-mine")

    result = runner.invoke(app, ["do", "target", "resolve-owned-identity"])
    assert result.exit_code == 0, result.output
    fields = {
        line.split("=", 1)[0]: line.split("=", 1)[1]
        for line in result.stdout.splitlines()
        if "=" in line
    }
    assert fields["HARNESS"] == "claude"
    assert fields["SESSION_ID"] == "aaaa1111-mine"
    assert fields["DISPOSITION"] == "proven"
    assert fields["COLLISION"] == ""
    assert fields["COLLISION_ID"] == ""


def test_resolve_owned_identity_self_row_is_not_contention(tmp_path, monkeypatch) -> None:
    """AC2-HP: a spawned worker whose own registry row holds its stamped id
    resolves that identity with no proof available. The prover is silent (a
    codex thread worker has no harness ancestor to walk); the canonical stamp
    and the worker's own row agree on (harness, id) because spawn minted both
    in one act, so the row reads as self, not as a competing holder. The
    disposition is "single", not "canonical": resolution rode the same
    uncontended elimination the marker loop calls the dominant case, so the
    worktree-manifest anchor guard for task claims stays armed (x-0992)."""
    from fno.agents.registry import register_existing_session
    from fno.paths_testing import use_tmpdir

    use_tmpdir(monkeypatch, tmp_path)
    mine = "019fc88e-aaaa-7c90-926a-6bdd7ebb186c"
    register_existing_session(harness="codex", session_id=mine, cwd="/x")
    monkeypatch.setattr(
        "fno.claims.session_pid.resolve_session_harness",
        lambda from_pid=None: None,
    )
    monkeypatch.setenv("FNO_HARNESS_NAME", "codex")
    monkeypatch.setenv("FNO_HARNESS_SESSION_ID", mine)
    monkeypatch.setenv("CODEX_THREAD_ID", mine)

    result = runner.invoke(app, ["do", "target", "resolve-owned-identity"])
    assert result.exit_code == 0, result.output
    fields = {
        line.split("=", 1)[0]: line.split("=", 1)[1]
        for line in result.stdout.splitlines()
        if "=" in line
    }
    assert fields["HARNESS"] == "codex"
    assert fields["SESSION_ID"] == mine
    assert fields["DISPOSITION"] == "single"
    assert fields["COLLISION"] == ""


def test_resolve_owned_identity_foreign_row_still_refused(tmp_path, monkeypatch) -> None:
    """AC3-ERR: a stamped id whose live registry row carries a DIFFERENT
    harness is another session's row, not self, and the refusal names it."""
    from fno.agents.registry import register_existing_session
    from fno.paths_testing import use_tmpdir

    use_tmpdir(monkeypatch, tmp_path)
    theirs = "019fc88e-bbbb-7c90-926a-6bdd7ebb186c"
    owner = register_existing_session(harness="claude", session_id=theirs, cwd="/x").name
    monkeypatch.setattr(
        "fno.claims.session_pid.resolve_session_harness",
        lambda from_pid=None: None,
    )
    monkeypatch.setenv("FNO_HARNESS_NAME", "codex")
    monkeypatch.setenv("FNO_HARNESS_SESSION_ID", theirs)
    monkeypatch.setenv("CODEX_THREAD_ID", theirs)

    result = runner.invoke(app, ["do", "target", "resolve-owned-identity"])
    assert result.exit_code == 0, result.output
    fields = {
        line.split("=", 1)[0]: line.split("=", 1)[1]
        for line in result.stdout.splitlines()
        if "=" in line
    }
    assert fields["HARNESS"] == ""
    assert fields["SESSION_ID"] == ""
    assert fields["COLLISION"] == owner
    assert fields["COLLISION_ID"] == theirs
