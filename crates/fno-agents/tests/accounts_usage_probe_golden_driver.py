#!/usr/bin/env python3
"""Capture driver for the `accounts_usage_probe` goldens.

`fno.adapters.providers.usage` is the parity oracle. Each case runs one parser
or one probe with its network, Keychain and route reads replaced by the case's
own answers, at `now = NOW`, and freezes the answer under `expect`.
`accounts_usage_probe_parity.rs` replays every case against
`accounts::usage_probe`. `{dir}` in a case names a scratch dir both sides make.

A case with `delta` is a recorded behavior change: `expect` is the Rust answer,
captured with the fix applied to the Python leg, and `python_expect` is what
the Python leg answers today.

    PYTHONPATH=cli/src uv run --project cli python crates/fno-agents/tests/accounts_usage_probe_golden_driver.py
"""

from __future__ import annotations

import dataclasses
import io
import json
import os
import shutil
import sys
import tempfile
import urllib.error
from pathlib import Path

GOLDEN = Path(__file__).resolve().parent / "golden" / "accounts_usage_probe"
FIXTURES = Path(__file__).resolve().parents[3] / "cli" / "tests" / "fixtures"
NOW = 1_789_400_000.0
MS = 1000


def blob(token):
    return json.dumps({"claudeAiOauth": {"accessToken": token, "refreshToken": "r"}})


def zai(*limits, success=True, code=200):
    return {"success": success, "code": code, "data": {"limits": list(limits)}}


def row(kind, unit, number, pct, reset=None, **extra):
    out = {"type": kind, "unit": unit, "number": number, "percentage": pct}
    if reset is not None:
        out["nextResetTime"] = reset
    return {**out, **extra}


CLAUDE_OK = {
    "five_hour": {"utilization": 41.0, "resets_at": "2026-09-14T12:00:00.521372+00:00"},
    "seven_day": {"utilization": 103, "resets_at": "2026-09-18T00:00:00Z"},
    "seven_day_opus": {"utilization": "88.5", "resets_at": NOW + 600},
    "seven_day_sonnet": None,
    "tangelo": {"utilization": 99, "resets_at": "2026-09-18T00:00:00Z"},
    "five_hour_extra": {"utilization": 5},
}

