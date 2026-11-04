#!/usr/bin/env python3
"""Unit tests for hookagg.py window selection and gate-row shape.

The hook-latency close probe passed a full ISO timestamp (date -u -v-6H
+%Y-%m-%dT%H:%M:%SZ) as hookagg.py's day argument. The script kept only
rows whose timestamp startswith that string, so n was always 0 and the
gate measured nothing (2026-11-04). A bare YYYY-MM-DD stays a prefix
match; a longer ISO string is now a window start (timestamp >= arg).

Also pins the per-hook row shape the close probe's awk parses: the hook
name in field 1, n in field 2, p50 in field 3.

Run: python3 scripts/release/test_hookagg_window.py
"""

import contextlib
import importlib.util
import io
import json
import tempfile
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent.parent
MODULE_PATH = REPO_ROOT / "scripts" / "release" / "hookagg.py"

_spec = importlib.util.spec_from_file_location("hookagg", MODULE_PATH)
hookagg = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(hookagg)

PASS = 0
FAIL = 0


def check(name: str, cond: bool) -> None:
    global PASS, FAIL
    if cond:
        print(f"  PASS: {name}")
        PASS += 1
    else:
        print(f"  FAIL: {name}")
        FAIL += 1


def row(ts: str, event: str, command: str, duration: int, kind: str = "hook_success") -> str:
    return json.dumps(
        {
            "timestamp": ts,
            "attachment": {
                "type": kind,
                "command": command,
                "hookEvent": event,
                "durationMs": duration,
            },
        }
    )


DISPATCH = 'bash "${CLAUDE_PLUGIN_ROOT}/hooks/pretooluse-bash-dispatch.sh"'
WRITE_GATE = 'bash "${CLAUDE_PLUGIN_ROOT}/hooks/write-gate.sh"'

# Rows: two inside a 6h window from 06:00Z, one 9h before it, one on the
# previous day, one PostToolUse row inside the window. The subagents file
# repeats the first row, proving the subagents glob is scanned.
ROWS = [
    row("2026-11-04T11:30:00.000Z", "PreToolUse", DISPATCH, 120),
    row("2026-11-04T11:59:00.000Z", "PreToolUse", DISPATCH, 140),
    row("2026-11-04T03:00:00.000Z", "PreToolUse", DISPATCH, 9000),
    row("2026-11-03T23:00:00.000Z", "PreToolUse", DISPATCH, 8000),
    row("2026-11-04T11:45:00.000Z", "PostToolUse", WRITE_GATE, 90),
]


def fixture_files(tmp: str):
    root = Path(tmp) / "projects" / "proj-a"
    root.mkdir(parents=True)
    f1 = root / "s1.jsonl"
    f1.write_text("\n".join(ROWS) + "\n")
    f2 = root / "subagents"
    f2.mkdir()
    (f2 / "sub1.jsonl").write_text(ROWS[0] + "\n")
    return [str(f1), str(f2 / "sub1.jsonl")]


with tempfile.TemporaryDirectory() as tmp:
    files = fixture_files(tmp)

    print("AC1: a bare day stays a prefix match")
    agg = hookagg.aggregate(files, "2026-11-04")
    dur = agg["dur"]
    check("day keeps every row of that day", sorted(dur["pretooluse-bash-dispatch.sh"]) == [120, 120, 140, 9000])
    check("day excludes the previous day", 8000 not in dur["pretooluse-bash-dispatch.sh"])

    print("AC2: a full timestamp is a window start, never a silent zero")
    agg = hookagg.aggregate(files, "2026-11-04T06:00:00Z")
    dur = agg["dur"]
    check("window keeps the in-window rows", sorted(dur["pretooluse-bash-dispatch.sh"]) == [120, 120, 140])
    check("window drops rows before the cutoff", 9000 not in dur["pretooluse-bash-dispatch.sh"])
    check("window keeps other events", dur["write-gate.sh"] == [90])

    print("AC3: the close-probe awk row shape (name $1, n $2, p50 $3)")
    agg = hookagg.aggregate(files, "2026-11-04T06:00:00Z")
    out = io.StringIO()
    with contextlib.redirect_stdout(out):
        hookagg.print_report(agg["dur"], agg["canc"], agg["evcalls"], agg["nf"], "2026-11-04T06:00:00Z")
    lines = [l for l in out.getvalue().splitlines() if l.startswith("pretooluse-bash-dispatch.sh")]
    fields = lines[0].split() if lines else []
    check("row starts with the hook name", bool(fields) and fields[0] == "pretooluse-bash-dispatch.sh")
    check("n is field 2", bool(fields) and fields[1] == "3")
    check("p50 is field 3", bool(fields) and fields[2] == "120")

total = PASS + FAIL
print()
if FAIL == 0:
    print(f"PASS: hookagg window tests ({PASS}/{total})")
    raise SystemExit(0)
print(f"FAIL: hookagg window tests ({PASS} passed, {FAIL} failed of {total})")
raise SystemExit(1)
