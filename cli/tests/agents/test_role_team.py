"""``fno agents team``: one read of every role and whether the graph agrees.

Agreement is a POSITIVE marker (AGENTS.md's positive-marker pitfall): a team
that shows no disagreements because it could not read anything must never
render the same as a healthy fleet. ``agree`` is ``None`` with a stated
reason on an unreadable graph - never ``True``, never ``False`` - and the
summary counts unknowns separately from disagreements.
"""
from __future__ import annotations
from tests.fixtures.graph_seed import seed_graph

import json
from pathlib import Path

import pytest

from fno.paths_testing import use_tmpdir


def _entry(name: str, **kw):
    from fno.agents.registry import AgentEntry

    harness = kw.pop("harness", "claude")
    kw.setdefault("cwd", "/w")
    kw.setdefault("harness_session_id", f"{name}-session")
    return AgentEntry(name=name, log_path="", harness=harness, **kw)


def _stub_project_resolver(monkeypatch, tmp_path) -> None:
    """Pin the native project resolver's door (the project-family port) to a stub binary:
    `_canonical_project` answers through one fno-agents round trip, so the
    fixture registry rides FNO_AGENTS_BIN instead of a pinned settings path.
    $4 is the name to resolve; every other spawn-overlay shape execs the real
    binary (the ladder door rides the same env pin)."""
    from fno.rust_binary import find_dev_binary, resolve_binary

    real = find_dev_binary() or resolve_binary()
    stub = tmp_path / "agents-stub"
    fallback = f'exec "{real}" "$@"\n' if real else "exit 1\n"
    stub.write_text(
        "#!/bin/sh\n"
        'if [ "$2" = "project" ]; then\n'
        '  if [ "$3" = "map" ]; then\n'
        "    echo '{\"alpha\":\"alpha\",\"a\":\"alpha\",\"beta\":\"beta\"}'; exit 0\n"
        "  fi\n"
        '  case "$4" in\n'
        "    alpha|a) echo alpha; exit 0;;\n"
        "    beta) echo beta; exit 0;;\n"
        "    *) exit 1;;\n"
        "  esac\n"
        "fi\n" + fallback,
        encoding="utf-8",
    )
    stub.chmod(0o755)
    monkeypatch.setenv("FNO_AGENTS_BIN", str(stub))


def _prepare(monkeypatch, tmp_path, rows, graph_entries=None) -> None:
    from fno import paths
    from fno.agents.registry import write_registry

    use_tmpdir(monkeypatch, tmp_path)
    write_registry(rows)
    _stub_project_resolver(monkeypatch, tmp_path)
    if graph_entries is not None:
        graph_path = paths.graph_json()
        graph_path.parent.mkdir(parents=True, exist_ok=True)
        seed_graph(graph_path, json.dumps({"entries": graph_entries}))


def test_a_role_over_a_real_live_epic_agrees(tmp_path: Path, monkeypatch) -> None:
    from fno.agents.team import gather_team

    _prepare(
        monkeypatch,
        tmp_path,
        [
            _entry(
                "lead",
                status="busy",
                role_level=2,
                role_scope="e-1",
                role_grantor="human",
            )
        ],
        graph_entries=[{"id": "e-1", "type": "epic", "project": "alpha", "status": "ready"}],
    )

    team = gather_team()

    assert team["roles"] == [
        {
            "holder": "lead",
            "level": 2,
            "scope": "e-1",
            "grantor": "human",
            "status": "busy",
            "agree": True,
            "reason": None,
            "manifest_path": None,
            "manifest_session": None,
            "role_source": "row",
        }
    ]
    s = team["summary"]
    assert (s["total"], s["disagreements"], s["unknowns"], s["splits"]) == (1, 0, 0, 0)
    assert s["manifest_only"] == 0


