#!/usr/bin/env python3
"""Capture driver for the `scoreboard` goldens.

The Python leg (`fno whoami scoreboard`, served by `fno.scoreboard.cli` over
`fno.scoreboard.fold`) is the parity oracle until the port deletes it. This
driver runs that leg through the same argv the native `fno-agents scoreboard`
door accepts, under pinned env and fixture trees both legs reproduce, and
freezes one golden triple per case (`<key>.exit` / `<key>.out` / `<key>.err`
when the case pins stderr) under `tests/golden/scoreboard/`. It also writes
the committed per-case fixture dirs under `tests/golden/scoreboard/cases/`
and `cases.json`, so the parity test runs the built binary against
byte-identical fixtures.

`scoreboard_parity.rs` is the lockstep consumer of the same files. Add a case
to BOTH or neither. After the deletion the driver is kept as provenance: a
bare run says the Python leg is gone.

Determinism contract, the whole reason the fixtures look like this:

- every ledger `completed` is a fixed past date and every case runs
  `--since 20000`, so the window holds capture and replay alike; the one
  out-of-window row sits at the 1970 epoch, outside any real window;
- `HOME`, `FNO_STATE_DIR`, `FNO_AGENTS_HOME` and `FNO_REPO_ROOT` pin the
  transcript scan, the state root, the registry and the skills git history;
- a stub `fno` on PATH answers the one call the fold makes by name
  (`fno mux stats --json`) with a fixed payload; a stub `gh` answers
  `gh pr diff --name-only` the same way. The store exec lane resolves the
  real binary by path, so the stubs never intercept it;
- the graph fixture is a minimal `entries` store the store folds on open,
  so the one classifier answers from identical rows on both legs.
"""

from __future__ import annotations

import json
import hashlib
import os
import shutil
import subprocess
import sys
from pathlib import Path

TESTS_DIR = Path(__file__).resolve().parents[2]
GOLDEN_DIR = TESTS_DIR / "golden" / "scoreboard"
FIXTURE_DIR = GOLDEN_DIR / "cases"

# Every prefix that can leak machine identity into the fold's env walks. The
# parity test scrubs the same set before pinning the child's env.
SCRUB_PREFIXES = (
    "FNO_",
    "TARGET_",
    "CLAUDE_",
    "CODEX_",
    "GEMINI_",
    "OPENCODE_",
    "ANTHROPIC_",
    "OPENAI_",
    "ZAI_",
    "GLM_",
)

STUB_FNO = """#!/bin/sh
# Answers the two by-name calls the fold makes: the mux stats snapshot, and
# the event store-path probe (whose answer names a store db that never
# exists, so both legs read the raw journal). Any other argv refuses loudly
# so a new by-name call cannot silently read live machine state in a golden.
if [ "$1" = "mux" ] && [ "$2" = "stats" ] && [ "$3" = "--json" ]; then
  printf '%s\\n' '{"touch_emit_failures": 2, "started_at": "2026-10-20T10:00:00Z", "measured_at": "2026-10-25T10:00:00Z"}'
  exit 0
fi
if [ "$1" = "doctor" ] && [ "$2" = "event" ] && [ "$3" = "rows" ]; then
  prev=""
  for a in "$@"; do
    if [ "$prev" = "--events" ]; then
      printf '%s\\n' "{\\"store\\": \\"$a.store.db\\"}"
      exit 0
    fi
    prev="$a"
  done
fi
if [ "$1" = "doctor" ] && [ "$2" = "event" ] && [ "$3" = "emit-envelope" ]; then
  # The store's judge is the one owner; the fixture's context_snapshot is
  # written to pass the real judge, so the stub's blanket pass agrees.
  cat > /dev/null
  exit 0
fi
echo "stub fno: unexpected argv: $*" >&2
exit 99
"""

SETTINGS_YAML = """config:
  obsidian:
    enabled: false
    vault: null
  paths:
    briefs_dir: null
    bus_dir: null
    cargo_targets_base: null
    chats: null
    evals_history: null
    fleet_dir: null
    handoffs_dir: null
    hook_logs_dir: null
    inbox_path: null
    ledger_json: null
    memory_dir: null
    observer_reports_dir: null
    operator_lane: null
    postmortems_dir: null
    retro_pending_dir: null
    spaces_dir: null
    worktrees_base: null
  plans_dir: .fno/plans/
  project:
    id: null
    vision: null
  state_dir: ~/.fno/
schema_version: 1
"""