CASES: dict[str, dict] = {
    # claude parser
    "claude_verified_shape": {"op": "claude_windows", "payload": CLAUDE_OK},
    "claude_not_an_object": {"op": "claude_windows", "payload": [1, 2]},
    "claude_bad_values_skip": {"op": "claude_windows", "payload": {
        "five_hour": {"utilization": "lots", "resets_at": "2026-09-14T12:00:00+00:00"},
        "seven_day": {"utilization": 10, "resets_at": "not a date"},
        "seven_day_x": {"utilization": None, "resets_at": "2026-09-14T12:00:00+00:00"},
        "seven_day_y": {"utilization": -4, "resets_at": 1},
    }},
    # codex parser
    "codex_position_is_not_the_span": {"op": "codex_windows", "payload": {
        "timestamp": "t", "type": "event_msg", "payload": {"rate_limits": {
            "primary": {"used_percent": 12.5, "resets_at": NOW + 9000, "window_minutes": 10080},
            "secondary": None}}}},
    "codex_two_windows": {"op": "codex_windows", "payload": {"rate_limits": {
        "primary": {"used_percent": 40, "resets_at": NOW + 100, "window_minutes": 300},
        "secondary": {"used_percent": "7", "resets_at": str(NOW + 900), "window_minutes": 10080}}}},
    "codex_odd_spans": {"op": "codex_windows", "payload": {"rate_limits": {
        "primary": {"used_percent": 1, "resets_at": NOW, "window_minutes": 10080.0},
        "secondary": {"used_percent": 2, "resets_at": NOW, "window_minutes": True}}}},
    "codex_missing_fields_skip": {"op": "codex_windows", "payload": {"rate_limits": {
        "primary": {"resets_at": NOW, "window_minutes": 300},
        "secondary": {"used_percent": 9, "resets_at": None}}}},
    "codex_nested_in_a_list": {"op": "codex_windows", "payload": {"a": [{"b": 1}, {"rate_limits": {
        "primary": {"used_percent": 3, "resets_at": NOW, "window_minutes": 4320}}}]}},
    "codex_no_rate_limits": {"op": "codex_windows", "payload": {"rate_limits": [1]}},
    # z.ai parser
    "zai_live_two_rows": {"op": "zai_windows", "payload": zai(
        row("TOKENS_LIMIT", 3, 5, 61, reset=int((NOW + 3600) * MS), usage=1000, remaining=300, currentValue=650),
        row("TIME_LIMIT", 6, 1, 4, reset=int((NOW + 86400) * MS), usage=4000, currentValue=160))},
    "zai_missing_reset_is_kept": {"op": "zai_windows", "payload": zai(row("TOKENS_LIMIT", 3, 5, 97))},
    "zai_far_reset_is_dropped": {"op": "zai_windows", "payload": zai(
        row("TOKENS_LIMIT", 3, 5, 10, reset=int((NOW + 40 * 3600) * MS)))},
    "zai_bad_rows_mark_partial": {"op": "zai_windows", "payload": zai(
        row("TOKENS_LIMIT", 9, 5, 10),
        row("TOKENS_LIMIT", 3, 0, 10),
        row("TOKENS_LIMIT", 3, 5, 10.5),
        row("TOKENS_LIMIT", 3, 5, 10, usage="many"),
        row("CREDIT_LIMIT", 1, 1, 22),
        {"type": "SOMETHING_ELSE", "unit": 3},
        "not a row")},
    "zai_refused_response": {"op": "zai_windows", "payload": zai(row("TOKENS_LIMIT", 3, 5, 1), success=False)},
    "zai_float_code_is_ok": {"op": "zai_windows", "payload": zai(row("TOKENS_LIMIT", 3, 5, 1), code=200.0)},
    "zai_bool_code_refused": {"op": "zai_windows", "payload": zai(row("TOKENS_LIMIT", 3, 5, 1), code=True)},
    "zai_unit_5_is_a_month": {"op": "zai_windows", "delta": "z.ai unit 5 is a month, not a minute", "payload": zai(
        row("TIME_LIMIT", 5, 1, 0, reset=int((NOW + 38 * 3600) * MS), usage=4000, currentValue=9, remaining=3991))},
    "zai_live_capture_20260827": {"op": "zai_windows", "now": 1787818296.0, "delta": "z.ai unit 5 is a month, not a minute",
        "payload": json.loads((FIXTURES / "zai_quota_live_20260827.json").read_text())},
    "zai_live_capture_confidence": {"op": "zai_confidence", "payload": json.loads((FIXTURES / "zai_quota_live_20260827.json").read_text())},
    "codex_live_capture_20260827": {"op": "codex_windows", "payload": json.loads((FIXTURES / "codex_rate_limits_20260827.json").read_text())},
    "zai_confidence_mixed": {"op": "zai_confidence", "payload": zai(
        row("TOKENS_LIMIT", 3, 5, 1, usage=10), row("TIME_LIMIT", 6, 1, 1))},
    "zai_confidence_exact": {"op": "zai_confidence", "payload": zai(row("TOKENS_LIMIT", 3, 5, 1, usage=10))},
    "zai_confidence_empty": {"op": "zai_confidence", "payload": zai()},
    # labels and timestamps
    "labels": {"op": "labels", "minutes": [None, 1, 60, 300, 1440, 4320, 10080, 43200, 90, 120]},
    "iso": {"op": "iso", "values": ["2026-07-12T02:09:59.521372+00:00", "2026-07-12T02:09:59Z",
                                    "2026-07-12T04:09:59+02:00", 1789400000, 12.5, True, "", "junk", None]},
    # probe_usage_detail
    "detail_no_probe_for_gemini": {"op": "detail", "record": {"id": "g1", "name": "g", "harness": "gemini", "auth": "oauth_dir", "credentials_source": "{dir}/g"}},
    "detail_api_key_has_no_bearer": {"op": "detail", "record": {"id": "k1", "name": "k", "harness": "claude", "auth": "api_key", "env": {"ANTHROPIC_API_KEY": "sk"}}},
    "detail_managed_off_slot_is_unattributed": {"op": "detail", "record": {"id": "m1", "name": "m", "harness": "claude", "auth": "managed"}},
    "codex_newest_session_wins": {"op": "detail", "record": {"id": "c1", "name": "c", "harness": "codex", "auth": "oauth_dir", "credentials_source": "{dir}/codex"},
        "files": [
            {"rel": "codex/sessions/2026/a.jsonl", "age": 500, "text": json.dumps({"rate_limits": {"primary": {"used_percent": 90, "resets_at": NOW, "window_minutes": 300}}})},
            {"rel": "codex/sessions/2026/b.jsonl", "age": 5, "text": "\n".join([
                json.dumps({"rate_limits": {"primary": {"used_percent": 20, "resets_at": NOW + 50, "window_minutes": 300}}}),
                "{broken rate_limits",
                json.dumps({"type": "x"}),
            ])},
            {"rel": "codex/sessions/2026/c.txt", "age": 1, "text": "rate_limits"},
        ]},
    "codex_cold_home_is_unknown": {"op": "detail", "record": {"id": "c2", "name": "c", "harness": "codex", "auth": "oauth_dir", "credentials_source": "{dir}/empty"}, "files": []},
    "claude_first_bearer_rejected_second_reads": {"op": "detail", "record": {"id": "o1", "name": "o", "harness": "claude", "auth": "oauth_dir", "credentials_source": "{dir}/o1"},
        "blobs": [blob("tok-a"), blob("tok-b")], "http": [{"status": 401, "body": "nope"}, {"status": 200, "body": json.dumps(CLAUDE_OK)}]},
    "claude_every_bearer_rejected": {"op": "detail", "record": {"id": "o1", "name": "o", "harness": "claude", "auth": "oauth_dir", "credentials_source": "{dir}/o1"},
        "blobs": [blob("tok-a")], "http": [{"status": 403, "body": ""}]},
    "claude_server_error": {"op": "detail", "record": {"id": "o1", "name": "o", "harness": "claude", "auth": "oauth_dir", "credentials_source": "{dir}/o1"},
        "blobs": [blob("tok-a")], "http": [{"status": 500, "body": "{}"}]},
    "claude_malformed_body": {"op": "detail", "record": {"id": "o1", "name": "o", "harness": "claude", "auth": "oauth_dir", "credentials_source": "{dir}/o1"},
        "blobs": [blob("tok-a")], "http": [{"status": 200, "body": "<html>"}]},
    "claude_no_credential": {"op": "detail", "record": {"id": "o1", "name": "o", "harness": "claude", "auth": "oauth_dir", "credentials_source": "{dir}/o1"},
        "blobs": [], "http": []},
    "zai_route_reads_the_quota": {"op": "detail", "record": {"id": "z1", "name": "z", "harness": "claude", "auth": "api_key", "route": "zai/glm-4.6"},
        "route_env": {"ANTHROPIC_AUTH_TOKEN": "zk", "ANTHROPIC_BASE_URL": "https://API.Z.AI/api/anthropic"},
        "http": [{"status": 200, "body": json.dumps(zai(row("TOKENS_LIMIT", 3, 5, 61, reset=int((NOW + 3600) * MS)), row("TIME_LIMIT", 9, 1, 1)))}]},
    "zai_route_on_a_foreign_host": {"op": "detail", "record": {"id": "z1", "name": "z", "harness": "claude", "auth": "api_key", "route": "zai/glm-4.6"},
        "route_env": {"ANTHROPIC_API_KEY": "zk", "ANTHROPIC_BASE_URL": "https://example.com/api"}, "http": []},
    "zai_route_refused": {"op": "detail", "record": {"id": "z1", "name": "z", "harness": "claude", "auth": "api_key", "route": "zai/glm-4.6"},
        "route_env": None, "http": []},
    "zai_route_http_error": {"op": "detail", "record": {"id": "z1", "name": "z", "harness": "claude", "auth": "api_key", "route": "zai/glm-4.6"},
        "route_env": {"ANTHROPIC_AUTH_TOKEN": "zk", "ANTHROPIC_BASE_URL": "https://open.bigmodel.cn/api/anthropic"},
        "http": [{"status": 429, "body": "{}"}]},
    "zai_route_no_windows": {"op": "detail", "record": {"id": "z1", "name": "z", "harness": "claude", "auth": "api_key", "route": "zai/glm-4.6"},
        "route_env": {"ANTHROPIC_AUTH_TOKEN": "zk", "ANTHROPIC_BASE_URL": "https://api.z.ai/api/anthropic"},
        "http": [{"status": 200, "body": json.dumps(zai())}]},
}


