#!/usr/bin/env python3
"""Capture the `fno agents register` goldens from the Python leg.

The freeze bars cargo builds, so the capture drives the Python command directly
with the venv interpreter - neither a build nor the test suite (the same
reading the logs PR recorded under this freeze). Run from the repo root:

    cli/.venv/bin/python crates/fno-agents/tests/golden/register/capture.py

Each case writes <case>.exit, <case>.out and <case>.err beside this script,
with the fixture root scrubbed to <ROOT>. A fake `fno-agents` on PATH stands
in for the binary so the mail-hold clock writes succeed the way they do for
an installed user; without the shim the --delivery-policy case dies on the
missing binary instead of exercising the stamp.
"""

from __future__ import annotations

import json
import os
import stat
import subprocess
import sys
import tempfile
from pathlib import Path

REPO = Path(__file__).resolve().parents[5]
# Run this script WITH the venv interpreter (see the README); the worktree
# checkout carries no venv of its own.
PY = Path(sys.executable)
OUT = Path(__file__).resolve().parent

SESSION_ID = "3f2b8c1a-9d4e-4f6a-b7c8-2e5d1a0b9c8d"
OTHER_ID = "7c1d4e9b-2a6f-4b8c-9d0e-5f1a2b3c4d5e"


def _write_goldens(case: str, root: Path, proc: subprocess.CompletedProcess) -> None:
    scrub = lambda text: text.replace(str(root), "<ROOT>")  # noqa: E731
    (OUT / f"{case}.exit").write_text(f"{proc.returncode}\n")
    (OUT / f"{case}.out").write_text(scrub(proc.stdout))
    (OUT / f"{case}.err").write_text(scrub(proc.stderr))


def _seed_registry(home: Path, entries: list[dict]) -> None:
    home.mkdir(parents=True, exist_ok=True)
    # 43: this tree's current registry schema. A lower version makes the
    # write refuse the raise (a source checkout may not upgrade a shared
    # registry deployed readers still share).
    (home / "registry.json").write_text(
        json.dumps({"schema_version": 43, "agents": entries})
    )


def _base_env(root: Path, home: Path, extra: dict[str, str]) -> dict[str, str]:
    env = dict(os.environ)
    # A hermetic run sees no ambient identity (the AMBIENT_IDENTITY_ENV scrub
    # the conftest module load performs) and no worker self markers.
    from fno.harness_identity import AMBIENT_IDENTITY_ENV

    for name in (*AMBIENT_IDENTITY_ENV, "FNO_AGENT_SELF", "FNO_AGENT_ROW_PENDING", "FNO_BG"):
        env.pop(name, None)
    for empty in (
        "empty-claude-projects",
        "empty-codex-sessions",
        "empty-opencode-storage",
        "empty-mux",
        "state",
    ):
        (root / empty).mkdir(exist_ok=True)
    # The path migration is sentinel-gated; the sentinel pre-exists so no run
    # prints the one-time bootstrap line (the Rust leg has no bootstrap).
    (root / "state" / ".path-migration-done").write_text("\n")
    # The REAL installed binary answers the doors the Python leg shells
    # (harness-roster, registry-commit, mail-hold); the command logic itself runs
    # from this tree's source. FNO_HOME pins the hold sidecars beside the
    # fixture so a bus-only stamp never touches the machine's journal.
    env.update({
        "PYTHONPATH": str(REPO / "cli" / "src"),
        "FNO_AGENTS_HOME": str(home),
        "FNO_HOME": str(root / "state"),
        "FNO_STATE_DIR": str(root / "state"),
        "FNO_CLAUDE_PROJECTS_DIR": str(root / "empty-claude-projects"),
        "FNO_CODEX_SESSIONS_DIR": str(root / "empty-codex-sessions"),
        "FNO_OPENCODE_STORAGE_DIR": str(root / "empty-opencode-storage"),
        "FNO_MUX_DIR": str(root / "empty-mux"),
    })
    for key, value in extra.items():
        if value is None:
            env.pop(key, None)
        else:
            env[key] = value
    return env


