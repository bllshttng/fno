"""Test: size-profile skip flags are present in the immutable session manifest.

After control-plane collapse (ab-d0337fbc, Task 2.2), target-state.md is an
immutable inputs-only manifest:

- skip_flags_initial: REMOVED (no drift-check needed; manifest is write-once)
- phase_init event emission: REMOVED (loop-check reads manifest directly)
- skip flags (no_external, no_docs, ...) still present as top-level inputs

This file tests the new behavior: flat skip flags present per size profile,
skip_flags_initial absent, and no phase_init event emitted.

Old tests that asserted skip_flags_initial and phase_init existence are
replaced here (BUG-LOOP-001 follow-up is complete: the immutable contract
supersedes the drift-check).
"""
from __future__ import annotations

import os
import subprocess
from pathlib import Path

import pytest
import yaml

from _init_space import install_state_path_stub

_REPO_ROOT = Path(__file__).parent.parent.parent.parent
_INIT_SCRIPT = _REPO_ROOT / "hooks" / "helpers" / "init-target-state.sh"

# Expected flat skip_flag values per size profile (matches init-target-state.sh)
_PROFILE_FLAGS: dict[str, dict[str, bool]] = {
    "S": {
        "no_external": True,
        "no_docs": True,
        "no_ship": False,
        "no_verify": True,
        "no_goals": True,
        "no_browser": True,
        "no_clean": True,
        "no_how_to": True,
        "no_memory": True,
    },
    "L": {
        "no_external": False,
        "no_docs": False,
        "no_ship": False,
        "no_verify": False,
        "no_goals": False,
        "no_browser": False,
        "no_clean": False,
        "no_how_to": False,
        "no_memory": False,
    },
}


_FNO_STUB = """\
#!/usr/bin/env bash
# Test stub: answers the fno round-trips hooks/helpers/init-target-state.sh makes
# in a bare env (no graph, no config), so this hook test pays ~0s/round-trip
# instead of ~0.45s of real CLI startup - the load-sensitive cost behind the
# flake (a different size profile blew the timeout each full-suite run). Per-command
# exit status below was captured from ONE real bare-env run (HOME=tmp, no graph),
# not guessed. NOT a blanket exit: the hook branches on fno's exit code (claim
# acquire sets _NODE_OWNED), so a flipped code changes the manifest path. stdout
# is empty everywhere; the hook's degrade paths (|| true, sanctioned-empty
# _SESSION_PID at init-target-state.sh:1044) handle it byte-for-byte as today's
# real-fno runs. Unknown commands exit nonzero = conservative bare-env behavior
# (real fno fails without a graph).
printf '%s\\n' "$*" >> "${FNO_CALLS_LOG:-./fno-calls.log}"
case "$1" in
  config) exit 1 ;;                       # config get -> 1 (no config; degrade to default)
  backlog) exit 1 ;;                      # backlog get/update -> 1 (no graph)
  paths) exit 0 ;;                        # paths shell-stub -> 0; hook evals stdout (empty => STATE_DIR fallback)
  worktree) exit 0 ;;                     # worktree policy -> 0; hook reads stdout (empty => harness-native default)
  do)
    case "$2:$3" in
      target:check-dispatch-hold) exit 0 ;;
      *) exit 1 ;;
    esac ;;
  target)
    case "$2" in
      check-dispatch-hold) exit 0 ;;
      *) exit 1 ;;
    esac ;;
  claim)
    case "$2" in
      status|session-pid|acquire) exit 0 ;;  # all -> 0 in bare env
      *) exit 1 ;;
    esac ;;
  *) exit 1 ;;
esac
"""


def _stub_fno(bin_dir: Path) -> None:
    """Write the PATH-shim `fno` (see _FNO_STUB) into bin_dir and chmod it executable."""
    bin_dir.mkdir(parents=True, exist_ok=True)
    fno = bin_dir / "fno"
    fno.write_text(_FNO_STUB, encoding="utf-8")
    fno.chmod(0o755)


def _space_state(tmpdir: Path) -> Path:
    """Where init writes the manifest once the state-path stub answers."""
    return tmpdir / "space" / "target-state.md"


def _ensure_feature_repo(root: Path) -> None:
    if (root / ".git").exists():
        return
    subprocess.run(["git", "init", "-q", str(root)], check=True)
    subprocess.run(
        ["git", "-C", str(root), "config", "user.email", "fno@test"],
        check=True,
    )
    subprocess.run(
        ["git", "-C", str(root), "config", "user.name", "fno"],
        check=True,
    )
    subprocess.run(
        ["git", "-C", str(root), "commit", "-q", "--allow-empty", "-m", "init"],
        check=True,
    )
    subprocess.run(
        ["git", "-C", str(root), "checkout", "-q", "-b", "feature/init-test"],
        check=True,
    )


