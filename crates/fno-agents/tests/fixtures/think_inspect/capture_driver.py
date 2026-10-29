"""Record the think-inspect goldens from the Python leg.

Run from the repo root so the WORKTREE source is imported:

    PYTHONPATH=cli/src python crates/fno-agents/tests/fixtures/think_inspect/capture_driver.py

Every case is hermetic: graph rows, command answers, decision rows, and the
codemap space are injected or stubbed, and every file a case reads lives in
a fresh temp dir whose path is normalized to ``<TMP>``.
"""
from __future__ import annotations

import json
import os
import shutil
import sys
import tempfile
from pathlib import Path

import fno.paths
import fno.rust_binary
import fno.think_inspect as ti

OUT = Path(__file__).resolve().parents[2] / "golden" / "think_inspect" / "pure.json"
CASES: list[dict] = []


def record(fn: str, inp, out) -> None:
    CASES.append({"fn": fn, "input": inp, "output": out})


class Tmp:
    def __enter__(self) -> Path:
        self.path = Path(tempfile.mkdtemp()).resolve()
        return self.path

    def __exit__(self, *exc) -> None:
        shutil.rmtree(self.path, ignore_errors=True)


def normalized(value, tmp: Path):
    return json.loads(json.dumps(value).replace(str(tmp), "<TMP>"))


def write_files(root: Path, files: dict) -> None:
    for rel, body in files.items():
        path = root / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(body, encoding="utf-8")


def set_mtimes(root: Path, artifact: Path, mtimes: dict) -> None:
    for rel, secs in mtimes.items():
        path = artifact if rel == "@artifact" else root / rel
        os.utime(path, (secs, secs))


class Patch:
    """Attribute patches undone on exit."""

    def __init__(self, *pairs):
        self.pairs = pairs

    def __enter__(self):
        self.saved = [(obj, name, getattr(obj, name)) for obj, name, _ in self.pairs]
        for obj, name, value in self.pairs:
            setattr(obj, name, value)

    def __exit__(self, *exc):
        for obj, name, value in self.saved:
            setattr(obj, name, value)


def command_stub(commands: dict, seen: dict):
    def _command(run, argv, repo):
        key = " ".join(argv[:3])
        if argv[:3] == ["gh", "pr", "list"]:
            seen["gh_search"] = argv[argv.index("--search") + 1]
        out, err = commands.get(key, [None, "unstubbed"])
        return out, err

    return _command


def decisions_stub(decisions):
    def call_front_json(payload, timeout=None):
        if isinstance(decisions, dict) and "raise" in decisions:
            raise RuntimeError(decisions["raise"])
        return {"decisions": decisions}

    return call_front_json


# -- node_summary ------------------------------------------------------------

SUMMARY_ENTRIES = [
    ({"id": "x-948e", "status": "ready", "superseded_by": "x-579e"}, False),
    ({"id": "x-948e", "status": "ready", "superseded_by": None}, False),
    ({"id": "a", "slug": "s", "title": "T", "status": "done", "superseded_by": "b",
      "deferred_kind": "expired", "domain": "code", "project": "p", "parent": "e",
      "plan_path": "/p.md", "pr_number": 7, "pr_url": "https://x/7",
      "additional_prs": [{"number": 8}], "merge_status": "merged",
      "details": "dropped", "type": "epic"}, False),
    ({"merge_status": "open", "pr_number": 0, "title": "", "id": "z"}, True),
    ({}, False),
    ({}, True),
    ({"id": "q", "additional_prs": [], "pr_url": None, "domain": False}, False),
]
for entry, archived in SUMMARY_ENTRIES:
    record("node_summary", {"entry": entry, "archived": archived},
           ti._node_summary(entry, archived=archived))


# -- repository --------------------------------------------------------------

