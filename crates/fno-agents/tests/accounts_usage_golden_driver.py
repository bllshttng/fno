#!/usr/bin/env python3
"""Capture driver for the `accounts_usage` goldens.

The `usage` command in `fno.adapters.providers.cli`, as it stood before it
became a transport, is the parity oracle. Each case configures records, seeds
the runtime-state file and the recorded principals, and replaces the probe,
the cache write and the Keychain read with its own answers. It runs the
command at `now = NOW` and freezes stdout under `expect`.
`accounts_usage_parity.rs` replays every case against `accounts::usage`.
`files` holds every file the case set up, with `{dir}` naming the scratch dir,
and `{remedy}` stands for the manual-switch text.

    PYTHONPATH=cli/src uv run --project cli python crates/fno-agents/tests/accounts_usage_golden_driver.py
"""

from __future__ import annotations

import json
import os
import shutil
import sys
import tempfile
import time
from pathlib import Path

GOLDEN = Path(__file__).resolve().parent / "golden" / "accounts_usage"
NOW = 1_789_400_000.0

A = {"account_uuid": "acct-a", "organization_uuid": "org-1"}
B = {"account_uuid": "acct-b", "organization_uuid": "org-1", "email": "other@example.com"}


def snap(*windows, probed_at=NOW - 30, source="oauth-endpoint", **kw):
    return {"windows": [list(w) for w in windows], "probed_at": probed_at, "source": source, **kw}


def pair():
    return [
        {"id": "readyrule", "name": "readyrule", "harness": "claude", "auth": "managed",
         "config_dir": "{dir}/claude-alt"},
        {"id": "makers", "name": "makers", "harness": "claude", "auth": "managed",
         "config_dir": "{dir}/claude-main"},
    ]


TWO = snap(("5h", 9.0, 1_800_000_000.0), ("weekly", 95.0, 1_800_600_000.0), probed_at=1_700_000_000.0)

CASES: dict[str, dict] = {
    "refresh_json_reports_the_probed_snapshot": {
        "args": ["--refresh", "--json"], "probe": {"readyrule": TWO}},
    "refresh_json_a_lost_write_still_shows_the_reading": {
        "args": ["--refresh", "--json"], "persist": False,
        "probe": {"readyrule": snap(("5h", 12.0, 1_800_000_000.0))}},
    "refresh_json_every_record_unknown": {"args": ["--refresh", "--json"], "probe": {}},
    "refresh_json_unattributed_borrows_nothing": {
        "args": ["--refresh", "--json"],
        "probe": {"readyrule": snap(("5h", 9.0, 1_800_000_000.0)), "makers": "unattributed"}},
    "refresh_json_no_windows": {
        "args": ["--refresh", "--json"], "probe": {"readyrule": snap()}},
    "refresh_json_reason_missing_reads_unknown": {
        "args": ["--refresh", "--json"], "probe": {"readyrule": None}},
    "refresh_text_one_line_per_state": {
        "args": ["--refresh"], "probe": {"readyrule": snap(("5h", 9.0, NOW + 3600))}},
    "refresh_text_reset_shapes": {
        "args": ["--refresh"], "probe": {"readyrule": snap(
            ("5h", 9.04, NOW + 7500), ("weekly", 100.0, NOW - 5),
            ("30d", 0.0, None), ("1m", 3.35, NOW + 59))}},
    "refresh_text_partial_and_coarse": {
        "args": ["--refresh"], "probe": {"readyrule": snap(
            ("5h", 50.0, NOW + 600), source="quota-endpoint", partial=True,
            confidence="percent_only")}},
    "cached_json_not_probed": {"args": ["--json"]},
    "cached_json_reads_the_fresh_cache": {
        "args": ["--json"],
        "state": {"usage": {
            "readyrule": {"windows": [{"label": "5h", "used_pct": 20.0, "resets_at": NOW + 900}],
                          "probed_at": NOW - 60, "source": "oauth-endpoint"},
            "makers": {"windows": [], "probed_at": NOW - 60, "source": "oauth-endpoint"}}}},
    "cached_json_stale_cache_is_not_probed": {
        "args": ["--json"],
        "state": {"usage": {"readyrule": {
            "windows": [{"label": "5h", "used_pct": 20.0, "resets_at": NOW + 900}],
            "probed_at": NOW - 3600, "source": "oauth-endpoint"}}}},
    "cached_text_reads_the_fresh_cache": {
        "args": [],
        "state": {"usage": {"readyrule": {
            "windows": [{"label": "weekly", "used_pct": 33.3, "resets_at": NOW + 90_000}],
            "probed_at": NOW - 60, "source": "session-events", "confidence": "exact"}}}},
    "identity_proven_names_the_account": {
        "args": ["--refresh"], "principals": {"readyrule": A}, "slot": A,
        "probe": {"readyrule": snap(("5h", 12.0, 1_800_000_000.0))}},
    "identity_low_account_is_told_the_manual_switch": {
        "args": ["--refresh"], "principals": {"readyrule": A}, "slot": A,
        "probe": {"readyrule": snap(("weekly", 95.0, 1_800_000_000.0))}},
    "identity_wrong_account_is_not_named": {
        "args": ["--refresh"], "principals": {"readyrule": A}, "slot": B,
        "probe": {"readyrule": snap(("5h", 12.0, 1_800_000_000.0))}},
    "identity_json_block": {
        "args": ["--refresh", "--json"], "principals": {"readyrule": A, "makers": B}, "slot": A,
        "probe": {"readyrule": snap(("5h", 12.0, 1_800_000_000.0)),
                  "makers": snap(("5h", 1.0, 1_800_000_000.0))}},
    "a_codex_record_has_no_identity": {
        "args": ["--refresh"],
        "records": [{"id": "codex-pro", "name": "codex-pro", "harness": "codex", "auth": "oauth_dir", "credentials_source": "{dir}/codex"}],
        "probe": {"codex-pro": snap(("weekly", 91.0, NOW + 4000), source="session-events")}},
    "no_records_text": {"args": [], "records": []},
    "no_records_json": {"args": ["--json"], "records": []},
}


