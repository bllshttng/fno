#!/usr/bin/env python3
"""Golden capture driver for the pane_spawn port (row 7, x-8ada).

Freezes the codex pane leg's observable bytes from the Python source of
truth into tests/golden/pane_spawn/codex_python_leg.json, before the Rust
arm lands (PR 2 of the d-636d3a31 split). The Rust arm's parity tests play
these cases and diff byte-for-byte; keep this file and that table in
lockstep (the rule mail_send_capture.py records in its header).

Function-level, not CLI-level: dispatch_spawn_pane runs against a fake
runner (no mux server, no codex daemon, no real codex), the registry and
claims store under a temp FNO_AGENTS_HOME/FNO_CLAIMS_ROOT, and patched
codex_pane seams for the daemon ensure, loaded-ids baseline, bind probe and
seed delivery. The frozen bytes are the contract: the `fno mux pane run`
argv, the refusal lines and exits, the registry row shape, the
MuxSpawnResult fields the receipt renders from.

Usage:
    cli/.venv/bin/python crates/fno-agents/tests/capture/pane_spawn_capture.py
"""
from __future__ import annotations

import contextlib
import io
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

REPO = Path(__file__).resolve().parents[4]
CLI_SRC = REPO / "cli" / "src"
GOLDEN = REPO / "crates" / "fno-agents" / "tests" / "golden" / "pane_spawn"

SID = "04b5621d-1111-2222-3333-444455556666"
WORKER_PID = 4242


def venv_python() -> str:
    venv = REPO / "cli" / ".venv" / "bin" / "python"
    if venv.is_file():
        return str(venv)
    common = subprocess.run(
        ["git", "-C", str(REPO), "rev-parse", "--path-format=absolute", "--git-common-dir"],
        capture_output=True,
        text=True,
    )
    if common.returncode == 0:
        canonical = Path(common.stdout.strip()).parent / "cli" / ".venv" / "bin" / "python"
        if canonical.is_file():
            return str(canonical)
    return sys.executable


def base_env(root: Path) -> dict:
    env = dict(os.environ)
    env.update(
        HOME=str(root),
        FNO_STATE_DIR=str(root),
        FNO_HOME=str(root),
        FNO_AGENTS_HOME=str(root / "agents-home"),
        FNO_CLAIMS_ROOT=str(root),
        FNO_HARNESS_NAME="claude",
        FNO_HARNESS_SESSION_ID="a1b2c3d4-1111-2222-3333-444455556666",
        CLAUDE_CODE_SESSION_ID="a1b2c3d4-1111-2222-3333-444455556666",
        CODEX_THREAD_ID="",
        GIT_CONFIG_GLOBAL="/dev/null",
        GIT_CONFIG_SYSTEM="/dev/null",
        # Hook trust rides (codex 0.156 >= 0.148.0).
        FNO_CODEX_VERSION="0.156.0",
    )
    env.pop("FNO_SERVER", None)
    env.pop("FNO_SESSION", None)
    env.pop("FNO_SPAWN_TRIGGER", None)
    env.pop("FNO_WORKER_ADD_DIRS", None)
    # The capturing session's own ambient identity must not season the
    # golden: provenance stamps, mail ids and agent-self marks are properties
    # of whoever runs the capture, not of the leg.
    for key in list(env):
        if key.startswith("FNO_SEED_PROV_") or key.startswith("FNO_MAIL_"):
            env.pop(key, None)
    env.pop("FNO_AGENT_SELF", None)
    env.pop("FNO_NODE", None)
    return env


