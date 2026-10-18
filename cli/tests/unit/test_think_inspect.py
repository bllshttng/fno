from __future__ import annotations
from tests.fixtures.graph_seed import seed_graph

import json
import subprocess
from pathlib import Path

from typer.testing import CliRunner


def _result(argv: list[str], cwd: Path | None = None, timeout: int = 10):
    if argv[:2] == ["git", "rev-parse"]:
        value = "feature/x-4007\n" if "--abbrev-ref" in argv else "abc123def456\n"
        return subprocess.CompletedProcess(argv, 0, value, "")
    if argv[:2] == ["git", "status"]:
        return subprocess.CompletedProcess(argv, 0, " M skills/think/SKILL.md\n", "")
    if argv[:2] == ["git", "log"]:
        return subprocess.CompletedProcess(argv, 0, "abc123\tPrior change\n", "")
    if argv[:3] == ["gh", "pr", "list"]:
        assert argv[argv.index("--search") + 1] == "Lean think discovery receipt"
        payload = [
            {
                "number": 608,
                "title": "Lean planning",
                "state": "MERGED",
                "url": "https://example.test/608",
            }
        ]
        return subprocess.CompletedProcess(argv, 0, json.dumps(payload), "")
    raise AssertionError(f"unexpected argv: {argv}")


def test_receipt_collects_grounding_without_writes(tmp_path: Path, monkeypatch) -> None:
    from fno.paths import worktree_space_dir
    from fno.think_inspect import build_receipt

    monkeypatch.setenv("FNO_SPACES_DIR", str(tmp_path / "spaces"))
    repo = tmp_path / "repo"
    repo.mkdir()
    (repo / "prisma").mkdir()
    (repo / "prisma" / "schema.prisma").write_text("model User {}\n")
    artifact = worktree_space_dir(repo) / "codemap.md"
    artifact.parent.mkdir(parents=True)
    artifact.write_text("# Map\n\n## Database Schema\n\n### public.users\n")
    (repo / "AGENTS.md").write_text(
        "# Agents\n\n## Pitfalls corpus (capped)\n\n"
        "### Guards can lie\n\nTrap.\n\n- graduates-to: lint\n- added: 2026-07-25\n\n"
        "## Repository\n\n### Not a pitfall\n"
    )
    plans = tmp_path / "plans"
    plans.mkdir()
    retro = plans / "20260725-retro-synthesis-x-abcd.md"
    retro.write_text("# Retro\n")
    home = tmp_path / "home"
    (home / ".fno").mkdir(parents=True)
    (home / ".fno" / "history").mkdir(parents=True, exist_ok=True)
    (home / ".fno" / "history" / "lesson-candidates.jsonl").write_text("{}\n{}\n")
    graph = [
        {
            "id": "x-4007",
            "slug": "lean-think",
            "title": "Lean think discovery receipt",
            "details": "deterministic discovery for think",
            "status": "in_progress",
            "domain": "code",
            "parent": "x-7a93",
        },
        {
            "id": "x-1111",
            "slug": "think-discovery",
            "title": "Think deterministic discovery",
            "details": "receipt for think discovery",
            "status": "done",
            "domain": "code",
        },
        {
            "id": "x-7a93",
            "slug": "context-evolution",
            "title": "Lean think graph evolution",
            "details": "context and graph evolution epic",
            "status": "ready",
            "domain": "code",
            "type": "epic",
        },
    ]
    before = {
        p.relative_to(tmp_path): p.stat().st_mtime_ns for p in tmp_path.rglob("*") if p.is_file()
    }

    receipt = build_receipt(
        "x-4007",
        repo=repo,
        graph_entries=graph,
        archive_entries=[],
        plans_path=plans,
        home=home,
        run=_result,
    )

    after = {
        p.relative_to(tmp_path): p.stat().st_mtime_ns for p in tmp_path.rglob("*") if p.is_file()
    }
    assert after == before
    assert receipt["version"] == 1
    assert receipt["repository"]["branch"] == "feature/x-4007"
    assert receipt["repository"]["dirty_paths"] == ["skills/think/SKILL.md"]
    assert receipt["graph"]["resolved"]["id"] == "x-4007"
    assert receipt["graph"]["parent"]["id"] == "x-7a93"
    assert [row["id"] for row in receipt["graph"]["duplicates"]] == ["x-1111"]
    assert receipt["graph"]["epic_candidates"][0]["id"] == "x-7a93"
    assert receipt["pull_requests"]["status"] == "ok"
    assert receipt["pull_requests"]["matches"][0]["number"] == 608
    assert receipt["database"] == {
        "detected": True,
        "signals": ["prisma/schema.prisma"],
        "schema_artifact": "codemap.md",
        "schema_status": "grounded",
    }
    assert receipt["pitfalls"]["entries"] == ["Guards can lie"]
    assert receipt["pitfalls"]["retro_syntheses"] == [str(retro)]
    assert receipt["pitfalls"]["lesson_candidates"] == 2
    # The injected pool has no graph file behind it, so the fts lane of the
    # duplicate recall did not run and the receipt refuses to read complete.
    assert receipt["complete"] is False
    assert any("without the fts lane" in w for w in receipt["warnings"])


