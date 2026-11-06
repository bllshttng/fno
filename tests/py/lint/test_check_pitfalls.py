"""Regression tests for scripts/ci/check-pitfalls.sh.

The gate parses AGENTS.md's `## Pitfalls corpus (capped)` section and fails on
an over-cap corpus, a missing field, a stale entry, or a title that names a
shipped `fno` command as the trap. Output is captured via subprocess (not piped
through a tee) so the asserted returncode is the real one.

The resolution pass is tested too: a `graduates-to:` value must resolve to a
filed backlog node (by id, or by title after normalization), an unreadable
graph fails loud, and an absent graph is the legitimate skip. Fixture runs pin
`FNO_GRAPH_JSON` to an absent path so the operator's live graph never decides
a test.
"""
from fixtures.graph_seed import seed_graph
import os
import shutil
import subprocess
import tempfile
from datetime import date
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
LINT = ROOT / "scripts" / "ci" / "check-pitfalls.sh"
AGENTS = ROOT / "AGENTS.md"

SECTION = "## Pitfalls corpus (capped)"
NEXT_HEADING = "## Repository"


def _run(
    target: Path, *, lint: Path = LINT, env_extra: dict | None = None
) -> subprocess.CompletedProcess:
    # Fixture corpora name no real nodes, so resolution must not consult the
    # operator's live graph: pin an empty state root with an absent store.
    with tempfile.TemporaryDirectory(prefix="fno-pitfalls-test-") as temp_dir:
        config_dir = Path(temp_dir) if target.resolve() == AGENTS.resolve() else target.parent
        state = config_dir / "state"
        config = config_dir / "config.toml"
        config.write_text(f'state_dir = "{state}"\n', encoding="utf-8")
        env = {
            **os.environ,
            "FNO_CONFIG": str(config),
            "FNO_GRAPH_JSON": str(state / "graph.json"),
        }
        if env_extra:
            env.update(env_extra)
        return subprocess.run(
            ["bash", str(lint), str(target)],
            capture_output=True,
            text=True,
            env=env,
        )


def _fixture(tmp_path: Path, entries):
    body = [f"# AGENTS\n\n{SECTION}\n\nrationale.\n\n"]
    for trap, grad, added in entries:
        body.append(
            f"### {trap}\n\n{trap} statement.\n\n"
            f"- graduates-to: {grad}\n- added: {added}\n\n"
        )
    body.append(f"{NEXT_HEADING}\n")
    path = tmp_path / "agents.md"
    path.write_text("".join(body), encoding="utf-8")
    return path


FRESH = date.today().isoformat()
GOOD = ("A trap", "a lint", FRESH)


def _copied_lint_repo(tmp_path: Path, entries) -> tuple[Path, Path]:
    """A fixture repo carrying its own copy of the lint plus a stub budget."""
    repo = tmp_path / "repo"
    (repo / "scripts" / "ci").mkdir(parents=True)
    lint = repo / "scripts" / "ci" / "check-pitfalls.sh"
    shutil.copy(LINT, lint)
    body = [f"# AGENTS\n\n{SECTION}\n\nrationale.\n\n"]
    for trap, grad, added in entries:
        body.append(
            f"### {trap}\n\n{trap} statement.\n\n"
            f"- graduates-to: {grad}\n- added: {added}\n\n"
        )
    body.append(f"{NEXT_HEADING}\n")
    (repo / "AGENTS.md").write_text("".join(body), encoding="utf-8")
    stub = repo / "scripts" / "ci" / "check-preamble-budget.sh"
    return lint, stub


def test_unreadable_verdict_and_over_cap_report_together(tmp_path: Path) -> None:
    """AC6-ERR: one run names both the byte error and the corpus violation."""
    lint, stub = _copied_lint_repo(tmp_path, [GOOD] * 11)
    stub.write_text('#!/usr/bin/env bash\necho "garbage"\n')

    r = _run(lint.parent.parent.parent / "AGENTS.md", lint=lint)

    assert r.returncode == 1
    assert "could not read a byte verdict" in r.stderr
    assert "exceed the 10-entry cap" in r.stderr


def _one_entry(tmp_path: Path, title: str, body: str = "trap.") -> Path:
    path = tmp_path / "agents.md"
    path.write_text(
        f"# AGENTS\n\n{SECTION}\n\nrationale.\n\n### {title}\n\n{body}\n\n"
        f"- graduates-to: a lint\n- added: {FRESH}\n\n{NEXT_HEADING}\n",
        encoding="utf-8",
    )
    return path


def test_title_naming_shipped_command_fails(tmp_path: Path) -> None:
    # The exact entry this check exists to prevent re-acquiring: a trap titled
    # for the command that already fixes it, so the command IS the graduating carrier.
    path = _one_entry(tmp_path, "`fno doctor test` can report a false green")
    r = _run(path)
    assert r.returncode == 1
    assert "fno doctor test" in r.stderr
    assert "carrier" in r.stderr


# --- the resolution pass: graduates-to must name a real filed node ----------


def _seeded_graph(tmp_path: Path) -> Path:
    graph = tmp_path / "graph.json"
    seed_graph(graph, '{"entries": [{"id": "x-1234", "title": "A real guard", "slug": "x-1234",'
        ' "type": "feature", "priority": "p2", "status": "idea"}]}')
    return graph


def test_graduates_to_resolving_to_no_node_fails(tmp_path: Path) -> None:
    graph = _seeded_graph(tmp_path)
    path = _fixture(tmp_path, [("A trap", "no such guard", FRESH)])
    r = _run(path, env_extra={"FNO_GRAPH_JSON": str(graph)})
    assert r.returncode == 1
    assert "resolves to no filed node" in r.stderr