class FakeRunner:
    """`pane run` mints pane 3; `pane ls` answers the matching row; pane
    reads answer empty. Everything else is recorded and answered empty."""

    def __init__(self) -> None:
        self.commands: list[list[str]] = []
        self.envs: list[dict | None] = []

    def __call__(self, cmd, **kwargs):
        self.commands.append(list(cmd))
        self.envs.append(kwargs.get("env"))
        joined = " ".join(cmd)
        if " pane run " in f" {joined} ":
            return subprocess.CompletedProcess(cmd, 0, stdout="3\n", stderr="")
        if " pane ls " in f" {joined} ":
            rows = [
                {
                    "pane_id": 3,
                    "cwd": kwargs.get("_cwd", "/x"),
                    "child_pid": WORKER_PID,
                    "tab_id": "id:12",
                }
            ]
            return subprocess.CompletedProcess(cmd, 0, stdout=json.dumps(rows), stderr="")
        return subprocess.CompletedProcess(cmd, 0, stdout="", stderr="")

    def pane_run_argv(self) -> list[str] | None:
        for cmd in self.commands:
            if len(cmd) > 4 and cmd[-4] != "pane" and "run" in cmd and "pane" in cmd:
                pass
        for cmd in self.commands:
            if "run" in cmd and "pane" in cmd:
                return cmd
        return None

    def pane_run_env(self) -> dict | None:
        for cmd, env in zip(self.commands, self.envs):
            if "run" in cmd and "pane" in cmd:
                return env
        return None