def test_unreadable_graph_answers_null_never_true_or_false(
    tmp_path: Path, monkeypatch
) -> None:
    """AC7-EDGE: every affected row reads agree=null with a stated reason, and
    the unknown count is tracked separately from disagreements."""
    from fno.agents.team import gather_team

    _prepare(
        monkeypatch,
        tmp_path,
        [
            _entry(
                "lead",
                status="busy",
                role_level=2,
                role_scope="e-1",
                role_grantor="human",
            )
        ],
    )
    monkeypatch.setattr(
        "fno.graph.api.read_entries",
        lambda *a, **k: (_ for _ in ()).throw(RuntimeError("unreadable")),
    )

    team = gather_team()

    row = team["roles"][0]
    assert row["agree"] is None
    assert row["reason"] is not None
    assert team["graph_readable"] is False
    s = team["summary"]
    assert (s["total"], s["disagreements"], s["unknowns"], s["splits"]) == (1, 0, 1, 0)
    assert s["manifest_only"] == 0


def test_a_role_over_a_set_of_epics_agrees_only_if_every_member_is_a_live_epic(
    tmp_path: Path, monkeypatch
) -> None:
    """Rung 2 stores a set; agreement must check every member, not the first -
    one dead member makes the whole role disagree."""
    from fno.agents.team import gather_team

    _prepare(
        monkeypatch,
        tmp_path,
        [
            _entry(
                "mux-lead",
                status="busy",
                role_level=2,
                role_scope="e-1,e-2",
                role_grantor="human",
            )
        ],
        graph_entries=[
            {"id": "e-1", "type": "epic", "project": "alpha", "status": "ready"},
            {"id": "e-2", "type": "epic", "project": "beta", "status": "done"},
        ],
    )

    team = gather_team()

    row = team["roles"][0]
    assert row["agree"] is False
    assert "e-2" in row["reason"]
    assert "done" in row["reason"]


def test_rivalry_is_reported_per_pair_never_per_merged_group(
    tmp_path: Path, monkeypatch, native_backlog_door
) -> None:
    """A rivals B over e-1, B rivals C over e-2, A and C share nothing. A
    merged group would claim three rows hold e-1,e-2; the truth is two
    rivalries, each naming its own pair and the members that pair shares."""
    from fno.agents.team import gather_team

    _prepare(
        monkeypatch,
        tmp_path,
        [
            _entry("lead-a", status="busy", role_level=2, role_scope="e-1"),
            _entry("lead-b", status="busy", role_level=2, role_scope="e-1,e-2"),
            _entry("lead-c", status="busy", role_level=2, role_scope="e-2"),
        ],
        graph_entries=[],
    )

    assert gather_team()["conflicts"] == [
        {"scope": "e-1", "holders": ["lead-a", "lead-b"]},
        {"scope": "e-2", "holders": ["lead-b", "lead-c"]},
    ]


def test_a_portfolio_and_its_project_lead_are_a_team_not_a_conflict(
    tmp_path: Path, monkeypatch, native_backlog_door
) -> None:
    """The ladder's documented shape: a portfolio lead's team IS project
    leads. Overlap-keyed conflicts would cry double-rule on every legitimate
    team, so rivalry is rung-aware - same rung overlaps, cross rung only
    names the same territory outright."""
    from fno.agents.team import gather_team

    _prepare(
        monkeypatch,
        tmp_path,
        [
            _entry("portfolio-lead", status="busy", role_level=0, role_scope="alpha,beta"),
            _entry("project-lead", status="idle", role_level=1, role_scope="alpha"),
        ],
        graph_entries=[],
    )

    assert gather_team()["conflicts"] == []


def test_render_team_table_names_scope_holder_and_agreement(
    tmp_path: Path, monkeypatch
) -> None:
    from fno.agents.team import render_team

    _prepare(
        monkeypatch,
        tmp_path,
        [
            _entry(
                "lead",
                status="busy",
                role_level=1,
                role_scope="alpha",
                role_grantor="human",
            )
        ],
        graph_entries=[],
    )

    text = render_team(as_json=False)

    assert "alpha" in text
    assert "lead" in text
    assert "team: 1 role, 0 disagreements, 0 unknowns" in text


# --- the manifest limb: manifest is the durable record, row the cache