REPO_CASES = [
    {"branch": ["feature/x\n", None], "head": ["abc123\n", None],
     "status": [" M skills/think/SKILL.md\n?? new.txt\nR  old.py -> new.py\n", None],
     "log": ["abc123\tPrior change\ndef456\tSecond\tTabbed\n", None]},
    {"branch": [None, "not a git repo"], "head": [None, "not a git repo"],
     "status": [None, "not a git repo"], "log": [None, "fatal: bad"]},
    {"branch": ["  \n", None], "head": ["", None], "status": ["", None], "log": ["", None]},
    {"branch": ["main", None], "head": [None, ""], "status": ["M a\r\n x\rab\n", None],
     "log": ["nosubject\n\n\tonlysubject\n", None]},
    {"branch": [None, "git not found"], "head": [None, "git timed out"],
     "status": [None, "git not found"], "log": [None, "git timed out"]},
]
for case in REPO_CASES:
    answers = {
        "git rev-parse --abbrev-ref": case["branch"],
        "git rev-parse HEAD": case["head"],
        "git status --porcelain": case["status"],
        "git log -5": case["log"],
    }
    with Patch((ti, "_command", command_stub(answers, {}))):
        out = ti._repository(Path("/r"), None)
    record("repository", dict(case, root="/r"), out)


# -- pull_requests -----------------------------------------------------------

PR_CASES = [
    [json.dumps([{"number": 608, "title": "Lean", "state": "MERGED", "url": "u"}]), None],
    [None, "gh not found"],
    [None, "HTTP 502"],
    ["", None],
    [None, ""],
    ['{"number": 1}', None],
    ["[]\n", None],
    ['[1, "two", null, {"a": 0.5}]', None],
    ["  [ ]  ", None],
]
for outcome in PR_CASES:
    with Patch((ti, "_command", command_stub({"gh pr list": outcome}, {}))):
        out = ti._pull_requests("seed", Path("/r"), None)
    record("pull_requests", {"stdout": outcome[0], "error": outcome[1]}, out)


# -- pitfall_headings --------------------------------------------------------

PITFALL_FILES = [
    {"AGENTS.md": "# Agents\n\n## Pitfalls corpus (capped)\n\n### Guards can lie\n\nTrap.\n\n"
                  "- added: 2026-07-25\n\n## Repository\n\n### Not a pitfall\n"},
    {"AGENTS.md": "# Agents\n\nNo corpus here.\n",
     "CLAUDE.md": "## Pitfalls corpus (capped)\n### One  \n###   Two words\n#### Deep\n"},
    {},
    {"AGENTS.md": "## Pitfalls corpus (capped)\n\nNo entries today.\n"},
    {"AGENTS.md": "## Pitfalls corpus (capped)\n### First\n## #hashed heading\n### Still in\n"
                  "## Next\n### Out\n"},
    {"AGENTS.md": "## Pitfalls corpus (capped)\r\n### Windows line\r\n## End\r\n### Out\r\n"},
    {"CLAUDE.md": "## Pitfalls corpus (capped)\n###\nWrapped heading\n### Tail"},
    {"AGENTS.md": "intro\n## Pitfalls corpus (capped)### Inline\n### Real\n## \n### After bare\n"},
]
for files in PITFALL_FILES:
    with Tmp() as tmp:
        write_files(tmp, files)
        source, headings = ti._pitfall_headings(tmp)
    record("pitfall_headings", {"files": files}, [source, headings])


# -- database ----------------------------------------------------------------