def _run_init_script(
    tmpdir: Path, extra_env: dict[str, str], *, timeout_s: int = 900
) -> subprocess.CompletedProcess:
    """Run init-target-state.sh in an isolated tmpdir with the given env overrides.

    A PATH-shim `fno` (tmpdir/bin/fno) replaces the ~17 real CLI round-trips the
    hook makes: the asserted manifest values are bash-computed, and the
    round-trips are startup-cost no-ops in this isolated env. With the stub a run
    is sub-second, so the timeout stops being load-sensitive.
    """
    _ensure_feature_repo(tmpdir)

    plan_file = tmpdir / "plan.md"
    plan_file.write_text("# Test plan\n")

    home_dir = tmpdir / "home"
    home_dir.mkdir(exist_ok=True)
    (home_dir / ".fno").mkdir(exist_ok=True)
    state_dir = tmpdir / ".fno"
    state_dir.mkdir(parents=True, exist_ok=True)

    bin_dir = tmpdir / "bin"
    _stub_fno(bin_dir)
    stub_env = install_state_path_stub(bin_dir, tmpdir / "space")

    env = {
        "HOME": str(home_dir),
        # Prepend the stub dir so the hook resolves tmp/bin/fno first; keep the
        # real PATH tail so bash/date/mkdir still resolve. A stub miss degrades
        # to slow (real fno), never wrong.
        "PATH": f"{bin_dir}{os.pathsep}" + os.environ.get("PATH", "/usr/local/bin:/usr/bin:/bin"),
        "TARGET_START": "1",
        "TARGET_INPUT": str(plan_file),
        "TARGET_AUTO_MERGE": "false",
        "FNO_CALLS_LOG": str(tmpdir / "fno-calls.log"),
        **stub_env,
    }
    env.update(extra_env)

    proc = subprocess.run(
        ["bash", str(_INIT_SCRIPT)],
        cwd=str(tmpdir),
        env=env,
        capture_output=True,
        text=True,
        # The stub makes each run sub-second in isolation. Keep the backstop well
        # above the parallel full-suite scheduling tail so load cannot look like
        # a functional failure, while still bounding a genuinely hung hook.
        timeout=timeout_s,
    )
    return proc


def _parse_target_state_frontmatter(state_file: Path) -> dict:
    """Extract YAML frontmatter from target-state.md (between --- delimiters)."""
    content = state_file.read_text()
    lines = content.splitlines()
    if not lines or lines[0].strip() != "---":
        raise ValueError(f"No opening --- in {state_file}")
    end = next(i for i, ln in enumerate(lines[1:], 1) if ln.strip() == "---")
    frontmatter_text = "\n".join(lines[1:end])
    return yaml.safe_load(frontmatter_text)


@pytest.mark.parametrize("target_size", ["S", "L"])
def test_flat_skip_flags_per_size_profile(target_size, tmp_path):
    """AC1-HP: immutable manifest contains correct flat skip flags for each size profile.

    After control-plane collapse (ab-d0337fbc): skip_flags_initial is GONE;
    flat flags (no_external, no_docs, ...) remain as top-level inputs.
    """
    assert _INIT_SCRIPT.exists(), f"init script missing: {_INIT_SCRIPT}"

    proc = _run_init_script(tmp_path, {"TARGET_SIZE": target_size})
    assert proc.returncode == 0, (
        f"init-target-state.sh exited {proc.returncode} for TARGET_SIZE={target_size}\n"
        f"stdout: {proc.stdout[:500]}\nstderr: {proc.stderr[:500]}"
    )

    state_file = _space_state(tmp_path)
    assert state_file.exists(), "target-state.md not created"

    fm = _parse_target_state_frontmatter(state_file)

    # Immutability: skip_flags_initial must NOT be present
    assert "skip_flags_initial" not in fm, (
        "skip_flags_initial must not be in the immutable manifest "
        "(control-plane collapse, ab-d0337fbc)"
    )

    # Flat flags must match the size profile
    expected = _PROFILE_FLAGS[target_size]
    for flag, expected_val in expected.items():
        actual_val = fm.get(flag)
        assert actual_val == expected_val, (
            f"TARGET_SIZE={target_size}: manifest.{flag} "
            f"expected {expected_val}, got {actual_val}"
        )


