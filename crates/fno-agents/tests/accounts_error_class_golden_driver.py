#!/usr/bin/env python3
"""Capture driver for the `accounts_error_class` goldens.

`fno.adapters.providers.error_taxonomy` is the parity oracle until the
adapters package is deleted. Each case runs one taxonomy call with the wall
clock pinned to NOW and freezes the answer in
`tests/golden/accounts_error_class/<case>.json`.
`accounts_error_class_parity.rs` replays the same file against
`accounts::error_class` with `now = NOW`.

    PYTHONPATH=cli/src python3 crates/fno-agents/tests/accounts_error_class_golden_driver.py
"""

from __future__ import annotations

import json
import sys
import time
from pathlib import Path

GOLDEN = Path(__file__).resolve().parent / "golden" / "accounts_error_class"
NOW = 1_789_400_000.0


def stamp(offset_hours: float) -> str:
    """An offset-bearing stamp `offset_hours` after NOW."""
    return time.strftime("%Y-%m-%dT%H:%M:%S", time.gmtime(NOW + offset_hours * 3600)) + "+00:00"


def norm(status, body, **kw) -> dict:
    return dict(op="normalize", status=status, body=body, **kw)


def rule(status, body) -> dict:
    return dict(op="classify_error", status=status, body=body)


def reset(body, tz=None) -> dict:
    return dict(op="parse_reset_stamp", body=body, tz=tz)


CASES = {
    "normalize_529_is_a_5xx_swap": norm(529, ""),
    "normalize_503_is_a_5xx_swap": norm(503, "server error"),
    "normalize_401_is_an_auth_swap": norm(401, "invalid api key"),
    "normalize_403_is_an_auth_swap": norm(403, "forbidden"),
    "normalize_402_is_a_quota_swap": norm(402, "payment required"),
    "normalize_429_is_a_quota_swap": norm(429, ""),
    "normalize_200_rate_limit_body_is_quota": norm(
        200, '{"error":{"type":"overloaded_error","message":"rate limit exceeded for org_..."}}'
    ),
    "normalize_200_quota_exceeded_body_is_quota": norm(200, '{"error":"quota exceeded"}'),
    "normalize_claude_usage_limit_is_quota": norm(None, "Claude usage limit reached. resets at 3pm"),
    "normalize_body_markers_ignore_case": norm(200, "RATE LIMIT exceeded"),
    "normalize_200_html_is_a_parser_error": norm(200, "  <html><body>Server Error</body></html>"),
    "normalize_200_clean_body_is_unknown": norm(200, '{"unexpected":"shape"}'),
    "normalize_404_is_unknown": norm(404, ""),
    "normalize_400_is_unknown": norm(400, "bad input"),
    "normalize_exit_zero_with_quota_stderr_is_quota": norm(None, "rate limit exceeded", exit_code=0),
    "normalize_exit_one_without_a_marker_is_unknown": norm(None, "something else", exit_code=1),
    "normalize_parser_failed_wins_on_a_200": norm(
        200, '{"choices":[{"message":{"content":"hi"}}]}', parser_failed=True
    ),
    "normalize_quota_marker_wins_over_parser_failed": norm(200, "rate limit", parser_failed=True),
    "normalize_truncates_the_body_excerpt": norm(500, "x" * 1024),
    "normalize_passes_a_model_through": norm(429, "rate limit", model="claude-opus-4-7"),
    "normalize_clamps_a_long_model_id": norm(429, "quota exceeded", model="m" * 1024),
    "normalize_refuses_an_empty_model": norm(429, "rate limit", model=""),
    "normalize_offset_stamp_sets_resets_at": norm(
        429, "rate limit exceeded; resets at 2026-09-15T07:19:38+08:00"
    ),
    "normalize_trailing_z_stamp_parses": norm(429, "rate limit; resets_at=2026-09-15T07:19:38Z"),
    "normalize_fractional_offset_stamp_parses": norm(
        429, "rate limit; resets 2026-09-15T07:19:38.521372+00:00"
    ),
    "normalize_colonless_offset_stamp_parses": norm(429, "rate limit; resets 2026-09-15T07:19+0800"),
    "normalize_reset_keyed_epoch_parses": norm(429, '{"error":"rate limit","resets_at":1789408778}'),
    "normalize_bare_number_is_no_epoch": norm(429, "rate limit exceeded (request 1789408778)"),
    "normalize_naive_stamp_without_zone_is_unparsed": norm(
        None, "Claude usage limit reached. Resets 2026-09-15 07:19:38"
    ),
    "normalize_naive_stamp_in_a_zone_parses": norm(
        None, "Claude usage limit reached. Resets 2026-09-15 07:19:38", tz="Asia/Singapore"
    ),
    "normalize_unknown_zone_refuses_like_none": norm(
        None, "usage limit reached. Resets 2026-09-15 07:19:38", tz="Mars/Olympus_Mons"
    ),
    "normalize_no_stamp_sets_neither_field": norm(429, "rate limit exceeded, retry later"),
    "normalize_non_quota_body_still_harvests_a_stamp": norm(
        503, "overloaded; retry after 2026-09-15T07:19:38Z"
    ),
    "normalize_quota_window_name_derives_a_bound": norm(
        None, "Usage limit reached for 5 hour. Your limit will reset at 2026-09-15 18:41:02"
    ),
    "normalize_auth_window_name_derives_nothing": norm(401, "Sessions are valid for 30 minutes"),
    "classify_text_rule_capacity": rule(200, "overloaded due to capacity"),
    "classify_text_wins_over_status": rule(503, "rate limit exceeded"),
    "classify_plain_503_matches_no_rule": rule(503, "internal server error"),
    "classify_absent_body_falls_to_status": rule(429, None),
    "classify_text_ignores_case": rule(200, "RATE LIMIT EXCEEDED"),
    "classify_no_match_is_none": rule(200, "OK"),
    "classify_status_401_cooldown": rule(401, "unauthorized"),
    "classify_unrelated_body_falls_to_status": rule(429, "unrelated body"),
    "classify_substring_in_a_long_body": rule(
        200,
        "Provider returned the following error: too many requests have been issued "
        "in this window. Please retry later.",
    ),
    "classify_usage_limit_earns_backoff": rule(None, "Claude usage limit reached. resets at 3pm"),
    "classify_request_not_allowed_short_cooldown": rule(400, "Request not allowed"),
    "rules_table": dict(op="rules"),
    "reset_millisecond_epoch_is_not_seconds": reset('{"error":"rate limit","ratelimit_reset":1786000000000}'),
    "reset_fourteen_digit_run_is_no_epoch": reset('{"ratelimit_reset":17860000000001}'),
    "reset_far_future_stamp_is_refused": reset("rate limit exceeded; your token expires 2035-01-01T00:00:00Z"),
    "reset_a_real_window_parses": reset(f"rate limit; resets at {stamp(9)}"),
    "reset_a_weekly_window_is_inside_the_horizon": reset(f"usage limit reached; resets {stamp(7 * 24)}"),
    "reset_naive_far_future_stamp_is_refused_and_named": reset(
        "usage limit reached. Resets 2035-01-01 07:19:38", "UTC"
    ),
    "reset_empty_body_is_nothing": reset(""),
    "window_names_five_hours": dict(op="window", body="Usage limit reached for 5 hour"),
    "window_names_one_week": dict(op="window", body="rate limit reached for 1 week"),
    "window_without_a_count_is_one": dict(op="window", body="limit reached for a day, sorry; for hour"),
    "window_of_zero_is_none": dict(op="window", body="limit reached for 0 hours"),
    "window_past_the_horizon_is_none": dict(op="window", body="limit reached for 3 weeks"),
    "window_not_named_is_none": dict(op="window", body="rate limit exceeded"),
}


