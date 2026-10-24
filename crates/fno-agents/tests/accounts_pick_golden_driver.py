#!/usr/bin/env python3
"""Capture driver for the `accounts_pick` goldens.

The `pick` command in `fno.adapters.providers.cli`, as it stood before it
became a transport, is the parity oracle. Each case writes the account
config, seeds the runtime-state usage, the login-gate probes and the slot
stamp, runs the command at `now = NOW` with the usage probe answering
`probe-failed` and the window stamp a no-op, and freezes stdout, stderr
and the exit code under `expect`. `accounts_pick_parity.rs` replays every
case against `accounts::pick`. `files` holds every file the case set up,
with `{dir}` naming the scratch dir.

    PYTHONPATH=<main's cli/src> uv run --project cli python crates/fno-agents/tests/accounts_pick_golden_driver.py
"""

from __future__ import annotations

import json
import os
import shutil
import sys
import tempfile
import time
from pathlib import Path

GOLDEN = Path(__file__).resolve().parent / "golden" / "accounts_pick"
NOW = 1_789_400_000.0


def config_dir_pair(**extra):
    return {
        "active": "readyrule",
        "records": [
            {"id": "readyrule", "name": "readyrule", "harness": "claude", "auth": "managed",
             "config_dir": "{dir}/claude-alt"},
            {"id": "makers", "name": "makers", "harness": "claude", "auth": "managed",
             "config_dir": "{dir}/claude-main"},
        ],
        **extra,
    }


MANAGED_PAIR = {
    "active": "readyrule",
    "records": [
        {"id": "readyrule", "name": "readyrule", "harness": "claude", "auth": "managed"},
        {"id": "makers", "name": "makers", "harness": "claude", "auth": "managed"},
    ],
}


def usage(**pct):
    return {"usage": {
        rid: {"windows": [{"label": "5h", "used_pct": p, "resets_at": NOW + 3600}],
              "probed_at": NOW, "source": "oauth-endpoint"}
        for rid, p in pct.items()
    }}


def refusal(dir_name, at=NOW, detail="Login expired"):
    return {"verdict": "logged_out", "detail": detail,
            "config_dir": "{dir}/" + dir_name, "at_epoch": at}


CASES: dict[str, dict] = {
    "managed_pair_has_no_launchable_candidate": {
        "args": [], "providers": MANAGED_PAIR, "slot": "readyrule"},
    "first_candidate_with_headroom": {
        "args": [], "providers": config_dir_pair(), "state": usage(readyrule=100.0, makers=4.0)},
    "every_candidate_exhausted": {
        "args": [], "providers": config_dir_pair(), "state": usage(readyrule=100.0, makers=100.0)},
    "unprobed_set_keeps_config_order": {"args": [], "providers": config_dir_pair()},
    "exclude_skips_an_account": {
        "args": ["--exclude", "readyrule"], "providers": config_dir_pair()},
    "exclude_both_names_no_records": {
        "args": ["--exclude", "readyrule", "--exclude=makers"], "providers": config_dir_pair()},
    "login_refusal_lands_on_the_live_account": {
        "args": [], "providers": config_dir_pair(),
        "probes": {"readyrule": refusal("claude-alt")}},
    "a_stale_login_refusal_reads_healthy": {
        "args": [], "providers": config_dir_pair(),
        "probes": {"readyrule": refusal("claude-alt", at=NOW - 1800)}},
    "a_refusal_for_another_dir_is_ignored": {
        "args": [], "providers": config_dir_pair(),
        "probes": {"readyrule": refusal("claude-main")}},
    "every_login_refused_names_the_remedy": {
        "args": [], "providers": config_dir_pair(),
        "probes": {"readyrule": refusal("claude-alt"), "makers": refusal("claude-main", detail="")}},
    "print_env_carries_the_clear_list": {"args": ["--print-env"], "providers": config_dir_pair()},
    "json_verdict": {
        "args": ["--json"], "providers": config_dir_pair(), "state": usage(readyrule=95.0, makers=50.0)},
    "json_verdict_on_refusal": {
        "args": ["-J", "--print-env"], "providers": config_dir_pair(),
        "state": usage(readyrule=100.0, makers=100.0)},
    "if_armed_declines_when_off": {
        "args": ["--if-armed"], "providers": config_dir_pair(quota={"pick_on_launch": False})},
    "if_armed_picks_when_on": {
        "args": ["--if-armed"], "providers": config_dir_pair(quota={"pick_on_launch": True})},
    "without_if_armed_the_knob_is_not_read": {
        "args": [], "providers": config_dir_pair(quota={"pick_on_launch": False})},
    "active_combo_orders_the_candidates": {
        "args": [], "providers": config_dir_pair(
            combos={"pair": {"providers": ["makers", "readyrule"]}}, active_combo="pair")},
    "named_combo_wins_over_the_active_one": {
        "args": ["--combo", "solo"], "providers": config_dir_pair(
            combos={"pair": {"providers": ["makers", "readyrule"]},
                    "solo": {"providers": ["readyrule"]}}, active_combo="pair"),
        "state": usage(readyrule=100.0)},
    "an_unknown_combo_falls_back_to_config_order": {
        "args": ["--combo", "nope"], "providers": config_dir_pair()},
}