def test_cancelled_claimless_session_is_archived_on_next_init(tmp_path):
    """A finalized explicit cancel must not strand the next target init."""
    state_dir = tmp_path / "space"
    state_dir.mkdir()
    (state_dir / "target-state.md").write_text(
        "---\nsession_id: old-run\ninput: old-plan\n---\n",
        encoding="utf-8",
    )
    # The cancel sentinel is written repo-relative (.fno/) by the documented
    # cancel procedure, and the finalize probe reads it there (init :651).
    (tmp_path / ".fno").mkdir(exist_ok=True)
    (tmp_path / ".fno" / ".target-cancelled").touch()
    (state_dir / "events.jsonl").write_text(
        '{"type":"session_finalized","data":{"session_id":"old-run",'
        '"termination_reason":"Interrupted"}}\n',
        encoding="utf-8",
    )

    proc = _run_init_script(tmp_path, {"TARGET_SIZE": "M"})

    assert proc.returncode == 0, proc.stderr
    assert "old-run" not in _space_state(tmp_path).read_text()
    assert list(state_dir.glob("target-state.terminal.*.md"))


def _run_without_fno(tmp_path: Path, target_input: str) -> subprocess.CompletedProcess:
    _ensure_feature_repo(tmp_path)
    (tmp_path / ".fno").mkdir(exist_ok=True)
    home_dir = tmp_path / "home"
    home_dir.mkdir(exist_ok=True)
    (home_dir / ".fno").mkdir(exist_ok=True)
    # The target-family port moved the hold doors onto fno-agents, the same
    # binary the run-id mint rides, so the python-era fiction that split them
    # (fno absent while fno-agents answers) no longer exists: with the door
    # binary present the hold state IS checkable, and proceeding is correct.
    # The degraded refusal under test is the true binary-less one - no fno and
    # no fno-agents - and it fires before the mint is ever reached. No
    # FNO_TEST_SPACE: the legacy $REPO_ROOT/.fno fallback stays pinned.
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir(exist_ok=True)
    env = {
        "HOME": str(home_dir),
        "PATH": f"{bin_dir}:/usr/bin:/bin",
        "TARGET_START": "1",
        "TARGET_INPUT": target_input,
        "TARGET_LOCATION_OK": "main-acknowledged",
    }
    return subprocess.run(
        ["bash", str(_INIT_SCRIPT)],
        cwd=str(tmp_path),
        env=env,
        capture_output=True,
        text=True,
        timeout=90,
    )


def test_fno_absent_refuses_named_node_hold_lookup(tmp_path):
    result = _run_without_fno(tmp_path, "x-5a5c")
    assert result.returncode == 2
    assert "dispatch hold state cannot be checked" in result.stderr
    assert not (tmp_path / ".fno" / "target-state.md").exists()


# ---- env grant posture (x-3855/x-01b9: operator-origin scrub) ----


def test_env_grant_honored_on_operator_origin_run(tmp_path):
    """AC-HP: TARGET_AUTO_MERGE=1 on an attended, agent-free run folds to the
    env grant. This is the documented per-run arm the merge command and finalize
    honor; the role's own origin bar (no FNO_AGENT_SELF) decides who holds
    it."""
    proc = _run_init_script(
        tmp_path, {"TARGET_AUTO_MERGE": "1", "FNO_AGENT_SELF": ""}
    )
    assert proc.returncode == 0, proc.stderr[:500]
    assert "TARGET_AUTO_MERGE=1 ignored" not in proc.stderr
    fm = _parse_target_state_frontmatter(_space_state(tmp_path))
    assert fm["auto_merge_approved"] is True
    assert fm["auto_merge_source"] == "env-target-auto-merge"


def test_env_grant_scrubbed_on_agent_origin_run(tmp_path):
    """codex P1 on PR 1131: an ATTENDED agent session must not mint the
    operator grant either. `_attended` alone reads only TARGET_UNATTENDED and
    config, so the FNO_AGENT_SELF bar is what separates an agent shell from
    an operator's."""
    proc = _run_init_script(
        tmp_path,
        {"TARGET_AUTO_MERGE": "1", "FNO_AGENT_SELF": "some-agent-session"},
    )
    assert proc.returncode == 0, proc.stderr[:500]
    assert "TARGET_AUTO_MERGE=1 ignored" in proc.stderr
    fm = _parse_target_state_frontmatter(_space_state(tmp_path))
    assert fm["auto_merge_approved"] is False
    assert fm["auto_merge_source"] != "env-target-auto-merge"
