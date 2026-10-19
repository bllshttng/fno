"""Phase 01 + 02a + 03: backlog sub-app + verb aliases + triage nesting.

`backlog` is the canonical graph sub-app (the deprecated `graph` top-level
alias was removed as a true duplicate). Verifies the `done` verb
works and the nested `triage` sub-app surface is present (the `intake`
verb answers at the native door; its adoption case lives in
`test_backlog_intake_door.py`).
"""
from __future__ import annotations
from tests.fixtures.graph_seed import seed_graph

import json
from pathlib import Path

import pytest
from typer.testing import CliRunner

from fno.cli import app
from tests.conftest import run_native_create

runner = CliRunner()


@pytest.fixture
def tmp_graph(tmp_path, monkeypatch) -> Path:
    """A fresh empty graph.json routed to tmp_path via monkeypatch."""
    g = tmp_path / "graph.json"
    seed_graph(g, '{"entries": []}\n')
    import fno.graph._constants as gc
    import fno.graph.store as gs

    monkeypatch.setattr(gc, "GRAPH_JSON", g)
    monkeypatch.setattr(gc, "GRAPH_MD", tmp_path / "graph.md")
    monkeypatch.setattr(gc, "GRAPH_ARCHIVE_JSON", tmp_path / "graph-archive.json")
    monkeypatch.setattr(gs, "GRAPH_JSON", g)
    # Seam readers resolve fno.paths.graph_json at call time; pin the
    # resolver to the same hermetic file (module-attr pins do not reach it).
    monkeypatch.setattr("fno.paths.graph_json", lambda: g)
    # The native read-backs resolve the store through FNO_CONFIG's state_dir.
    (tmp_path / "config.toml").write_text(f'state_dir = "{tmp_path}"\n')
    monkeypatch.setenv("FNO_CONFIG", str(tmp_path / "config.toml"))
    return g


def _native_backlog(*args: str) -> tuple[int, str, str]:
    import os as _os
    import subprocess as _sp

    from fno.rust_binary import find_dev_binary, resolve_binary

    binary = find_dev_binary() or resolve_binary()
    if binary is None:
        pytest.skip("no fno-agents dev build (cargo build -p fno-agents)")
    proc = _sp.run(
        [str(binary), "backlog", *args],
        capture_output=True,
        text=True,
        env={**_os.environ, "FNO_TRACKER_BACKEND": "graph"},
    )
    return proc.returncode, proc.stdout, proc.stderr


def _invoke(*args, input=None):
    argv = list(args)
    if "backlog" in argv:
        index = argv.index("backlog")
        if argv[index : index + 2] == ["backlog", "add"] and "--difficulty" not in argv:
            argv.extend(["--difficulty", "medium"])
    return runner.invoke(app, argv, input=input, catch_exceptions=False)


# ---------------------------------------------------------------------------
# Phase 02a - done verb
# ---------------------------------------------------------------------------


def test_ac1_hp_done_marks_node_completed(tmp_graph):
    """`fno backlog done <id>` sets completed_at and status derives to done."""
    add = run_native_create(tmp_graph, "add", "DoneTest", "--difficulty", "medium")
    assert add.exit_code == 0
    node_id = json.loads(add.stdout)["id"]

    r = _invoke("backlog", "done", node_id, "--note", "marks the node completed")
    assert r.exit_code == 0, r.output

    # Fetch and assert completed_at is set (the native read the door serves).
    get_code, get_out, get_err = _native_backlog("get", node_id)
    assert get_code == 0, get_err
    node = json.loads(get_out)
    assert node.get("completed_at"), "completed_at must be set"
    # status is derived by recompute_statuses; it may not be in the JSON
    # serialization but the completed_at presence is the canonical signal.


def test_ac4_err_done_rejects_invalid_id(tmp_graph):
    """`fno backlog done not-a-real-id` exits non-zero with a clear error."""
    r = _invoke("backlog", "done", "not-a-real-id")
    assert r.exit_code != 0, "done should reject invalid IDs"


