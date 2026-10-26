#!/usr/bin/env python3
"""Capture driver for the `accounts_window` goldens.

The `window` command in `fno.adapters.providers.cli`, as it stood before it
became a transport, is the parity oracle. Each case configures records and
seeds the runtime-state file. It replaces the once-per-window warn writer
with its own answer per record and records the event the command emits. It
runs the command at `now = NOW` and freezes the exit code, stdout, stderr,
the warn calls and the events under `expect`.
`accounts_window_parity.rs` replays every case against `accounts::window`.
`files` holds every file the case set up, with `{dir}` naming the scratch dir.

    PYTHONPATH=cli/src uv run --project cli python crates/fno-agents/tests/accounts_window_golden_driver.py
"""

from __future__ import annotations

import json
import os
import shutil
import sys
import tempfile
import time
from pathlib import Path

GOLDEN = Path(__file__).resolve().parent / "golden" / "accounts_window"
NOW = 1_789_400_000.0


def rec(rid, harness="opencode", minutes=None):
    out = {"id": rid, "name": rid, "harness": harness, "auth": "api_key", "env": {"ANTHROPIC_AUTH_TOKEN": "sk-test"}}
    if minutes is not None:
        out["window_minutes"] = minutes
    return out


def opened(at, warned=False):
    return {"window": {"opened_at": at, "warned": warned}}


MIXED = [rec("zai", minutes=300), rec("glm-b", minutes=300), rec("plain"), rec("probeless", minutes=60)]
MIXED_STATE = {
    "schema_version": 2,
    "windows_opened": {
        "zai": opened(NOW - 4.5 * 3600 - 0.25),
        "glm-b": opened(NOW - 6 * 3600),
        "plain": opened(NOW - 120.0),
    },
}
LOCKED_STATE = {
    "schema_version": 2,
    "provider_health": {"zai": {"backoff_level": 3, "rate_limited_until": NOW + 900.5, "last_error_at": NOW - 60.0}},
    "windows_opened": {"zai": opened(NOW - 7000.0)},
}

CASES: dict[str, dict] = {
    "no_records_prints_the_empty_line": {"args": [], "records": []},
    "no_records_json_prints_an_empty_map": {"args": ["--json"], "records": []},
    "mixed_rows_json": {"args": ["--json"], "records": MIXED, "state": MIXED_STATE},
    "mixed_rows_text": {"args": [], "records": MIXED, "state": MIXED_STATE},
    "no_state_file_reads_unknown": {"args": ["-J"], "records": [rec("zai", minutes=300)]},
    "a_fresh_lock_reads_exhausted_with_its_reset": {
        "args": ["--json"], "records": [rec("zai", minutes=120)], "state": LOCKED_STATE},
    "a_fresh_lock_text_row": {"args": [], "records": [rec("zai", minutes=120)], "state": LOCKED_STATE},
    "a_closing_window_warns_once_and_emits": {
        "args": [], "records": MIXED, "state": MIXED_STATE, "warned": {"zai": True}},
    "an_already_warned_window_emits_nothing": {
        "args": ["--json"], "records": MIXED, "state": MIXED_STATE, "warned": {"zai": False}},
    "a_wider_warn_threshold_reaches_more_rows": {
        "args": ["--warn-seconds", "4000", "--json"], "records": MIXED, "state": MIXED_STATE,
        "warned": {"zai": True}},
    "a_fractional_warn_threshold": {
        "args": ["--warn-seconds=1799.5"], "records": MIXED, "state": MIXED_STATE, "warned": {"zai": True}},
    # The loader text past this prefix is pydantic's; the Rust loader prints
    # its own detail there, so the case pins the refusal headline.
    "a_duplicate_id_refuses": {"args": [], "records": [rec("zai"), rec("zai")],
                               "stderr_prefix": "error: invalid providers config: duplicate_record_ids"},
}


def sub(value, root: Path):
    return json.loads(json.dumps(value).replace("{dir}", str(root)))


def run_case(name: str, case: dict) -> dict:
    import tomli_w
    from typer.testing import CliRunner

    import fno.adapters.providers.runtime_state as rs
    import fno.events as events
    from fno.adapters.providers.cli import cli as providers_app

    root = Path(tempfile.mkdtemp(prefix="window-golden-"))
    try:
        cfg = root / ".fno" / "config.toml"
        cfg.parent.mkdir(parents=True)
        if "config" in case:
            cfg.write_text(case["config"], encoding="utf-8")
        else:
            records = sub(case.get("records", []), root)
            cfg.write_text(tomli_w.dumps({"accounts": {"records": records}}), encoding="utf-8")
        if "state" in case:
            (root / "runtime-state.json").write_text(json.dumps(case["state"]), encoding="utf-8")
        env = {"HOME": str(root), "PWD": str(root), "FNO_STATE_DIR": str(root / ".fno"),
               "FNO_RUNTIME_STATE_PATH": str(root / "runtime-state.json")}
        saved = {k: os.environ.get(k) for k in env}
        os.environ.update(env)
        files = [
            {"rel": str(p.relative_to(root)), "text": p.read_text(encoding="utf-8").replace(str(root), "{dir}")}
            for p in sorted(root.rglob("*")) if p.is_file()
        ]

        warned = case.get("warned", {})
        calls: list[str] = []
        emitted: list[dict] = []

        def mark(provider_id, repo_root=None):
            calls.append(provider_id)
            return warned.get(provider_id, False)

        def append(event, path):
            emitted.append({"type": event["type"], "source": event["source"],
                            "data": event["data"]})

        old = (time.time, rs.mark_window_warned, events.append_event)
        time.time = lambda: NOW
        rs.mark_window_warned = mark
        events.append_event = append
        try:
            result = CliRunner().invoke(providers_app, ["window", *case["args"]], env=env,
                                        catch_exceptions=False)
        finally:
            time.time, rs.mark_window_warned, events.append_event = old
            for k, v in saved.items():
                if v is None:
                    os.environ.pop(k, None)
                else:
                    os.environ[k] = v

        def scrub(text: str) -> str:
            return text.replace(str(root), "{dir}")

        stderr = scrub(result.stderr)
        prefix = case.get("stderr_prefix")
        if prefix:
            assert stderr.startswith(prefix), (name, stderr)
            stderr = prefix
        return {
            "args": case["args"], "files": files, "warned": warned,
            "stderr_prefix": bool(case.get("stderr_prefix")),
            "expect": {
                "exit": result.exit_code,
                "stdout": scrub(result.stdout),
                "stderr": stderr,
                "warn_calls": calls,
                "events": json.loads(scrub(json.dumps(emitted))),
            },
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
        print(name, out["expect"]["exit"], repr(out["expect"]["stdout"][:150]), repr(out["expect"]["stderr"][:200]),
              out["expect"]["warn_calls"], len(out["expect"]["events"]))
    print(f"wrote {len(CASES)} goldens to {GOLDEN}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