def _door_passthrough() -> str:
    """Script prelude: hand the registry door to the real binary."""
    from fno import rust_binary

    real = str(rust_binary.find_dev_binary() or rust_binary.resolve_binary())
    return (
        "import os\n"
        "if 'registry-commit' in sys.argv:\n"
        f"    os.execv({real!r}, [{real!r}, *sys.argv[1:]])\n"
    )


def _stub_term_reader(monkeypatch, tmp_path: Path, payload: dict, orphans=None, sweep_fail=False) -> None:
    """Answer lead-state with a canned payload and org-vacancies with a
    canned array (test_role_team pins the RENDER, not the reader;
    lead_state.rs pins the comparison and the sweep). The sweep answer honors
    --held like the real binary (a held scope is filtered out) and can be
    made to fail, pinning the ran-marker."""
    import stat

    script = tmp_path / "fno-agents"
    body = (
        "#!/usr/bin/env python3\n"
        "import json, sys\n"
        + _door_passthrough()
        + f"TERM = {json.dumps(json.dumps(payload))}\n"
        f"FAIL = {repr(bool(sweep_fail))}\n"
        f"ORPHANS = {json.dumps(orphans or [])}\n"
        "if 'org-vacancies' not in sys.argv:\n"
        "    print(TERM, end='')\n"
        "    sys.exit(0)\n"
        "if FAIL:\n"
        "    sys.exit(1)\n"
        "held = {v for i, v in enumerate(sys.argv) if i and sys.argv[i-1] == '--held'}\n"
        "print(json.dumps([o for o in ORPHANS if o.get('scope') not in held]), end='')\n"
    )
    script.write_text(body, encoding="utf-8")
    script.chmod(script.stat().st_mode | stat.S_IEXEC)
    monkeypatch.setattr("fno.rust_binary.resolve_binary", lambda: script)


def _agreeing_reader(scope: str, session: str) -> dict:
    return {
        "promoted": True,
        "scope": scope,
        "shape": "pass",
        "manifest_session": session,
        "registry_session": session,
        "live": True,
        "split": False,
        "unknown_reason": None,
    }


@pytest.mark.dev_build
def test_team_names_the_manifest_and_role_source_per_scope(
    tmp_path: Path, monkeypatch
) -> None:
    """A promoted row with a matching promoted manifest renders source `both`."""
    import fno.lead.state as lead_state
    from fno.agents.team import gather_team
    from fno.paths import space_dir

    row = _entry(
        "lead",
        cwd=str(tmp_path),
        status="busy",
        role_level=2,
        role_scope="e-1",
        role_grantor="human",
    )
    _prepare(
        monkeypatch,
        tmp_path,
        [row],
        graph_entries=[{"id": "e-1", "type": "epic", "project": "alpha", "status": "ready"}],
    )
    manifest = space_dir(tmp_path) / "leads" / "e-1.md"
    lead_state.write_manifest(
        manifest,
        scope="e-1",
        harness_session_id="lead-session",
        owner_cwd=str(tmp_path),
        role_level=2,
        role_scope="e-1",
        role_grantor="human",
    )
    payload = _agreeing_reader("e-1", "lead-session")
    payload.update(role_on_manifest=True, manifest_path=str(manifest))
    _stub_term_reader(monkeypatch, tmp_path, payload)

    team = gather_team()

    entry = team["roles"][0]
    assert entry["role_source"] == "both"
    assert entry["manifest_session"] == "lead-session"
    assert entry["manifest_path"] == str(manifest)
    assert team["summary"]["splits"] == 0


