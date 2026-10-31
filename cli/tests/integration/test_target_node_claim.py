"""init-target-state.sh node:<id> claim acquire/refuse.

Drives the real init script with a MOCK `fno` on PATH so the shell wiring is
covered deterministically (exit codes controlled, args/env recorded) without
depending on the installed `fno` snapshot. Proves:

  * a bare `/target ab-XXXX` input (no plan) acquires node:<id>,
  * the acquire uses a TTL and the global root (FNO_CLAIMS_ROOT=$HOME),
  * exit 1 (held-by-other) refuses via the .target-cancelled sentinel,
  * a session with no proven harness identity does not claim.
"""
from __future__ import annotations

import os
import subprocess
import sys
from pathlib import Path
from typing import NamedTuple

import pytest

from tests._init_space import install_state_path_stub
from tests.fixtures.graph_seed import seed_graph

from fno.rust_binary import find_dev_binary

REPO_ROOT = Path(__file__).resolve().parents[3]
INIT_SCRIPT = REPO_ROOT / "hooks" / "helpers" / "init-target-state.sh"

# NO module-level skipif here, and the omission is load-bearing. It used to read
# `skipif(not INIT_SCRIPT.exists())`, so deleting the very script this file
# exists to test made the ENTIRE file pass silently. A missing script under test
# is a failure, not a reason to abstain, and it fails at the first subprocess
# call with the path in the message.

NODE_ID = "ab-deadbeef"  # matches ^ab-[0-9a-f]{8}$

MOCK_ABI = """#!/usr/bin/env bash
# Mock `fno`: log argv + the claims-root env, control claim-acquire exit code.
echo "ARGS:$* ROOT:${FNO_CLAIMS_ROOT:-UNSET}" >> "$MOCK_ABI_LOG"
if [[ "$1" == "agents" && "$2" == "claim" && "$3" == "acquire" ]]; then
  # A canned success fakes the lock: under the claim projection the graph row
  # answers from the store, so the success path must write a REAL claim. The
  # human flags (--ttl 2h, handover, harness) are front-surface, so delegate
  # to this checkout's fno front - exactly what production runs. Production's
  # `agents_client_route.rs` strips the leading `agents` word before handing
  # `claim acquire ...` to the Python front (fno's own `claim` group is flat,
  # not nested under `agents`), so the mock must drop it too ("${@:2}", not
  # "$@") or the front sees an `agents` command it no longer has.
  # Only the refusal codes stay canned - those tests assert the refusal
  # wiring, not the store.
  if [[ "${MOCK_ABI_ACQUIRE_RC:-0}" == "0" ]]; then
    # The init script discards the front's stdout and sends stderr to
    # .claim-err; a silent rc=1 keeps its real refusal on stdout, so this
    # delegation records both streams for the failing assertion to print.
    "${MOCK_ABI_FRONT:?no fno front}" "${@:2}" > "$MOCK_ABI_LOG.front-out" 2> "$MOCK_ABI_LOG.front-err"
    rc=$?
    echo "FRONT_RC:$rc" >> "$MOCK_ABI_LOG"
    # A silent front refusal is not decidable from its streams alone: record
    # WHICH binaries answered, the FNO env they saw, and run the native
    # directly on the same argv (again without the routed-away `agents` word)
    # so a front-side death separates from a native-side one.
    if [[ "$rc" != "0" ]]; then
      echo "FRONT_BIN: $MOCK_ABI_FRONT" >> "$MOCK_ABI_LOG"
      ls -la "$MOCK_ABI_FRONT" >> "$MOCK_ABI_LOG" 2>&1
      echo "NATIVE_BIN: $MOCK_ABI_NATIVE" >> "$MOCK_ABI_LOG"
      ls -la "$MOCK_ABI_NATIVE" >> "$MOCK_ABI_LOG" 2>&1
      env | grep -E "^(FNO_|HOME=|TARGET_|PYTHON)" | sort >> "$MOCK_ABI_LOG"
      "${MOCK_ABI_NATIVE:?no native}" "${@:2}" > "$MOCK_ABI_LOG.native-out" 2> "$MOCK_ABI_LOG.native-err"
      echo "NATIVE_RC:$?" >> "$MOCK_ABI_LOG"
    fi
    exit "$rc"
  fi
  exit "$MOCK_ABI_ACQUIRE_RC"
fi
# `backlog get` is how the node guard establishes that a token IS a graph node.
# The fixture reader answers from the seeded store. Delegating to the
# roadmap-tasks shim recursed mock -> shim -> mock once the shim became a
# front-door forwarder: the shim's own `fno` resolves to THIS mock on the
# sandbox PATH, and the spawn chain never terminates.
if [[ "$1" == "backlog" && "$2" == "get" ]]; then
  exec python3 "$MOCK_ABI_GET" "${@:2}"
fi
# The owned-identity command: a test that pins the refusal wording controls the
# KEY=value answer here (empty by default, which reads as the stale-fno
# fail-open path).
if [[ "$1" == "do" && "$2" == "target" && "$3" == "resolve-owned-identity" ]]; then
  if [[ -n "${MOCK_ABI_OWNED_OUT:-}" ]]; then
    printf '%s\n' "$MOCK_ABI_OWNED_OUT"
  fi
  exit 0
fi
# The graph lock stamp is the one call whose EFFECT a test asserts, so swallowing
# it as a bare success would hollow out the identity assertion. The update leaf
# answers natively now, so delegate to this checkout's fno-agents dev build - the
# same writer the installed fno front dispatches to.
if [[ "$1" == "backlog" && "$2" == "update" ]]; then
  export FNO_STATE_DIR="${MOCK_ABI_STATE_DIR}"
  exec "${MOCK_ABI_NATIVE:?no dev fno-agents build}" "${@:1}"
fi
exit 0
"""

