#!/usr/bin/env python3
"""Capture the name, whoami, registry-repair and heal-token goldens from the
Python leg.

The freeze bars cargo builds, so the capture drives the Python commands
directly with the venv interpreter - neither a build nor the test suite (the
reading the logs and register PRs recorded under the same freeze). Run from
the repo root with the canonical checkout's venv:

    <venv>/bin/python crates/fno-agents/tests/golden/registry_verbs/capture.py

Each case writes <case>.exit, <case>.out and <case>.err beside this script,
with the fixture root scrubbed to <ROOT> and a repair backup stamp scrubbed to
<STAMP>. The installed fno-agents binary answers the doors the Python leg
shells (name-mint, registry-commit); the command logic runs from this tree.
"""

from __future__ import annotations

import json
import os
import re
import subprocess
import sys
import tempfile
from pathlib import Path

REPO = Path(__file__).resolve().parents[5]
PY = Path(sys.executable)
OUT = Path(__file__).resolve().parent

SESSION_ID = "3f2b8c1a-9d4e-4f6a-b7c8-2e5d1a0b9c8d"
OTHER_ID = "7c1d4e9b-2a6f-4b8c-9d0e-5f1a2b3c4d5e"
TWIN_ID = "3f2b8c1a-0000-4000-8000-000000000001"
CODEX_ID = "019a2b3c-4d5e-7f60-8123-456789abcdef"
STAMP_RE = re.compile(r"schema-repair-\d{8}T\d{6}Z")


def scrub(text: str, root: Path) -> str:
    text = text.replace(str(root.resolve()), "<ROOT>").replace(str(root), "<ROOT>")
    return STAMP_RE.sub("schema-repair-<STAMP>", text)


def write_goldens(case: str, root: Path, proc: subprocess.CompletedProcess) -> None:
    (OUT / f"{case}.exit").write_text(f"{proc.returncode}\n")
    (OUT / f"{case}.out").write_text(scrub(proc.stdout, root))
    (OUT / f"{case}.err").write_text(scrub(proc.stderr, root))


def seed_registry(home: Path, rows: list[dict], version: int = 43) -> None:
    home.mkdir(parents=True, exist_ok=True)
    (home / "registry.json").write_text(json.dumps({"schema_version": version, "agents": rows}))


def row(name: str, harness: str, sid: str, root: Path, **extra) -> dict:
    return {"name": name, "cwd": str(root), "log_path": "", "harness": harness,
            "harness_session_id": sid, "status": "idle", **extra}


def base_env(root: Path, home: Path, extra: dict) -> dict:
    env = dict(os.environ)
    from fno.harness_identity import AMBIENT_IDENTITY_ENV

    for name in (*AMBIENT_IDENTITY_ENV, "FNO_AGENT_SELF", "FNO_AGENT_SESSION",
                 "FNO_AGENT_ROW_PENDING", "FNO_BG", "FNO_AGENTS_NAME_MODEL"):
        env.pop(name, None)
    for d in ("claude-projects", "codex-sessions", "opencode-storage", "mux", "state"):
        (root / d).mkdir(exist_ok=True)
    (root / "state" / ".path-migration-done").write_text("\n")
    env.update({
        "PYTHONPATH": str(REPO / "cli" / "src"),
        "FNO_AGENTS_HOME": str(home),
        "FNO_HOME": str(root / "state"),
        "FNO_STATE_DIR": str(root / "state"),
        "FNO_CLAUDE_PROJECTS_DIR": str(root / "claude-projects"),
        "FNO_CODEX_SESSIONS_DIR": str(root / "codex-sessions"),
        "FNO_OPENCODE_STORAGE_DIR": str(root / "opencode-storage"),
        "FNO_MUX_DIR": str(root / "mux"),
        # No claude on PATH beyond the system dirs: the whoami live-status
        # probe is never reached by these cases (no claude short id).
    })
    for key, value in extra.items():
        if value is None:
            env.pop(key, None)
        else:
            env[key] = value
    return env