@pytest.mark.dev_build
def test_a_split_role_counts_apart_from_disagreements_and_unknowns(
    tmp_path: Path, monkeypatch
) -> None:
    """Two stores naming different holders is a SPLIT, not a graph
    disagreement and not an unknown: the summary counts it separately."""
    from fno.agents.team import gather_team
    from fno.paths import space_dir
    import fno.lead.state as lead_state

    row = _entry(
        "lead",
        cwd=str(tmp_path),
        status="busy",
        role_level=2,
        role_scope="e-1",
        role_grantor="human",
    )
    _prepare(
        monkeypatch,
        tmp_path,
        [row],
        graph_entries=[{"id": "e-1", "type": "epic", "project": "alpha", "status": "ready"}],
    )
    lead_state.write_manifest(
        space_dir(tmp_path) / "leads" / "e-1.md",
        scope="e-1",
        harness_session_id="someone-else",
        owner_cwd=str(tmp_path),
        role_level=2,
        role_scope="e-1",
        role_grantor="human",
    )
    payload = _agreeing_reader("e-1", "lead-session")
    payload.update(manifest_session="someone-else", split=True)
    _stub_term_reader(monkeypatch, tmp_path, payload)

    team = gather_team()

    entry = team["roles"][0]
    assert entry["role_source"] == "split"
    # The graph limb still answers its own question; the split is not folded in.
    assert entry["agree"] is True
    assert team["summary"]["disagreements"] == 0
    assert team["summary"]["unknowns"] == 0
    assert team["summary"]["splits"] == 1


@pytest.mark.dev_build
def test_a_manifest_role_survives_its_project_having_no_rows_at_all(
    tmp_path: Path, monkeypatch
) -> None:
    """The vanished-row case this read exists for: the row is GONE, so no row
    names the project and no cwd can be derived. The sweep must key on the
    spaces root, not on rows, or the orphan role is invisible exactly when
    the fleet lost it."""
    import fno.lead.state as lead_state
    from fno.agents.team import gather_team
    from fno.paths import space_dir

    _prepare(monkeypatch, tmp_path, [], graph_entries=[])
    manifest = space_dir(tmp_path) / "leads" / "x-dede.md"
    lead_state.write_manifest(
        manifest,
        scope="x-dede",
        harness_session_id="gone-lead",
        owner_cwd=str(tmp_path),
        role_level=2,
        role_scope="x-dede",
        role_grantor="operator",
    )
    _stub_term_reader(
        monkeypatch, tmp_path, _agreeing_reader("x-dede", "gone-lead"),
        orphans=[{
            "scope": "x-dede", "level": 2, "grantor": "operator",
            "manifest_session": "gone-lead", "manifest_path": str(manifest),
        }],
    )

    team = gather_team()

    orphan = next(e for e in team["roles"] if e["scope"] == "x-dede")
    assert orphan["role_source"] == "manifest"
    assert orphan["manifest_path"] == str(manifest)
    assert orphan["manifest_session"] == "gone-lead"
    assert orphan["agree"] is None
    assert "no live registry row" in orphan["reason"]


@pytest.mark.dev_build
def test_total_counts_row_roles_only_so_the_census_keeps_its_arithmetic(
    tmp_path: Path, monkeypatch
) -> None:
    """doctor_lanes._census reads summary.total as a ROW count and computes
    workers = len(rows) - total; an orphan inside total subtracts a worker
    that still exists. manifest-only roles count in their own field."""
    import fno.lead.state as lead_state
    from fno.agents.team import gather_team
    from fno.paths import space_dir

    _prepare(
        monkeypatch, tmp_path, [_entry("plain-worker", cwd=str(tmp_path), status="busy")]
    )
    manifest = space_dir(tmp_path) / "leads" / "x-dede.md"
    lead_state.write_manifest(
        manifest, scope="x-dede", harness_session_id="gone-lead",
        owner_cwd=str(tmp_path), role_level=2, role_scope="x-dede",
    )
    _stub_term_reader(
        monkeypatch, tmp_path, _agreeing_reader("x-dede", "gone-lead"),
        orphans=[{"scope": "x-dede", "level": 2, "manifest_session": "gone-lead"}],
    )

    s = gather_team()["summary"]
    assert s["total"] == 0
    assert s["manifest_only"] == 1
    assert s["sweep_ran"] is True


def test_a_sweep_that_cannot_run_is_an_absence_never_zero_orphans(
    tmp_path: Path, monkeypatch
) -> None:
    """A stale binary without the org-vacancies command exits non-zero; reading
    that as [] would print a clean team. The ran marker must say it never ran."""
    from fno.agents.team import gather_team, render_team

    _prepare(
        monkeypatch,
        tmp_path,
        [_entry("lead", cwd=str(tmp_path), status="busy", role_level=1,
                role_scope="alpha", role_grantor="human")],
    )
    _stub_term_reader(
        monkeypatch, tmp_path, _agreeing_reader("alpha", "lead-session"),
        orphans=[{"scope": "x-dede", "level": 2}], sweep_fail=True,
    )

    team = gather_team()
    assert team["summary"]["sweep_ran"] is False
    assert team["summary"]["manifest_only"] == 0
    text = render_team(as_json=False)
    assert "orphan sweep did not run" in text