def normalize(value, root: Path):
    if isinstance(value, dict):
        return {k: normalize(v, root) for k, v in sorted(value.items())}
    if isinstance(value, list):
        return [normalize(v, root) for v in value]
    if isinstance(value, str):
        text = value.replace(str(root), "<ROOT>")
        text = re.sub(r"mux-placement:\d+:[0-9a-f]{16}", "mux-placement:<PID>:<HEX>", text)
        text = re.sub(r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(\.\d+)?Z?", "<TS>", text)
        text = re.sub(r"\bfmail-[0-9a-fA-F]{4,}", "fmail-HEX", text)
        text = re.sub(r"\bsp-[0-9a-f]{16}\b", "sp-HEX", text)
        text = re.sub(r"\b[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}\b", "<UUID>", text)
        return text
    return value


def load_row(root: Path, name: str):
    sys.path.insert(0, str(CLI_SRC))
    from fno.agents.registry import load_registry

    rows = load_registry()
    for r in rows:
        if r.name == name:
            from dataclasses import asdict

            return normalize(asdict(r), root)
    return None


def build_cases() -> dict:
    cases: dict = {}

    # --- argv goldens (pure: build_pane_argv + identity leaves) ---------
    sys.path.insert(0, str(CLI_SRC))
    from fno.agents.mux_spawn import build_pane_argv
    from fno.agents.codex_pane import codex_shell_env_args

    def argv_case(label: str, version: str, message: str) -> dict:
        os.environ["FNO_CODEX_VERSION"] = version
        argv = build_pane_argv(
            "codex",
            message,
            Path("/repo"),
            None,
            name="w",
            passthrough=None,
            computed_dirs=None,
        )
        env_args = codex_shell_env_args({})
        os.environ["FNO_CODEX_VERSION"] = "0.156.0"
        return {"pane_argv": argv, "identity_leaves": env_args}

    cases["argv_plain"] = argv_case("plain", "0.156.0", "do x")
    cases["argv_seed_leading_flag"] = argv_case("seed", "0.156.0", "--model opus do x")
    cases["argv_hook_trust_old"] = argv_case("old", "0.147.0", "do x")
    cases["argv_hook_trust_new"] = argv_case("new", "0.156.0", "do x")

    # --- lease held by another holder (exit 2) --------------------------
    from fno.claims.core import acquire_claim

    root = Path(tempfile.mkdtemp(prefix="fnopanecap"))
    try:
        os.environ.update(base_env(root))
        acquire_claim(
            "placement:main:global",
            "mux-placement:111:aaaa",
            ttl_ms=60_000,
            root=Path(os.environ["FNO_CLAIMS_ROOT"]),
            reason="capture",
        )
        from fno.agents.mux_spawn import dispatch_spawn_bounded_pane

        try:
            dispatch_spawn_bounded_pane(
                provider="codex",
                message="do x",
                cwd=root / "repo",
                runner=FakeRunner(),
                bounded_placement=True,
            )
            cases["lease_held"] = {"exit": 0, "line": None}
        except Exception as exc:
            cases["lease_held"] = {
                "exit": getattr(exc, "exit_code", 1),
                "line": normalize(str(exc), root),
            }
        finally:
            from fno.claims.core import release_claim

            with contextlib.suppress(Exception):
                release_claim(
                    "placement:main:global",
                    "mux-placement:111:aaaa",
                    strict=True,
                    root=Path(os.environ["FNO_CLAIMS_ROOT"]),
                )
    finally:
        shutil.rmtree(root, ignore_errors=True)

    # --- full-leg captures under fakes ----------------------------------
    for label, kwargs, name in [
        (
            "plain_pane",
            dict(provider="codex", message="do x", runner=None),
            "w-plain",
        ),
        (
            "bounded_pane",
            dict(provider="codex", message="do x", bounded_placement=True, runner=None),
            "w-bounded",
        ),
        (
            "seed_leading_flag",
            dict(provider="codex", message="--model opus do x", runner=None),
            "w-seed",
        ),
    ]:
        root = Path(tempfile.mkdtemp(prefix="fnopanecap"))
        try:
            os.environ.update(base_env(root))
            repo = root / "repo"
            repo.mkdir(exist_ok=True)
            runner = FakeRunner()
            kwargs["runner"] = runner
            from fno.agents import codex_pane
            from fno.agents import mux_spawn

            codex_pane.ensure_codex_daemon = lambda *a, **k: None
            codex_pane._codex_session_ids_loaded = lambda cwd, **k: {SID}
            codex_pane._make_codex_bind_probe = lambda **k: (lambda: SID)
            codex_pane.deliver_seed = lambda *a, **k: True
            if "bounded_placement" in kwargs:
                from fno.agents.mux_spawn import dispatch_spawn_bounded_pane as dispatch
            else:
                from fno.agents.mux_spawn import dispatch_spawn_pane as dispatch
            from dataclasses import asdict

            result = dispatch(
                name=name,
                cwd=repo,
                session="main",
                **kwargs,
            )
            cases[label] = {
                "pane_run_argv": normalize(runner.pane_run_argv(), root),
                "pane_run_env_bridges": {
                    k: (runner.pane_run_env() or {}).get(k)
                    for k in [
                        "FNO_MUX_SHELL_INTEGRATION",
                        "FNO_PROCESS_ADMISSION_MAX",
                        "FNO_MUX_PANE_GROUP_MAX",
                    ]
                },
                "result": normalize(asdict(result), root),
                "row": load_row(root, name),
            }
        except Exception as exc:
            cases[label] = {"error": f"{type(exc).__name__}: {exc}"}
        finally:
            shutil.rmtree(root, ignore_errors=True)

    # --- codex code payload with no git common dir (exit 2) -------------
    root = Path(tempfile.mkdtemp(prefix="fnopanecap"))
    try:
        os.environ.update(base_env(root))
        bare = root / "bare"
        bare.mkdir(exist_ok=True)
        from fno.agents.rust_runtime import _refuse_codex_code_spawn_without_git_grant

        err = io.StringIO()
        code = None
        try:
            with contextlib.redirect_stderr(err):
                _refuse_codex_code_spawn_without_git_grant(
                    [
                        "spawn", "w", "-H", "codex", "--cwd", str(bare),
                        "--node", "fno-abcd", "--", "do x",
                    ]
                )
        except SystemExit as exc:
            code = exc.code
        cases["no_git_grant"] = {"exit": code, "line": normalize(err.getvalue().strip(), root)}
    finally:
        shutil.rmtree(root, ignore_errors=True)

    return cases


def main() -> None:
    GOLDEN.mkdir(parents=True, exist_ok=True)
    cases = build_cases()
    out = GOLDEN / "codex_python_leg.json"
    out.write_text(json.dumps({"captured_at": "2026-10-21", "cases": cases}, indent=1) + "\n")
    print(f"goldens written to {out}")
    for label, case in cases.items():
        flag = "error" if "error" in case else "ok"
        print(f"  {label}: {flag}")


if __name__ == "__main__":
    main()
