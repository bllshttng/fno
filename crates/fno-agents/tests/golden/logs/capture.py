#!/usr/bin/env python3
"""Capture the `fno agents logs` goldens from the Python leg (now deleted).

Recipe (run from the repo root of a checkout whose cli/src still carries the
Python leg; a fresh worktree reuses the canonical checkout's venv):

    PYTHONPATH="$PWD/cli/src" cli/.venv/bin/python \
        crates/fno-agents/tests/golden/logs/capture.py

The script builds one throwaway root per case (registry, empty harness
stores, a PATH shim with the fake `claude` the case needs), runs
`FNO_AGENTS_RUNTIME=python python -m fno.cli agents logs ...` against it,
and freezes `<slug>.out`, `<slug>.err` and `<slug>.exit` next to this file
after the normalization pipeline (drop setup noise, skip the events.emit
multi-line warning, rstrip, scrub the temp root to `ROOT`).

`SURVIVOR_BYTES` freezes bytes the survivor prints and the deleted leg did
not; re-running the capture reproduces the frozen files exactly.
"""
from __future__ import annotations

import json
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

GOLDEN_DIR = Path(__file__).resolve().parent
REPO_ROOT = GOLDEN_DIR.parents[3]

# The setup noise the Python CLI prints into a fresh state root, and the
# events.emit multi-line warning (a top-level `Error: ` closes it). Mirrored
# line-for-line by logs_parity.rs's normalize().
DROP_PREFIXES = [
    "[setup] path migration complete",
    "  detected install:",
    "  settings written to:",
    "  obsidian enabled:",
    "  sentinel:",
]


def normalize(text: str, root: Path) -> str:
    kept: list[str] = []
    skipping = False
    for line in text.split("\n"):
        if any(line.startswith(p) for p in DROP_PREFIXES):
            continue
        if line.startswith("fno agents: warning: events.emit("):
            skipping = True
            continue
        if skipping:
            if line.startswith("Error: "):
                skipping = False
            continue
        kept.append(line)
    text = "\n".join(kept)
    text = text.rstrip("\n")
    real = os.path.realpath(root)
    text = text.replace(real, "ROOT")
    return text.replace(str(root), "ROOT")


def slug(label: str) -> str:
    out = []
    prev_us = False
    for ch in label:
        if ch.isalnum():
            out.append(ch.lower())
            prev_us = False
        elif not prev_us:
            out.append("_")
            prev_us = True
    return "".join(out).strip("_")


CLAUDE_SHIMS = {
    "two-line": 'echo "raw line 1"; echo "raw line 2"',
    "twenty-line": 'i=1; while [ $i -le 20 ]; do echo "line$i"; i=$((i+1)); done',
    "fail17": 'echo "claude: not found" >&2; exit 17',
    "rawoutput": 'echo "claude raw output"',
}

CLAUDE_ROW = {
    "name": "alpha",
    "short_id": "abc12345",
    "harness": "claude",
    "harness_session_id": "sess-alpha-0001",
    "cwd": "/repo",
    "project_root": "/repo",
    "log_path": "/repo/alpha/output.jsonl",
    "status": "live",
    "created_at": "2026-05-20T17:00:00Z",
}

CODEX_SESSION_ID = "019fb417-1111-7222-8333-4444deadbeef"


def codex_row(log_path: str | None) -> dict:
    row = {
        "name": "cx",
        "short_id": "cx",
        "harness": "codex",
        "harness_session_id": CODEX_SESSION_ID,
        "cwd": "/tmp/x",
        "project_root": "/tmp/x",
        "status": "live",
        "created_at": "2026-05-26T09:00:00Z",
    }
    if log_path is not None:
        row["log_path"] = log_path
    return row


def make_shim(root: Path, body: str) -> None:
    shim_dir = root / "shim"
    shim_dir.mkdir(exist_ok=True)
    script = shim_dir / "claude"
    script.write_text(f"#!/bin/sh\n{body}\n")
    script.chmod(0o755)


def build_fixture(root: Path, rows: list[dict], shim: str | None) -> None:
    agents = root / "agents-home"
    agents.mkdir(parents=True, exist_ok=True)
    (agents / "registry.json").write_text(
        json.dumps({"schema_version": 3, "agents": rows})
    )
    for name in (
        "empty-claude-projects",
        "empty-codex-sessions",
        "empty-opencode-storage",
        "empty-mux",
    ):
        (root / name).mkdir()
    if shim is not None:
        make_shim(root, CLAUDE_SHIMS[shim])