def test_unreadable_registry_nulls_the_team_rather_than_reporting_it_empty(
    tmp_path: Path, monkeypatch
) -> None:
    """The absence-lie one layer below the agreement verdict. Degrading a failed
    registry read to [] would print a healthy, empty team, so a caller gating
    on summary.disagreements == 0 passes on a read that saw nothing."""
    from fno.agents import team as team_mod
    from fno.agents.team import gather_team, render_team

    _prepare(monkeypatch, tmp_path, [], graph_entries=[])
    monkeypatch.setattr(
        team_mod,
        "load_registry",
        lambda *a, **k: (_ for _ in ()).throw(RuntimeError("truncated")),
        raising=False,
    )
    monkeypatch.setattr(
        "fno.agents.registry.load_registry",
        lambda *a, **k: (_ for _ in ()).throw(RuntimeError("truncated")),
    )

    team = gather_team()

    assert team["registry_readable"] is False
    assert team["roles"] is None
    # Every count is null, so no naive zero-gate can read this as healthy.
    assert team["summary"]["total"] is None
    assert team["summary"]["disagreements"] is None
    assert team["summary"]["unknowns"] is None
    text = render_team(as_json=False)
    assert "CANNOT READ" in text
    assert "nothing was checked" in text


def test_a_scope_with_no_level_is_surfaced_not_silently_dropped(
    tmp_path: Path, monkeypatch
) -> None:
    """The mirror corruption: a row carries a scope but no level. role_reading
    gates on role_label, which registry.py returns None whenever role_level
    is None regardless of role_scope - so this row would otherwise vanish
    from the team entirely: not counted, not flagged unknown, not flagged
    disagreeing. That is the absence-lie this module exists to prevent."""
    from fno.agents.team import gather_team

    _prepare(
        monkeypatch,
        tmp_path,
        [_entry("half-scope", status="busy", role_level=None, role_scope="alpha")],
        graph_entries=[],
    )

    team = gather_team()

    assert team["summary"]["total"] == 1
    assert team["roles"][0]["holder"] == "half-scope"
    assert team["roles"][0]["agree"] is False
    assert "half a role" in team["roles"][0]["reason"]
    assert team["summary"]["disagreements"] == 1


# --- the scope fold: the native fold, relayed onto the role rows


def _stub_team_fold(monkeypatch, tmp_path: Path, scope_nodes: dict, fail: bool = False) -> None:
    """A stub fno-agents binary that answers `org-fold` from a canned
    scope_nodes map and org-vacancies from an empty list (the fold tests pin
    the RELAY, not the fold computation; team_fold.rs pins that)."""
    import stat

    script = tmp_path / "fno-agents"
    body = (
        "#!/usr/bin/env python3\n"
        "import json, sys\n"
        + _door_passthrough()
        + f"NODES = {scope_nodes!r}\n"
        f"FAIL = {repr(bool(fail))}\n"
        "TERM = json.dumps({'promoted': True, 'scope': 'x', 'shape': 'pass',\n"
        "    'manifest_session': 's', 'registry_session': 's', 'live': True,\n"
        "    'split': False, 'unknown_reason': None, 'role_on_manifest': False})\n"
        "if 'org-fold' in sys.argv:\n"
        "    if FAIL:\n"
        "        sys.exit(3)\n"
        "    print(json.dumps({'scope_nodes': NODES}), end='')\n"
        "    sys.exit(0)\n"
        "if 'org-vacancies' in sys.argv:\n"
        "    print('[]', end='')\n"
        "    sys.exit(0)\n"
        "print(TERM, end='')\n"
    )
    script.write_text(body, encoding="utf-8")
    script.chmod(script.stat().st_mode | stat.S_IEXEC)
    monkeypatch.setattr("fno.rust_binary.resolve_binary", lambda: script)


