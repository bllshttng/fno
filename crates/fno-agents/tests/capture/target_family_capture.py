#!/usr/bin/env python3
"""Golden capture driver for the target-family port.

Plays the case tables against the live Python leg (`fno do state|phase|target`
subcommands, `do pr stub-manifest`, and `python -m fno.state.target_handoff`)
and freezes `tests/golden/target_family/` in the `assert_golden` on-disk
format: `<key>.exit` ("<code>\\n"), `<key>.out`, `<key>.err`. The
`target_family_parity.rs` tables mirror these cases; keep them in lockstep
when a case is added.

Captured before the Python body deletes, per step 2 of the port protocol
(docs/architecture/dual-implementation-inventory.md). The hermetic temp root
cannot offer a live PR or a populated review registry, so the network lanes
surface here as their designed refusals. That IS the contract the
characterization freezes.

Every case runs the worktree Python front through the same env block the
parity test applies: temp HOME/state roots, the claude session pins, the
other harness markers blanked, git config pinned to /dev/null. Children
(the native doors) resolve `fno-agents` through FNO_AGENTS_BIN, pinned to
the installed binary at capture time.

Usage:
    python3 crates/fno-agents/tests/capture/target_family_capture.py
"""
from __future__ import annotations

import json
import os
import re
import shutil
import stat
import subprocess
import sys
import tempfile
from pathlib import Path

REPO = Path(__file__).resolve().parents[4]
CLI_SRC = REPO / "cli" / "src"
GOLDEN_DIR = REPO / "crates" / "fno-agents" / "tests" / "golden" / "target_family"

SID = "a1b2c3d4-1111-2222-3333-444455556666"
CREATED = "2026-10-25T09:00:00Z"

DROP_PREFIXES = (
    "fno: this claude session joined fno",
)

# A minimal immutable target manifest, frontmatter plus the body keys
# init-target-state.sh writes below it (target/manifest.py BODY_KEYS).
MANIFEST = """---
harness: claude
harness_session_id: {sid}
fno_id: fno-test-0001
graph_node_id: {node}
target_claim_key: node:{node}
target_claim_holder: review-session:{sid}
owner_cwd: "<ROOT>"
input: fixture target
plan_path: ""
---

graph_node_id: {node}
target_claim_key: node:{node}
target_claim_holder: review-session:{sid}
"""

# A plan whose frontmatter carries the dispatch_hold block the gate reads.
HOLD_PLAN = """---
status: ready
dispatch_hold:
  reason: fixture hold
  release_when: never
  set_by: capture
  review_on: 2026-12-01
---

# Fixture plan
"""

STUB_MANIFEST = {
    "node": "PLACE",
    "contract_version": 1,
    "contract_ref": "design.md#interface-contract",
    "reconciled": False,
    "contract_test": "true",
    "stubs": [
        {"stub_id": "create-user", "file": "src/api.ts", "symbol": "createUser",
         "contract_ref": "design.md#interface-contract", "kind": "function"}
    ],
}


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


def installed_agents_bin() -> str:
    found = shutil.which("fno-agents")
    if found is None:
        raise SystemExit("fno-agents not on PATH; the capture pins children to it")
    return found


def installed_front() -> str:
    found = shutil.which("fno")
    if found is None:
        raise SystemExit("fno not on PATH; the capture routes non-family commands to it")
    return found


FAMILY_HEADS = (("do", "state"), ("do", "phase"), ("do", "target"), ("do", "pr", "stub-manifest"))


def make_shim(bindir: Path) -> None:
    """A PATH shim named `fno`: family commands reach the worktree's Python app
    (the leg under capture); every other spelling execs the installed front,
    so seeding doors (`backlog idea`) and the native body behind a transport
    leaf behave as production."""
    bindir.mkdir(parents=True, exist_ok=True)
    shim = bindir / "fno"
    shim.write_text(
        f"#!{venv_python()}\n"
        "import os, sys\n"
        f"sys.path.insert(0, {str(CLI_SRC)!r})\n"
        "args = sys.argv[1:]\n"
        "is_family = args[:2] in [%s] or args[:3] == ['do', 'pr', 'stub-manifest']\n"
        % ", ".join(repr(list(h)) for h in FAMILY_HEADS)
        + "if not is_family:\n"
        f"    os.execv({installed_front()!r}, ['fno', *args])\n"
        "from fno.cli import app\n"
        "app()\n"
    )
    shim.chmod(shim.stat().st_mode | stat.S_IEXEC)


