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
from tests.fixtures.graph_seed import seed_graph
import os
import shutil
import subprocess
import tempfile
from datetime import date, timedelta
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
STALE = (date.today() - timedelta(days=90)).isoformat()
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


def test_failing_budget_refuses_even_when_line_parses(tmp_path: Path) -> None:
    """AC5-ERR: a valid line from a failing budget run is not a pass."""
    lint, stub = _copied_lint_repo(tmp_path, [GOOD])
    stub.write_text('#!/usr/bin/env bash\necho "preamble: 10 / 100 B"\nexit 1\n')

    r = _run(lint.parent.parent.parent / "AGENTS.md", lint=lint)

    assert r.returncode == 1
    assert "exited 1" in r.stderr
    assert "preamble: 10 / 100 B" in r.stderr


def test_unreadable_verdict_and_over_cap_report_together(tmp_path: Path) -> None:
    """AC6-ERR: one run names both the byte error and the corpus violation."""
    lint, stub = _copied_lint_repo(tmp_path, [GOOD] * 11)
    stub.write_text('#!/usr/bin/env bash\necho "garbage"\n')

    r = _run(lint.parent.parent.parent / "AGENTS.md", lint=lint)

    assert r.returncode == 1
    assert "could not read a byte verdict" in r.stderr
    assert "exceed the 10-entry cap" in r.stderr


def test_missing_added_fails(tmp_path: Path) -> None:
    path = tmp_path / "agents.md"
    path.write_text(
        f"# AGENTS\n\n{SECTION}\n\nrationale.\n\n### Bad\n\ntrap.\n\n- graduates-to: a lint\n\n{NEXT_HEADING}\n",
        encoding="utf-8",
    )
    r = _run(path)
    assert r.returncode == 1
    assert "missing an 'added:' field" in r.stderr


def test_missing_section_fails(tmp_path: Path) -> None:
    path = tmp_path / "agents.md"
    path.write_text("# AGENTS\n\n## Repository\n", encoding="utf-8")
    r = _run(path)
    assert r.returncode == 1
    assert "no" in r.stderr and SECTION in r.stderr


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


def test_title_with_pipe_parses_staleness(tmp_path: Path) -> None:
    # A `|` in the heading must not corrupt the staleness record (tab-delimited).
    path = tmp_path / "agents.md"
    path.write_text(
        f"# AGENTS\n\n{SECTION}\n\nrationale.\n\n### A | B trap\n\ntrap.\n\n"
        f"- graduates-to: a lint\n- added: {STALE}\n\n{NEXT_HEADING}\n",
        encoding="utf-8",
    )
    r = _run(path)
    assert r.returncode == 1
    assert "over the 60-day limit" in r.stderr
    assert "A | B trap" in r.stderr
    assert "unparseable" not in r.stderr


def _transplant(tmp_path: Path, registry_text: str | None) -> Path:
    """A copy of the gate whose sibling registry is ours.

    REGISTRY resolves from BASH_SOURCE, so moving the script moves the file it
    reads. That is what lets these two cases run without adding a test-only
    environment knob to the gate itself.
    """
    lint = tmp_path / "scripts" / "ci" / LINT.name
    lint.parent.mkdir(parents=True)
    lint.write_text(LINT.read_text(encoding="utf-8"), encoding="utf-8")
    if registry_text is not None:
        registry = tmp_path / "cli" / "src" / "fno" / "cli.py"
        registry.parent.mkdir(parents=True)
        registry.write_text(registry_text, encoding="utf-8")
    return lint


def test_a_registry_that_yields_no_commands_fails_loud(tmp_path: Path) -> None:
    """An empty command list makes every title match nothing.

    The gate would then pass on the exact entry it exists to reject, which is
    the absence trap it is written to enforce against. A PRESENT registry that
    parses to nothing is a broken read, not a CLI without commands.
    """
    target = _one_entry(tmp_path, "fno backlog silently drops a node")
    lint = _transplant(tmp_path, "# a registry with no lazy-subcommand rows\n")

    r = _run(target, lint=lint)
    assert r.returncode == 1
    assert "read 0 commands" in r.stderr
    assert "broken read" in r.stderr


# --- pinned claim-bearing qualifiers (x-f3be) -------------------------------
#
# A structural check counts headings, fields, dates and backticked tokens. A
# qualifier is none of those, so a prose diet can delete the word carrying the
# claim while every count still passes. These tests verify by DELETING the
# qualifier and asserting the gate fails, never by re-running the counts that
# already pass on that deletion.


