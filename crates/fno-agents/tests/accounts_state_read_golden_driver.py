#!/usr/bin/env python3
"""Capture driver for the `accounts_state_read` goldens.

The read side of `fno.adapters.providers.runtime_state` is the parity oracle
until the adapters package is deleted. Each case writes a runtime-state file,
pins it with `FNO_RUNTIME_STATE_PATH`, runs one read at `now = NOW`, and
freezes the answer in `tests/golden/accounts_state_read/<case>.json`.
`accounts_state_read_parity.rs` replays the same file against
`accounts::state`.

    PYTHONPATH=cli/src uv run --project cli python crates/fno-agents/tests/accounts_state_read_golden_driver.py
"""

from __future__ import annotations

import json
import os
import shutil
import sys
import tempfile
from pathlib import Path

GOLDEN = Path(__file__).resolve().parent / "golden" / "accounts_state_read"
NOW = 1_789_400_000.0
FRESH = NOW - 60
OLD = NOW - 4000


def usage(*windows, probed_at=FRESH, **extra):
    return {"probed_at": probed_at, "windows": list(windows), **extra}


def win(label, used_pct, resets_at=None):
    return {"label": label, "used_pct": used_pct, "resets_at": resets_at}


FULL = {
    "schema_version": 2,
    "provider_health": {
        "a1": {"backoff_level": 3, "rate_limited_until": NOW + 600, "last_error_at": FRESH,
               "model_locks": {"opus": NOW + 300}},
        "old": {"backoff_level": 1, "rate_limited_until": NOW + 600, "last_error_at": OLD},
    },
    "combo_cursors": {
        "fast": {"cursor_index": 1, "consecutive_use_count": 2, "providers_hash": "h1",
                 "last_rotated_at": FRESH},
        "stale": {"cursor_index": 0, "consecutive_use_count": 1, "providers_hash": "h2",
                  "last_rotated_at": NOW - 90000},
    },
    "usage": {"a1": usage(win("5h", 40.0, NOW + 3600), source="oauth-endpoint",
                          confidence="exact")},
    "windows_opened": {"zai": {"window": {"opened_at": NOW - 600, "warned": False}}},
}

COERCE = {
    "provider_health": {
        "high": {"backoff_level": 99, "last_error_at": FRESH},
        "low": {"backoff_level": -3, "last_error_at": FRESH},
        "strlvl": {"backoff_level": "3", "last_error_at": FRESH},
        "floatlvl": {"backoff_level": 2.7, "last_error_at": FRESH},
        "nulllvl": {"backoff_level": None},
        "badrlu": {"rate_limited_until": "abc"},
        "strrlu": {"rate_limited_until": str(NOW + 100)},
        "notdict": [1, 2],
        "": {"backoff_level": 1},
        "locks": {"last_error_at": FRESH, "model_locks": {
            "": NOW + 5, "neg": -1, "zero": 0, "word": "soon", "ok": str(NOW + 9)}},
        "listlocks": {"last_error_at": FRESH, "model_locks": [1]},
    },
    "usage": {
        "strpct": usage(win("5h", "50")),
        "clamp": usage(win("5h", 130), win("weekly", -5)),
        "nolabel": usage({"used_pct": 10}),
        "nullreset": usage(win("5h", 10, None), partial=1, confidence=""),
        "badwindows": {"probed_at": FRESH, "windows": "x"},
        "noprobe": {"windows": []},
        "numlabel": usage(win(5, 10)),
    },
    "combo_cursors": {
        "nohash": {"cursor_index": 0, "consecutive_use_count": 1, "last_rotated_at": FRESH},
        "negidx": {"cursor_index": -1, "consecutive_use_count": 1, "providers_hash": "h",
                   "last_rotated_at": FRESH},
        "emptyhash": {"cursor_index": 0, "consecutive_use_count": 1, "providers_hash": "",
                      "last_rotated_at": FRESH},
        "strs": {"cursor_index": "2", "consecutive_use_count": 1.9, "providers_hash": 7,
                 "last_rotated_at": str(FRESH)},
    },
    "windows_opened": {
        "listy": [1],
        "noopen": {"window": {"warned": True}},
        "mixed": {"window": {"opened_at": str(NOW - 5), "warned": 1}, "bad": "x"},
    },
}

MODELS = {"provider_health": {
    "m": {"last_error_at": FRESH, "model_locks": {"opus": NOW + 300, "sonnet": NOW + 300}},
}}

WINDOWS = {"windows_opened": {
    "zai": {"window": {"opened_at": NOW - 600, "warned": True}},
    "gone": {"window": {"opened_at": NOW - 20000}},
}}


def read_state(state):
    return dict(op="read_state", state=state)


def cooldown(pid, model=None, state=None):
    return dict(op="is_in_cooldown", state=state or FULL, id=pid, model=model)