# ---------------------------------------------------------------------------
# Phase 03 - triage sub-app surface
# ---------------------------------------------------------------------------


@pytest.mark.usefixtures("native_backlog_door")
def test_ac1_hp_triage_projects_empty_graph(tmp_graph):
    """`fno backlog triage projects` on an empty graph returns an empty projects list.

    Shape must be ``{"projects": [{"name", "pending_count"}, ...]}`` so the
    /triage skill's ``each`` iterator can read counts for its banner — a
    flat list would strip that context.
    """
    r = _invoke("backlog", "triage", "projects")
    assert r.exit_code == 0, r.output
    data = json.loads(r.stdout)
    assert data == {"projects": []}


# ---------------------------------------------------------------------------
# In-N-Out tiering: `fno backlog --help` advertises at most 12 verbs; the
# rest are hidden but invocable.
# ---------------------------------------------------------------------------

_ADVERTISED_BACKLOG_VERBS = {
    # `update` moved with the update port, `add`/`idea` with the create
    # port, `view` with the projections port, and `defer` with the
    # lifecycle port: the native binary advertises them, the python menu
    # does not.
    "get", "next", "done",
    "triage", "note",
}


def test_backlog_help_advertises_only_the_menu():
    """AC1-HP: `fno backlog --help` lists exactly the advertised verbs (<=12)."""
    import click
    import typer.main

    from fno.graph.cli import cli as backlog_app

    group = typer.main.get_command(backlog_app)
    ctx = click.Context(group)
    listed = [
        name
        for name in group.list_commands(ctx)
        if not (cmd := group.get_command(ctx, name)) or not cmd.hidden
    ]
    assert set(listed) == _ADVERTISED_BACKLOG_VERBS, (
        f"advertised backlog verbs drifted: {sorted(listed)}"
    )
    assert len(listed) <= 12


@pytest.mark.usefixtures("native_backlog_door")
def test_ac2_hp_triage_propose_dry_run_emits_template(tmp_graph):
    """`fno backlog triage propose --dry-run` emits a stub proposal."""
    r = _invoke("backlog", "triage", "propose", "--dry-run", "--all")
    assert r.exit_code == 0, r.output
    data = json.loads(r.stdout)
    for key in ("dependencies", "priority_changes", "duplicates"):
        assert key in data, f"proposal template missing {key!r}"


# ---------------------------------------------------------------------------
# Phase 03 - intake --project flag + settings-workspace warning
# ---------------------------------------------------------------------------


def test_intake_project_flag_overrides_frontmatter(tmp_graph, tmp_path):
    """--project beats frontmatter beats cwd inference."""
    plan = tmp_path / "plan-A.md"
    plan.write_text(
        "---\ncreated: 2026-05-05\nproject: from-frontmatter\n---\n# title\nbody\n\n## Files to Modify\n\n| File | Action |\n|---|---|\n| `cli/src/fno/example.py` | modify |\n"
    )
    r_code, r_out, r_err = _native_backlog(
        "intake", str(plan), "--project", "from-flag"
    )
    assert r_code == 0, r_err
    nodes = _read_store(tmp_graph)
    assert any(n.get("project") == "from-flag" for n in nodes), (
        f"expected node with project=from-flag, got: {[n.get('project') for n in nodes]}"
    )