def test_receipt_exposes_unavailable_evidence(tmp_path: Path) -> None:
    from fno.think_inspect import build_receipt

    repo = tmp_path / "repo"
    repo.mkdir()
    (repo / "drizzle.config.ts").write_text("export default {}\n")

    def unavailable(argv: list[str], cwd: Path | None = None, timeout: int = 10):
        if argv[0] == "gh":
            raise FileNotFoundError("gh")
        return subprocess.CompletedProcess(argv, 1, "", "not a git repo")

    receipt = build_receipt(
        "new database feature",
        repo=repo,
        graph_entries=None,
        graph_error="graph unreadable",
        archive_entries=[],
        plans_path=tmp_path / "missing-plans",
        home=tmp_path,
        run=unavailable,
    )

    assert receipt["graph"]["status"] == "error"
    assert receipt["pull_requests"] == {
        "status": "unavailable",
        "matches": [],
        "detail": "gh not found",
    }
    assert receipt["database"]["schema_status"] == "missing"
    assert receipt["complete"] is False
    assert "database schema evidence is missing" in receipt["warnings"]


def test_corrupt_graph_is_not_recast_as_empty(tmp_path: Path) -> None:
    from fno.think_inspect import _load_graph

    graph = tmp_path / "graph.json"
    graph.write_text("{not json")

    entries, error = _load_graph(graph)

    assert entries is None
    # The store import refuses an unreadable seed; the wording names the store,
    # not the old JSON parse.
    assert error and ("could not be read" in error or "not valid JSON" in error)


def _result_without_title_assertion(argv: list[str], cwd: Path | None = None, timeout: int = 10):
    if argv[:3] == ["gh", "pr", "list"]:
        return subprocess.CompletedProcess(argv, 0, "[]", "")
    return _result(argv, cwd, timeout)


# --- AC3-HP / AC4-EDGE / AC5-EDGE: seed lane recall and its honesty marker ---


def _seeded_graph_repo(tmp_path: Path, entries: list[dict]) -> tuple[Path, Path]:
    """A repo whose config points graph_json at a tmp graph file.

    build_receipt derives the graph path from settings; pointing the repo's
    own config at the tmp file is the supported override, and it keeps the
    fts lane live against bytes this test wrote instead of the real graph.
    """
    repo = tmp_path / "repo"
    (repo / ".fno").mkdir(parents=True)
    graph = tmp_path / "graph.json"
    seed_graph(graph, json.dumps({"entries": entries}) + "\n")
    (repo / ".fno" / "config.toml").write_text(f'state_dir = "{graph.parent}"\n', encoding="utf-8")
    return repo, graph


def _wombat_corpus() -> tuple[str, list[dict]]:
    """Seed + graph where only the fts lane can see the true family.

    fts.search is an implicit AND over every query token, so `fam` contains
    all eight seed words plus 160 fillers: raw Jaccard 8/168 = 0.048, below
    the 0.05 seed floor, so the relatedness lane drops it. Five noise nodes
    each share five seed words (5/9 = 0.556) and outrank fam on Jaccard
    alone. Without the fts lane fam is invisible; with it, the gate sees
    what the fold offer sees.
    """
    animals = ["wombat", "burrow", "falcon", "heron", "ibex", "jaguar", "kraken", "lynx"]
    seed = " ".join(animals)
    fam = {
        "id": "fam",
        "slug": "fam",
        "title": seed + " " + " ".join(f"filler{i:03d}" for i in range(1, 161)),
        "status": "ready",
        "domain": "code",
    }
    groups = [(0, 5), (1, 6), (2, 7), (3, 8), (0, 5)]
    noise = [
        {
            "id": f"noise{n}",
            "slug": f"noise{n}",
            "title": " ".join(animals[lo:hi]) + f" noiseword{n}",
            "status": "ready",
            "domain": "code",
        }
        for n, (lo, hi) in enumerate(groups, start=1)
    ]
    return seed, [fam, *noise]