def base_env(root: Path, bindir: Path) -> dict:
    env = dict(os.environ)
    # Ambient FNO_* values are the capture machine's fleet posture, not the
    # oracle's contract: an inherited FNO_ROUTE_PROVIDER once froze
    # provider=zai into a golden and every other machine read it as drift.
    # Keep only the pins this driver sets.
    keep = {
        "FNO_STATE_DIR", "FNO_HOME", "FNO_AGENTS_HOME", "FNO_BUS_DIR",
        "FNO_EVENTS_PATH", "FNO_CLAIMS_ROOT", "FNO_HARNESS_NAME",
        "FNO_HARNESS_SESSION_ID", "FNO_AGENTS_BIN",
        "FNO_TEST_HERMETIC", "FNO_STYLE_ENFORCE",
    }
    for key in [k for k in env if k.startswith("FNO_") and k not in keep]:
        del env[key]
    env.update(
        HOME=str(root),
        FNO_STATE_DIR=str(root),
        FNO_HOME=str(root),
        FNO_AGENTS_HOME=str(root / "agents-home"),
        FNO_BUS_DIR=str(root / "bus"),
        FNO_EVENTS_PATH=str(root / "events.jsonl"),
        FNO_CLAIMS_ROOT=str(root),
        FNO_HARNESS_NAME="claude",
        FNO_HARNESS_SESSION_ID=SID,
        CLAUDE_CODE_SESSION_ID=SID,
        CODEX_THREAD_ID="",
        CODEX_SESSION_ID="",
        GEMINI_SESSION_ID="",
        OPENCODE_SESSION_ID="",
        CLAUDE_SESSION_ID="",
        FNO_AGENTS_BIN=installed_agents_bin(),
        GIT_CONFIG_GLOBAL="/dev/null",
        GIT_CONFIG_SYSTEM="/dev/null",
        PATH=f"{bindir}{os.pathsep}{env.get('PATH', '')}",
    )
    env.pop("FNO_AGENT_SELF", None)
    env.pop("TARGET_INPUT", None)
    env.pop("TARGET_PLAN_PATH", None)
    env["FNO_TEST_HERMETIC"] = "1"
    env["FNO_STYLE_ENFORCE"] = "0"
    return env


def write_manifest(root: Path, node: str) -> None:
    state = root / ".fno"
    state.mkdir(parents=True, exist_ok=True)
    text = MANIFEST.replace("{sid}", SID).replace("{node}", node)
    text = text.replace("<ROOT>", str(root))
    (state / "target-state.md").write_text(text)


def seed_node(env: dict, root: Path, title: str) -> str:
    """Mint one node through the installed front and return its id.

    The id comes from stdout's JSON doc only: the stderr session line names
    the session's most recent node, which for a second seed is a different
    node than the one this call mints.
    """
    proc = subprocess.run(
        ["fno", "backlog", "idea", title, "--difficulty", "high", "--separate"],
        env=env, cwd=root, capture_output=True, text=True, timeout=120,
    )
    match = re.search(r'"id":\s*"([a-z]{1,8}-[0-9a-f]{4,8})"', proc.stdout)
    if match is None:
        raise SystemExit(f"node seed failed: {proc.stdout!r} {proc.stderr!r}")
    return match.group(1)