def _windows(ws):
    return [dataclasses.asdict(w) for w in ws]


def _snapshot(s):
    if s is None:
        return None
    out = dataclasses.asdict(s)
    out.pop("provider_id")
    return out


def _sub(value, d):
    return json.loads(json.dumps(value).replace("{dir}", str(d)))


class _Resp(io.BytesIO):
    def __enter__(self):
        return self

    def __exit__(self, *a):
        return False


def run_case(u, case, d):
    op = case["op"]
    if op == "claude_windows":
        return _windows(u._parse_claude_windows(case["payload"]))
    if op == "codex_windows":
        return _windows(u._parse_codex_rate_limits(case["payload"]))
    if op == "zai_windows":
        ws, partial = u._parse_zai_windows(case["payload"], with_partial=True, now=case.get("now", NOW))
        return {"windows": _windows(ws), "partial": partial}
    if op == "zai_confidence":
        return u._zai_confidence(case["payload"])
    if op == "labels":
        return [u._label_for_minutes(m) for m in case["minutes"]]
    if op == "iso":
        return [u._iso_to_epoch(v) for v in case["values"]]
    if op == "detail":
        from fno.adapters.providers.model import ProviderRecord

        for f in case.get("files", []):
            p = d / f["rel"]
            p.parent.mkdir(parents=True, exist_ok=True)
            p.write_text(f["text"], encoding="utf-8")
            os.utime(p, (NOW - f["age"], NOW - f["age"]))
        record = ProviderRecord(**_sub(case["record"], d))
        blobs = list(case.get("blobs", []))
        u.credential_blobs = lambda harness, src: list(blobs)
        responses = list(case.get("http", []))

        def urlopen(req, timeout=None):
            r = responses.pop(0)
            if r["status"] >= 400:
                raise urllib.error.HTTPError(req.full_url, r["status"], "x", {}, io.BytesIO(r["body"].encode()))
            return _Resp(r["body"].encode())

        u.urllib.request.urlopen = urlopen
        env = case.get("route_env", {})

        def dispatch_env(provider_id, repo_root=None):
            if env is None:
                raise RuntimeError("refused")
            return dict(env)

        u.dispatch_env = dispatch_env
        snap, reason = u.probe_usage_detail(record, now=NOW)
        return {"snapshot": _snapshot(snap), "reason": reason}
    raise ValueError(op)


def main() -> int:
    from fno.adapters.providers import usage as u

    GOLDEN.mkdir(parents=True, exist_ok=True)
    for stale in GOLDEN.glob("*.json"):
        stale.unlink()
    original = dict(u._ZAI_UNIT_MINUTES)
    for name, case in CASES.items():
        d = Path(tempfile.mkdtemp(prefix="acct-usage-"))
        os.environ["FNO_STATE_DIR"] = str(d / "state")
        os.environ["HOME"] = str(d / "home")
        try:
            out = {**case, "expect": run_case(u, case, d)}
            if case.get("delta"):
                out["python_expect"] = out["expect"]
                u._ZAI_UNIT_MINUTES[5] = 43_200
                out["expect"] = run_case(u, case, d)
                u._ZAI_UNIT_MINUTES.clear()
                u._ZAI_UNIT_MINUTES.update(original)
        finally:
            shutil.rmtree(d, ignore_errors=True)
        (GOLDEN / f"{name}.json").write_text(json.dumps(out) + "\n", encoding="utf-8")
    print(f"wrote {len(CASES)} cases to {GOLDEN}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