DB_CASES = [
    {"files": {"prisma/schema.prisma": "model User {}\n"},
     "artifact": "# Map\n\n## Database Schema\n", "mtimes": {}},
    {"files": {"drizzle.config.ts": "export default {}\n"}, "artifact": None, "mtimes": {}},
    {"files": {}, "artifact": None, "mtimes": {}},
    {"files": {}, "artifact": "# Map only\n", "mtimes": {}},
    {"files": {}, "artifact": "## Database Schema\n", "mtimes": {}},
    {"files": {".env": "DATABASE_URL=postgres://x\n  DIRECT_URL = y\nOTHER=1\n",
               ".env.local": "# POSTGRES_URL=commented\nSUPABASE_DB_URL=z\nDATABASE_URL=again\n",
               ".env.test": "NOTHING=here\n"},
     "artifact": None, "mtimes": {}},
    {"files": {"supabase/migrations/001.sql": "create table t();\n",
               "drizzle/meta/_journal.json": "{}"},
     "artifact": "## Database Schema\n",
     "mtimes": {"supabase/migrations/001.sql": 2000000000, "drizzle/meta/_journal.json": 1000,
                "@artifact": 1500000000}},
    {"files": {"prisma/schema.prisma": "model A {}\n"},
     "artifact": "## Database Schema\n",
     "mtimes": {"prisma/schema.prisma": 1000, "@artifact": 2000}},
    {"files": {".env.development": "DATABASE_URL=a\n"},
     "artifact": "x\n## Database Schema\n",
     "mtimes": {".env.development": 3000, "@artifact": 2000}},
    {"files": {"drizzle.config.mjs": "", "drizzle.config.cjs": "", "drizzle.config.js": ""},
     "artifact": None, "mtimes": {}},
]
for case in DB_CASES:
    with Tmp() as tmp:
        repo = tmp / "repo"
        repo.mkdir()
        space = tmp / "space"
        space.mkdir()
        write_files(repo, case["files"])
        artifact = space / "codemap.md"
        if case["artifact"] is not None:
            artifact.write_text(case["artifact"], encoding="utf-8")
        set_mtimes(repo, artifact, case["mtimes"])
        with Patch((fno.paths, "worktree_space_dir", lambda _root=None: space)):
            out = ti._database(repo)
    record("database", case, out)


# -- pitfalls ----------------------------------------------------------------

PITFALLS_CASES = [
    {"files": {"AGENTS.md": "## Pitfalls corpus (capped)\n### Guards can lie\n"},
     "plans": ["20260725-retro-synthesis-x-abcd.md", "plain.md"], "lessons": "{}\n{}\n"},
    {"files": {}, "plans": None, "lessons": None},
    {"files": {}, "plans": [], "lessons": "\n  \n{}\n"},
    {"files": {},
     "plans": ["20260101-retro-synthesis-a.md", "20260201-retro-synthesis-b.md",
               "20260301-retro-synthesis-c.md", "20260401-retro-synthesis-d.md",
               "20260501-retro-synthesis-e.md", "20260601-retro-synthesis-f.md",
               "retro-synthesis.md", "retro-synthesis.txt", "my-retro-synthesis-notes.md"],
     "lessons": "{}\r\n{}\r\n\r\n"},
]
for case in PITFALLS_CASES:
    with Tmp() as tmp:
        repo = tmp / "repo"
        repo.mkdir()
        write_files(repo, case["files"])
        plans = tmp / "plans"
        if case["plans"] is not None:
            plans.mkdir()
            for name in case["plans"]:
                (plans / name).write_text("# Retro\n", encoding="utf-8")
        home = tmp / "home"
        if case["lessons"] is not None:
            history = home / ".fno" / "history"
            history.mkdir(parents=True)
            (history / "lesson-candidates.jsonl").write_text(case["lessons"], encoding="utf-8")
        out = ti._pitfalls(repo, plans, home)
        record("pitfalls", case, normalized(out, tmp))


# -- graph_section -----------------------------------------------------------