def normalize(text: str, root: Path) -> str:
    kept = [line for line in text.split("\n") if not any(line.startswith(p) for p in DROP_PREFIXES)]
    text = "\n".join(kept)
    text = text.rstrip("\n")
    try:
        value = json.loads(text)
        if isinstance(value, dict):
            text = json.dumps(value, sort_keys=True, ensure_ascii=False, separators=(",", ":"))
    except Exception:
        pass
    text = re.sub(r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?Z?", "<TS>", text)
    text = re.sub(r"\d{4}-\d{2}-\d{2}", "<DATE>", text)
    text = re.sub(r"\d{2}:\d{2}:\d{2}", "<TIME>", text)
    text = re.sub(r"\.archived\.\d+\.md", ".archived.<TS>.md", text)
    text = re.sub(r"\b[a-z]{1,8}-[0-9a-f]{4,8}\b", "NODE-HEX", text)
    text = re.sub(r"\b[0-9a-fA-F]{40}\b", "SHA40", text)
    text = re.sub(r"\b[0-9a-fA-F]{8}\b", "HEX8", text)
    real = os.path.realpath(root)
    text = text.replace(real, "<ROOT>").replace(str(root), "<ROOT>")
    return text


def slug(label: str) -> str:
    out = []
    prev_us = False
    for ch in label:
        if ch.isascii() and ch.isalnum():
            out.append(ch.lower())
            prev_us = False
        elif not prev_us:
            out.append("_")
            prev_us = True
    return "".join(out).strip("_")


def write_golden(label: str, exit_code: int, out: str, err: str) -> None:
    GOLDEN_DIR.mkdir(parents=True, exist_ok=True)
    key = slug(label)
    (GOLDEN_DIR / f"{key}.exit").write_text(f"{exit_code}\n")
    (GOLDEN_DIR / f"{key}.out").write_text(out)
    (GOLDEN_DIR / f"{key}.err").write_text(err)


CASES: list[dict] = [
    # -- do state --
    dict(label="state show missing file", argv=["do", "state", "show"]),
    dict(label="state show full", argv=["do", "state", "show"], manifest=True),
    dict(label="state show field", argv=["do", "state", "show", "--field", "plan_path"], manifest=True),
    dict(label="state show json", argv=["do", "state", "--json", "show"], manifest=True),
    dict(label="state set other field refused", argv=["do", "state", "set", "--field", "status", "--value", "done"], manifest=True),
    dict(label="state set plan_path twice refused", argv=["do", "state", "set", "--field", "plan_path", "--value", "plans/x.md"],
         manifest=True, preset_plan_path=True),
    dict(label="state init bootstrap redirect", argv=["do", "state", "init"]),
    dict(label="state init allow stub", argv=["do", "state", "init", "--allow-stub"]),
    dict(label="state init force overwrite", argv=["do", "state", "init", "--allow-stub", "--force"]),
    dict(label="state archive", argv=["do", "state", "archive"], manifest=True),
    # -- do phase --
    dict(label="phase kill check default no state", argv=["do", "phase", "kill-check"]),
    dict(label="phase kill check with plan arg", argv=["do", "phase", "kill-check", "plans/no-kills.md"], plan_no_kills=True),
    # -- do target gates --
    # No status cases: the command is already native (PR #3887 made it a
    # transport leaf over target-status), so its remaining contract is the
    # front-door claim, not a golden; the pinned capture binary predates the
    # door and would freeze a stale-binary refusal the branch-built binary
    # cannot replay.
    dict(label="target check review gate clear", argv=["do", "target", "check-review-gate"]),
    dict(label="target check review gate refused", argv=["do", "target", "check-review-gate"],
         config="[review]\nreviewers = [\"sigma\"]\n"),
    dict(label="target check contained no input", argv=["do", "target", "check-contained"]),
    dict(label="target check contained plain node", argv=["do", "target", "check-contained"],
         seed="fixture plain target node"),
    dict(label="target check contained child refused", argv=["do", "target", "check-contained"],
         seed="fixture epic parent", contain=True),
    dict(label="target check dispatch hold no input", argv=["do", "target", "check-dispatch-hold"]),
    dict(label="target check dispatch hold held", argv=["do", "target", "check-dispatch-hold"],
         seed="fixture held node", hold_plan=True),
    dict(label="target resolve owned identity", argv=["do", "target", "resolve-owned-identity"]),
    dict(label="target denominator ratio empty", argv=["do", "target", "denominator-ratio", "--json"]),
    dict(label="target resolve model no pin", argv=["do", "target", "resolve-model"], seed="fixture unpinned node", node_arg=True),
    dict(label="target resolve model missing node", argv=["do", "target", "resolve-model", "x-0000"]),
    dict(label="target request self review no repo", argv=["do", "target", "request-self-review"]),
    dict(label="target request self review no pr", argv=["do", "target", "request-self-review"], git_init=True),
    dict(label="target review invocation", argv=["do", "target", "review-invocation"]),
    # -- do pr stub-manifest --
    dict(label="stub manifest write valid", argv=["do", "pr", "stub-manifest", "write", "--node", "x-1234",
         "--contract-version", "1", "--contract-ref", "design.md#interface-contract",
         "--contract-test", "true", "--stubs-json", json.dumps(STUB_MANIFEST["stubs"])]),
    dict(label="stub manifest write malformed", argv=["do", "pr", "stub-manifest", "write", "--node", "x-1234",
         "--contract-version", "1", "--contract-ref", "ref", "--contract-test", "true",
         "--stubs-json", "[{\"stub_id\": \"x\"}]"]),
    dict(label="stub manifest write missing contract test", argv=["do", "pr", "stub-manifest", "write", "--node", "x-1234",
         "--stubs-json", "[]"]),
    dict(label="stub manifest reconcile validate authorize", argv=["do", "pr", "stub-manifest", "reconcile-validate",
         "--node", "x-1234"], preset_stub=True),
    dict(label="stub manifest reconcile validate drift", argv=["do", "pr", "stub-manifest", "reconcile-validate",
         "--node", "x-1234"], preset_stub_drift=True),
    dict(label="stub manifest reconcile validate missing manifest", argv=["do", "pr", "stub-manifest", "reconcile-validate",
         "--node", "x-9999"]),
    dict(label="stub manifest reconcile finalize valid", argv=["do", "pr", "stub-manifest", "reconcile-finalize",
         "--node", "x-1234"], preset_stub=True),
    dict(label="stub manifest reconcile finalize missing", argv=["do", "pr", "stub-manifest", "reconcile-finalize",
         "--node", "x-9999"]),
    # -- module call --
    dict(label="handoff prepare absent claim", module=True),
]


def run_case(case: dict, bindir: Path) -> None:
    root = Path(tempfile.mkdtemp(prefix="fnotfamcap"))
    try:
        env = base_env(root, bindir)
        # Skip the installed front's one-time uv provisioning: the sentinel
        # and settings it would write carry no command behavior, and writing them
        # here keeps the goldens free of machine-speed install noise.
        (root / ".path-migration-done").write_text("captured\n")
        (root / "settings.yaml").write_text("")
        node = "x-1234"
        if case.get("manifest"):
            write_manifest(root, node)
        if case.get("preset_plan_path"):
            write_manifest(root, node)
            p = root / ".fno" / "target-state.md"
            p.write_text(p.read_text().replace('plan_path: ""', "plan_path: plans/x.md"))
        if case.get("config"):
            cf = root / ".fno"
            cf.mkdir(parents=True, exist_ok=True)
            (cf / "config.toml").write_text(case["config"])
        if case.get("plan_no_kills"):
            plans = root / "plans"
            plans.mkdir(exist_ok=True)
            (plans / "no-kills.md").write_text("---\nstatus: ready\n---\n\n# no kill criteria here\n")
        if case.get("git_init"):
            subprocess.run(["git", "init", "-q", "-b", "main"], env=env, cwd=root, check=True, timeout=60)
            subprocess.run(["git", "config", "user.email", "cap@example.com"], env=env, cwd=root, check=True, timeout=60)
            subprocess.run(["git", "config", "user.name", "capture"], env=env, cwd=root, check=True, timeout=60)
            (root / "seed.txt").write_text("seed\n")
            subprocess.run(["git", "add", "seed.txt"], env=env, cwd=root, check=True, timeout=60)
            subprocess.run(["git", "commit", "-q", "-m", "seed"], env=env, cwd=root, check=True, timeout=60)
        if case.get("seed"):
            node = seed_node(env, root, case["seed"])
            env["TARGET_INPUT"] = node
            if case.get("manifest"):
                write_manifest(root, node)
        if case.get("contain"):
            child = seed_node(env, root, "fixture contained child")
            proc = subprocess.run(
                ["fno", "backlog", "contain", node, child],
                env=env, cwd=root, capture_output=True, text=True, timeout=120,
            )
            env["TARGET_INPUT"] = child
        if case.get("hold_plan"):
            plans = root / "plans"
            plans.mkdir(exist_ok=True)
            (plans / "hold.md").write_text(HOLD_PLAN)
            env["TARGET_PLAN_PATH"] = "plans/hold.md"
            subprocess.run(
                ["fno", "backlog", "update", node, "plan_path=plans/hold.md"],
                env=env, cwd=root, capture_output=True, text=True, timeout=120,
            )
        if case.get("preset_stub") or case.get("preset_stub_drift"):
            mf = root / ".fno"
            mf.mkdir(parents=True, exist_ok=True)
            doc = dict(STUB_MANIFEST)
            doc["node"] = node
            if case.get("preset_stub_drift"):
                doc["contract_test"] = "false"
            (mf / "stub-manifest-x-1234.json").write_text(json.dumps(doc, indent=2))
        if case.get("node_arg"):
            argv = [*case["argv"], node]
        else:
            argv = case.get("argv", [])
        if case.get("module"):
            menv = dict(env)
            menv["PYTHONPATH"] = str(CLI_SRC)
            proc = subprocess.run(
                [venv_python(), "-m", "fno.state.target_handoff", "prepare",
                 "--state", str(root / ".fno" / "target-state.md"),
                 "--archive", str(root / ".fno" / "archives" / "target-state.md"),
                 "--claim-key", f"node:{node}",
                 "--holder", f"review-session:{SID}"],
                env=menv, cwd=root, capture_output=True, text=True, timeout=300,
            )
        else:
            proc = subprocess.run(
                ["fno", *argv],
                env=env,
                cwd=root,
                capture_output=True,
                text=True,
                timeout=300,
            )
        write_golden(
            case["label"],
            proc.returncode,
            normalize(proc.stdout, root),
            normalize(proc.stderr, root),
        )
        print(f"captured {slug(case['label'])}: exit {proc.returncode}")
    finally:
        shutil.rmtree(root, ignore_errors=True)


def main() -> None:
    bindir = Path(tempfile.mkdtemp(prefix="fnotfamshim"))
    make_shim(bindir)
    try:
        for case in CASES:
            run_case(case, bindir)
    finally:
        shutil.rmtree(bindir, ignore_errors=True)
    print(f"goldens written to {GOLDEN_DIR}")


if __name__ == "__main__":
    main()