def _run(root: Path, home: Path, extra: dict[str, str], args: list[str]):
    # fno has no __main__; the console entry is fno.cli:main (project.scripts).
    return subprocess.run(
        [str(PY), "-c", "from fno.cli import main; main()", "agents", "register", *args],
        env={**_base_env(root, home, extra)},
        cwd=str(root),
        capture_output=True,
        text=True,
        timeout=60,
    )


def main() -> int:
    cases: list[tuple[str, Path, dict, list[str]]] = []

    root = Path(tempfile.mkdtemp(prefix="register-golden-"))
    cases.append(("no-identity", root / "home", {"CLAUDE_CODE_SESSION_ID": None, "CODEX_THREAD_ID": None}, []))

    cases.append((
        "bad-policy",
        root / "home",
        {"CLAUDE_CODE_SESSION_ID": SESSION_ID},
        ["--delivery-policy", "always"],
    ))

    cases.append((
        "mint-json",
        root / "home-mint",
        {"CLAUDE_CODE_SESSION_ID": SESSION_ID},
        ["--json"],
    ))

    # The plain (non --json) branch: stdout is a pipe, so the command still
    # emits the JSON line - the tty branch is not capturable here.
    cases.append((
        "mint-plain",
        root / "home-mint-plain",
        {"CLAUDE_CODE_SESSION_ID": SESSION_ID},
        [],
    ))

    # Refresh: the same session id registered twice into one home. The second
    # run must refresh in place (same name), not mint a duplicate row.
    refresh_home = root / "home-refresh"
    first = _run(root, refresh_home, {"CLAUDE_CODE_SESSION_ID": SESSION_ID}, ["--json"])
    cases.append(("refresh", refresh_home, {"CLAUDE_CODE_SESSION_ID": SESSION_ID}, ["--json"]))
    _write_goldens("refresh-first", root, first)

    cases.append((
        "bus-only",
        root / "home-bus",
        {"CLAUDE_CODE_SESSION_ID": SESSION_ID},
        ["--json", "--delivery-policy", "bus-only"],
    ))

    home_off = root / "home-off"
    _seed_registry(home_off, [{
        "name": "calm-otter",
        "cwd": str(root),
        "log_path": "",
        "harness": "claude",
        "harness_session_id": SESSION_ID,
        "status": "idle",
        "origin": "operator",
        "delivery_policy": "bus-only",
    }])
    cases.append((
        "policy-off",
        home_off,
        {"CLAUDE_CODE_SESSION_ID": SESSION_ID},
        ["--json", "--delivery-policy", "off"],
    ))

    home_self = root / "home-self"
    _seed_registry(home_self, [{
        "name": "busy-otter",
        "cwd": str(root),
        "log_path": "",
        "harness": "claude",
        "harness_session_id": OTHER_ID,
        "status": "live",
        "origin": "spawn",
    }])
    cases.append((
        "self-refusal",
        home_self,
        {"CLAUDE_CODE_SESSION_ID": SESSION_ID, "FNO_AGENT_SELF": "busy-otter"},
        ["--json"],
    ))

    home_origin = root / "home-origin"
    _seed_registry(home_origin, [{
        "name": "swift-otter",
        "cwd": str(root),
        "log_path": "",
        "harness": "claude",
        "harness_session_id": SESSION_ID,
        "status": "idle",
        "origin": "spawn",
    }])
    cases.append((
        "origin-note",
        home_origin,
        {"CLAUDE_CODE_SESSION_ID": SESSION_ID},
        ["--json"],
    ))

    # Warmup: the first run into a fresh state dir prints the one-time
    # "[setup] path migration complete" bootstrap line. Spend that line here
    # so no golden carries Python-bootstrap output the Rust leg would never
    # print.
    warm = _run(root, root / "home-warmup", {}, [])
    assert warm.returncode in (0, 3), f"warmup failed oddly: {warm.stderr!r}"

    # The mixed-markers case is deliberately absent: identity disambiguation
    # between two marker families is runtime-proven (the process-tree
    # attester walk), so its disposition depends on the ancestry of whoever
    # runs this script. The exit-4 path and its message are covered by the
    # owned-identity ladder's own Rust tests, not by a cross-language golden.

    for case, home, extra, args in cases:
        _write_goldens(case, root, _run(root, home, extra, args))

    written = sorted(p.name for p in OUT.glob("*.exit"))
    print(f"captured {len(written)} cases: {', '.join(written)}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