LEAN = [
    {"id": "x-4007", "slug": "lean-think", "title": "Lean think discovery receipt",
     "details": "deterministic discovery for think", "status": "in_progress",
     "domain": "code", "parent": "x-7a93"},
    {"id": "x-1111", "slug": "think-discovery", "title": "Think deterministic discovery",
     "details": "receipt for think discovery", "status": "done", "domain": "code"},
    {"id": "x-7a93", "slug": "context-evolution", "title": "Lean think graph evolution",
     "details": "context and graph evolution epic", "status": "ready", "domain": "code",
     "type": "epic"},
]
ANIMALS = ["wombat", "burrow", "falcon", "heron", "ibex", "jaguar", "kraken", "lynx"]
WOMBAT = [
    {"id": "fam", "slug": "fam",
     "title": " ".join(ANIMALS) + " " + " ".join(f"filler{i:03d}" for i in range(1, 161)),
     "status": "ready", "domain": "code"},
] + [
    {"id": f"noise{n}", "slug": f"noise{n}",
     "title": " ".join(ANIMALS[lo:hi]) + f" noiseword{n}", "status": "ready", "domain": "code"}
    for n, (lo, hi) in enumerate([(0, 5), (1, 6), (2, 7), (3, 8), (0, 5)], start=1)
]
RICH = [
    {"id": "x-aaaa", "slug": "deploy-pipeline", "title": "Deploy pipeline flake fix",
     "details": "the deploy pipeline flakes on retry", "status": "ready", "domain": "infra",
     "parent": "x-eeee", "pr_number": 12, "pr_url": "https://github.com/o/r/pull/12",
     "superseded_by": None},
    {"id": "x-eeee", "slug": "deploy-epic", "title": "Deploy pipeline reliability",
     "details": "epic for deploy pipeline", "status": "ready", "domain": "infra",
     "type": "epic", "parent": "x-ffff"},
    {"id": "x-ffff", "slug": "root-epic", "title": "Platform deploy pipeline roadmap",
     "details": "root of deploy pipeline", "status": "ready", "domain": "infra", "type": "epic"},
    {"id": "x-bbbb", "slug": "deploy-retry", "title": "Deploy pipeline retry flake",
     "details": "pipeline flakes on retry deploy", "status": "ready", "domain": "infra"},
    {"id": "x-cccc", "slug": "deploy-old", "title": "Deploy pipeline flake old",
     "details": "deploy pipeline flake", "status": "superseded", "domain": "infra",
     "superseded_by": "x-bbbb"},
    {"id": "x-dddd", "slug": "deploy-deferred", "title": "Deploy pipeline flake deferred",
     "details": "deploy pipeline flakes on retry", "status": "deferred",
     "deferred_kind": "expired", "domain": "infra"},
    {"id": "x-9999", "slug": "retired-epic", "title": "Deploy pipeline flake epic retired",
     "details": "deploy pipeline", "status": "done", "domain": "infra", "type": "epic"},
    {"id": "ab-1234abcd", "slug": "legacy", "title": "Legacy deploy pipeline", "status": "done",
     "domain": "infra"},
]
ARCHIVE = [
    {"id": "x-0a0a", "slug": "archived-deploy", "title": "Deploy pipeline flake archived",
     "details": "deploy pipeline flakes on retry", "status": "done", "domain": "infra",
     "parent": "x-0b0b"},
    {"id": "x-0b0b", "slug": "archived-epic", "title": "Archived deploy pipeline epic",
     "details": "deploy pipeline", "status": "ready", "domain": "infra", "type": "epic"},
    {"id": "x-aaaa", "slug": "shadowed", "title": "Shadowed archive copy", "status": "done"},
]


def live_rows(n: int, **extra) -> list[dict]:
    return [
        dict({"decision_id": f"d-{i:08x}", "ts": f"2026-09-{i % 28 + 1:02d}T00:00:00Z",
              "lane": "law", "subject": "x-aaaa", "decision": f"ruling {i}",
              "lifecycle": "live", "superseded_by": None}, **extra)
        for i in range(n)
    ]