def run(root: Path, home: Path, extra: dict, argv: list[str], cwd: Path | None = None):
    return subprocess.run(
        [str(PY), "-c", "from fno.cli import main; main()", "agents", *argv],
        env=base_env(root, home, extra), cwd=str(cwd or root),
        capture_output=True, text=True, timeout=120,
    )


def claude_transcript(root: Path, sid: str, cwd: str) -> None:
    project = root / "claude-projects" / "-work-repo"
    project.mkdir(parents=True, exist_ok=True)
    lines = [
        {"type": "summary"},
        {"type": "user", "cwd": cwd, "timestamp": "2026-10-20T08:00:00.000Z"},
        {"type": "assistant", "timestamp": "2026-10-20T08:05:30.000Z"},
    ]
    (project / f"{sid}.jsonl").write_text("".join(json.dumps(x) + "\n" for x in lines))


def codex_rollout(root: Path, sid: str, cwd: str) -> None:
    day = root / "codex-sessions" / "2026" / "10" / "20"
    day.mkdir(parents=True, exist_ok=True)
    meta = {"type": "session_meta", "timestamp": "2026-10-20T09:00:00.000Z",
            "payload": {"id": sid, "cwd": cwd}}
    (day / f"rollout-2026-10-20T09-00-00-{sid}.jsonl").write_text(json.dumps(meta) + "\n")


