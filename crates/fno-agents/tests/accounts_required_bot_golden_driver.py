#!/usr/bin/env python3
"""Capture driver for the `accounts_required_bot` goldens.

The `required-bot-check` command in `fno.adapters.providers.cli`, as it stood
before it became a transport, is the parity oracle. Each case writes a config
with review bots and account records, and seeds the runtime-state file. It
runs the command at `now = NOW`, records the events it emits, and freezes the
exit code, stdout, stderr and the events under `expect`.
`accounts_required_bot_parity.rs` replays every case against
`accounts::required_bot`. `files` holds every file the case set up.

    PYTHONPATH=cli/src uv run --project cli python crates/fno-agents/tests/accounts_required_bot_golden_driver.py
"""

from __future__ import annotations

import json
import os
import shutil
import sys
import tempfile
import time
from pathlib import Path

GOLDEN = Path(__file__).resolve().parent / "golden" / "accounts_required_bot"
NOW = 1_789_400_000.0
CODEX_BOT = "chatgpt-codex-connector"
GEMINI_BOT = "gemini-code-assist"
ENV = {"codex": "OPENAI_API_KEY", "gemini": "GEMINI_API_KEY", "claude": "ANTHROPIC_API_KEY"}


def rec(rid, harness="codex"):
    return {"id": rid, "name": rid, "harness": harness, "auth": "api_key", "env": {ENV[harness]: "sk-test"}}


def full(resets=NOW + 3600.0):
    return {"probed_at": NOW - 60.0, "windows": [{"label": "5h", "used_pct": 100, "resets_at": resets}]}


def cool():
    return {"probed_at": NOW - 60.0, "windows": [{"label": "5h", "used_pct": 20, "resets_at": NOW + 3600.0}]}


def state(usage=None, health=None):
    return {"schema_version": 2, "usage": usage or {}, "provider_health": health or {}}


CASES: dict[str, dict] = {
    "no_bots_configured_prints_nothing": {
        "args": [], "review": {}, "records": [rec("codex-pro")], "state": state({"codex-pro": full()})},
    "an_exhausted_codex_bot_warns": {
        "args": [], "review": {"github_apps": [CODEX_BOT]}, "records": [rec("codex-pro")],
        "state": state({"codex-pro": full()})},
    "an_exhausted_codex_bot_json": {
        "args": ["--json"], "review": {"github_apps": [CODEX_BOT]}, "records": [rec("codex-pro")],
        "state": state({"codex-pro": full()})},
    "a_healthy_bot_is_quiet": {
        "args": [], "review": {"github_apps": [CODEX_BOT]}, "records": [rec("codex-pro")],
        "state": state({"codex-pro": cool()})},
    "a_healthy_bot_json_is_an_empty_list": {
        "args": ["-J"], "review": {"github_apps": [CODEX_BOT]}, "records": [rec("codex-pro")],
        "state": state({"codex-pro": cool()})},
    "one_healthy_account_of_the_kind_keeps_it_quiet": {
        "args": [], "review": {"github_apps": [CODEX_BOT]}, "records": [rec("codex-a"), rec("codex-b")],
        "state": state({"codex-a": full(), "codex-b": cool()})},
    "every_account_exhausted_names_the_first": {
        "args": ["--json"], "review": {"github_apps": [CODEX_BOT]}, "records": [rec("codex-a"), rec("codex-b")],
        "state": state({"codex-a": full(NOW + 7200.5), "codex-b": full()})},
    "a_live_lock_reads_exhausted": {
        "args": [], "review": {"github_apps": [CODEX_BOT]}, "records": [rec("codex-pro")],
        "state": state(health={"codex-pro": {"rate_limited_until": NOW + 900.0, "last_error_at": NOW - 10.0}})},
    "a_resetless_full_window_reads_unknown_reset": {
        "args": [], "review": {"github_apps": [CODEX_BOT]}, "records": [rec("codex-pro")],
        "state": state({"codex-pro": full(None)})},
    "a_resetless_full_window_json": {
        "args": ["--json"], "review": {"github_apps": [CODEX_BOT]}, "records": [rec("codex-pro")],
        "state": state({"codex-pro": full(None)})},
    "an_unmappable_bot_is_skipped": {
        "args": [], "review": {"github_apps": ["coderabbitai"]}, "records": [rec("codex-pro")],
        "state": state({"codex-pro": full()})},
    "a_bot_with_no_record_of_its_kind_is_skipped": {
        "args": [], "review": {"github_apps": [GEMINI_BOT]}, "records": [rec("codex-pro")],
        "state": state({"codex-pro": full()})},
    "github_apps_and_required_bots_union_in_order": {
        "args": [], "review": {"github_apps": [CODEX_BOT], "required_bots": [CODEX_BOT, GEMINI_BOT]},
        "records": [rec("gem", "gemini"), rec("codex-pro")],
        "state": state({"codex-pro": full(), "gem": full(NOW + 30.0)})},
    "required_bots_alone_count": {
        "args": [], "review": {"required_bots": ["Claude-Reviewer"]}, "records": [rec("cl", "claude")],
        "state": state({"cl": full(NOW + 125.0)})},
    "no_state_file_is_quiet": {
        "args": [], "review": {"github_apps": [CODEX_BOT]}, "records": [rec("codex-pro")]},
    "a_bad_records_config_is_quiet": {
        "args": [], "review": {"github_apps": [CODEX_BOT]}, "records": [rec("codex-pro"), rec("codex-pro")],
        "state": state({"codex-pro": full()})},
}