GRAPH_CASES = [
    {"seed": "x-4007", "entries": LEAN, "archive": [], "error": None, "archive_error": None,
     "decisions": []},
    {"seed": "LEAN-THINK", "entries": LEAN, "archive": [], "error": None, "archive_error": None,
     "decisions": []},
    {"seed": "  x-4007  ", "entries": LEAN, "archive": [], "error": None, "archive_error": None,
     "decisions": []},
    {"seed": "Lean think discovery receipt", "entries": LEAN, "archive": [], "error": None,
     "archive_error": None, "decisions": []},
    {"seed": "-x-4007", "entries": LEAN, "archive": [], "error": None, "archive_error": None,
     "decisions": []},
    {"seed": "", "entries": LEAN, "archive": [], "error": None, "archive_error": None,
     "decisions": []},
    {"seed": "x-4007", "entries": None, "archive": [], "error": "graph unreadable",
     "archive_error": None, "decisions": []},
    {"seed": "x-4007", "entries": None, "archive": [], "error": None, "archive_error": "boom",
     "decisions": []},
    {"seed": "x-4007", "entries": None, "archive": [], "error": "", "archive_error": "",
     "decisions": []},
    {"seed": " ".join(ANIMALS), "entries": WOMBAT, "archive": [], "error": None,
     "archive_error": None, "decisions": []},
    {"seed": "x-aaaa", "entries": RICH, "archive": ARCHIVE, "error": None,
     "archive_error": None, "decisions": live_rows(3)},
    {"seed": "deploy-pipeline", "entries": RICH, "archive": ARCHIVE, "error": None,
     "archive_error": "archive corrupt", "decisions": []},
    {"seed": "x-0a0a", "entries": RICH, "archive": ARCHIVE, "error": None,
     "archive_error": None, "decisions": []},
    {"seed": "1234abcd", "entries": RICH, "archive": [], "error": None, "archive_error": None,
     "decisions": []},
    {"seed": "deploy pipeline flake retry", "entries": RICH, "archive": ARCHIVE, "error": None,
     "archive_error": None, "decisions": []},
    {"seed": "zzz unrelated words", "entries": RICH, "archive": [], "error": None,
     "archive_error": None, "decisions": []},
    {"seed": "x-bbbb", "entries": RICH + ["junk", 3, {"slug": "no-id"}], "archive": [],
     "error": None, "archive_error": None, "decisions": {"raise": "index corrupt"}},
    {"seed": "x-bbbb", "entries": RICH, "archive": [], "error": None, "archive_error": None,
     "decisions": live_rows(25)},
    {"seed": "x-bbbb", "entries": RICH, "archive": [], "error": None, "archive_error": None,
     "decisions": live_rows(2) + live_rows(2, superseded_by="d-ffffffff")},
    {"seed": "x-bbbb", "entries": RICH, "archive": [], "error": None, "archive_error": None,
     "decisions": [
         {"decision_id": "d-62cd9d80", "ts": "2026-08-22T12:27:04Z", "lane": "coord",
          "subject": "x-bbbb", "decision": "CORRECTION, SUPERSEDES d-85352ee7. WITHDRAWN.",
          "supersedes": "d-85352ee7", "superseded_by": None},
         {"decision_id": "d-85352ee7", "ts": "2026-08-22T12:12:00Z", "lane": "coord",
          "subject": "x-bbbb", "decision": "HYBRID", "supersedes": None,
          "superseded_by": "d-62cd9d80"},
     ]},
    {"seed": "x-bbbb", "entries": RICH, "archive": [], "error": None, "archive_error": None,
     "decisions": [
         {"decision_id": "d-unknown", "lane": "coord", "subject": "x-bbbb",
          "decision": "coord ruling", "lifecycle": "unknown",
          "lifecycle_reason": "the graph could not be read (database is locked)"},
         {"decision_id": "d-unknown2", "lane": "coord", "subject": "x-bbbb",
          "decision": None, "lifecycle": "unknown"},
     ]},
    {"seed": "x-bbbb", "entries": RICH, "archive": [], "error": None, "archive_error": None,
     "decisions": [
         {"decision_id": "d-long", "decision": "w" * 250, "lifecycle": "live"},
         {"decision_id": "d-edge", "decision": "e" * 200},
         {"decision_id": "d-uni", "decision": "é" * 201 + "tail"},
         {"decision_id": "d-num", "decision": 42},
     ]},
]
for case in GRAPH_CASES:
    with Patch((fno.rust_binary, "call_front_json", decisions_stub(case["decisions"]))):
        out = ti._graph_section(
            case["seed"],
            case["entries"],
            case["archive"],
            case["error"],
            case["archive_error"],
            graph_path=None,
        )
    record("graph_section", case, out)