def run_case(root: Path, args: list[str]) -> tuple[int, str, str]:
    env = {**os.environ}
    env.update(
        {
            "HOME": str(root),
            "FNO_STATE_DIR": str(root),
            "FNO_HOME": str(root),
            "FNO_AGENTS_HOME": str(root / "agents-home"),
            "FNO_CLAUDE_PROJECTS_DIR": str(root / "empty-claude-projects"),
            "FNO_CODEX_SESSIONS_DIR": str(root / "empty-codex-sessions"),
            "FNO_OPENCODE_STORAGE_DIR": str(root / "empty-opencode-storage"),
            "FNO_MUX_DIR": str(root / "empty-mux"),
            "FNO_AGENTS_RUNTIME": "python",
            "CODEX_THREAD_ID": "",
            "CODEX_SESSION_ID": "",
            "GEMINI_SESSION_ID": "",
            "OPENCODE_SESSION_ID": "",
            "CLAUDE_SESSION_ID": "",
            "CLAUDE_CODE_SESSION_ID": "",
            "PYTHONUNBUFFERED": "1",
        }
    )
    env.pop("FNO_AGENT_SELF", None)
    env.pop("FNO_RUST_FRONT", None)
    inherited = env.get("PYTHONPATH")
    source = str(REPO_ROOT / "cli" / "src")
    env["PYTHONPATH"] = f"{source}{os.pathsep}{inherited}" if inherited else source
    if (root / "shim").is_dir():
        env["PATH"] = f"{root / 'shim'}{os.pathsep}{env.get('PATH', '')}"
    proc = subprocess.run(
        [sys.executable, "-m", "fno.cli", "agents", "logs", *args],
        env=env,
        capture_output=True,
        text=True,
        cwd=str(root),
    )
    return proc.returncode, proc.stdout, proc.stderr


# Cases where the frozen golden carries the SURVIVING leg's bytes rather than
# the deleted Python leg's, with the ruling that diverged them. `capture`
# still runs the Python leg so the recipe proves the fixture; the frozen
# streams come from here.
SURVIVOR_BYTES: dict[str, dict] = {
    # d-e952ed19: the registry resolver became session-id-first and its
    # not-found message changed shape. read.py printed
    # "agent not found: ghost"; the surviving resolver in resolve_tier.rs
    # prints the accepted-forms line below. The Rust message was pinned at
    # base by test_logs_agent_not_found_parity, which this port deletes into
    # this golden.
    "unknown_name": {
        "exit": 13,
        "out": "",
        "err": "no agent matching 'ghost'; accepted forms: name, canonical handle, "
        "transport short id, or full session id",
    },
}


def main() -> int:
    log_file_rel = "agents-home/cx.log.jsonl"
    cases = [
        ("unknown name", [], ["ghost"], None),
        ("claude passthrough", [CLAUDE_ROW], ["alpha"], "two-line"),
        ("claude tail 3", [CLAUDE_ROW], ["alpha", "--tail", "3"], "twenty-line"),
        ("tail zero", [CLAUDE_ROW], ["alpha", "--tail", "0"], "twenty-line"),
        ("negative tail", [CLAUDE_ROW], ["alpha", "--tail", "-5"], "two-line"),
        (
            "claude non-zero exit propagates",
            [CLAUDE_ROW],
            ["alpha"],
            "fail17",
        ),
        ("claude json warning", [CLAUDE_ROW], ["alpha", "--json"], "rawoutput"),
        ("claude row with no short id", [{**CLAUDE_ROW, "short_id": ""}], ["alpha"], "two-line"),
        (
            "codex one-shot tail 2 by name",
            [codex_row(log_file_rel)],
            ["cx", "--tail", "2"],
            None,
        ),
        (
            "codex one-shot tail 2 by session head",
            [codex_row(log_file_rel)],
            ["deadbeef", "--tail", "2"],
            None,
        ),
        (
            "codex row with no log file",
            [codex_row("absent.jsonl")],
            ["cx"],
            None,
        ),
        (
            "codex no log_path recorded",
            [codex_row("")],
            ["cx"],
            None,
        ),
    ]
    for label, rows, args, shim in cases:
        key = slug(label)
        root = Path(tempfile.mkdtemp(prefix="fno-logs-golden-"))
        try:
            abs_rows = [dict(r) for r in rows]
            for row in abs_rows:
                if row.get("log_path") == log_file_rel:
                    log = root / log_file_rel
                    log.parent.mkdir(parents=True, exist_ok=True)
                    log.write_text('{"line":1}\n{"line":2}\n{"line":3}\n{"line":4}')
                    row["log_path"] = str(log)
                elif row.get("log_path") == "absent.jsonl":
                    row["log_path"] = str(root / "absent.jsonl")
            build_fixture(root, abs_rows, shim)
            exit_code, out, err = run_case(root, args)
            survivor = SURVIVOR_BYTES.get(key)
            if survivor is not None:
                print(
                    f"[{key}] python leg printed exit={exit_code} "
                    f"err={err!r}; freezing the survivor's bytes per "
                    f"the ruling noted in SURVIVOR_BYTES"
                )
                exit_code, out, err = (
                    survivor["exit"],
                    survivor["out"],
                    survivor["err"],
                )
            (GOLDEN_DIR / f"{key}.exit").write_text(f"{exit_code}\n")
            (GOLDEN_DIR / f"{key}.out").write_text(normalize(out, root))
            (GOLDEN_DIR / f"{key}.err").write_text(normalize(err, root))
            print(f"[{key}] exit={exit_code} out={len(out)}B err={len(err)}B")
        finally:
            shutil.rmtree(root, ignore_errors=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