def main() -> int:
    for stale in OUT.glob("*.exit"):
        for ext in (".exit", ".out", ".err"):
            stale.with_suffix(ext).unlink(missing_ok=True)
    root = Path(tempfile.mkdtemp(prefix="registry-verbs-golden-"))
    cases: list[tuple] = []

    # Warmup: spend the one-time bootstrap line so no golden carries it.
    run(root, root / "home-warmup", {}, ["name", "x-1", "--command", "t"])

    # --- name -------------------------------------------------------------
    h = root / "home-name"
    cases += [
        ("name-legacy", h, {}, ["name", "target", "x-245d", "--slug", "port-verbs"]),
        ("name-dispatch", h, {}, ["name", "--source", "kl", "--command", "th", "x-245d",
                                  "--discriminator", "ab12"]),
        ("name-command-word", h, {}, ["name", "--command", "target", "x-1234"]),
        ("name-model-env", h, {"FNO_AGENTS_NAME_MODEL": "opus"}, ["name", "--command", "t", "x-1"]),
        ("name-missing-node", h, {}, ["name"]),
        ("name-unknown-source", h, {}, ["name", "--source", "zz", "--command", "t", "x-1"]),
        ("name-both-forms", h, {}, ["name", "target", "x-1", "--command", "t"]),
        ("name-no-command", h, {}, ["name", "--source", "kl", "x-1"]),
    ]

    # --- whoami -----------------------------------------------------------
    h = root / "home-whoami"
    seed_registry(h, [
        row("calm-otter", "codex", CODEX_ID, root, status="live"),
        row("lead-heron", "claude", SESSION_ID, root, role_level=1, role_scope="x-245d",
            role_grantor="Gavin"),
    ])
    node_cwd = root / "node-cwd"
    (node_cwd / ".fno").mkdir(parents=True, exist_ok=True)
    (node_cwd / ".fno" / "target-state.md").write_text(
        f"---\nclaude_session_id: \"{SESSION_ID}\"\ngraph_node_id: x-245d\n---\n")
    cases += [
        ("whoami-unregistered", h, {}, ["whoami"]),
        ("whoami-env", h, {"FNO_AGENT_SESSION": CODEX_ID, "FNO_AGENT_SELF": "calm-otter"}, ["whoami", "--json"]),
        ("whoami-stale-env", h, {"FNO_AGENT_SESSION": OTHER_ID, "CLAUDE_CODE_SESSION_ID": SESSION_ID},
         ["whoami", "--json"]),
        ("whoami-fallback-role", h, {"CLAUDE_CODE_SESSION_ID": SESSION_ID}, ["whoami"]),
    ]
    cases_cwd = {"whoami-node": node_cwd}
    cases.append(("whoami-node", h, {"CLAUDE_CODE_SESSION_ID": SESSION_ID}, ["whoami", "--json"]))

    # --- registry-repair --------------------------------------------------
    poisoned = [
        row("calm-otter", "codex", CODEX_ID, root, future_field=None, future_list=[]),
        row("swift-heron", "claude", SESSION_ID, root),
    ]
    h = root / "home-repair-dry"
    seed_registry(h, poisoned, version=99)
    cases.append(("repair-dry", h, {}, ["registry-repair", "--to", "43"]))
    h = root / "home-repair-apply"
    seed_registry(h, poisoned, version=99)
    cases.append(("repair-apply", h, {}, ["registry-repair", "--to", "43", "--apply"]))
    h = root / "home-repair-clean"
    seed_registry(h, [row("swift-heron", "claude", SESSION_ID, root)], version=99)
    cases.append(("repair-clean", h, {}, ["registry-repair", "--to", "43"]))
    h = root / "home-repair-low"
    seed_registry(h, [row("swift-heron", "claude", SESSION_ID, root)])
    cases.append(("repair-not-above", h, {}, ["registry-repair", "--to", "43"]))
    h = root / "home-repair-data"
    seed_registry(h, [row("calm-otter", "codex", CODEX_ID, root, future_field=0)], version=99)
    cases.append(("repair-carries-data", h, {}, ["registry-repair", "--to", "43", "--apply"]))
    cases.append(("repair-no-to", h, {}, ["registry-repair"]))

    # --- heal-token -------------------------------------------------------
    h = root / "home-heal"
    seed_registry(h, [row("busy-otter", "codex", CODEX_ID, root, status="live")])
    claude_transcript(root, SESSION_ID, "/work/repo")
    codex_rollout(root, CODEX_ID, "/work/other")
    reg = str(h / "registry.json")
    cases += [
        ("heal-miss", h, {}, ["heal-token", "deadbeef", "--registry", reg]),
        ("heal-not-shaped", h, {}, ["heal-token", "calm-otter", "--registry", reg]),
        ("heal-all-registry-hit", h, {}, ["heal-token", CODEX_ID, "--registry", reg, "--all-sources"]),
        ("heal-all-name-miss", h, {}, ["heal-token", "nobody-here", "--registry", reg, "--all-sources"]),
    ]
    # Adoption runs once into its own home (it writes the row).
    h_adopt = root / "home-heal-adopt"
    seed_registry(h_adopt, [])
    cases.append(("heal-adopt-claude", h_adopt, {},
                  ["heal-token", SESSION_ID[:8], "--registry", str(h_adopt / "registry.json")]))
    h_tomb = root / "home-heal-tomb"
    seed_registry(h_tomb, [])
    import time
    (h_tomb / "rm_tombstones.json").write_text(json.dumps(
        [{"harness": "claude", "session_id": SESSION_ID, "removed_at": int(time.time())}]))
    cases.append(("heal-tombstone", h_tomb, {},
                  ["heal-token", SESSION_ID, "--registry", str(h_tomb / "registry.json")]))

    for case, home, extra, argv in cases:
        write_goldens(case, root, run(root, home, extra, argv, cases_cwd.get(case)))

    # The twin transcript lands last: before it the prefix is unique.
    claude_transcript(root, TWIN_ID, "/work/repo")
    h_amb = root / "home-heal-amb"
    seed_registry(h_amb, [])
    write_goldens("heal-ambiguous", root, run(root, h_amb, {},
                  ["heal-token", SESSION_ID[:8], "--registry", str(h_amb / "registry.json")]))

    written = sorted(p.name for p in OUT.glob("*.exit"))
    print(f"captured {len(written)} cases under {root}: {', '.join(written)}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
