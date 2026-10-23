#!/usr/bin/env python3
"""Capture driver for the `accounts_state_write` goldens.

The write side of `fno.adapters.providers.runtime_state` was the parity oracle
when the writers moved to Rust; its answers are frozen here. Each case writes a
runtime-state file, pins it with `FNO_RUNTIME_STATE_PATH`, runs one writer at
`now = NOW` and freezes both the writer's answer and the file it left under
`expect`. A `locked` case holds the update lock first, so the writer takes its
timeout branch. `accounts_state_write_parity.rs` replays every case against
`accounts::state`.

    PYTHONPATH=cli/src uv run --project cli python crates/fno-agents/tests/accounts_state_write_golden_driver.py

The capture needs the Python writers this port replaced: run it from a
checkout before the transport commit to re-freeze.
"""

from __future__ import annotations

import dataclasses
import json
import os
import shutil
import sys
import tempfile
import types
from pathlib import Path

GOLDEN = Path(__file__).resolve().parent / "golden" / "accounts_state_write"
NOW = 1_789_400_000.0
FRESH = NOW - 60
OLD = NOW - 4000
LOCKED_TIMEOUT = 0.2


def health(level=0, rlu=None, at=FRESH, locks=None):
    out = {"backoff_level": level, "rate_limited_until": rlu, "last_error_at": at}
    if locks is not None:
        out["model_locks"] = locks
    return out


def cursor(idx, count, h="h1", at=FRESH):
    return {"cursor_index": idx, "consecutive_use_count": count, "providers_hash": h,
            "last_rotated_at": at}


def snap(used=40.0, probed_at=FRESH, resets_at=NOW + 3600):
    return {"windows": [{"label": "5h", "used_pct": used, "resets_at": resets_at}],
            "probed_at": probed_at, "source": "oauth-endpoint", "partial": False,
            "confidence": "exact"}


def opened(at, warned=False):
    return {"opened_at": at, "warned": warned}


MIXED = {
    "schema_version": 2,
    "provider_health": {"a1": health(3, NOW + 600, FRESH, {"opus": NOW + 300}),
                        "old": health(1, NOW + 600, OLD)},
    "combo_cursors": {"fast": cursor(1, 2), "stale": cursor(0, 1, "h2", NOW - 90000)},
    "usage": {"a1": snap()},
    "windows_opened": {"zai": {"window": opened(NOW - 600)}},
}

BACKOFF = {"backoff": True, "cooldown_ms": None}
FIXED = {"backoff": False, "cooldown_ms": 30000}