GRAPH_GET = """#!/usr/bin/env python3
\"\"\"Fixture-graph reader behind the sandbox mock's `backlog get`.

Reads the store the way the product does (fno.graph.store), with a raw-JSON
fallback for bare-list seeds. An absent node exits 1; a present node prints
the requested --field as JSON, or the whole row without one. --strict is
accepted and ignored: the fixture has no archive leg, so store presence IS
liveness.
\"\"\"
import json
import os
import sys


def _rows(path):
    try:
        from fno.graph.store import read_graph_strict

        return read_graph_strict(path)
    except Exception:
        pass
    with open(path, encoding="utf-8") as handle:
        doc = json.load(handle)
    if isinstance(doc, list):
        return doc
    return doc.get("entries") or doc.get("nodes") or []


def main() -> int:
    raw = sys.argv[1:]
    node_id = next((a for a in raw if not a.startswith("--") and a != "get"), None)
    field = sys.argv[sys.argv.index("--field") + 1] if "--field" in raw else None
    path = os.path.join(os.environ["MOCK_ABI_STATE_DIR"], "db", "graph.json")
    rows = _rows(path)
    node = next(
        (r for r in rows if isinstance(r, dict) and r.get("id") == node_id), None
    )
    if node is None:
        return 1
    value = node.get(field) if field else node
    # The command's text output prints scalars bare (ab-deadbeef, True, 123);
    # only containers ride the JSON spelling.
    if isinstance(value, (str, bool, int, float)) or value is None:
        print(value)
    else:
        print(json.dumps(value))
    return 0


if __name__ == "__main__":
    sys.exit(main())
"""


