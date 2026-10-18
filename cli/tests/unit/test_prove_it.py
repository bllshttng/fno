"""prove-it: runtime evidence with a refusal where /verify would shrug.

AC6-MARKER is the load-bearing case: a PASS whose Steps list carries no
marked probe is REFUSED, and the refusal names the missing probe. Asserting
that a good run passes proves nothing about the case the rule exists for, so
every case here pins the refusal or the no-verdict state it produces.
"""

from __future__ import annotations

import subprocess
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[3]
VALIDATOR = REPO_ROOT / "skills/review/scripts/validate-prove-it.sh"
PLAN_VALIDATOR = REPO_ROOT / "skills/blueprint/scripts/validate-plan.sh"


GOOD_CLAIM = "the change works at its surface"
GOOD_CLAIMS = (
    "### Claims\n"
    f"1. CLAIM: {GOOD_CLAIM}\n"
    "   CMD: `curl -s localhost:8080/header`\n"
    "   EXIT: 0\n"
    "   OUT: x-prove-it: 1\n"
    "   VERDICT: PASS\n"
)


def _report(
    tmp_path: Path,
    name: str,
    body: str,
    verdict: str,
    claims: str = GOOD_CLAIMS,
    claim_text: str = GOOD_CLAIM,
) -> Path:
    path = tmp_path / name
    path.write_text(
        f'{body}\n{claims}fno-prove-it: {{"verdict":"{verdict}","claim":"{claim_text}"}}\n',
        encoding="utf-8",
    )
    return path


def _validate(report: Path) -> subprocess.CompletedProcess:
    return subprocess.run(["bash", str(VALIDATOR), str(report)], capture_output=True, text=True)


def test_selftest_passes():
    out = subprocess.run(["bash", str(VALIDATOR), "--selftest"], capture_output=True, text=True)
    assert out.returncode == 0, out.stdout + out.stderr
    assert "14 passed, 0 failed" in out.stdout
    assert "PASS: a claim with no command is refused" in out.stdout


def test_a_pass_with_no_probe_is_refused_and_named(tmp_path):
    """AC6-MARKER: the refusal is the behavior under test."""
    report = _report(
        tmp_path,
        "no-probe.md",
        "### Steps\n1. ✅ ran the route -> 200\n### Findings\n(none)",
        "PASS",
    )
    out = _validate(report)
    assert out.returncode == 1
    assert "REFUSED" in out.stderr
    assert "no marked probe" in out.stderr


def test_a_terminal_claim_no_row_evidences_is_refused(tmp_path):
    """A narrower or unrelated row cannot prove a broader terminal claim."""
    report = _report(
        tmp_path,
        "uncovered.md",
        "### Steps\n1. ✅ ran the route -> 200\n2. 🔍 empty value -> clean error\n",
        "PASS",
        claims=(
            "### Claims\n"
            "1. CLAIM: curl can start\n"
            "   CMD: `curl -s localhost:8080/health`\n"
            "   EXIT: 0\n"
            "   OUT: ok\n"
            "   VERDICT: PASS\n"
        ),
        claim_text="everything works",
    )
    out = _validate(report)
    assert out.returncode == 1
    assert "REFUSED" in out.stderr
    assert "everything works" in out.stderr
    """A claim with no command is a claim nobody checked; the refusal quotes it."""
    report = _report(
        tmp_path,
        "no-cmd.md",
        "### Steps\n1. ✅ ran the route -> 200\n2. 🔍 empty value -> clean error\n",
        "PASS",
        claims=(
            "### Claims\n"
            "1. CLAIM: the badge refreshes on every focus event\n"
            "   EXIT: 0\n"
            "   OUT: badge text changed to saved\n"
            "   VERDICT: PASS\n"
        ),
        claim_text="the badge refreshes on every focus event",
    )
    out = _validate(report)
    assert out.returncode == 1
    assert "REFUSED" in out.stderr
    assert "the badge refreshes on every focus event" in out.stderr


def test_a_pass_with_no_claims_section_is_refused_and_named(tmp_path):
    report = _report(
        tmp_path,
        "no-claims.md",
        "### Steps\n1. ✅ ran the route -> 200\n2. 🔍 empty value -> clean error\n",
        "PASS",
        claims="",
    )
    out = _validate(report)
    assert out.returncode == 1
    assert "REFUSED" in out.stderr
    assert "### Claims" in out.stderr


def test_a_pass_with_a_failing_claim_row_is_refused(tmp_path):
    """No partial pass: a claim row recording VERDICT: FAIL blocks the PASS."""
    report = _report(
        tmp_path,
        "partial.md",
        "### Steps\n1. ✅ ran the route -> 200\n2. 🔍 empty value -> clean error\n",
        "PASS",
        claims=(
            "### Claims\n"
            "1. CLAIM: the route returns the header\n"
            "   CMD: `curl -s localhost:8080/header`\n"
            "   EXIT: 0\n"
            "   OUT: x-prove-it: 1\n"
            "   VERDICT: FAIL\n"
        ),
        claim_text="the route returns the header",
    )
    out = _validate(report)
    assert out.returncode == 1
    assert "REFUSED" in out.stderr


def test_a_code_plan_without_probes_warns_advisory(tmp_path):
    """The advisory names the absence and records the reason it is not a gate."""
    plan = tmp_path / "plan.md"
    plan.write_text(
        "---\ntitle: t\nstatus: ready\nproject: fno\ndomain: code\n---\n# t\n",
        encoding="utf-8",
    )
    out = subprocess.run(["bash", str(PLAN_VALIDATOR), str(plan)], capture_output=True, text=True)
    joined = out.stdout + out.stderr
    assert "code plan declares no done_probes" in joined
    assert "advisory" in joined