CASES: dict[str, dict] = {
    # update-health
    "update_empty_file_first_backoff": {"op": "update-health", "state": None, "id": "a1", "rule": BACKOFF},
    "update_progression_from_level_3": {"op": "update-health", "state": {"provider_health": {"a1": health(3)}}, "id": "a1", "rule": BACKOFF},
    "update_level_caps_at_15": {"op": "update-health", "state": {"provider_health": {"a1": health(15)}}, "id": "a1", "rule": BACKOFF},
    "update_level_14_hits_the_max_cooldown": {"op": "update-health", "state": {"provider_health": {"a1": health(14)}}, "id": "a1", "rule": BACKOFF},
    "update_fixed_rule_keeps_level": {"op": "update-health", "state": {"provider_health": {"a1": health(2)}}, "id": "a1", "rule": FIXED},
    "update_model_writes_only_model_lock": {"op": "update-health", "state": {"provider_health": {"a1": health(1, NOW + 50)}}, "id": "a1", "rule": BACKOFF, "model": "opus"},
    "update_model_with_harvested_reset": {"op": "update-health", "state": None, "id": "a1", "rule": BACKOFF, "model": "opus", "resets_at": NOW + 32400},
    "update_harvested_reset_is_the_lock": {"op": "update-health", "state": None, "id": "a1", "rule": BACKOFF, "resets_at": NOW + 32400},
    "update_past_reset_never_shortens": {"op": "update-health", "state": None, "id": "a1", "rule": BACKOFF, "resets_at": NOW - 10},
    "update_harvest_stamps_the_window": {"op": "update-health", "state": None, "id": "zai", "rule": BACKOFF, "resets_at": NOW + 3600, "window_minutes": 300},
    "update_harvest_same_window_keeps_warned": {"op": "update-health", "state": {"windows_opened": {"zai": {"window": opened(NOW + 3600 - 18000 + 0.5, True)}}}, "id": "zai", "rule": BACKOFF, "resets_at": NOW + 3600, "window_minutes": 300},
    "update_harvest_new_window_rearms": {"op": "update-health", "state": {"windows_opened": {"zai": {"window": opened(NOW - 40000, True)}}}, "id": "zai", "rule": BACKOFF, "resets_at": NOW + 3600, "window_minutes": 300},
    "update_harvest_keeps_a_sibling_label": {"op": "update-health", "state": {"windows_opened": {"zai": {"weekly": opened(NOW - 9000, True)}}}, "id": "zai", "rule": BACKOFF, "resets_at": NOW + 3600, "window_minutes": 300},
    "update_harvest_without_a_length_stamps_nothing": {"op": "update-health", "state": None, "id": "zai", "rule": BACKOFF, "resets_at": NOW + 3600},
    "update_drops_stale_health_and_cursors": {"op": "update-health", "state": MIXED, "id": "b2", "rule": FIXED},
    "update_keeps_schema_1": {"op": "update-health", "state": {"schema_version": 1, "provider_health": {}}, "id": "a1", "rule": BACKOFF},
    "update_coerces_a_hand_edited_entry": {"op": "update-health", "state": {"provider_health": {"a1": {"backoff_level": "2", "rate_limited_until": str(NOW + 5), "last_error_at": FRESH, "model_locks": {"opus": str(NOW + 9), "bad": -1}}, "x": [1]}}, "id": "b2", "rule": FIXED},
    "update_corrupt_file_reads_empty": {"op": "update-health", "state": "{not json", "id": "a1", "rule": BACKOFF},
    "update_locked_returns_last_known_good": {"op": "update-health", "state": {"provider_health": {"a1": health(4, NOW + 99, OLD)}}, "id": "a1", "rule": BACKOFF, "locked": True},
    "update_locked_absent_returns_zero_state": {"op": "update-health", "state": None, "id": "a1", "rule": BACKOFF, "locked": True},
    # write-usage
    "usage_first_snapshot": {"op": "write-usage", "state": None, "id": "a1", "snapshot": snap(55.5)},
    "usage_replaces_and_drops_stale": {"op": "write-usage", "state": MIXED, "id": "a1", "snapshot": snap(91.0)},
    "usage_clamps_and_keeps_a_null_reset": {"op": "write-usage", "state": None, "id": "c3", "snapshot": snap(140.0, resets_at=None)},
    "usage_locked_is_not_persisted": {"op": "write-usage", "state": MIXED, "id": "a1", "snapshot": snap(10.0), "locked": True},
    # advance-cursor
    "cursor_fresh_starts_at_zero": {"op": "advance-cursor", "state": None, "combo": "fast", "sticky": 3, "hash": "h1", "count": 3},
    "cursor_sticks_under_the_limit": {"op": "advance-cursor", "state": {"combo_cursors": {"fast": cursor(1, 2)}}, "combo": "fast", "sticky": 3, "hash": "h1", "count": 3},
    "cursor_rolls_at_the_limit": {"op": "advance-cursor", "state": {"combo_cursors": {"fast": cursor(1, 3)}}, "combo": "fast", "sticky": 3, "hash": "h1", "count": 3},
    "cursor_wraps_modulo": {"op": "advance-cursor", "state": {"combo_cursors": {"fast": cursor(2, 1)}}, "combo": "fast", "sticky": 1, "hash": "h1", "count": 3},
    "cursor_hash_mismatch_resets": {"op": "advance-cursor", "state": {"combo_cursors": {"fast": cursor(2, 1, "old")}}, "combo": "fast", "sticky": 1, "hash": "h1", "count": 3},
    "cursor_stale_resets": {"op": "advance-cursor", "state": MIXED, "combo": "stale", "sticky": 1, "hash": "h2", "count": 2},
    "cursor_writes_schema_2": {"op": "advance-cursor", "state": {"schema_version": 1}, "combo": "fast", "sticky": 2, "hash": "h1", "count": 2},
    "cursor_locked_computes_lock_free": {"op": "advance-cursor", "state": {"combo_cursors": {"fast": cursor(0, 2)}}, "combo": "fast", "sticky": 2, "hash": "h1", "count": 2, "locked": True},
    # stamp-window
    "stamp_first_open": {"op": "stamp-window", "state": None, "id": "zai", "opened_at": NOW - 100, "window_minutes": 300},
    "stamp_inside_the_window_is_a_no_op": {"op": "stamp-window", "state": {"windows_opened": {"zai": {"window": opened(NOW - 600, True)}}}, "id": "zai", "opened_at": NOW, "window_minutes": 300},
    "stamp_after_the_window_rolled": {"op": "stamp-window", "state": {"windows_opened": {"zai": {"window": opened(NOW - 20000, True)}}}, "id": "zai", "opened_at": NOW, "window_minutes": 300},
    "stamp_without_a_length_overwrites": {"op": "stamp-window", "state": {"windows_opened": {"zai": {"window": opened(NOW - 600, True)}}}, "id": "zai", "opened_at": NOW},
    "stamp_a_named_label": {"op": "stamp-window", "state": MIXED, "id": "zai", "opened_at": NOW, "label": "weekly", "window_minutes": 300},
    "stamp_locked_is_not_written": {"op": "stamp-window", "state": None, "id": "zai", "opened_at": NOW, "window_minutes": 300, "locked": True},
    # mark-warned
    "warn_sets_the_flag_once": {"op": "mark-warned", "state": MIXED, "id": "zai"},
    "warn_already_set_is_false": {"op": "mark-warned", "state": {"windows_opened": {"zai": {"window": opened(NOW - 600, True)}}}, "id": "zai"},
    "warn_nothing_recorded_is_false": {"op": "mark-warned", "state": MIXED, "id": "a1"},
    "warn_locked_is_false": {"op": "mark-warned", "state": MIXED, "id": "zai", "locked": True},
    # fold of a legacy per-cwd file
    "fold_newer_legacy_merges_newest_wins": {"op": "fold", "state": MIXED, "legacy_newer": True, "legacy": {
        "schema_version": 1,
        "provider_health": {"a1": health(1, NOW + 5, FRESH - 100), "b2": health(2, NOW + 50, FRESH + 10), "old2": health(1, None, OLD)},
        "combo_cursors": {"fast": cursor(2, 1, "h1", FRESH + 5), "slow": cursor(0, 1, "h9")},
        "usage": {"a1": snap(70.0, FRESH - 30), "b2": snap(12.0)},
        "windows_opened": {"zai": {"window": opened(NOW - 900), "weekly": opened(NOW - 50000)}, "b2": {"window": opened(NOW - 10)}},
    }},
    "fold_skipped_when_the_target_is_newer": {"op": "fold", "state": MIXED, "legacy_newer": False, "legacy": {"provider_health": {"b2": health(2)}}},
    "fold_into_a_missing_target": {"op": "fold", "state": None, "legacy_newer": True, "legacy": MIXED},
}