def sub(value, root: Path):
    return json.loads(json.dumps(value).replace("{dir}", str(root)))


def run_case(name: str, case: dict) -> dict:
    import tomli_w
    from typer.testing import CliRunner

    import fno.adapters.providers.runtime_state as rs
    import fno.adapters.providers.usage as usage_mod
    from fno.adapters.providers import managed
    from fno.adapters.providers.cli import cli as providers_app

    root = Path(tempfile.mkdtemp(prefix="pick-golden-")).resolve()
    cwd = os.getcwd()
    try:
        for d in ("claude-alt", "claude-main"):
            (root / d).mkdir()
            (root / d / ".credentials.json").write_text(
                '{"claudeAiOauth":{"accessToken":"t","refreshToken":"r"}}')
        cfg = root / "config.toml"
        cfg.write_text(tomli_w.dumps({"accounts": sub(case["providers"], root)}), encoding="utf-8")
        if "state" in case:
            (root / "runtime-state.json").write_text(json.dumps(case["state"]), encoding="utf-8")
        if "probes" in case:
            (root / "agents").mkdir()
            (root / "agents" / "login-probes.json").write_text(
                json.dumps({"probes": sub(case["probes"], root)}), encoding="utf-8")
        env = {"HOME": str(root), "PWD": str(root), "FNO_CONFIG": str(cfg),
               "FNO_STATE_DIR": str(root / "state"), "FNO_AGENTS_HOME": str(root / "agents"),
               "FNO_RUNTIME_STATE_PATH": str(root / "runtime-state.json"),
               "FNO_GLOBAL_SETTINGS_PATH": str(root / "absent-global.json"),
               "FNO_NO_CANONICAL_CONFIG": "1"}
        saved = {k: os.environ.get(k) for k in env}
        os.environ.update(env)
        os.chdir(root)
        if "slot" in case:
            managed.stamp_active_slot("claude", case["slot"])
        files = [
            {"rel": str(p.relative_to(root)), "text": p.read_text(encoding="utf-8").replace(str(root), "{dir}")}
            for p in sorted(root.rglob("*")) if p.is_file()
        ]
        old = (time.time, usage_mod.probe_usage_detail, rs.stamp_window_open)
        time.time = lambda: NOW
        usage_mod.probe_usage_detail = lambda record, now=None, **k: (None, "probe-failed")
        rs.stamp_window_open = lambda *a, **k: True
        try:
            result = CliRunner().invoke(providers_app, ["pick", *case["args"]], env=env,
                                        catch_exceptions=False)
        finally:
            time.time, usage_mod.probe_usage_detail, rs.stamp_window_open = old
            os.chdir(cwd)
            for k, v in saved.items():
                if v is None:
                    os.environ.pop(k, None)
                else:
                    os.environ[k] = v
        scrub = lambda text: text.replace(str(root), "{dir}")  # noqa: E731
        return {
            "args": case["args"], "files": files,
            "expect": {"stdout": scrub(result.stdout), "stderr": scrub(result.stderr),
                       "exit": result.exit_code},
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
