#!/usr/bin/env python3
"""Run 1 driver: four lanes over the replay bank in one seeded order, then the top-up.

  drive_run1.py

Every lane runs with the branch's fno-agents (FNO_AGENTS_BIN from ~/.fno/.env,
set by setup-imac.sh). A lane with rows already in history is skipped, so a killed
driver restarts safely. Before each lane, spent dollars plus 3 times the lane's
projection must stay under the manifest ceiling, else stop.

Top-up (Amendment 4): per available lane and task, rerun the attempts that never
started, up to 3 graded or stalled per task, at most 3 passes. A worker that ran
out its budget started: a stall, scored, never rerun.

A `logs/drain` file holds new work before it starts (Amendment 9), so a held
attempt loses nothing. A lane whose newest history rows are consecutive
identical `worker_started: false` spawn errors is stopped after
BREAKER_N of them: the rows pollute the history and the driver looks alive to a
liveness watcher, so the lane logs a `stop` event (the same row the ceiling
stop writes, which status.py reads) and never re-enters the top-up.
"""
import json
import os
import random
import subprocess
import sys
import time

from paths import HERE, HISTORY, LOGS, MANIFEST, REPO

LOG = LOGS / "run1-driver.jsonl"
LANES = ["claude", "opencode", "pi", "zcode"]
TOPUP_LANES = ["claude", "opencode"]
SEED = 272
REPEAT = 3
TASKS = 10
FLOOR = 0.25  # USD per attempt, as Run 0's per-task floor
ROUNDS = 3
COOLDOWN = 900
DRAIN = LOGS / "drain"
BREAKER_N = 3


def log(**row) -> None:
    row["at"] = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
    with LOG.open("a") as f:
        f.write(json.dumps(row) + "\n")


def rows() -> list:
    out = []
    if HISTORY.exists():
        for line in HISTORY.read_text().splitlines():
            try:
                out.append(json.loads(line))
            except ValueError:
                continue
    return out


def spent(prices: dict) -> float:
    """Run 0 Harbor jobs + Run 1 cohorts + the pilot. An attempt with no usage is charged the floor."""
    sys.path.insert(0, str(HERE))
    import launch  # noqa: E402  (needs pyyaml only for its own runs)
    report = subprocess.run(["fno", "doctor", "evals", "report", "--by-cohort", "--prices", json.dumps(prices), "--json"],
                            capture_output=True, text=True, cwd=REPO)
    total = sum(launch.spend(prices).values()) + launch.pilot_spend(json.loads(MANIFEST.read_text()))
    for name, c in json.loads(report.stdout)["cohorts"].items():
        if name.startswith("harness-fit-") and not name.startswith("harness-fit-smoke"):
            total += (c["cost"]["dollars_total"] or 0.0) + c["excluded"]["usage_missing"] * FLOOR
    return total


def done_counts() -> dict:
    out = {}
    for r in rows():
        stalled = "timed out after" in (r.get("reason") or "")
        if r.get("status") == "graded" or stalled:
            key = (r.get("experiment_id"), r.get("task_id"))
            out[key] = out.get(key, 0) + 1
    return out


def lane(name: str, repeat: int, task: str = "") -> int:
    args = ["bash", str(HERE / "run1.sh"), name, str(repeat), str(LOGS / f"run1-{name}.log")]
    return subprocess.call(args + ([task] if task else []))


def wait_drain(name: str) -> None:
    """Hold before new work while a logs/drain file exists; a held attempt loses nothing."""
    held = False
    while DRAIN.exists():
        if not held:
            log(event="hold", lane=name, reason="drain")
            held = True
        time.sleep(30)


def spawn_fail_streak(name: str) -> tuple[int, str]:
    """Consecutive newest history rows for this lane whose worker_started read false, and their shared reason.

    Any started, graded or differently-failing row breaks the streak: only
    identical consecutive spawn failures are the breaker's signal.
    """
    streak = 0
    reason = ""
    for r in reversed(rows()):
        if r.get("experiment_id") != f"harness-fit-{name}":
            continue
        if (r.get("obs") or {}).get("worker_started") is not False:
            break
        row_reason = r.get("reason") or ""
        if streak == 0:
            reason = row_reason
        elif row_reason != reason:
            break
        streak += 1
    return streak, reason


def main() -> int:
    LOGS.mkdir(parents=True, exist_ok=True)
    manifest = json.loads(MANIFEST.read_text())
    prices, ceiling = manifest["prices"], manifest["ceiling_usd"]
    log(event="binary", fno_agents=os.environ.get("FNO_AGENTS_BIN", "deployed (PATH)"))
    order = LANES[:]
    random.Random(SEED).shuffle(order)
    log(event="order", seed=SEED, order=order)
    broken: set[str] = set()
    for name in order:
        if any(r.get("experiment_id") == f"harness-fit-{name}" for r in rows()):
            log(event="skip-ran", lane=name)
            continue
        s = spent(prices)
        projection = 3 * TASKS * REPEAT * FLOOR
        log(event="check", lane=name, spent=round(s, 4), projection=projection, ceiling=ceiling)
        if s + projection >= ceiling:
            log(event="stop", lane=name, reason="ceiling")
            return 3
        wait_drain(name)
        log(event="start", lane=name)
        log(event="end", lane=name, rc=lane(name, REPEAT))
        streak, why = spawn_fail_streak(name)
        if streak >= BREAKER_N:
            broken.add(name)
            log(event="stop", lane=name, reason="spawn-failures", streak=streak, error=why)
    tasks = sorted(p.stem for p in (REPO / "evals/harness-fit/bank").glob("*.yaml"))
    for rnd in range(1, ROUNDS + 1):
        counts = done_counts()
        todo = [(n, t, REPEAT - counts.get((f"harness-fit-{n}", t), 0)) for n in TOPUP_LANES for t in tasks]
        todo = [x for x in todo if x[2] > 0]
        log(event="topup-round", round=rnd, missing=sum(x[2] for x in todo))
        if not todo:
            break
        for name, task, missing in todo:
            if name in broken:
                continue
            streak, why = spawn_fail_streak(name)
            if streak >= BREAKER_N:
                broken.add(name)
                log(event="stop", lane=name, reason="spawn-failures", streak=streak, error=why)
                continue
            wait_drain(name)
            s = spent(prices)
            if s + 3 * missing * FLOOR >= ceiling:
                log(event="stop", lane=name, reason="ceiling", spent=round(s, 4))
                return 3
            before = done_counts().get((f"harness-fit-{name}", task), 0)
            rc = lane(name, missing, task)
            gained = done_counts().get((f"harness-fit-{name}", task), 0) - before
            log(event="topup", round=rnd, lane=name, task=task, repeat=missing, gained=gained, rc=rc)
            if gained == 0:
                time.sleep(COOLDOWN)  # refused again: wait for provider lanes to free
    log(event="done")
    return 0


if __name__ == "__main__":
    sys.exit(main())