def _health(h):
    return None if h is None else dataclasses.asdict(h)


def run_case(rs, case: dict, path: Path):
    minutes = case.get("window_minutes")
    rs._record_window_seconds = lambda _pid: float(minutes) * 60.0 if minutes else None
    op = case["op"]
    if op == "update-health":
        from fno.adapters.providers.error_taxonomy import ErrorRule

        rule = ErrorRule(status=429, **case["rule"])
        return _health(rs.update_provider_health(
            case["id"], rule, model=case.get("model"), now=NOW, resets_at=case.get("resets_at")
        ))
    if op == "write-usage":
        from fno.adapters.providers.usage import UsageSnapshot, UsageWindow

        s = case["snapshot"]
        return rs.write_usage_snapshot(UsageSnapshot(
            provider_id=case["id"],
            windows=tuple(UsageWindow(**w) for w in s["windows"]),
            probed_at=s["probed_at"], source=s["source"], partial=s["partial"],
            confidence=s["confidence"],
        ), now=NOW)
    if op == "advance-cursor":
        return dataclasses.asdict(rs.advance_cursor(
            case["combo"], sticky_limit=case["sticky"], providers_hash=case["hash"],
            providers_count=case["count"], now=NOW,
        ))
    if op == "stamp-window":
        return rs.stamp_window_open(case["id"], case["opened_at"], label=case.get("label", "window"))
    if op == "mark-warned":
        return rs.mark_window_warned(case["id"])
    if op == "fold":
        legacy = path.parent / "legacy.json"
        legacy.write_text(json.dumps(case["legacy"]), encoding="utf-8")
        older, newer = NOW - 50, NOW
        if path.exists():
            os.utime(path, (newer, newer) if not case["legacy_newer"] else (older, older))
        os.utime(legacy, (newer, newer) if case["legacy_newer"] else (older, older))
        rs._fold_one_legacy_file(path, legacy)
        return None
    raise ValueError(op)


def main() -> int:
    import filelock

    from fno.adapters.providers import runtime_state as rs

    rs.time = types.SimpleNamespace(time=lambda: NOW)
    GOLDEN.mkdir(parents=True, exist_ok=True)
    for stale in GOLDEN.glob("*.json"):
        stale.unlink()
    for name, case in CASES.items():
        tmp = Path(tempfile.mkdtemp(prefix="acct-state-w-"))
        path = tmp / "provider-runtime-state.json"
        state = case["state"]
        if isinstance(state, str):
            path.write_text(state, encoding="utf-8")
        elif state is not None:
            path.write_text(json.dumps(state), encoding="utf-8")
        os.environ["FNO_RUNTIME_STATE_PATH"] = str(path)
        held = None
        rs.LOCK_TIMEOUT_SECONDS = 5
        if case.get("locked"):
            held = filelock.FileLock(str(rs._lock_path(path)))
            held.acquire()
            rs.LOCK_TIMEOUT_SECONDS = LOCKED_TIMEOUT
        try:
            result = run_case(rs, case, path)
            text = path.read_text(encoding="utf-8") if path.exists() else None
            try:
                file = None if text is None else json.loads(text)
            except ValueError:
                file = text
        finally:
            if held is not None:
                held.release()
            shutil.rmtree(tmp, ignore_errors=True)
        (GOLDEN / f"{name}.json").write_text(
            json.dumps({**case, "expect": {"result": result, "file": file}}) + "\n",
            encoding="utf-8",
        )
    print(f"wrote {len(CASES)} cases to {GOLDEN}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
