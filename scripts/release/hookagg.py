#!/usr/bin/env python3
"""Aggregate per-call hook timings from claude transcripts.

Reads ~/.claude/projects/*/*.jsonl (plus each project's subagents/) and
prints, per hook command, the distribution of durationMs over a window,
then a per-event wall/CPU summary. The hook-latency close gate parses
the per-hook table: the hook name in field 1, n in field 2, p50 in
field 3.

WINDOW argument (optional):
  YYYY-MM-DD            prefix match, rows of that day only
  longer ISO timestamp  window start, rows with timestamp >= arg
  absent                last six hours (UTC)

Rows come from hook_success / hook_cancelled / hook_non_blocking_error
attachments, which claude writes per hook invocation with durationMs.
"""

import glob, os, collections, json, re, sys
from datetime import datetime, timedelta, timezone

# The 2026-10-08 slowness audit only reads transcripts touched since it
# started; older files carry no hook rows.
AUDIT_START_EPOCH = 1791400000

DEFAULT_WINDOW_HOURS = 6


def window_start(argv):
    if len(argv) > 1:
        return argv[1]
    return (datetime.now(timezone.utc) - timedelta(hours=DEFAULT_WINDOW_HOURS)).strftime(
        "%Y-%m-%dT%H:%M:%SZ"
    )


def in_window(ts, window):
    if len(window) == 10:
        return ts.startswith(window)
    # ISO timestamps sort lexicographically; rows inside the cutoff second
    # itself lose the trailing 'Z' comparison, an error of under one second.
    return ts >= window


def collect_files():
    def fresh(f):
        try:
            return os.path.getmtime(f) > AUDIT_START_EPOCH
        except OSError:
            return False  # reaped between glob and stat

    return [
        f
        for f in glob.glob(os.path.expanduser("~/.claude/projects/*/*.jsonl"))
        + glob.glob(os.path.expanduser("~/.claude/projects/*/*/subagents/*.jsonl"))
        if fresh(f)
    ]


def name(cmd):
    if "ORCA_AGENT_HOOK" in cmd:
        # Wire sentinel, not provenance: some fleet hooks carry this env
        # marker in their command line; the label describes the class.
        return "external-agent-hook"
    m = re.findall(r"([\w.-]+\.(?:sh|py|js))", cmd)
    if m:
        mm = re.search(r"session-state\.sh claude (\w+)", cmd)
        return m[0] + (" " + mm.group(1) if mm else "")
    return cmd[:50]


def aggregate(files, window):
    dur = collections.defaultdict(list)
    canc = collections.Counter()
    evcalls = collections.defaultdict(lambda: collections.defaultdict(list))
    nf = 0
    for f in files:
        nf += 1
        for l in open(f, errors="ignore"):
            if '"hook_' not in l:
                continue
            try:
                d = json.loads(l)
            except Exception:
                continue
            if not in_window(d.get("timestamp", ""), window):
                continue
            a = d.get("attachment") or {}
            t = a.get("type")
            if t not in ("hook_success", "hook_cancelled", "hook_non_blocking_error"):
                continue
            if "durationMs" not in a:
                continue
            n = name(a.get("command", ""))
            dur[n].append(a["durationMs"])
            if t == "hook_cancelled":
                canc[n] += 1
            evcalls[a.get("hookEvent")][(f, a.get("toolUseID"))].append(a["durationMs"])
    return {"nf": nf, "dur": dur, "canc": canc, "evcalls": evcalls}


def print_report(dur, canc, evcalls, nf, window):
    print(f"files {nf} window {window}")
    print(f"{'hook':45s} {'n':>7s} {'p50':>6s} {'p90':>6s} {'p99':>7s} {'max':>7s} {'sum_s':>8s} cancel")
    rows = sorted(dur.items(), key=lambda kv: -sum(kv[1]))
    for n, v in rows[:45]:
        v = sorted(v)
        q = lambda p: v[min(len(v) - 1, int(p * len(v)))]
        print(
            f"{n[:45]:45s} {len(v):7d} {q(.5):6d} {q(.9):6d} {q(.99):7d} {v[-1]:7d} {sum(v)/1000:8.0f} {canc[n]}"
        )
    print("\nper-event: calls, wall(max of parallel) p50/p90/p99 ms, cpu(sum) p50/p90, total wall s, total sum s")
    for ev, calls in evcalls.items():
        mx = sorted(max(x) for x in calls.values())
        sm = sorted(sum(x) for x in calls.values())
        nh = sorted(len(x) for x in calls.values())
        q = lambda v, p: v[min(len(v) - 1, int(p * len(v)))]
        print(
            f"{ev:18s} calls={len(calls):6d} hooks/call p50={q(nh,.5)} wall p50={q(mx,.5)} p90={q(mx,.9)} p99={q(mx,.99)}  sum p50={q(sm,.5)} p90={q(sm,.9)}  wall_total={sum(mx)/1000:.0f}s sum_total={sum(sm)/1000:.0f}s"
        )


def main():
    window = window_start(sys.argv)
    agg = aggregate(collect_files(), window)
    print_report(agg["dur"], agg["canc"], agg["evcalls"], agg["nf"], window)


if __name__ == "__main__":
    main()