def run_case(case: dict):
    from fno.adapters.providers import error_taxonomy as et

    op = case["op"]
    if op == "normalize":
        try:
            got = et.normalize(
                case["status"],
                case.get("exit_code"),
                case["body"],
                parser_failed=case.get("parser_failed", False),
                model=case.get("model"),
                reset_timezone=case.get("tz"),
                now=NOW,
            )
        except ValueError as exc:
            return {"error": str(exc)}
        return {
            "error_class": got.error_class.value,
            "raw_status": got.raw_status,
            "raw_exit_code": got.raw_exit_code,
            "body_excerpt": got.body_excerpt,
            "triggers_swap": got.triggers_swap,
            "model": got.model,
            "resets_at": got.resets_at,
            "reset_stamp_unparsed": got.reset_stamp_unparsed,
            "reset_is_derived": got.reset_is_derived,
        }
    if op == "classify_error":
        got = et.classify_error(case["status"], case["body"])
        return None if got is None else _rule(got)
    if op == "rules":
        return [_rule(r) for r in et.ERROR_RULES]
    if op == "parse_reset_stamp":
        return list(et._parse_reset_stamp(case["body"], case["tz"]))
    if op == "window":
        return et.reset_window_seconds_from(case["body"])
    raise ValueError(op)


def _rule(r) -> dict:
    return {"text": r.text, "status": r.status, "cooldown_ms": r.cooldown_ms, "backoff": r.backoff}


def main() -> int:
    time.time = lambda: NOW
    GOLDEN.mkdir(parents=True, exist_ok=True)
    for stale in GOLDEN.glob("*.json"):
        stale.unlink()
    for name, case in CASES.items():
        (GOLDEN / f"{name}.json").write_text(
            json.dumps({**case, "expect": run_case(case)}) + "\n", encoding="utf-8"
        )
    print(f"wrote {len(CASES)} cases to {GOLDEN}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
