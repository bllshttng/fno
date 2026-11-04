#!/usr/bin/env python3
"""Measure the mux client's own background children against the CPU budget.

The slowness audit (2026-10-08) found the attached mux client's children
burning 0.19-0.22 cores while the operator read one pane: the org block
folded `fno doctor lanes` through Python on its TTL even minimized, the
meter respawned `macmon`, and the server spawned `fno agents list`. The
budget the audit set: under 0.02 cores of child CPU per client. This script
is that budget's measurement, made durable - the audit's ad-hoc
`childcpu.py` never landed, so the after-reading had nothing to answer to.

What counts as the client's children: every descendant of the client
process EXCEPT the `fno --server <sock>` child and its whole subtree. The
server branch carries the pane ptys (the workers themselves, tens of
cores on a busy box); the budget is about the client's own background
probes and folds. Children that exited between two polls are missed by at
most one poll interval - poll at 0.5 s against probes that live seconds.

Measure with the org block minimized (the default state); the expanded
block's `fno doctor lanes` fold is a deliberate read the operator opened,
and the audit's budget describes steady state, not that fold.

Exit 0 under budget, 1 over, 2 when no client was found.
"""
from __future__ import annotations

import argparse
import re
import subprocess
import sys
import time

#: Per-client child-CPU budget, cores. The audit's verify line.
DEFAULT_BUDGET = 0.02

DEFAULT_WINDOW_S = 45.0
DEFAULT_POLL_S = 0.5


def parse_cputime(raw: str) -> float:
    """`[[hh:]mm:]ss[.cc]` into seconds. Unparseable reads as 0.0."""
    text = raw.strip()
    if not text or text == "-":
        return 0.0
    frac = 0.0
    if "." in text:
        text, _, centis = text.partition(".")
        try:
            frac = int(centis) / (10 ** len(centis))
        except ValueError:
            return 0.0
    try:
        parts = [int(p) for p in text.split(":")]
    except ValueError:
        return 0.0
    while len(parts) < 3:
        parts.insert(0, 0)
    h, m, s = parts[-3:]
    return h * 3600 + m * 60 + s + frac


def read_proc_table() -> dict[int, tuple[int, float, str]]:
    """pid -> (ppid, cumulative cpu seconds, command line), one ps walk."""
    out = subprocess.run(
        ["ps", "-axo", "pid=,ppid=,cputime=,command="],
        capture_output=True,
        text=True,
    ).stdout
    table: dict[int, tuple[int, float, str]] = {}
    for line in out.splitlines():
        m = re.match(r"\s*(\d+)\s+(\d+)\s+(\S+)\s+(.*)$", line)
        if not m:
            continue
        pid, ppid, cpu, comm = int(m.group(1)), int(m.group(2)), m.group(3), m.group(4)
        table[pid] = (ppid, parse_cputime(cpu), comm)
    return table


def is_server_process(comm: str) -> bool:
    """The mux server child: `fno --server <sock>`. Its subtree is out of budget."""
    return "--server" in comm


def client_descendants(client_pid: int, table: dict[int, tuple[int, float, str]]):
    """Descendants of `client_pid` with the server child's subtree cut off.

    Returns {pid: (cpu_seconds, command)}. The cut runs at the first
    `--server` descendant, wherever it sits: everything under it is the
    pane tree, not the client's own work.
    """
    children: dict[int, list[int]] = {}
    for pid, (ppid, _cpu, _comm) in table.items():
        children.setdefault(ppid, []).append(pid)
    result: dict[int, tuple[float, str]] = {}
    stack = [client_pid]
    while stack:
        cur = stack.pop()
        for child in children.get(cur, ()):
            row = table.get(child)
            if row is None:
                continue
            _ppid, cpu, comm = row
            if is_server_process(comm):
                continue
            result[child] = (cpu, comm)
            stack.append(child)
    return result


def discover_clients(table: dict[int, tuple[int, float, str]]) -> list[int]:
    """Bare `fno` launches: the TUI clients. Servers and subcommand runs excluded."""
    clients = []
    for pid, (_ppid, _cpu, comm) in table.items():
        parts = comm.split()
        if len(parts) != 1:
            continue
        if parts[0].rstrip("/").endswith("/fno") or parts[0] == "fno":
            clients.append(pid)
    return sorted(clients)