def _sandbox(tmp_path: Path, node_id: str = NODE_ID):
    home = tmp_path / "home"
    (home / ".fno").mkdir(parents=True)
    (home / ".fno" / "db").mkdir(parents=True, exist_ok=True)
    seed_graph(
        home / ".fno" / "db" / "graph.json",
        [{"id": node_id, "title": "t", "status": "idea", "priority": "p2",
          "project": "fno", "plan_path": None}],
    )
    repo = tmp_path / "repo"
    repo.mkdir()
    subprocess.run(["git", "init", "-q"], cwd=repo, check=True)
    (repo / ".fno").mkdir()

    bindir = tmp_path / "bin"
    bindir.mkdir()
    mock = bindir / "fno"
    mock.write_text(MOCK_ABI)
    mock.chmod(0o755)
    getr = bindir / "graph-get.py"
    getr.write_text(GRAPH_GET)
    getr.chmod(0o755)
    log = tmp_path / "fno.log"

    # The stub answers `fno-agents state path` so init writes its state into the
    # test-owned space dir whether or not a real fno-agents sits on PATH. The
    # legacy $REPO_ROOT/.fno leg stays pinned by the no-binary tests in
    # test_init_target_state_skip_flags.py.
    space = tmp_path / "space"
    stub_env = install_state_path_stub(bindir, space)

    # Pin `python3` to the interpreter running these tests. Production no longer
    # shells an interpreter for the stamp, but the mock's delegation to the
    # real writer below does, and it needs fno's dependencies importable; an
    # ambient python3 (homebrew's, say) has no typer. Which python3 sits first on
    # PATH must not decide whether the stamped-node assertion holds.
    py = bindir / "python3"
    py.write_text(f'#!/usr/bin/env bash\nexec "{sys.executable}" "$@"\n')
    py.chmod(0o755)

    native = find_dev_binary()
    if native is None:
        pytest.skip("no dev fno-agents build under crates/fno-agents/target")

    # The real acquire delegate: this checkout's fno front (fno.cli:main),
    # same interpreter the tests run under.
    front = Path(sys.executable).with_name("fno-py")
    if not front.exists():
        pytest.skip("no fno-py console script beside the test interpreter")

    env = os.environ.copy()
    env.update({
        "TARGET_START": "1",
        "TARGET_INPUT": node_id,
        "TARGET_SIZE": "S",
        "TARGET_SESSION_ID": "worker-session",
        "HOME": str(home),
        "PATH": f"{bindir}:{env['PATH']}",
        "MOCK_ABI_LOG": str(log),
        "MOCK_ABI_GET": str(getr),
        "MOCK_ABI_NATIVE": str(native),
        "MOCK_ABI_STATE_DIR": str(home / ".fno"),
        "MOCK_ABI_FRONT": str(front),
        **stub_env,
    })
    # The state-path stub owns FNO_AGENTS_BIN, and the ported claim leaf
    # resolves that env FIRST, so the front's claim delegation would exec the
    # stub and inherit its silent no-binary exit 1 (a smoke runner keeps no
    # fno-agents on PATH). The stub's own contract answers this: other commands
    # delegate to the next real fno-agents on PATH, so give the walk one.
    env["PATH"] = f"{bindir}:{native.parent}:{env['PATH']}"
    return repo, home, log, env


class _InitResult(NamedTuple):
    returncode: int
    stderr: str


def _run_init(repo: Path, env: dict):
    """Run the init script under a bounded process group. A hung delegate
    (the mock `exec`s into the real front) survives a lone child kill and
    holds the output pipes, so the group dies together and a stall reports
    its own captured output instead of hanging the runner."""
    import os
    import signal

    proc = subprocess.Popen(
        ["bash", str(INIT_SCRIPT)],
        cwd=repo,
        env=env,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        start_new_session=True,
    )
    try:
        out, err = proc.communicate(timeout=90)
        return _InitResult(proc.returncode, err)
    except subprocess.TimeoutExpired:
        os.killpg(proc.pid, signal.SIGKILL)
        out, err = proc.communicate()
        raise AssertionError(
            f"init stalled: group-killed at 90s\nSTDOUT tail:\n{out[-2000:]}\n"
            f"STDERR tail:\n{err[-2000:]}"
        )


def _state(repo: Path) -> str:
    assert not (repo / ".fno" / "target-state.md").exists(), \
        "state must live in the stub space, not the legacy path"
    f = repo.parent / "space" / "target-state.md"
    return f.read_text() if f.exists() else ""


def test_bare_node_id_acquires_global_ttl_claim(tmp_path):
    node_id = NODE_ID
    repo, home, log, env = _sandbox(tmp_path, node_id)
    env["TARGET_SESSION_ID"] = "worker-session-a"
    env["MOCK_ABI_ACQUIRE_RC"] = "0"
    r = _run_init(repo, env)
    state = _state(repo)
    assert state, f"no state written: rc={r.returncode} stderr={r.stderr[:600]!r}"

    # The state names only the blocked class; init also writes the acquire's
    # stderr to .claim-err and its own stderr, and the refusal lives there.
    # The mock's delegation records the front's stdout/stderr beside the log.
    claim_err = repo.parent / "space" / ".claim-err"
    err_text = claim_err.read_text() if claim_err.exists() else ""
    front_out = Path(f"{log}.front-out")
    front_err = Path(f"{log}.front-err")
    front_text = ""
    if front_out.exists() or front_err.exists():
        front_text = (
            f"--- front stdout ---\n{front_out.read_text() if front_out.exists() else ''}"
            f"--- front stderr ---\n{front_err.read_text() if front_err.exists() else ''}"
        )
    native_out = Path(f"{log}.native-out")
    native_err = Path(f"{log}.native-err")
    native_text = ""
    if native_out.exists() or native_err.exists():
        native_text = (
            f"--- native stdout ---\n{native_out.read_text() if native_out.exists() else ''}"
            f"--- native stderr ---\n{native_err.read_text() if native_err.exists() else ''}"
        )
    home_walk = sorted(
        str(p2.relative_to(home)) for p2 in home.rglob("*") if p2.is_file()
    )
    assert f'target_claim_key: "node:{node_id}"' in state, (
        f"{state}\n--- init stderr ---\n{r.stderr}\n"
        f"--- .claim-err ---\n{err_text}\n{front_text}{native_text}"
        f"--- sandbox home files ---\n{chr(10).join(home_walk)}\n"
        f"--- mock log ---\n{log.read_text() if log.exists() else '(absent)'}"
    )
    assert "target_claim_holder:" in state
    assert 'target_claim_ttl: "2h"' in state
    assert not (repo / ".fno" / ".target-cancelled").exists()

    log_text = log.read_text()
    acquire_lines = [ln for ln in log_text.splitlines()
                     if "claim acquire" in ln and node_id in ln]
    assert acquire_lines, log_text
    line = acquire_lines[0]
    assert "--ttl 2h" in line, line
    assert "--holder target-session:worker-session-a" in line, line
    assert "--pid-unavailable" in line, line
    assert f"ROOT:{home}" in line, "acquire must set FNO_CLAIMS_ROOT=$HOME: " + line


