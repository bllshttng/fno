"""`fno do target init --deliverables N` stamps the scope denominator.

A plan-less code run historically had no denominator, so "shipped 1 of 4" was
inexpressible rather than unreported. `--deliverables N` declares the count into
the immutable manifest; with no flag and no plan, init now DERIVES the count
from the node's own details and stamps that. Omitting both leaves the field
ABSENT (a non-code payload or an idea run), never 0.

AC1-DENOM  `--deliverables 3` writes `deliverables: 3`. Omitting leaves it absent.

The parse, plumbing, and derivation halves moved with the init decision layer into the native door; the frozen Rust parity goldens hold that contract now (tests/golden/target_init).
"""
from __future__ import annotations

import os
import subprocess
from pathlib import Path

import yaml

from tests._init_space import install_state_path_stub

_REPO_ROOT = Path(__file__).resolve().parents[3]
_INIT_SCRIPT = _REPO_ROOT / "hooks" / "helpers" / "init-target-state.sh"


def _ensure_feature_repo(root: Path) -> None:
    if (root / ".git").exists():
        return
    subprocess.run(["git", "init", "-q", str(root)], check=True)
    subprocess.run(["git", "-C", str(root), "config", "user.email", "fno@test"], check=True)
    subprocess.run(["git", "-C", str(root), "config", "user.name", "fno"], check=True)
    subprocess.run(
        ["git", "-C", str(root), "commit", "-q", "--allow-empty", "-m", "init"],
        check=True,
    )
    subprocess.run(
        ["git", "-C", str(root), "checkout", "-q", "-b", "feature/target-denominator"],
        check=True,
    )

# Bare-env `fno` stub: the asserted manifest value is bash-computed, so the CLI
# round-trips the hook makes are startup-cost no-ops here. Exit codes mirror a
# bare env (no graph/config): config/backlog -> 1, paths/worktree -> 0, claim -> 0.
_FNO_STUB = """\
#!/usr/bin/env bash
case "$1" in
  config) exit 1 ;;
  backlog) exit 1 ;;
  paths) exit 0 ;;
  worktree) exit 0 ;;
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
      status|session-pid|acquire) exit 0 ;;
      *) exit 1 ;;
    esac ;;
  *) exit 1 ;;
esac
"""


def _run_init_script(tmp_path: Path, extra_env: dict[str, str]) -> subprocess.CompletedProcess:
    _ensure_feature_repo(tmp_path)
    plan_file = tmp_path / "plan.md"
    plan_file.write_text("# Test plan\n")
    home_dir = tmp_path / "home"
    home_dir.mkdir(exist_ok=True)
    (home_dir / ".fno").mkdir(exist_ok=True)
    (tmp_path / ".fno").mkdir(parents=True, exist_ok=True)
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir(parents=True, exist_ok=True)
    fno = bin_dir / "fno"
    fno.write_text(_FNO_STUB)
    fno.chmod(0o755)
    stub_env = install_state_path_stub(bin_dir, tmp_path / "space")
    env = {
        "HOME": str(home_dir),
        "PATH": f"{bin_dir}{os.pathsep}" + os.environ.get("PATH", "/usr/local/bin:/usr/bin:/bin"),
        "TARGET_START": "1",
        "TARGET_INPUT": str(plan_file),
        "TARGET_AUTO_MERGE": "false",
        **stub_env,
    }
    env.update(extra_env)
    return subprocess.run(
        ["bash", str(_INIT_SCRIPT)], cwd=str(tmp_path), env=env,
        capture_output=True, text=True, timeout=90,
    )


def _frontmatter(state_file: Path) -> dict:
    lines = state_file.read_text().splitlines()
    assert lines[0].strip() == "---", f"No opening --- in {state_file}"
    end = next(i for i, ln in enumerate(lines[1:], 1) if ln.strip() == "---")
    return yaml.safe_load("\n".join(lines[1:end]))


def test_deliverables_flag_stamps_the_manifest(tmp_path: Path):
    """AC1-DENOM: TARGET_DELIVERABLES=3 lands as `deliverables: 3`."""
    proc = _run_init_script(tmp_path, {"TARGET_DELIVERABLES": "3"})
    assert proc.returncode == 0, f"stderr: {proc.stderr[:500]}"
    fm = _frontmatter(tmp_path / "space" / "target-state.md")
    assert fm.get("deliverables") == 3


def test_omitting_deliverables_leaves_the_field_absent(tmp_path: Path):
    """AC1-DENOM: absence, not 0. A stamped 0 would invert the gate; absence is
    the unmeasurable state the denominator gate and the ratio measurement key on."""
    proc = _run_init_script(tmp_path, {})
    assert proc.returncode == 0, f"stderr: {proc.stderr[:500]}"
    fm = _frontmatter(tmp_path / "space" / "target-state.md")
    assert "deliverables" not in fm, fm