def run_case(name: str, case: dict) -> dict:
    import tomli_w
    from typer.testing import CliRunner

    import fno.events as events
    from fno.adapters.providers.cli import cli as providers_app

    root = Path(tempfile.mkdtemp(prefix="required-bot-golden-"))
    cwd = os.getcwd()
    try:
        cfg = root / ".fno" / "config.toml"
        cfg.parent.mkdir(parents=True)
        body: dict = {"accounts": {"records": case["records"]}}
        if case["review"]:
            body["review"] = case["review"]
        cfg.write_text(tomli_w.dumps(body), encoding="utf-8")
        if "state" in case:
            (root / "runtime-state.json").write_text(json.dumps(case["state"]), encoding="utf-8")
        env = {"HOME": str(root), "PWD": str(root), "FNO_REPO_ROOT": str(root),
               "FNO_STATE_DIR": str(root / ".fno"),
               "FNO_RUNTIME_STATE_PATH": str(root / "runtime-state.json")}
        saved = {k: os.environ.get(k) for k in env}
        os.environ.update(env)
        os.chdir(root)
        files = [
            {"rel": str(p.relative_to(root)), "text": p.read_text(encoding="utf-8")}
            for p in sorted(root.rglob("*")) if p.is_file()
        ]

        emitted: list[dict] = []

        def append(event, *args, **kwargs):
            emitted.append({"type": event["type"], "source": event["source"], "data": event["data"]})

        old = (time.time, events.append_event)
        time.time = lambda: NOW
        events.append_event = append
        try:
            result = CliRunner().invoke(providers_app, ["required-bot-check", *case["args"]], env=env,
                                        catch_exceptions=False)
        finally:
            time.time, events.append_event = old
            os.chdir(cwd)
            for k, v in saved.items():
                if v is None:
                    os.environ.pop(k, None)
                else:
                    os.environ[k] = v
        return {
            "args": case["args"], "files": files,
            "expect": {"exit": result.exit_code, "stdout": result.stdout, "stderr": result.stderr,
                       "events": emitted},
        }
    finally:
        shutil.rmtree(root, ignore_errors=True)


def main() -> int:
    GOLDEN.mkdir(parents=True, exist_ok=True)
    for old in GOLDEN.glob("*.json"):
        old.unlink()
    for name, case in CASES.items():
        out = run_case(name, case)
        (GOLDEN / f"{name}.json").write_text(json.dumps(out) + "\n", encoding="utf-8")
        print(name, out["expect"]["exit"], repr(out["expect"]["stdout"][:160]), repr(out["expect"]["stderr"][:160]),
              len(out["expect"]["events"]))
    print(f"wrote {len(CASES)} goldens to {GOLDEN}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
