#!/usr/bin/env python3
"""Print an events.jsonl from extract.py at a compressed, timed pace.

Usage: replay.py <events.jsonl> [budget_seconds]

The real gaps between events are kept in order but squashed by a square
root, then scaled so the whole replay fits the budget. The header says it
is a replay and states the real run length, so nobody reads it as live.
"""

import json
import sys
import time

DIM, BOLD, CYAN, RESET = "\033[2m", "\033[1m", "\033[36m", "\033[0m"
STYLE = {
    "prompt": lambda s: f"{BOLD}> {s}{RESET}",
    "say": lambda s: f"{CYAN}● {RESET}{s}",
    "tool": lambda s: f"{DIM}  ⎿ {s}{RESET}",
}


def clock(seconds):
    m, s = divmod(int(seconds), 60)
    return f"{m}m{s:02d}s"


def main(argv):
    if not argv:
        sys.exit(__doc__)
    with open(argv[0]) as f:
        evs = [json.loads(line) for line in f if line.strip()]
    budget = float(argv[1]) if len(argv) > 1 else 24.0
    real = evs[-1]["t"] if evs else 0
    gaps = [max(b["t"] - a["t"], 0) ** 0.5 for a, b in zip(evs, evs[1:])]
    scale = budget / sum(gaps) if sum(gaps) else 0
    print(f"{DIM}replay of a real fno run · {clock(real)} shown in {int(budget)}s{RESET}\n")
    for i, ev in enumerate(evs):
        stamp = f"{DIM}{clock(ev['t']):>6}{RESET} "
        print(stamp + STYLE.get(ev["kind"], str)(ev["text"]), flush=True)
        if i < len(gaps):
            time.sleep(gaps[i] * scale)


if __name__ == "__main__":
    main(sys.argv[1:])
