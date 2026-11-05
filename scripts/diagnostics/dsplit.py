#!/usr/bin/env python3
"""Split the fno daemon's CPU into its own, its fleet workers', and its maintenance children's.

Rebuilt 2026-10-12 from the method notes in the 2026-10-08 slowness audit.
The work/maintenance split landed after a gate reading showed fleet loop
workers dominating the child figure. The gate names the full child figure,
so a reading reports the gate against the child total, and the split keeps
both figures on one page beside it.

Samples `ps` several times a second for SECONDS. A descendant is any process
whose parent chain reaches the daemon pid; once seen it stays in the set. The
child CPU is the sum of the CPU time each descendant gained inside the window.
A process that lives shorter than one sample gap is missed, and CPU a child
burns after its last sample is missed, so the child figure is a lower bound.

Classes come from the tree, not from a binary allowlist over the whole tree.
Every descendant inherits the class of its nearest ancestor that is a direct
child of the daemon. A direct child whose executable is the per-session
worker binary (fno-agents-worker) roots a fleet-work subtree; every other
direct child roots a maintenance subtree. The maintenance table carries each
command's leading arguments, so an over-budget reading names its largest
child without a second investigation pass.

usage: dsplit.py [SECONDS=60] [DAEMON_PID] [--gap 0.2]
"""
import subprocess
import sys
import time
from collections import defaultdict


WORKER_EXE = "fno-agents-worker"
DAEMON_EXE = "fno-agents-daemon"


def cpu_seconds(s):
    # ps cputime: [dd-]hh:mm:ss.cc or mm:ss.cc
    days = 0
    if "-" in s:
        d, s = s.split("-", 1)
        days = int(d)
    parts = [float(x) for x in s.split(":")]
    sec = 0.0
    for p in parts:
        sec = sec * 60 + p
    return days * 86400 + sec


def is_work(command):
    # A direct child of the daemon is fleet work only when its executable is
    # the per-session worker binary, by exact name; command= never truncates.
    parts = command.split()
    if not parts:
        return False
    return parts[0].rsplit("/", 1)[-1] == WORKER_EXE


def adopt(rows, members, classes, root):
    """Close members over one rows snapshot; classify every new member.

    A direct child of the daemon classifies from its own command; a deeper
    descendant inherits its parent's class. The parent is always classified
    first: it joined members in an earlier pass, or it is the root.
    """
    changed = True
    while changed:
        changed = False
        for pid, (ppid, _, cmd) in rows.items():
            if pid not in members and ppid in members:
                members.add(pid)
                classes[pid] = is_work(cmd) if ppid == root else classes[ppid]
                changed = True
    return members, classes


def pick_daemon(rows):
    """The one resident daemon pid among the rows, or a refusal naming the count.

    The daemon binary ignores unknown argv, so any process whose executable is
    fno-agents-daemon may be the resident. The match keys on the executable,
    not the full argv, so a wrapper that merely names the daemon in its
    command text is never a candidate. A transient `version` probe can still
    double the count and refuse; retry once it exits.
    """
    out = []
    for pid, (_, _, command) in rows.items():
        parts = command.split()
        if parts and parts[0].rsplit("/", 1)[-1] == DAEMON_EXE:
            out.append(pid)
    if len(out) != 1:
        sys.exit(f"expected one resident {DAEMON_EXE}, found {len(out)}: {out}; "
                 "pass the pid")
    return out[0]


def snapshot():
    out = subprocess.run(["ps", "-axo", "pid=,ppid=,cputime=,command="],
                         capture_output=True, text=True).stdout
    rows = {}
    for line in out.splitlines():
        f = line.split(None, 3)
        if len(f) < 4:
            continue
        try:
            rows[int(f[0])] = (int(f[1]), cpu_seconds(f[2]), f[3])
        except ValueError:
            continue
    return rows


def daemon_pid():
    return pick_daemon(snapshot())


def label(command):
    # Basename plus the first two arguments: names the command kind so an
    # over-budget maintenance row points at its cut.
    parts = command.split()
    if not parts:
        return "?"
    name = parts[0].rsplit("/", 1)[-1]
    return " ".join([name] + parts[1:3])[:48]


def main(argv):
    gap = 0.2
    if "--gap" in argv:
        i = argv.index("--gap")
        gap = float(argv[i + 1])
        del argv[i:i + 2]
    seconds = float(argv[0]) if argv else 60.0
    root = int(argv[1]) if len(argv) > 1 else daemon_pid()

    first, last, cmd_of = {}, {}, {}
    born_in_window = set()
    members = {root}
    classes = {}
    t0 = time.time()
    samples = 0
    while True:
        rows = snapshot()
        samples += 1
        if root not in rows:
            sys.exit(f"daemon pid {root} exited during the window")
        members, classes = adopt(rows, members, classes, root)
        for pid in members:
            if pid in rows:
                _, cpu, name = rows[pid]
                if pid not in first:
                    first[pid] = cpu
                    cmd_of[pid] = name
                    if samples > 1:
                        born_in_window.add(pid)
                last[pid] = cpu
        if time.time() - t0 >= seconds:
            break
        time.sleep(gap)
    wall = time.time() - t0

    own = last[root] - first[root]
    child = work_cpu = maint_cpu = 0.0
    by_work = defaultdict(lambda: [0, 0.0])
    by_maint = defaultdict(lambda: [0, 0.0])
    for pid in members - {root}:
        if pid not in first:
            continue
        # a process born inside the window spent all its CPU inside it; a pid
        # reused mid-window can read its predecessor's first sample against its
        # own last, so the floor is zero, never a negative contribution
        gained = last[pid] if pid in born_in_window else max(0.0, last[pid] - first[pid])
        child += gained
        if classes[pid]:
            work_cpu += gained
            table = by_work
        else:
            maint_cpu += gained
            table = by_maint
        name = label(cmd_of[pid])
        table[name][0] += 1
        table[name][1] += gained

    print(f"daemon pid {root}  window {wall:.1f} s  samples {samples}  gap {gap} s")
    print(f"own CPU        {own:8.2f} s   {own / wall:6.3f} cores")
    print(f"child CPU      {child:8.2f} s   {child / wall:6.3f} cores   (lower bound)")
    print(f"  work         {work_cpu:8.2f} s   {work_cpu / wall:6.3f} cores"
          "   fleet workers (per-session worker subtrees)")
    print(f"  maintenance  {maint_cpu:8.2f} s   {maint_cpu / wall:6.3f} cores"
          "   daemon upkeep (not part of the gate figure)")
    print(f"descendants seen {len(first) - 1}, born in window {len(born_in_window)}")

    for title, table in (("work children by command", by_work),
                         ("maintenance children by command", by_maint)):
        print(f"\n{title}")
        for name, (n, cpu) in sorted(table.items(), key=lambda kv: -kv[1][1]):
            print(f"  {name:<48}{n:>6} procs {cpu:8.2f} s  {cpu / wall:6.3f} cores")


if __name__ == "__main__":
    main(sys.argv[1:])
