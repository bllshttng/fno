#!/usr/bin/env python3
"""Tests for the PRODUCT.md prereq check used by /blueprint (Phase 02.1).

Acceptance criteria:
  AC1-HP: PRODUCT.md present (>=200 chars) -> no prerequisites block, no warning
  AC2-HP: No PRODUCT.md found -> prerequisites block written + warning on stderr
  AC3-ERR: PRODUCT.md exists with 50 chars of [TODO] -> treated as missing (stale)
  AC4-EDGE: /blueprint does NOT block plan creation when PRODUCT.md is missing

Run: python3 -m pytest tests/spec/test_product_md_check.py -v
"""
import subprocess
import tempfile
import textwrap
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parent.parent.parent
CHECK_SCRIPT = REPO_ROOT / "scripts" / "check-product-md.sh"


def _run_check(plan_path: Path, repo_root: Path) -> subprocess.CompletedProcess:
    """Run check-product-md.sh and return the result."""
    return subprocess.run(
        ["bash", str(CHECK_SCRIPT), str(plan_path)],
        capture_output=True,
        text=True,
        env={
            "PATH": "/usr/bin:/bin:/usr/local/bin",
            "REPO_ROOT": str(repo_root),
        },
    )


def _make_plan_with_executor(plan_path: Path, executor: str = "impeccable") -> None:
    """Write a minimal single-doc plan with executor set."""
    plan_path.parent.mkdir(parents=True, exist_ok=True)
    plan_path.write_text(
        textwrap.dedent(f"""\
        ---
        title: Test Plan
        executor: {executor}
        created: 2026-05-06
        ---

        # Test Plan
        """)
    )


def test_ac1_hp_product_md_present_no_warning():
    """AC1-HP: PRODUCT.md >= 200 chars -> no prerequisites block, no stderr warning."""
    with tempfile.TemporaryDirectory() as tmp:
        repo_root = Path(tmp) / "repo"
        plan_path = repo_root / "plans" / "my-plan.md"
        _make_plan_with_executor(plan_path, "impeccable")

        # Write a valid PRODUCT.md (>= 200 chars, no [TODO])
        product_md = repo_root / "PRODUCT.md"
        product_md.write_text(
            "A" * 200 + "\nThis is the product description that satisfies the 200-char minimum.\n"
        )

        result = _run_check(plan_path, repo_root)

        assert result.returncode == 0, f"Expected exit 0, got {result.returncode}.\nstdout: {result.stdout}\nstderr: {result.stderr}"
        assert "prerequisites:" not in result.stdout, "Should not add prerequisites block when PRODUCT.md is present"
        assert "warning:" not in result.stderr.lower(), f"Unexpected warning: {result.stderr}"


def test_ac2_hp_product_md_missing_writes_prerequisites():
    """AC2-HP: No PRODUCT.md -> prerequisites block in plan frontmatter + warning."""
    with tempfile.TemporaryDirectory() as tmp:
        repo_root = Path(tmp) / "repo"
        plan_path = repo_root / "plans" / "my-plan.md"
        _make_plan_with_executor(plan_path, "impeccable")
        # No PRODUCT.md anywhere

        result = _run_check(plan_path, repo_root)

        # Exit 0 (plan still ships - this is heads-up only)
        assert result.returncode == 0, f"Expected exit 0 (plan ships), got {result.returncode}.\nstdout: {result.stdout}\nstderr: {result.stderr}"

        # The plan doc should now have a prerequisites block
        plan_content = plan_path.read_text()
        assert "prerequisites:" in plan_content, "Missing prerequisites: block in frontmatter"
        assert "kind: file" in plan_content, "Missing kind: file in prerequisites"
        assert "PRODUCT.md" in plan_content, "Missing PRODUCT.md path in prerequisites"

        # Warning should appear on stderr
        assert "warning:" in result.stderr.lower() or "warning:" in result.stdout.lower(), \
            f"Expected a warning. stderr={result.stderr!r} stdout={result.stdout!r}"
