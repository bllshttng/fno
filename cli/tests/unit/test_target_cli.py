"""Unit tests for `fno do target init` and the `fno do state init` redirect.

Change 3 of the worktree-binding plan (backlog ab-02e44aa6): a discoverable
bootstrap verb that records input/plan_path + the owner_cwd binding and
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


def _binding_answers(monkeypatch, receipt):
    """Route the transport to a canned native receipt and record payloads."""
    calls = []

    def _verb_call(verb, payload, *a, **k):
        calls.append((list(verb), dict(payload)))
        return dict(receipt)

    monkeypatch.setattr("fno.rust_binary.verb_call", _verb_call)
    return calls


def test_target_init_help_documents_inputs():
    result = runner.invoke(app, ["do", "target", "init", "--help"])
    assert result.exit_code == 0
    assert "--input" in result.stdout
    assert "--plan-path" in result.stdout






def _fake_plugin_root(tmp_path):
    fake_root = tmp_path / "plugin"
    (fake_root / "hooks" / "helpers").mkdir(parents=True)
    (fake_root / "hooks" / "helpers" / "init-target-state.sh").write_text("#!/bin/bash\n")
    return fake_root


def _retro_dispatch_node(tmp_path, *, source_pr=555, evidence=None):
    details = (
        f"Source: PR #{source_pr}, https://github.com/o/r/pull/{source_pr}#discussion_r123\n"
        f"<!-- retro-triage source_pr={source_pr} finding_hash=deadbeef -->"
    )
    node = {"id": "x-retro", "details": details, "cwd": str(tmp_path)}
    if evidence is not None:
        node["evidence"] = evidence
    return node


def test_retro_dispatch_preflight_non_retro_makes_no_probe(monkeypatch):
    calls = []
    target_cli._retro_dispatch_preflight(
        {"id": "x-plain", "details": "ordinary target"},
        gh_runner=lambda args: calls.append(args),
    )
    assert calls == []


def test_retro_dispatch_preflight_closed_node_is_a_noop(tmp_path):
    node = _retro_dispatch_node(tmp_path)
    node["completed_at"] = "2026-07-23T00:00:00Z"
    calls = []
    target_cli._retro_dispatch_preflight(
        node, gh_runner=lambda args: calls.append(args)
    )
    assert calls == []


def test_retro_dispatch_preflight_source_pr_none_warns_each_skipped_tier(
    tmp_path, capsys
):
    node = {
        "id": "x-postmortem",
        "details": "<!-- retro-triage source_pr=None finding_hash=deadbeef -->",
        "cwd": str(tmp_path),
    }
    calls = []
    target_cli._retro_dispatch_preflight(
        node, gh_runner=lambda args: calls.append(args)
    )
    output = capsys.readouterr().err
    assert output.count("WARN target init:") == 2
    assert "Tier 1 skipped" in output and "Tier 2 skipped" in output
    assert calls == []


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


def test_retro_dispatch_preflight_warns_on_tier1_uncertainty(tmp_path, capsys):
    node = _retro_dispatch_node(tmp_path)

    def scan(entries, *, warnings, **kwargs):
        warnings.append("thread state unavailable")
        return []

    target_cli._retro_dispatch_preflight(node, scan_fn=scan)
    output = capsys.readouterr().err
    assert "Tier 1 skipped" in output
    assert "Tier 2 skipped" in output


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


def test_retro_dispatch_preflight_attended_overlap_is_advisory_only(tmp_path):
    node = _retro_dispatch_node(
        tmp_path, evidence={"git:merged-region:cli/src/fno/target_cli.py": "region"}
    )
    closed = []

    def gh(args):
        if args[:2] == ["pr", "view"]:
            return 0, json.dumps({"mergedAt": "2026-07-01T00:00:00Z"}), ""
        return 0, json.dumps([{
            "number": 568,
            "title": "selector fix",
            "mergedAt": "2026-07-02T00:00:00Z",
            "files": [{"path": "cli/src/fno/target_cli.py"}],
        }]), ""

    target_cli._retro_dispatch_preflight(
        node,
        scan_fn=lambda entries, **kwargs: [],
        gh_runner=gh,
        supersede_fn=lambda node_id, reason: closed.append(node_id) or True,
    )
    assert closed == []














def test_target_init_empty_model_rejected(monkeypatch, tmp_path):
    """AC2-ERR: an empty --model exits 2. The usage refusal is the native
    door's wording (the parity goldens characterize it); the forward's
    observable surface is the propagated exit code."""
    fake_root = _fake_plugin_root(tmp_path)
    monkeypatch.delenv("CLAUDE_PLUGIN_ROOT", raising=False)
    monkeypatch.setenv("FNO_REPO_ROOT", str(fake_root))
    result = runner.invoke(app, ["do", "target", "init", "--input", "x", "--model", "  "])
    assert result.exit_code == 2


def test_target_init_rejects_invalid_size(monkeypatch, tmp_path):
    """Invalid --size exits 2. Same shape as the empty-model test: the wording
    is the native door's, the propagated code is the seam's contract."""
    fake_root = _fake_plugin_root(tmp_path)
    monkeypatch.delenv("CLAUDE_PLUGIN_ROOT", raising=False)
    monkeypatch.setenv("FNO_REPO_ROOT", str(fake_root))
    result = runner.invoke(app, ["do", "target", "init", "--input", "x", "--size", "XL"])
    assert result.exit_code == 2


def test_target_init_help_documents_size():
    result = runner.invoke(app, ["do", "target", "init", "--help"])
    assert result.exit_code == 0
    assert "--size" in result.stdout


def test_target_init_missing_script_exits_2(monkeypatch, tmp_path):
    """AC3-ERR: bare-install degrade is actionable, exits 2, no traceback.
    FNO_REPO_ROOT is authoritative. The refusal text is the native door's now
    (the thin forward only propagates its exit code), so the wording is
    characterized by the parity goldens, not re-asserted here."""
    fake_root = tmp_path / "empty"
    fake_root.mkdir()
    monkeypatch.delenv("CLAUDE_PLUGIN_ROOT", raising=False)
    monkeypatch.setenv("FNO_REPO_ROOT", str(fake_root))
    result = runner.invoke(app, ["do", "target", "init", "--input", "x"])
    assert result.exit_code == 2


def test_target_init_degrade_writes_no_state(monkeypatch, tmp_path):
    """AC3-FR / AC3-EDGE: when the init script is unresolvable (bare install or
    binary-present-but-skills-absent), the run exits 2, writes no partial
    target-state.md, and the bash writer never runs."""
    fake_root = tmp_path / "empty"
    fake_root.mkdir()
    proj = tmp_path / "proj"
    proj.mkdir()
    monkeypatch.chdir(proj)
    monkeypatch.delenv("CLAUDE_PLUGIN_ROOT", raising=False)
    monkeypatch.setenv("FNO_REPO_ROOT", str(fake_root))

    bash_calls = []
    real_run = target_cli.subprocess.run

    def _record_run(cmd, *a, **k):
        # The forward may exec the native decision layer; the bash writer
        # must never run when the init script is missing.
        cmd = list(cmd)
        if cmd and cmd[0] == "bash":
            bash_calls.append(cmd)
        return real_run(cmd, *a, **k)

    monkeypatch.setattr(target_cli.subprocess, "run", _record_run)
    result = runner.invoke(app, ["do", "target", "init", "--input", "x"])
    assert result.exit_code == 2
    assert bash_calls == []
    assert not (proj / ".fno").exists()




def test_state_init_redirects_target_bootstrap():
    """AC (redirect): bare `state init` (default type=target) -> non-zero redirect."""
    result = runner.invoke(app, ["do", "state", "init"])
    assert result.exit_code == 2
    assert "fno do target init" in result.output


def test_state_init_explicit_output_is_spared(tmp_path):
    """A deliberate --output is NOT a bootstrap; it must still create a file."""
    out = tmp_path / "explicit-state.md"
    result = runner.invoke(app, ["do", "state", "init", "--type", "target", "--output", str(out)])
    assert result.exit_code == 0, result.output
    assert out.exists()


def test_state_init_allow_stub_escape(tmp_path, monkeypatch):
    """--allow-stub bypasses the redirect for internal/test use."""
    monkeypatch.chdir(tmp_path)
    result = runner.invoke(app, ["do", "state", "init", "--allow-stub"])
    assert result.exit_code == 0, result.output
    assert (tmp_path / ".fno" / "target-state.md").exists()


def test_resolve_node_model_defers_difficulty_to_grid(monkeypatch):
    """The start seam pins only model: a canonical difficulty defers to the
    capacity grid at the spawn seam (parity with advance's dispatch), and the
    retired model_tier key reads as unset (x-baef AC4-HP negative half)."""
    monkeypatch.setattr(
        target_cli, "_find_node", lambda node_id: {"id": node_id, "difficulty": "high"}
    )
    model, source = target_cli._resolve_node_model("x-def1")
    assert model is None
    assert source == "provider-default(no-difficulty)"

    from fno.adapters.providers import benchmarks as _bm

    monkeypatch.setattr(_bm, "load_snapshot", lambda path=None: None)
    monkeypatch.setattr(
        target_cli, "_find_node", lambda node_id: {"id": node_id, "model_tier": "low"}
    )
    model, source = target_cli._resolve_node_model("x-def2")
    assert model is None
    assert source == "provider-default(no-difficulty)"


def test_resolve_model_command_resolves_difficulty_band(monkeypatch):
    """x-baef round-6: the bash dispatch lane pins --harness in its spawn
    argv, standing the capacity grid down, so resolve-model resolves the band
    itself - the resolution model_tier gave that lane before retirement."""
    from fno import route_resolve as rr
    from fno.adapters.providers import benchmarks as _bm

    monkeypatch.setattr(_bm, "load_snapshot", lambda path=None: None)
    # Reachability is config-declared now, so the band this test resolves has
    # to name its own fleet. Nothing built-in answers for it.
    inv = _inv([
        {"name": "glm-4.7", "harness": "claude", "model": "glm-4.7", "band": "low"},
    ])


def _inv(rows):
    from fno import route_resolve as _rr

    built = {}
    for r in rows:
        r = dict(r)
        built[r.get("name", "")] = _rr.InventoryRow(
            name=r.get("name", ""), harness=r.get("harness", ""),
            model=r.get("model", ""), band=r.get("band", ""),
        )
    return _rr.Inventory(rows=built, declared=True)
    monkeypatch.setattr(rr, "resolve_inventory", lambda **_kw: inv)
    monkeypatch.setattr(target_cli, "_resolve_node_id", lambda n, entries=None: n)
    monkeypatch.setattr(
        target_cli, "_find_node", lambda node_id: {"id": node_id, "difficulty": "low"}
    )
    result = runner.invoke(app, ["do", "target", "resolve-model", "x-band1"])
    assert result.exit_code == 0, result.output
    assert result.stdout.strip() == "glm-4.7"


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


def test_redirect_helper_ignores_non_contained_and_malformed_input():
    """Fail-open on anything that is not an affirmative owner id.

    Raising on a missing/odd node would turn a fail-open resolver into a
    dispatch-blocker, and the resolver returns None for every free-text input.
    """
    target_cli._redirect_if_contained(None)
    target_cli._redirect_if_contained({"id": "x-a"})
    target_cli._redirect_if_contained({"id": "x-a", "contained_in": None})
    target_cli._redirect_if_contained({"id": "x-a", "contained_in": ""})
    target_cli._redirect_if_contained({"id": "x-a", "contained_in": 0})


# ---------------------------------------------------------------------------
# codex P1: containment must hold on EVERY bootstrap path, not just `init`
# ---------------------------------------------------------------------------


def test_dispatch_node_resolves_dashless_literal_id():
    """A dash-less literal id binds at the claim boundary like its dashed kin.

    A config prefix with no separator once minted bare "<prefix><4hex>"
    literals; resolution is format-agnostic everywhere else, so the dispatch
    token shape must admit the literal and match it against the graph. An
    all-hex-shaped token matches too (a letter-led prefix makes one); the
    graph lookup, not the shape, decides identity.
    """
    assert target_cli._TARGET_NODE_TOKEN_RE.fullmatch("x4d12")
    assert target_cli._TARGET_NODE_TOKEN_RE.fullmatch("a3f9c1d2")
    assert not target_cli._TARGET_NODE_TOKEN_RE.fullmatch("4d12")  # digit-led stays out
    entry = {"id": "x4d12", "title": "legacy id"}
    assert target_cli._resolve_dispatch_node("x4d12", None, entries=[entry]) == entry


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


def test_check_contained_passes_a_delivery_unit_and_a_bare_idea(tmp_path, monkeypatch):
    """Exit 0 for anything that is not an affirmative contained node."""
    _contained_graph(tmp_path, monkeypatch)

    monkeypatch.setenv("TARGET_INPUT", "x-6320")
    assert runner.invoke(app, ["do", "target", "check-contained"]).exit_code == 0

    monkeypatch.setenv("TARGET_INPUT", "fix the login redirect")
    assert runner.invoke(app, ["do", "target", "check-contained"]).exit_code == 0

    monkeypatch.delenv("TARGET_INPUT", raising=False)
    assert runner.invoke(app, ["do", "target", "check-contained"]).exit_code == 0


def test_init_script_wires_the_containment_gate():
    """The verb is only a guard if the documented direct path actually calls it.

    A refusal nothing invokes is the decorative-guard failure this whole task is
    about, so assert the wiring, not just the verb.
    """
    from pathlib import Path as _P

    from fno.paths import resolve_plugin_script

    script = _P(resolve_plugin_script("hooks/helpers/init-target-state.sh"))
    text = script.read_text(encoding="utf-8")
    assert "fno do target check-contained" in text
    # rc 9 refuses; anything else must fall through rather than brick bootstrap.
    assert '"$_CT_RC" -eq 9' in text




def test_contained_nodes_naming_different_owners_stay_ambiguous(tmp_path, monkeypatch):
    """Two owners means no single destination; do not pick one."""
    gp = tmp_path / "graph.json"
    seed_graph(gp, {"entries": [
        {"id": "x-261c", "plan_path": "/p/one.md", "contained_in": "x-6320"},
        {"id": "x-3f8d", "plan_path": "/p/one.md", "contained_in": "x-8a4f"},
    ]})
    monkeypatch.setattr("fno.paths.graph_json", lambda: gp)
    assert target_cli._resolve_dispatch_node(None, "/p/one.md") is None




def test_check_contained_reads_through_the_keeper(tmp_path, monkeypatch):
    """The ordering the flock used to totalize is structural now.

    decompose's commit is a keeper transaction and every read is served under
    the keeper's gate, so a reader sees pre-commit or post-commit, never a
    torn graph; the client exposes no raw flock pair to take. The gate still
    refuses a contained node through the new path.
    """
    import fno.graph.store as gs

    _contained_graph(tmp_path, monkeypatch)
    assert not hasattr(gs, "_acquire_flock"), "the raw flock helpers are retired"
    assert not hasattr(gs, "_release_flock")
    monkeypatch.setenv("TARGET_INPUT", "x-261c")

    result = runner.invoke(app, ["do", "target", "check-contained"])
    assert result.exit_code == 9, result.output


def test_check_contained_proceeds_when_the_store_is_unavailable(tmp_path,
                                                                monkeypatch):
    """An unreachable store must not block every dispatch.

    Fail-open matches the rest of this gate: a refused store is a broken gate,
    and the pre-claim check plus decompose's own live-claim refusal still
    stand.
    """
    import fno.graph.store as gs

    _contained_graph(tmp_path, monkeypatch)

    def boom(*a, **k):
        raise gs.StoreUnavailable("unreachable", "no store for you")

    monkeypatch.setattr(target_cli, "_resolve_dispatch_node", boom)
    monkeypatch.setenv("TARGET_INPUT", "x-261c")
    # The check did not run; the bootstrap proceeds (exit 0), never exit 9.
    result = runner.invoke(app, ["do", "target", "check-contained"])
    assert result.exit_code == 0, result.output




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


def test_resolve_owned_identity_verb_proven_claude_wins_foreign_codex_refused(
    tmp_path, monkeypatch
) -> None:
    """AC1-HP + AC3-ERR at the verb: the proven claude marker wins and the
    foreign codex marker is refused by the walk (it contradicts the prover),
    with no registry collision consulted - a session with no stamp resolves by
    elimination and contradiction only, because colliding there read a
    hand-started joined session's OWN registered row as contention and refused
    every role grantor. Collision reporting lives on the stamped branches.

    Proven in Python so it does not depend on the PATH-resolved `fno` carrying
    the verb - the bash hook test's CI limitation, since the PR's own CI runs a
    `fno` that predates the verb. The prover is pinned to claude (a real claude
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


def test_resolve_owned_identity_absent_registry_degrades(tmp_path, monkeypatch) -> None:
    """AC8-EDGE: no registry at all means no collision evidence; the verb
    answers exactly as before (ambiguous, nothing named) and never blocks."""
    from fno.paths_testing import use_tmpdir

    use_tmpdir(monkeypatch, tmp_path)
    monkeypatch.setattr(
        "fno.claims.session_pid.resolve_session_harness",
        lambda from_pid=None: None,
    )
    monkeypatch.setenv("FNO_HARNESS_NAME", "codex")
    monkeypatch.setenv("FNO_HARNESS_SESSION_ID", "019fc88e-cccc-7c90-926a-6bdd7ebb186c")

    result = runner.invoke(app, ["do", "target", "resolve-owned-identity"])
    assert result.exit_code == 0, result.output
    fields = {
        line.split("=", 1)[0]: line.split("=", 1)[1]
        for line in result.stdout.splitlines()
        if "=" in line
    }
    assert fields["DISPOSITION"] == "ambiguous"
    assert fields["COLLISION"] == ""


def test_force_supersede_dispatch_node_records_its_reason(monkeypatch):
    """The retro force-closer closes with a note, not a bare force.

    The store refuses an evidence-less close, so the reason must ride as
    the completion note for an addressed retro node to close at all.
    """
    calls = []

    def _run(cmd, **kwargs):
        calls.append(list(cmd))
        return SimpleNamespace(returncode=0)

    monkeypatch.setattr(target_cli.subprocess, "run", _run)
    assert target_cli._force_supersede_dispatch_node("x-retro", "addressed upstream")
    assert calls == [
        ["fno", "backlog", "done", "x-retro", "--note", "addressed upstream"]
    ]