CASES = {
    "read_state_missing_file_is_empty": read_state(None),
    "read_state_empty_file_is_empty": read_state(""),
    "read_state_non_object_is_empty": read_state([1, 2]),
    "read_state_unparseable_is_empty": read_state("{not json"),
    "read_state_full_drops_stale_health_and_cursors": read_state(FULL),
    "read_state_coerces_and_drops_malformed_entries": read_state(COERCE),
    "read_state_v1_without_schema_version": read_state({"provider_health": {}}),
    "cooldown_model_lock": cooldown("a1", "opus"),
    "cooldown_provider_lock_covers_any_model": cooldown("a1", "sonnet"),
    "cooldown_stale_record_is_free": cooldown("old"),
    "cooldown_unknown_id_is_free": cooldown("ghost"),
    "cooldown_model_lock_holds_its_model": cooldown("m", "sonnet", MODELS),
    "cooldown_model_lock_leaves_other_models_free": cooldown("m", "haiku", MODELS),
    "cooldown_model_locks_are_not_a_provider_lock": cooldown("m", None, MODELS),
    "cooldown_expired_lock_is_free": cooldown(
        "x", state={"provider_health": {"x": {"rate_limited_until": NOW - 1, "last_error_at": FRESH}}}
    ),
    "read_usage_fresh": dict(op="read_usage", state=FULL, id="a1", ttl=300.0),
    "read_usage_stale_past_ttl": dict(op="read_usage", state=FULL, id="a1", ttl=30.0),
    "read_usage_absent": dict(op="read_usage", state=FULL, id="ghost", ttl=300.0),
    "read_cursor_valid": dict(op="read_cursor", state=FULL, combo="fast", hash="h1"),
    "read_cursor_edited_combo": dict(op="read_cursor", state=FULL, combo="fast", hash="other"),
    "read_cursor_stale": dict(op="read_cursor", state=FULL, combo="stale", hash="h2"),
    "read_cursor_absent": dict(op="read_cursor", state=FULL, combo="ghost", hash="h1"),
}


def _snapshot(s):
    if s is None:
        return None
    return {
        "windows": [
            {"label": w.label, "used_pct": w.used_pct, "resets_at": w.resets_at} for w in s.windows
        ],
        "probed_at": s.probed_at,
        "source": s.source,
        "partial": s.partial,
        "confidence": s.confidence,
    }


def _cursor(c):
    if c is None:
        return None
    return {
        "cursor_index": c.cursor_index,
        "consecutive_use_count": c.consecutive_use_count,
        "providers_hash": c.providers_hash,
        "last_rotated_at": c.last_rotated_at,
    }


def run_case(case: dict, path: Path):
    from fno.adapters.providers import runtime_state as rs

    op = case["op"]
    if op == "read_state":
        st = rs.read_state(now=NOW)
        return {
            "provider_health": {
                k: {
                    "backoff_level": h.backoff_level,
                    "rate_limited_until": h.rate_limited_until,
                    "last_error_at": h.last_error_at,
                    "model_locks": h.model_locks,
                }
                for k, h in st.provider_health.items()
            },
            "combo_cursors": {k: _cursor(c) for k, c in st.combo_cursors.items()},
            "usage": {k: _snapshot(s) for k, s in st.usage.items()},
            "windows_opened": st.windows_opened,
            "schema_version": st.schema_version,
        }
    if op == "is_in_cooldown":
        return rs.is_in_cooldown(case["id"], model=case["model"], now=NOW)
    if op == "read_usage":
        return _snapshot(rs.read_usage(case["id"], ttl_seconds=case["ttl"], now=NOW))
    if op == "read_cursor":
        return _cursor(rs.read_cursor(case["combo"], case["hash"], now=NOW))
    raise ValueError(op)


# Cases whose Python oracle is deleted: runtime_state.project_window went
# with the window port, and the headroom verdict went with the quota-signal
# deletion (the verdict lives in accounts::state now). Their goldens stay
# frozen, and the Rust parity replays them; a re-run never rewrites or
# removes them.
FROZEN = {
    "project_window_nothing_recorded",
    "project_window_without_a_length",
    "project_window_rolled_window",
    "project_window_open_window",
    "headroom_lock_newer_than_probe",
    "headroom_newer_probe_overrides_older_lock",
    "headroom_exhausted_window",
    "headroom_partial_is_low",
    "headroom_reset_window_reads_ok",
    "headroom_hot_window_is_low_at_threshold",
    "headroom_hot_window_is_ok_below_threshold",
    "headroom_cool_window_is_ok",
    "headroom_lock_without_usage_is_exhausted",
    "headroom_stale_usage_is_unknown",
    "headroom_absent_is_unknown",
    "headroom_empty_windows_is_unknown",
    "headroom_stale_lock_record_is_unknown",
    "headroom_resetless_full_window_is_exhausted",
    "headroom_short_ttl_makes_usage_stale",
    "headroom_live_lock_beside_stale_window_is_exhausted",
}


def main() -> int:
    GOLDEN.mkdir(parents=True, exist_ok=True)
    for stale in GOLDEN.glob("*.json"):
        if stale.stem not in FROZEN:
            stale.unlink()
    for name, case in CASES.items():
        tmp = Path(tempfile.mkdtemp(prefix="acct-state-"))
        path = tmp / "provider-runtime-state.json"
        state = case["state"]
        if isinstance(state, str):
            path.write_text(state, encoding="utf-8")
        elif state is not None:
            path.write_text(json.dumps(state), encoding="utf-8")
        os.environ["FNO_RUNTIME_STATE_PATH"] = str(path)
        try:
            expect = run_case(case, path)
        finally:
            shutil.rmtree(tmp, ignore_errors=True)
        (GOLDEN / f"{name}.json").write_text(
            json.dumps({**case, "expect": expect}) + "\n", encoding="utf-8"
        )
    print(f"wrote {len(CASES)} cases to {GOLDEN}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