# -- build_receipt -----------------------------------------------------------

GIT_OK = {
    "git rev-parse --abbrev-ref": ["feature/x-4007\n", None],
    "git rev-parse HEAD": ["abc123def456\n", None],
    "git status --porcelain": [" M skills/think/SKILL.md\n", None],
    "git log -5": ["abc123\tPrior change\n", None],
}
GIT_DOWN = {
    "git rev-parse --abbrev-ref": [None, "not a git repo"],
    "git rev-parse HEAD": [None, "not a git repo"],
    "git status --porcelain": [None, "not a git repo"],
    "git log -5": [None, "not a git repo"],
}
REJECTING_PLAN = (
    "---\nclaims: x-aaaa\ntitle: T\nconsolidation:\n  outcome: proceed_alone\n"
    "  rejected:\n    - id: x-bbbb\n      reason: R\n---\n\n# T\n"
)
BUILD_CASES = [
    {"seed": "x-4007", "graph": {"entries": LEAN, "error": None}, "archive": [],
     "repo_files": {"prisma/schema.prisma": "model User {}\n",
                    "AGENTS.md": "# Agents\n\n## Pitfalls corpus (capped)\n\n### Guards can lie\n\n"
                                 "Trap.\n\n## Repository\n\n### Not a pitfall\n"},
     "artifact": "# Map\n\n## Database Schema\n\n### public.users\n",
     "plans": {"20260725-retro-synthesis-x-abcd.md": "# Retro\n"}, "lessons": "{}\n{}\n",
     "commands": dict(GIT_OK, **{"gh pr list": [json.dumps(
         [{"number": 608, "title": "Lean planning", "state": "MERGED",
           "url": "https://example.test/608"}]), None]}),
     "decisions": []},
    {"seed": "new database feature", "graph": {"entries": None, "error": "graph unreadable"},
     "archive": [], "repo_files": {"drizzle.config.ts": "export default {}\n"},
     "artifact": None, "plans": None, "lessons": None,
     "commands": dict(GIT_DOWN, **{"gh pr list": [None, "gh not found"]}), "decisions": []},
    {"seed": "x-bbbb", "graph": {"entries": [
        {"id": "x-bbbb", "slug": "ruled-out", "title": "Ruled out node",
         "details": "the node a sibling plan rejected", "status": "ready", "domain": "code"}],
        "error": None},
     "archive": [], "repo_files": {}, "artifact": None,
     "plans": {"plan-x-aaaa.md": REJECTING_PLAN, "other.md": "---\ntitle: x-bbbb only\n---\n",
               "broken.md": "---\nx-bbbb: [unclosed\n---\n"},
     "lessons": None,
     "commands": dict(GIT_OK, **{"gh pr list": ["[]", None]}),
     "decisions": [{"decision_id": "d-1", "ts": "t", "lane": "law", "subject": "x-bbbb",
                    "decision": "keep", "lifecycle": "live", "superseded_by": None}]},
    {"seed": "x-38d3", "graph": {"entries": [
        {"id": "x-38d3", "slug": "spike-keep-fno-mail", "title": "SPIKE", "status": "done",
         "domain": "code"},
        {"id": "x-953b", "slug": "no-decisions", "title": "No decisions", "status": "ready",
         "domain": "code"}], "error": None},
     "archive": [], "repo_files": {}, "artifact": None, "plans": None, "lessons": None,
     "commands": dict(GIT_DOWN, **{"gh pr list": [None, "unavailable"]}),
     "decisions": {"raise": "index corrupt"}},
    {"seed": " ".join(ANIMALS), "graph": {"entries": WOMBAT, "error": None}, "archive": [],
     "repo_files": {"supabase/migrations/1.sql": "x"}, "artifact": "## Database Schema\n",
     "mtimes": {"supabase/migrations/1.sql": 2000000000, "@artifact": 1000},
     "plans": {}, "lessons": "",
     "commands": dict(GIT_OK, **{"gh pr list": ['{"not": "a list"}', None]}),
     "decisions": []},
    {"seed": "zzz nothing matches", "graph": {"entries": LEAN, "error": None},
     "archive": [], "repo_files": {}, "artifact": None, "plans": {}, "lessons": None,
     "commands": dict(GIT_OK, **{"gh pr list": ["[]", None]}), "decisions": []},
]
for case in BUILD_CASES:
    with Tmp() as tmp:
        repo = tmp / "repo"
        repo.mkdir()
        space = tmp / "space"
        space.mkdir()
        write_files(repo, case["repo_files"])
        artifact = space / "codemap.md"
        if case["artifact"] is not None:
            artifact.write_text(case["artifact"], encoding="utf-8")
        set_mtimes(repo, artifact, case.get("mtimes", {}))
        plans = tmp / "plans"
        if case["plans"] is not None:
            plans.mkdir()
            write_files(plans, case["plans"])
        home = tmp / "home"
        if case["lessons"] is not None:
            history = home / ".fno" / "history"
            history.mkdir(parents=True)
            (history / "lesson-candidates.jsonl").write_text(case["lessons"], encoding="utf-8")
        seen: dict = {}
        with Patch(
            (ti, "_command", command_stub(case["commands"], seen)),
            (fno.rust_binary, "call_front_json", decisions_stub(case["decisions"])),
            (fno.paths, "worktree_space_dir", lambda _root=None: space),
        ):
            receipt = ti.build_receipt(
                case["seed"],
                repo=repo,
                graph_entries=case["graph"]["entries"],
                graph_error=case["graph"]["error"],
                archive_entries=case["archive"],
                plans_path=plans,
                home=home,
            )
        out = {"receipt": receipt, "gh_search": seen.get("gh_search"),
               "render": ti.render_receipt(receipt)}
        record("build_receipt", case, normalized(out, tmp))


