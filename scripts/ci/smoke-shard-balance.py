"""Print the estimated seconds of each smoke-rest leg and refuse an unbalanced split.

Run: uv run --project cli python scripts/ci/smoke-shard-balance.py [--legs 4] [--max-leg-seconds 600]
Exit: 0 every leg fits the bound, 1 a leg exceeds it.

The estimate sums scripts/ci/smoke-step-seconds.tsv over the steps the real
split gives each leg (a step missing from the table counts 2 seconds). It
reads the same function the runner calls, so it measures the split CI runs.
"""
from __future__ import annotations

import argparse
import sys
from pathlib import Path

from fno.test_cmd import _name_matches, _shard_selected_indices, smoke_step_weights, smoke_steps

ROOT = Path(__file__).resolve().parents[2]


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--legs", type=int, default=4)
    parser.add_argument("--max-leg-seconds", type=float, default=600.0)
    args = parser.parse_args()
    steps = smoke_steps(ROOT)
    weights = smoke_step_weights(ROOT)
    selected = [i for i, (name, _cwd, _cmd) in enumerate(steps) if not _name_matches(name, "Pytest*")]
    worst = 0.0
    for leg in range(1, args.legs + 1):
        kept = _shard_selected_indices(steps, selected, leg, args.legs, weights)
        seconds = sum(weights.get(steps[i][0], 2.0) for i in kept)
        worst = max(worst, seconds)
        print(f"leg {leg}/{args.legs} estimate={seconds:.0f}s steps={len(kept)}")
    verdict = "fits" if worst <= args.max_leg_seconds else "over"
    print(f"widest leg {worst:.0f}s {verdict} the {args.max_leg_seconds:.0f}s bound")
    return 0 if worst <= args.max_leg_seconds else 1


if __name__ == "__main__":
    sys.exit(main())