def _pinned_lint_repo(tmp_path: Path, entries) -> Path:
    """A fixture repo whose lint carries an injected pin.

    Pins ride live entries, so the shipped table can be empty while the
    mechanism stays tested: the pin and its corpus travel together here.
    """
    lint, stub = _copied_lint_repo(tmp_path, entries)
    text = lint.read_text(encoding="utf-8")
    old = "PINNED_PHRASES=''"
    assert old in text, "shipped pin table shape changed; update this fixture"
    lint.write_text(
        text.replace(old, "PINNED_PHRASES=$'capability probe\\tmail probe'", 1),
        encoding="utf-8",
    )
    stub.write_text('#!/usr/bin/env bash\necho "preamble: 1 / 100 B"\nexit 0\n')
    return lint


def test_dropping_a_pinned_qualifier_fails(tmp_path: Path) -> None:
    """Broadened to "mail", it rejects a worker's own autonomous evidence."""
    lint = _pinned_lint_repo(tmp_path, [("capability probe", "a probe", FRESH)])
    r = _run(lint.parent.parent.parent / "AGENTS.md", lint=lint)
    assert r.returncode == 1
    assert "mail probe" in r.stderr


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


def test_an_unreadable_graph_fails_loud(tmp_path: Path) -> None:
    graph_db = tmp_path / "state" / "graph.db"
    graph_db.parent.mkdir(parents=True)
    graph_db.write_bytes(b"not sqlite")
    path = _fixture(tmp_path, [GOOD])
    r = _run(path)
    assert r.returncode == 1
    assert "cannot be read" in r.stderr


def test_a_shallow_clone_keeps_the_prose_date(tmp_path: Path) -> None:
    # CI checks out one commit, so every title reads as first appearing in it
    # and the git date is always today. Trusting it there makes the 60-day
    # test inert: a stale entry passes. A shallow clone is no history, which
    # the gate already answers by keeping the prose date.
    origin = tmp_path / "origin"
    lint = _transplant(origin, None)
    path = origin / "agents.md"
    path.write_text(
        f"# AGENTS\n\n{SECTION}\n\nrationale.\n\n### Old trap\n\ntrap.\n\n"
        f"- graduates-to: a lint\n- added: {STALE}\n\n{NEXT_HEADING}\n",
        encoding="utf-8",
    )
    for args in (
        ["git", "init", str(origin)],
        ["git", "-C", str(origin), "add", "-A"],
        [
            "git",
            "-C",
            str(origin),
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-m",
            "corpus",
        ],
        ["git", "clone", "--depth", "1", f"file://{origin}", str(tmp_path / "clone")],
    ):
        step = subprocess.run(args, capture_output=True, text=True)
        assert step.returncode == 0, step.stderr

    clone = tmp_path / "clone"
    assert _run(path, lint=lint).returncode == 0, "the deep original still passes"
    r = _run(clone / "agents.md", lint=clone / "scripts" / "ci" / LINT.name)
    assert r.returncode == 1
    assert "over the 60-day limit" in r.stderr
    assert "the git date decides staleness" not in r.stderr


def test_an_unanswerable_shallow_probe_keeps_the_prose_date(tmp_path: Path) -> None:
    # git older than 2.15 has no --is-shallow-repository, and the probe can
    # fail for other reasons. An empty answer is not "deep": trusting it there
    # restores the collapsed-date bug the guard exists to close.
    origin = tmp_path / "origin"
    lint = _transplant(origin, None)
    path = origin / "agents.md"
    path.write_text(
        f"# AGENTS\n\n{SECTION}\n\nrationale.\n\n### Old trap\n\ntrap.\n\n"
        f"- graduates-to: a lint\n- added: {STALE}\n\n{NEXT_HEADING}\n",
        encoding="utf-8",
    )
    for args in (
        ["git", "init", str(origin)],
        ["git", "-C", str(origin), "add", "-A"],
        [
            "git",
            "-C",
            str(origin),
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-m",
            "corpus",
        ],
    ):
        step = subprocess.run(args, capture_output=True, text=True)
        assert step.returncode == 0, step.stderr

    # A deep clone whose depth probe fails: every other git call still works.
    real_git = shutil.which("git")
    assert real_git
    stub_dir = tmp_path / "stub"
    stub_dir.mkdir()
    stub = stub_dir / "git"
    stub.write_text(
        '#!/bin/sh\nfor a in "$@"; do\n'
        '  [ "$a" = "--is-shallow-repository" ] && exit 128\n'
        f'done\nexec {real_git} "$@"\n',
        encoding="utf-8",
    )
    stub.chmod(0o755)

    r = _run(
        path,
        lint=lint,
        env_extra={"PATH": f"{stub_dir}{os.pathsep}{os.environ['PATH']}"},
    )
    assert r.returncode == 1
    assert "over the 60-day limit" in r.stderr
    assert "the git date decides staleness" not in r.stderr