STUB_GH = """#!/bin/sh
# Answers `gh pr diff <n> --name-only [--repo slug]` with a fixed file list.
for arg in "$@"; do
  if [ "$arg" = "--name-only" ]; then
    printf '%s\\n' 'cli/src/fno/scoreboard/fold.py' 'cli/src/fno/new_surface.py' 'migrations/0002_nodes.sql'
    exit 0
  fi
done
echo "stub gh: unexpected argv: $*" >&2
exit 99
"""


def _write(path: Path, body: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(body, encoding="utf-8")


def _git(repo: Path, *args: str, env: dict | None = None) -> None:
    subprocess.run(["git", *args], cwd=repo, check=True,
                   capture_output=True, env=env)


def build_case(label: str) -> Path:
    """Build one case's fixture tree and return its root."""
    root = FIXTURE_DIR / label
    if root.exists():
        shutil.rmtree(root)
    state = root / "state"
    home = root / "home"
    repo = root / "repo"
    for sub in (state / "db", state / "agents", home / ".claude" / "projects" / "-repo-x", repo):
        sub.mkdir(parents=True)
    # Pre-written so the first run never prints the fresh-install migration
    # line (a machine path) to stderr.
    _write(state / "settings.yaml", SETTINGS_YAML)

    # The skills git history the version resolver walks: one commit, pinned
    # dates, fixed content, so the short hash is stable everywhere.
    _write(repo / "skills" / "review" / "SKILL.md", "# review\n")
    git_env = dict(os.environ)
    git_env.update({
        "GIT_AUTHOR_NAME": "fixture", "GIT_AUTHOR_EMAIL": "fixture@example.com",
        "GIT_COMMITTER_NAME": "fixture", "GIT_COMMITTER_EMAIL": "fixture@example.com",
        "GIT_AUTHOR_DATE": "2026-06-01T00:00:00+00:00",
        "GIT_COMMITTER_DATE": "2026-06-01T00:00:00+00:00",
    })
    _git(repo, "init", "-q")
    _git(repo, "add", ".", env=git_env)
    _git(repo, "commit", "-q", "-m", "skills", env=git_env)

    # The plan docs, summary and PR body the plan-fidelity join reads.
    _write(repo / "plans" / "p1.md", """---
done_probes:
  - fno doctor test cli
---
# Plan p1

- AC1-HP: the thing ships.
- AC2-ERR: errors refuse.

## File Ownership Map

| file | action |
|---|---|
| `cli/src/fno/scoreboard/fold.py` | modify |
""")
    _write(repo / "plans" / "p2.md", "---\ndone_probes: []\n---\n# Plan p2\n\n- AC9-X: never joined.\n")
    _write(repo / "plans" / "SUMMARY-p1.md", """# Summary

- AC1-HP done. AC2-ERR done.
Deviation: one line.
""")

    _write(home / ".claude" / "projects" / "-repo-x" / "11111111-1111-1111-1111-111111111111.jsonl",
           json.dumps({"message": {"content": [{"type": "tool_use", "name": "Skill",
                                                "input": {"skill": "fno:review"}}]}}) + "\n")

    ledger = {"entries": [
        {"session_id": "s-ship", "type": "target", "completed": "2026-06-15T10:00:00",
         "termination_reason": "DonePRGreen", "graph_node_id": "x-1", "cost_usd": 5.0,
         "tokens_total": 1000, "duration_minutes": 30, "pr_number": 11,
         "pr_url": "https://github.com/ex/repo/pull/11",
         "summary_path": str(repo / "plans" / "SUMMARY-p1.md"),
         "plan_path": str(repo / "plans" / "p1.md"), "project": "ex/repo",
         "sessions": ["11111111-1111-1111-1111-111111111111"]},
        {"session_id": "s-doc", "type": "target", "completed": "2026-06-14T10:00:00",
         "termination_reason": "DoneAdvisory", "graph_node_id": "x-2", "cost_usd": 1.5},
        {"session_id": "s-wedge", "type": "target", "completed": "2026-06-13T10:00:00",
         "termination_reason": "NoProgress", "graph_node_id": "x-3", "cost_usd": 2.0},
        {"session_id": "s-bare", "type": "target", "completed": "2026-06-12T10:00:00",
         "cost_usd": 1.0},
        {"session_id": "s-old", "type": "target", "completed": "1970-01-01T00:00:00",
         "termination_reason": "DonePRGreen", "cost_usd": 99.0},
        {"session_id": "s-plan", "type": "plan", "completed": "2026-06-11T10:00:00",
         "termination_reason": "DonePlanned", "graph_node_id": "x-4", "cost_usd": 0.5,
         "plan_path": str(repo / "plans" / "p1.md"), "project": "ex/repo",
         "summary_path": str(repo / "plans" / "SUMMARY-p1.md"),
         "pr_number": 12, "pr_url": "https://github.com/ex/repo/pull/12",
         "phases_completed": ["think", "plan"]},
        {"session_id": "s-exec", "type": "execution", "completed": "2026-06-10T10:00:00",
         "termination_reason": "DonePRGreen", "graph_node_id": "x-1", "cost_usd": 3.0,
         "provider": "zai", "model": "glm-5", "effort": "high",
         "carveouts_filed": 1, "duration_minutes": 42},
        {"session_id": "s-delegated", "type": "target", "completed": "2026-06-09T10:00:00",
         "termination_reason": "delegated", "cost_usd": 0.25},
    ]}
    _write(state / "ledger.json", json.dumps(ledger, indent=1) + "\n")

    events = [
        {"ts": "2026-06-15T09:00:00Z", "type": "human_touch", "graph_node_id": "x-1"},
        {"ts": "2026-06-15T09:30:00Z", "type": "human_touch", "graph_node_id": "x-1"},
        {"type": "human_touch"},
        {"ts": "2026-06-15T08:00:00Z", "type": "verifier_verdict",
         "data": {"verdict": "pass", "graph_node_id": "x-1"}},
        {"ts": "2026-06-14T08:00:00Z", "type": "verifier_verdict",
         "data": {"verdict": "pass", "graph_node_id": "x-2"}},
        {"ts": "2026-06-13T08:00:00Z", "type": "verifier_verdict",
         "data": {"verdict": "concerns", "graph_node_id": "x-3"}},
        {"ts": "2026-06-12T08:00:00Z", "type": "verifier_verdict",
         "data": {"verdict": "pass", "graph_node_id": "x-9"}},
        {"ts": "2026-06-11T08:00:00Z", "type": "verifier_verdict",
         "data": {"verdict": "pass", "graph_node_id": "x-10"}},
        {"ts": "2026-06-10T08:00:00Z", "type": "verifier_verdict",
         "data": {"verdict": "pass", "graph_node_id": "x-11"}},
        {"ts": "2026-06-09T08:00:00Z", "type": "verifier_verdict",
         "data": {"verdict": "pass", "graph_node_id": "x-12"}},
        {"ts": "2026-06-08T08:00:00Z", "type": "verifier_verdict",
         "data": {"verdict": "pass", "graph_node_id": "x-13"}},
        {"ts": "2026-06-07T08:00:00Z", "type": "verifier_verdict",
         "data": {"verdict": "fail", "graph_node_id": "x-14"}},
        {"ts": "2026-06-06T08:00:00Z", "type": "verifier_verdict",
         "data": {"verdict": "pass", "graph_node_id": "x-15"}},
        {"ts": "2026-06-05T08:00:00Z", "type": "verifier_verdict",
         "data": {"verdict": "concerns", "graph_node_id": "x-16"}},
        {"ts": "2026-06-04T08:00:00Z", "type": "verifier_verdict",
         "data": {"verdict": "error", "graph_node_id": "x-17"}},
        {"ts": "2026-06-15T07:00:00Z", "type": "loop_check",
         "data": {"session_id": "s-ship", "ci": "FAILURE:smoke", "reviewed": True,
                  "fingerprint": "abc1234|rest"}},
        {"ts": "2026-06-15T07:30:00Z", "type": "loop_check",
         "data": {"session_id": "s-ship", "ci": "FAILURE:smoke"}},
        {"ts": "2026-06-15T08:00:00Z", "type": "loop_check",
         "data": {"session_id": "s-ship", "ci": "SUCCESS",
                  "done_probes": {"fno doctor test cli": "pass"}}},
        {"ts": "2026-06-13T07:00:00Z", "type": "loop_check",
         "data": {"session_id": "s-wedge", "ci": "WEIRD_SHAPE"}},
        {"ts": "2026-06-15T06:00:00Z", "type": "provider_rate_limited",
         "data": {"provider": "zai"}},
        {"ts": "2026-06-15T06:30:00Z", "type": "session_satisfied",
         "data": {"session_id": "s-ship", "source": "pr_merge"}},
        {"ts": "2026-06-15T05:00:00Z", "type": "context_snapshot",
         "data": {"harness": "claude", "entry_state": "startup", "context_bytes": 10,
                  "estimated_tokens": 3, "source_hashes": ["abc"],
                  "context_hash": hashlib.sha256(b"abc").hexdigest(),
                  "source_manifest": [{"source_id": "s1", "status": "observed",
                                       "bytes": 10, "content_hash": "abc"}],
                  "measurement_complete": True,
                  "session_id": "s-ship"}},
        {"ts": "2026-06-15T05:30:00Z", "type": "context_comparison_declared",
         "data": {"comparison_contract": {
             "declared_at": "2026-06-15T05:00:00Z", "cohorts": {"baseline": ["s-old"], "candidate": ["s-ship"]},
             "window": {"start": "2026-06-01T00:00:00Z", "end": "2026-06-30T00:00:00Z"},
             "exclusions": [], "budgets": {"max_candidate_latency_minutes": 90},
             "claim": "context_pruning"}}},
    ]
    _write(state / "events.jsonl", "".join(json.dumps(e) + "\n" for e in events))

    # The minimal entries store the store folds on open: one merged-and-
    # delivered node, one doc delivery, one reverted, one fix node (caused_by
    # -> x-1 bounced), one planned carrier, plus per-node sizes.
    nodes = [
        {"id": "x-1", "slug": "one", "title": "One", "type": "feature", "status": "done",
         "priority": "p2", "domain": "code", "size": "M", "project": "ex/repo",
         "created_at": "2026-06-01T00:00:00+00:00",
         "merged_at": "2026-06-15T10:30:00+00:00", "merge_status": "merged"},
        {"id": "x-2", "slug": "two", "title": "Two", "type": "feature", "status": "done",
         "priority": "p2", "domain": "code", "size": "S",
         "created_at": "2026-06-01T00:00:00+00:00"},
        {"id": "x-3", "slug": "three", "title": "Three", "type": "feature", "status": "in_progress",
         "priority": "p2", "domain": "code", "size": "L",
         "created_at": "2026-06-01T00:00:00+00:00"},
        {"id": "x-4", "slug": "four", "title": "Four", "type": "feature", "status": "in_progress",
         "priority": "p2", "domain": "code",
         "created_at": "2026-06-01T00:00:00+00:00"},
        {"id": "x-9", "slug": "nine", "title": "Nine", "type": "bug", "status": "in_progress",
         "priority": "p1", "domain": "code", "caused_by": "x-1",
         "created_at": "2026-06-20T00:00:00+00:00"},
        {"id": "x-8", "slug": "eight", "title": "Eight", "type": "feature", "status": "done",
         "priority": "p2", "domain": "code", "reverted": True,
         "created_at": "2026-06-01T00:00:00+00:00"},
    ]
    import sqlite3

    db = state / "db" / "graph.db"
    conn = sqlite3.connect(db)
    conn.execute("CREATE TABLE entries (id TEXT PRIMARY KEY, ordinal INTEGER NOT NULL, row TEXT NOT NULL)")
    for i, n in enumerate(nodes):
        conn.execute("INSERT INTO entries VALUES (?, ?, ?)", (n["id"], i, json.dumps(n)))
    # The registry table rides the same store (registry.json is only the
    # fence name); `imported` parks the fence import so the fixture rows are
    # the whole table.
    conn.execute("CREATE TABLE registry (identity TEXT PRIMARY KEY, ordinal INTEGER NOT NULL, payload TEXT NOT NULL)")
    conn.execute("CREATE TABLE registry_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL)")
    conn.execute(
        "INSERT INTO registry_meta VALUES ('imported', '1'), ('revision', '0'),"
        " ('document', '{\"schema_version\": 1}')"
    )
    registry = [
        {"name": "w-1", "status": "busy", "provider": "zai", "model": "glm-5", "effort": "high",
         "cwd": str(repo), "log_path": str(home / ".claude" / "projects" / "-repo-x" / "22222222-2222-2222-2222-222222222222.jsonl")},
        {"name": "w-2", "status": "idle", "provider": "zai", "model": "glm-5", "effort": "high",
         "cwd": str(repo), "log_path": str(home / ".claude" / "projects" / "-repo-x" / "33333333-3333-3333-3333-333333333333.jsonl")},
        {"name": "w-3", "status": "busy", "provider": "claude", "model": "sonnet", "effort": "medium",
         "cwd": str(repo), "log_path": ""},
        {"name": "w-4", "status": "retired", "provider": "zai", "model": "glm-5", "effort": "high",
         "cwd": str(repo), "log_path": ""},
    ]
    for i, row in enumerate(registry):
        conn.execute("INSERT INTO registry VALUES (?, ?, ?)", (row["name"], i, json.dumps(row)))
    conn.commit()
    conn.close()
    return root


def case_env(root: Path) -> dict:
    env = {}
    for key in os.environ:
        if key.startswith(SCRUB_PREFIXES):
            continue
        env[key] = os.environ[key]
    stub_bin = root / "bin"
    stub_bin.mkdir(exist_ok=True)
    fno_stub = stub_bin / "fno"
    fno_stub.write_text(STUB_FNO, encoding="utf-8")
    fno_stub.chmod(0o755)
    gh_stub = stub_bin / "gh"
    gh_stub.write_text(STUB_GH, encoding="utf-8")
    gh_stub.chmod(0o755)
    env["PATH"] = f"{stub_bin}:{env.get('PATH', '')}"
    env["HOME"] = str(root / "home")
    env["FNO_STATE_DIR"] = str(root / "state")
    env["FNO_AGENTS_HOME"] = str(root / "state" / "agents")
    env["FNO_REPO_ROOT"] = str(root / "repo")
    env["LC_ALL"] = "C"
    return env


# label -> (case fixture override, argv after `whoami scoreboard`). Every
# windowed case pins `--since 20000`, so the fixed 2026-06 fixture dates sit
# inside the window on capture day and on every replay after it.
CASES: dict[str, list[str]] = {
    "main_json": ["--since", "20000", "--json"],
    "main_text": ["--since", "20000"],
    "partial_coverage": ["--since", "20000", "--json"],
    "events_malformed": ["--since", "20000", "--json"],
    "no_data": ["--since", "20000", "--json"],
    "broken_ledger": ["--since", "20000", "--json"],
    "since_zero": ["--since", "0"],
    "mutually_exclusive": ["--calibration", "--by-skill"],
    "calibration_ok": ["--calibration", "--json"],
    "calibration_insufficient": ["--calibration", "--json"],
    "by_skill_json": ["--since", "20000", "--by-skill", "--json"],
    "efficiency_json": ["--since", "20000", "--efficiency", "--json"],
    "efficiency_ci_unparsed": ["--since", "20000", "--efficiency", "--json"],
    "lanes_json": ["--since", "20000", "--lanes", "--json"],
    "plan_fidelity_json": ["--since", "20000", "--plan-fidelity", "--json"],
    "plan_fidelity_unjoined": ["--since", "20000", "--plan-fidelity", "--json"],
    "project_scope": ["--since", "20000", "--project", "ex/repo", "--json"],
    "by_provider_json": ["--since", "20000", "--by-provider", "--json"],
}


def specialize(label: str, root: Path) -> None:
    """Rewrite the shared fixture tree into the per-case shape."""
    state = root / "state"
    ledger_path = state / "ledger.json"
    events_path = state / "events.jsonl"
    if label == "partial_coverage":
        rows = json.loads(ledger_path.read_text())["entries"]
        for row in rows:
            row.pop("termination_reason", None)
        ledger_path.write_text(json.dumps({"entries": rows}, indent=1) + "\n")
    elif label == "events_malformed":
        with events_path.open("a", encoding="utf-8") as f:
            f.write("{not json at all\n")
    elif label == "no_data":
        ledger_path.write_text(json.dumps({"entries": []}) + "\n")
        events_path.write_text("")
    elif label == "broken_ledger":
        ledger_path.write_text('{"entries": [ oops\n')
    elif label == "calibration_insufficient":
        lines = [line for line in events_path.read_text().splitlines()
                 if '"verifier_verdict"' not in line]
        lines.append(json.dumps({"ts": "2026-06-02T08:00:00Z", "type": "verifier_verdict",
                                 "data": {"verdict": "pass", "graph_node_id": "x-1"}}) + "\n")
        lines.append(json.dumps({"ts": "2026-06-02T09:00:00Z", "type": "verifier_verdict",
                                 "data": {"verdict": "fail", "graph_node_id": "x-2"}}) + "\n")
        events_path.write_text("".join(lines))
    elif label == "efficiency_ci_unparsed":
        lines = events_path.read_text().splitlines()
        events_path.write_text("".join(
            line + "\n" for line in lines if '"loop_check"' not in line))
    elif label == "plan_fidelity_unjoined":
        rows = json.loads(ledger_path.read_text())["entries"]
        rows = [r for r in rows if r.get("session_id") != "s-ship"]
        ledger_path.write_text(json.dumps({"entries": rows}, indent=1) + "\n")
    elif label == "by_provider_json":
        # by-provider keys on (harness, provider, model); give the rows the
        # provider axes the view groups on.
        rows = json.loads(ledger_path.read_text())["entries"]
        for row in rows:
            if row.get("session_id") == "s-doc":
                row["provider"] = "claude"
                row["model"] = "sonnet"
                row["effort"] = "medium"
        ledger_path.write_text(json.dumps({"entries": rows}, indent=1) + "\n")


def run_case(label: str, root: Path) -> dict[str, str]:
    env = case_env(root)
    proc = subprocess.run(
        [sys.executable, "-m", "fno.cli", "whoami", "scoreboard", *CASES[label]],
        cwd=root / "repo", env=env, capture_output=True, text=True, timeout=120,
    )
    return {"out": proc.stdout, "err": proc.stderr, "exit": str(proc.returncode)}


def scrub_streams(label: str, root: Path, streams: dict[str, str]) -> dict[str, str]:
    """Cut the pieces no two machines agree on: the driver ran on the capture
    host, and the parity replay runs elsewhere. The case root's absolute path
    reaches stdout (plan_path echo) and stderr (refusals naming the state
    root); both sides normalize their own root to <CASE> before comparing.
    The `[setup]` lines are fresh-state-dir migration chatter with a
    wall-clock backup stamp, outside the fold's contract on either leg."""
    out = {}
    for name, text in streams.items():
        text = text.replace(str(root), "<CASE>")
        if name == "err":
            text = "".join(
                line + "\n" for line in text.splitlines()
                if not line.startswith("[setup] ")
            )
        out[name] = text
    return out


def main() -> int:
    if "--only" in sys.argv:
        print("scoreboard: --only capture is gone with the Python leg; "
              "the goldens are frozen provenance now")
        return 1
    if GOLDEN_DIR.exists():
        shutil.rmtree(GOLDEN_DIR)
    GOLDEN_DIR.mkdir(parents=True)
    results: dict[str, dict[str, str]] = {}
    for label in CASES:
        root = build_case(label)
        specialize(label, root)
        streams = run_case(label, root)
        streams = scrub_streams(label, root, streams)
        key = label.replace("_", "-")
        (GOLDEN_DIR / f"{key}.out").write_text(streams["out"], encoding="utf-8")
        (GOLDEN_DIR / f"{key}.exit").write_text(streams["exit"] + "\n", encoding="utf-8")
        if streams["err"]:
            (GOLDEN_DIR / f"{key}.err").write_text(streams["err"], encoding="utf-8")
        results[label] = {"argv": CASES[label], "key": key}
    import secrets

    _write(GOLDEN_DIR / "cases.json", json.dumps(results, indent=1) + "\n")
    prune_capture_artifacts()
    print(f"captured {len(results)} scoreboard goldens under {GOLDEN_DIR}")
    return 0


def prune_capture_artifacts() -> None:
    """Remove the run-time writes a fresh clone must not carry: the case
    repos' .git dirs (the parity replay rebuilds the pinned history), the
    settings backup dirs (a wall-clock name), and the registry snapshot
    dirs (a unix-stamp name). The committed fixtures stay pristine."""
    for case_dir in sorted(FIXTURE_DIR.iterdir()):
        for relative in (
            "repo/.git",
            "state/backups",
            "state/agents/registry-snapshots",
        ):
            victim = case_dir / relative
            if victim.is_dir():
                shutil.rmtree(victim)
            elif victim.exists():
                victim.unlink()


if __name__ == "__main__":
    sys.exit(main())