# -- render_receipt ----------------------------------------------------------

BASE = {
    "version": 1, "seed": "s", "complete": True,
    "repository": {"root": "/r", "status": "ok", "branch": "main", "head": "abc",
                   "dirty_paths": ["a", "b"], "recent_commits": [], "detail": None},
    "graph": {"status": "ok", "resolved": {"id": "x-1"}, "duplicates": [{}], "epic_candidates": [],
              "recall": {"lane": "node", "seed_tokens": 4}},
    "pull_requests": {"status": "ok", "matches": [{}, {}, {}], "detail": None},
    "database": {"detected": False, "signals": [], "schema_artifact": None,
                 "schema_status": "not-applicable"},
    "pitfalls": {"source": "AGENTS.md", "entries": ["a"], "retro_syntheses": [],
                 "lesson_candidates": 3},
    "warnings": [],
}
RENDER_CASES = [
    BASE,
    dict(BASE, complete=False, warnings=["one", "two"],
         repository=dict(BASE["repository"], branch=None, head="", status="error"),
         graph={"status": "error", "resolved": None, "duplicates": [], "epic_candidates": []},
         database=dict(BASE["database"], detected=True, schema_status="missing")),
    dict(BASE, graph=dict(BASE["graph"], resolved={}, recall={"lane": "seed", "seed_tokens": 0})),
    dict(BASE, graph=dict(BASE["graph"], resolved={"slug": "no-id"})),
]
for receipt in RENDER_CASES:
    record("render_receipt", receipt, ti.render_receipt(receipt))


# -- the command's missing-repository refusal --------------------------------

with Tmp() as tmp:
    from typer.testing import CliRunner

    from fno.provenance.cli import think_app

    missing = tmp / "missing"
    result = CliRunner().invoke(think_app, ["inspect", "idea", "--repo", str(missing)])
    record("cli_missing_repo", {"args": ["idea", "--repo", "<TMP>/missing"]},
           {"exit": result.exit_code,
            "stderr": normalized(result.output.strip(), tmp)})


OUT.parent.mkdir(parents=True, exist_ok=True)
OUT.write_text(json.dumps({"cases": CASES}, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
print(f"wrote {len(CASES)} cases to {OUT}", file=sys.stderr)