def measure(client_pid: int, window_s: float, poll_s: float) -> tuple[float, int]:
    """(child CPU cores, distinct children seen) over one window."""
    cpu_high: dict[int, float] = {}
    seen: set[int] = set()
    total = 0.0
    start = time.time()
    while time.time() - start < window_s:
        table = read_proc_table()
        for pid, (cpu, _comm) in client_descendants(client_pid, table).items():
            seen.add(pid)
            high = cpu_high.get(pid, 0.0)
            if cpu > high:
                total += cpu - high
                cpu_high[pid] = cpu
        time.sleep(poll_s)
    wall = time.time() - start
    rate = total / wall if wall > 0 else 0.0
    return rate, len(seen)


# ---------------------------------------------------------------------------
# Self-test: the fixtures below are synthetic ps tables. A guard that cannot
# prove it detects is the false zero this budget exists to prevent.
# ---------------------------------------------------------------------------

FIXTURE_TABLE = {
    100: (1, 80.0, "fno"),
    200: (100, 400.0, "/Users/x/.cargo/bin/fno --server /tmp/main.sock"),
    201: (100, 1.0, "/Users/x/.cargo/bin/fno-agents"),
    202: (201, 0.5, "/Users/x/.cargo/bin/fno mux pane wait"),
    210: (200, 900.0, "claude"),
    211: (200, 700.0, "codex"),
    300: (1, 5.0, "fno --server /tmp/other.sock"),
    301: (300, 50.0, "claude"),
}


def self_test() -> bool:
    ok = True
    desc = client_descendants(100, FIXTURE_TABLE)
    if 200 in desc or 210 in desc or 211 in desc:
        print("self-test FAILED: the server subtree leaked into the budget", file=sys.stderr)
        ok = False
    for expected in (201, 202):
        if expected not in desc:
            print(f"self-test FAILED: child {expected} missing from the walk", file=sys.stderr)
            ok = False
    clients = discover_clients(FIXTURE_TABLE)
    if clients != [100]:
        print(
            f"self-test FAILED: discovery read {clients}, want [100] "
            "(a --server process is not a client)",
            file=sys.stderr,
        )
        ok = False
    cases = [("1:01.5", 61.5), ("01:02.03", 62.03), ("1:02:03.4", 3723.4), ("45.1", 45.1)]
    for raw, want in cases:
        got = parse_cputime(raw)
        if abs(got - want) > 1e-6:
            print(f"self-test FAILED: cputime {raw!r} read {got}, want {want}", file=sys.stderr)
            ok = False
    print("self-test OK: server subtree cut, discovery exact, cputime parses")
    return ok


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--client-pid", type=int, action="append", dest="client_pids",
                    help="pin a client pid (repeatable); default discovers bare fno clients")
    ap.add_argument("--window", type=float, default=DEFAULT_WINDOW_S,
                    help=f"sample seconds (default {DEFAULT_WINDOW_S:g})")
    ap.add_argument("--poll", type=float, default=DEFAULT_POLL_S,
                    help=f"poll seconds (default {DEFAULT_POLL_S:g})")
    ap.add_argument("--budget", type=float, default=DEFAULT_BUDGET,
                    help=f"per-client cores (default {DEFAULT_BUDGET})")
    ap.add_argument("--json", action="store_true", help="machine-readable verdict")
    ap.add_argument("--list", action="store_true", help="list discovered clients and exit")
    ap.add_argument("--self-test", action="store_true", help="prove the walker detects")
    args = ap.parse_args()
    if args.self_test:
        return 0 if self_test() else 1

    table = read_proc_table()
    clients = args.client_pids or discover_clients(table)
    missing = [p for p in (args.client_pids or []) if p not in table]
    if missing:
        print(f"pinned client pid(s) absent: {missing}", file=sys.stderr)
        return 2
    if args.list:
        for pid in clients:
            print(f"{pid}\t{table[pid][2]}")
        return 0
    if not clients:
        print("no mux client found (a bare `fno` TUI process)", file=sys.stderr)
        return 2

    readings = {}
    for pid in clients:
        rate, seen = measure(pid, args.window, args.poll)
        readings[pid] = (rate, seen)
    worst_pid, (worst_rate, _) = max(readings.items(), key=lambda kv: kv[1][0])
    passed = worst_rate < args.budget
    if args.json:
        import json
        print(json.dumps({
            "budget_cores": args.budget,
            "window_s": args.window,
            "clients": {str(p): {"cores": r, "children": s} for p, (r, s) in readings.items()},
            "verdict": "pass" if passed else "fail",
        }))
    else:
        for pid, (rate, seen) in sorted(readings.items()):
            print(f"client {pid}: {rate:.4f} cores across {seen} children")
        print(f"budget {args.budget} cores: " + ("PASS" if passed else "FAIL"))
    return 0 if passed else 1


if __name__ == "__main__":
    sys.exit(main())