def test_a_failed_fold_marks_roles_unresolved_never_empty(
    tmp_path: Path, monkeypatch
) -> None:
    """A fold that cannot run (stale binary, unreadable graph) is stated per
    role - the rows never render as empty scopes."""
    from fno.agents.team import fold_scope_nodes

    _stub_team_fold(monkeypatch, tmp_path, {}, fail=True)
    roles = [{"holder": "lead", "level": 2, "scope": "e-1"}]

    fold_scope_nodes(roles)

    sn = roles[0]["scope_nodes"]
    assert sn["status"] == "unresolved"
    assert "could not run" in sn["reason"]


def test_nodes_flag_folds_json_and_moves_no_existing_key(
    tmp_path: Path, monkeypatch
) -> None:
    """AC5-JSON: -n -J adds scope_nodes per role and nothing else moves, so a
    caller gating on summary or conflicts is unaffected."""
    from fno.agents.team import render_team

    _prepare(
        monkeypatch,
        tmp_path,
        [_entry("lead", status="busy", role_level=2, role_scope="e-1")],
        graph_entries=[{"id": "e-1", "type": "epic", "project": "alpha", "status": "ready"}],
    )
    _stub_team_fold(
        monkeypatch,
        tmp_path,
        {
            "e-1": {
                "status": "ok", "total": 1, "counts": {"ready": 1},
                "nodes": [{"id": "e-1", "slug": "", "status": "ready",
                           "worker": None, "pr_number": 7, "sessions": []}],
                "omitted": 0,
            }
        },
    )

    before = json.loads(render_team(as_json=True))
    after = json.loads(render_team(as_json=True, nodes=True))

    assert after["summary"] == before["summary"]
    assert after["conflicts"] == before["conflicts"]
    assert len(after["roles"]) == len(before["roles"])
    for plain, folded in zip(before["roles"], after["roles"]):
        plain_view = {k: v for k, v in folded.items() if k != "scope_nodes"}
        assert plain_view == plain
    sn = after["roles"][0]["scope_nodes"]
    assert sn["status"] == "ok"
    assert [r["pr_number"] for r in sn["nodes"]] == [7]


# --- the wake's agreement-free team


def test_an_agreement_free_team_skips_the_graph_and_reads_the_same_roles(
    tmp_path: Path, monkeypatch
) -> None:
    """AC2-HP: agree=False never touches the graph; holder, level and scope
    match the agreeing read, and the rung-2 role reads agree None."""
    from fno.agents import team as team_mod
    from fno.agents.team import gather_team

    rows = [
        _entry(
            "lead-a",
            status="busy",
            role_level=2,
            role_scope="e-1,e-2",
            role_grantor="human",
        ),
        _entry(
            "lead-b",
            status="busy",
            role_level=1,
            role_scope="alpha",
            role_grantor="human",
        ),
    ]
    _prepare(
        monkeypatch,
        tmp_path,
        rows,
        graph_entries=[
            {"id": "e-1", "type": "epic", "project": "alpha", "status": "ready"},
            {"id": "e-2", "type": "epic", "project": "alpha", "status": "ready"},
        ],
    )

    def _refuse():
        raise AssertionError("an agreement-free team must not read the graph")

    real_index = team_mod._graph_index
    monkeypatch.setattr(team_mod, "_graph_index", _refuse)
    free = gather_team(rows, agree=False)
    monkeypatch.setattr(team_mod, "_graph_index", real_index)
    full = gather_team(rows)

    by_free = {c["holder"]: c for c in free["roles"]}
    by_full = {c["holder"]: c for c in full["roles"]}
    for holder in ("lead-a", "lead-b"):
        assert (by_free[holder]["level"], by_free[holder]["scope"]) == (
            by_full[holder]["level"],
            by_full[holder]["scope"],
        )
    assert by_free["lead-a"]["agree"] is None
    assert by_full["lead-a"]["agree"] is True
    assert by_free["lead-b"]["agree"] is True