def test_seed_lane_duplicates_carry_the_fts_lane(tmp_path: Path) -> None:
    from fno.think_inspect import build_receipt

    seed, entries = _wombat_corpus()
    repo, _graph = _seeded_graph_repo(tmp_path, entries)

    receipt = build_receipt(
        seed,
        repo=repo,
        plans_path=tmp_path / "missing-plans",
        home=tmp_path,
        run=_result_without_title_assertion,
    )

    rows = {row["id"]: row for row in receipt["graph"]["duplicates"]}
    assert "fam" in rows, f"fts-only family missing from duplicates: {sorted(rows)}"
    assert "fts" in rows["fam"]["lanes"]
    assert receipt["graph"]["recall"]["fts"] == "ok"


def test_cli_rejects_missing_repository(monkeypatch, tmp_path: Path) -> None:
    from fno.provenance.cli import think_app

    monkeypatch.setattr(
        "fno.think_inspect.build_receipt",
        lambda seed, repo: (_ for _ in ()).throw(AssertionError("must not inspect")),
    )
    missing = tmp_path / "missing"
    result = CliRunner().invoke(think_app, ["inspect", "idea", "--repo", str(missing)])

    assert result.exit_code == 2
    assert "repository directory not found" in result.output


# -- plan rulings on the receipt (x-6f98) --


def _rulings_run(argv: list[str], cwd: Path | None = None, timeout: int = 10):
    if argv[:2] == ["git", "rev-parse"]:
        value = "feature/x-4007\n" if "--abbrev-ref" in argv else "abc123def456\n"
        return subprocess.CompletedProcess(argv, 0, value, "")
    if argv[:2] == ["git", "status"]:
        return subprocess.CompletedProcess(argv, 0, "", "")
    if argv[:2] == ["git", "log"]:
        return subprocess.CompletedProcess(argv, 0, "abc123\tprior\n", "")
    if argv[:3] == ["gh", "pr", "list"]:
        return subprocess.CompletedProcess(argv, 0, "[]", "")
    raise AssertionError(f"unexpected argv: {argv}")


def _ruled_out_graph() -> list[dict]:
    return [
        {
            "id": "x-bbbb",
            "slug": "ruled-out",
            "title": "Ruled out node",
            "details": "the node a sibling plan rejected",
            "status": "ready",
            "domain": "code",
        },
    ]


def _rejecting_plan_text() -> str:
    return (
        "---\nclaims: x-aaaa\ntitle: T\n"
        "consolidation:\n"
        "  outcome: proceed_alone\n"
        "  rejected:\n"
        "    - id: x-bbbb\n"
        "      reason: R\n"
        "---\n\n# T\n"
    )


def test_receipt_carries_a_sibling_plan_rejection(tmp_path: Path) -> None:
    from fno.think_inspect import build_receipt

    repo = tmp_path / "repo"
    repo.mkdir()
    plans = tmp_path / "plans"
    plans.mkdir()
    plan = plans / "plan-x-aaaa.md"
    plan.write_text(_rejecting_plan_text(), encoding="utf-8")

    receipt = build_receipt(
        "x-bbbb",
        repo=repo,
        graph_entries=_ruled_out_graph(),
        archive_entries=[],
        plans_path=plans,
        home=tmp_path / "home",
        run=_rulings_run,
    )

    graph = receipt["graph"]
    assert graph["plan_rulings_status"] == "ok"
    assert len(graph["plan_rulings"]) == 1
    row = graph["plan_rulings"][0]
    assert row["by"] == ["x-aaaa"]
    assert row["plan_path"] == str(plan)
    assert row["reason"] == "R"