def test_intake_warns_when_project_not_in_settings(tmp_graph, tmp_path, monkeypatch):
    """Resolved project not in any settings workspace -> stderr warning, exit 0."""
    plan = tmp_path / "plan-B.md"
    plan.write_text(
        "---\ncreated: 2026-05-05\nproject: brand-new-project\n---\n# title\n\n## Files to Modify\n\n| File | Action |\n|---|---|\n| `cli/src/fno/example.py` | modify |\n"
    )

    # Project-local settings declares only 'existing-project'
    settings = tmp_path / ".fno" / "settings.yaml"
    settings.parent.mkdir(parents=True, exist_ok=True)
    settings.write_text(
        "work:\n  workspaces:\n    main:\n      projects:\n"
        "        - name: existing-project\n          path: ~/code/existing-project\n"
    )
    # Isolate HOME so the user's real ~/.fno/settings.yaml is not consulted
    fake_home = tmp_path / "home"
    fake_home.mkdir()
    monkeypatch.setenv("HOME", str(fake_home))
    monkeypatch.chdir(tmp_path)

    r_code, r_out, r_err = _native_backlog("intake", str(plan))
    assert r_code == 0, r_err
    combined = r_out + r_err
    assert "brand-new-project" in combined, combined
    assert "not declared in any settings.yaml workspace" in combined, combined


# ---------------------------------------------------------------------------
# Phase 04 - cmd_update project= / cwd=, validator warn
# ---------------------------------------------------------------------------


def _read_store(g: Path) -> list[dict]:
    # The store owns state; graph.json is a frozen export, so read-backs
    # come from store rows.
    from fno.graph.store import read_graph_strict

    return read_graph_strict(g)


def _seed_node(tmp_graph: Path, node: dict) -> None:
    # The file is a frozen mirror; seeds go through the store write seam.
    from fno.graph.store import commit_rows_via_store

    commit_rows_via_store(tmp_graph, lambda rows: rows + [node])


def _native_update(tmp_graph: Path, *args: str):
    """The update leaf answers natively now; drive the dev binary over the
    same store the fixture seeded (in-process monkeypatches cannot reach a
    subprocess)."""
    import os as _os
    import subprocess as _sp

    from fno.rust_binary import find_dev_binary, resolve_binary

    binary = find_dev_binary() or resolve_binary()
    if binary is None:
        pytest.skip("no fno-agents dev build (cargo build -p fno-agents)")
    proc = _sp.run(
        [str(binary), "backlog", "update", *args],
        capture_output=True,
        text=True,
        timeout=60,
        env={
            "PATH": _os.environ["PATH"],
            "HOME": str(tmp_graph.parent),
            "FNO_STATE_DIR": str(tmp_graph.parent),
            "FNO_TRACKER_BACKEND": "graph",
        },
        cwd=str(tmp_graph.parent),
    )
    return proc.returncode, proc.stdout + proc.stderr


def test_cmd_update_empty_project_errors(tmp_graph):
    """project= '' is rejected with exit 1."""
    _seed_node(tmp_graph, {
        "id": "ab-bbbbbbbb", "project": "myvault", "cwd": "/old",
        "title": "z", "type": "feature",
    })
    code, out = _native_update(tmp_graph, "ab-bbbbbbbb", "project=")
    assert code == 1, out
    assert "must be a non-empty string" in out


def test_intake_routes_to_frontmatter_project_end_to_end(tmp_graph, tmp_path, monkeypatch):
    """Journey 1: shared-vault intake routes to frontmatter project regardless of cwd.

    Without --project, a plan whose frontmatter declares project: foo must
    land in graph.json with project=foo even when cwd resolves to a different
    git repo. This is the core bug the plan fixes.
    """
    # Plan declares project: from-frontmatter
    plan = tmp_path / "plan-shared-vault.md"
    plan.write_text(
        "---\ncreated: 2026-05-05\nproject: from-frontmatter\n---\n# title\nbody\n\n## Files to Modify\n\n| File | Action |\n|---|---|\n| `cli/src/fno/example.py` | modify |\n"
    )
    # Isolate from any settings.yaml that would emit a workspace warning
    fake_home = tmp_path / "home"
    fake_home.mkdir()
    monkeypatch.setenv("HOME", str(fake_home))

    r_code, r_out, r_err = _native_backlog("intake", str(plan))
    assert r_code == 0, r_err
    nodes = _read_store(tmp_graph)
    assert len(nodes) == 1
    assert nodes[0]["project"] == "from-frontmatter", (
        f"expected node project=from-frontmatter, got: {nodes[0].get('project')}"
    )