def sub(value, root: Path):
    return json.loads(json.dumps(value).replace("{dir}", str(root)))


def run_case(name: str, case: dict) -> dict:
    import tomli_w
    from typer.testing import CliRunner

    import fno.adapters.providers.runtime_state as rs
    from fno.adapters.providers import binding, managed
    from fno.adapters.providers.cli import cli as providers_app
    from fno.adapters.providers.loader import load_quota_config
    from fno.adapters.providers.usage import UsageSnapshot, UsageWindow

    root = Path(tempfile.mkdtemp(prefix="usage-golden-"))
    try:
        for d in ("claude-alt", "claude-main"):
            (root / d).mkdir()
            (root / d / ".credentials.json").write_text("{}")
        records = sub(case.get("records", pair()), root)
        cfg = root / ".fno" / "config.toml"
        cfg.parent.mkdir(parents=True)
        cfg.write_text(tomli_w.dumps({"accounts": {"records": records}}), encoding="utf-8")
        if "state" in case:
            (root / "runtime-state.json").write_text(json.dumps(case["state"]), encoding="utf-8")
        env = {"HOME": str(root), "PWD": str(root), "FNO_STATE_DIR": str(root / ".fno"),
               "FNO_RUNTIME_STATE_PATH": str(root / "runtime-state.json")}
        saved = {k: os.environ.get(k) for k in env}
        os.environ.update(env)
        for rid, principal in case.get("principals", {}).items():
            managed.write_record_principal(rid, principal, root / ".fno" / "providers")
        files = [
            {"rel": str(p.relative_to(root)), "text": p.read_text(encoding="utf-8").replace(str(root), "{dir}")}
            for p in sorted(root.rglob("*")) if p.is_file()
        ]

        probes = case.get("probe", {})

        def probe(record, now=None):
            got = probes.get(record.id, "probe-failed")
            if got is None or isinstance(got, str):
                return None, got
            return UsageSnapshot(
                record.id, tuple(UsageWindow(*w) for w in got["windows"]), got["probed_at"],
                got["source"], partial=got.get("partial", False),
                confidence=got.get("confidence", "exact"),
            ), None

        slot = case.get("slot")
        old = (time.time, rs.write_usage_snapshot, binding.credential_blobs, managed.slot_principal)
        import fno.adapters.providers.usage as usage_mod
        old_probe = usage_mod.probe_usage_detail
        time.time = lambda: NOW
        rs.write_usage_snapshot = lambda s, now=None, **k: case.get("persist", True)
        binding.credential_blobs = lambda *_a: ["blob"] if slot else []
        managed.slot_principal = lambda _b: (slot, None)
        usage_mod.probe_usage_detail = probe
        try:
            quota = load_quota_config()
            # The remedy names a Keychain service hashed from HOME, a scratch
            # path here; its own text is pinned by the reconcile goldens.
            remedy = managed.slot_switch_remedy("the other account")
            result = CliRunner().invoke(providers_app, ["usage", *case["args"]], env=env,
                                        catch_exceptions=False)
        finally:
            time.time, rs.write_usage_snapshot, binding.credential_blobs, managed.slot_principal = old
            usage_mod.probe_usage_detail = old_probe
            for k, v in saved.items():
                if v is None:
                    os.environ.pop(k, None)
                else:
                    os.environ[k] = v
        assert result.exit_code == 0, (name, result.output)
        return {
            "args": case["args"], "files": files, "probe": probes, "slot": slot,
            "persist": case.get("persist", True), "ttl": quota.probe_ttl_seconds,
            "threshold": quota.defer_threshold_pct,
            "expect": result.output.replace(remedy, "{remedy}").replace(str(root), "{dir}"),
        }
    finally:
        shutil.rmtree(root, ignore_errors=True)


def main() -> int:
    GOLDEN.mkdir(parents=True, exist_ok=True)
    for old in GOLDEN.glob("*.json"):
        old.unlink()
    for name, case in CASES.items():
        out = run_case(name, case)
        (GOLDEN / f"{name}.json").write_text(json.dumps(out, indent=2) + "\n", encoding="utf-8")
    print(f"wrote {len(CASES)} goldens to {GOLDEN}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