def test_node_without_proven_harness_identity_does_not_claim(tmp_path):
    repo, home, log, env = _sandbox(tmp_path)
    env["MOCK_ABI_ACQUIRE_RC"] = "0"
    env.pop("TARGET_SESSION_ID", None)
    env.pop("CODEX_THREAD_ID", None)
    env.pop("CLAUDE_CODE_SESSION_ID", None)
    result = _run_init(repo, env)
    state = _state(repo)
    assert result.returncode == 0, result.stderr
    assert "target_claim_blocked_reason: holder_unattributable" in state
    assert "claim acquire" not in log.read_text()
    assert 'target_claim_key: "node:' not in state


def test_codex_thread_identity_aligns_manifest_graph_and_claim(
    tmp_path, monkeypatch
):
    repo, home, log, env = _sandbox(tmp_path)
    # The projection reads the claims db through the ROOT env (claims live at
    # <root>/.fno/claims); the init script acquires with the same root.
    monkeypatch.setenv("FNO_CLAIMS_ROOT", str(home))
    (repo / "scripts").symlink_to(REPO_ROOT / "scripts", target_is_directory=True)
    thread_id = "019f48e4-codex-owner"
    env.update(
        {
            "CODEX_THREAD_ID": thread_id,
            "CODEX_SESSION_ID": "legacy-must-not-own",
            "MOCK_ABI_ACQUIRE_RC": "0",
        }
    )
    env.pop("CLAUDE_CODE_SESSION_ID", None)
    env.pop("TARGET_SESSION_ID", None)

    result = _run_init(repo, env)
    state = _state(repo)
    assert state, result.stderr
    # The store owns state; the json mirror can lag the last write.
    from fno.graph.store import read_graph_strict

    graph = read_graph_strict(home / ".fno" / "db" / "graph.json")[0]
    acquire = next(
        line for line in log.read_text().splitlines() if "claim acquire" in line
    )

    manifest_session_id = next(
        line.split(":", 1)[1].strip()
        for line in state.splitlines()
        if line.startswith("session_id:")
    )
    assert manifest_session_id != thread_id
    assert f"codex_thread_id: {thread_id}" in state
    assert "harness: codex" in state, state
    assert f"harness_session_id: {thread_id}" in state, state
    # The projection serves the claim HOLDER, and a target dispatch holds
    # under the target-session: prefix; the raw thread id stays manifest-only.
    assert graph["session_id"] == f"target-session:{thread_id}"
    assert f'--holder target-session:{thread_id}' in acquire
    assert f'target_claim_holder: "target-session:{thread_id}"' in state


def test_held_by_other_refuses(tmp_path):
    repo, home, log, env = _sandbox(tmp_path)
    env["MOCK_ABI_ACQUIRE_RC"] = "1"  # ClaimHeldByOther
    r = _run_init(repo, env)
    state = _state(repo)
    assert state, f"no state written: rc={r.returncode} stderr={r.stderr[:600]!r}"

    assert (repo.parent / "space" / ".target-cancelled").exists(), \
        "exit 1 must touch the cancel sentinel"
    assert "target_claim_blocked_reason: claim_held_by_other" in state
    assert f"graph_node_id: {NODE_ID}" in state
    assert f'target_claim_key: "node:{NODE_ID}"' not in state

