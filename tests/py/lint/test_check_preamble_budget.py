"""Regression tests for scripts/ci/check-preamble-budget.sh.

Gate output is captured directly with subprocess.run, never piped through a
formatter, so each assertion reads the gate's own return code.
"""
from __future__ import annotations

import os
import re
import subprocess
from pathlib import Path

import yaml

ROOT = Path(__file__).resolve().parents[3]
GATE = ROOT / "scripts" / "ci" / "check-preamble-budget.sh"


def _ceiling_bytes_from_script() -> int:
    """Read CEILING_BYTES from the gate so a raise can never desync the test."""
    match = re.search(r"^CEILING_BYTES=(\d+)", GATE.read_text(), re.MULTILINE)
    assert match is not None, f"CEILING_BYTES assignment not found in {GATE}"
    return int(match.group(1))


CEILING_BYTES = _ceiling_bytes_from_script()


def _run(repo_root: Path) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        ["bash", str(GATE), str(repo_root)],
        capture_output=True,
        text=True,
        timeout=5,
    )


def _write_fixed_roots(
    repo_root: Path,
    *,
    agents_bytes: int,
    claude_bytes: int = 10,
    skill_bytes: int = 100,
) -> None:
    (repo_root / ".claude" / "rules").mkdir(parents=True)
    (repo_root / "skills" / "using-fno").mkdir(parents=True)
    (repo_root / "AGENTS.md").write_bytes(b"a" * agents_bytes)
    (repo_root / "CLAUDE.md").write_bytes(b"c" * claude_bytes)
    (repo_root / "skills" / "using-fno" / "SKILL.md").write_bytes(
        b"s" * skill_bytes
    )


def _reported_total(result: subprocess.CompletedProcess[str]) -> int:
    match = re.search(r"^check-preamble-budget: (\d+) /", result.stdout, re.MULTILINE)
    assert match is not None, result.stdout
    return int(match.group(1))


def test_exact_ceiling_passes_and_one_byte_over_fails(tmp_path: Path) -> None:
    """AC2-HP: the comparison is strictly greater than the ceiling."""
    _write_fixed_roots(tmp_path, agents_bytes=CEILING_BYTES - 110)
    at_ceiling = _run(tmp_path)
    assert at_ceiling.returncode == 0, at_ceiling.stderr
    assert _reported_total(at_ceiling) == CEILING_BYTES

    (tmp_path / "AGENTS.md").write_bytes(b"a" * (CEILING_BYTES - 109))
    over_ceiling = _run(tmp_path)
    assert over_ceiling.returncode == 1
    assert _reported_total(over_ceiling) == CEILING_BYTES + 1
    assert "ceiling by 1 " in over_ceiling.stderr


def test_missing_fixed_root_fails_loud(tmp_path: Path) -> None:
    """AC3-ERR: an absent fixed root is never treated as zero bytes."""
    _write_fixed_roots(tmp_path, agents_bytes=100)
    (tmp_path / "AGENTS.md").unlink()

    result = _run(tmp_path)

    assert result.returncode != 0
    assert "AGENTS.md" in result.stderr
    assert f" / {CEILING_BYTES} bytes" not in result.stdout


# --- The second budget: always-loaded skill descriptions ---------------------


def _gate_constant(name: str) -> int:
    match = re.search(rf"^{name}=(\d+)", GATE.read_text(), re.MULTILINE)
    assert match is not None, f"{name} assignment not found in {GATE}"
    return int(match.group(1))


def _add_skill(
    repo_root: Path, name: str, description: str, *, model_invoked: bool = True
) -> None:
    skill = repo_root / "skills" / name / "SKILL.md"
    skill.parent.mkdir(parents=True, exist_ok=True)
    lines = ["---", f"name: {name}", f'description: "{description}"']
    if not model_invoked:
        lines.append("disable-model-invocation: true")
    lines += ["---", "", "body", ""]
    skill.write_text("\n".join(lines), encoding="utf-8")


def _measured_tree(
    tmp_path: Path, *, descriptions: dict[str, str] | None = None
) -> Path:
    """A fixture repo the gate can measure, with real skill frontmatter."""
    _write_fixed_roots(tmp_path, agents_bytes=500)
    for name, description in (descriptions or {"alpha": "a" * 100}).items():
        _add_skill(tmp_path, name, description)
    return tmp_path


def _run_at(
    repo_root: Path, *, force_band: bool = False
) -> subprocess.CompletedProcess[str]:
    env = dict(os.environ)
    if force_band:
        env["PREAMBLE_BAND_FORCE"] = "1"
    return subprocess.run(
        ["bash", str(GATE), str(repo_root)],
        capture_output=True,
        text=True,
        timeout=30,
        env=env,
    )


def test_every_frontmatter_parses_strictly() -> None:
    """No skill or agent frontmatter may fail a strict YAML parse.

    The reader that ships is line-based, which is why this went unnoticed: a
    colon mid-line survives, because the reader takes the rest of the line. A
    value that spans lines does not. `agents/verifier.md` wrote 18 unquoted
    lines, so a strict parser got nothing and the harness rendered the literal
    string "Agent from fno plugin" in place of its description.

    Same root cause as the wrapped-scalar defect in the plan stamper: one
    line-based frontmatter reader, two independent symptoms.
    """
    unparseable = []
    for path in sorted((ROOT / "agents").glob("*.md")) + sorted(
        (ROOT / "skills").glob("*/SKILL.md")
    ):
        match = re.match(r"^---\n(.*?)\n---\n", path.read_text(encoding="utf-8"), re.S)
        if not match:
            continue
        try:
            yaml.safe_load(match.group(1))
        except yaml.YAMLError as exc:
            unparseable.append(f"{path.name}: {str(exc).splitlines()[0]}")

    assert unparseable == [], f"frontmatter a strict parser rejects: {unparseable}"


# --- The two-sided file ceiling ----------------------------------------------


def test_unbanked_file_cut_fails_and_names_the_value(tmp_path: Path) -> None:
    """This path used to print an advisory and exit 0, so no cut was banked."""
    band = _gate_constant("RATCHET_NUDGE_BYTES")
    # Park the descriptions budget inside its own band, or it fails first and
    # this test reads the wrong gate's message.
    in_band = _gate_constant("DESCRIPTIONS_CEILING_BYTES") - 100
    repo = _measured_tree(tmp_path, descriptions={"alpha": "a" * in_band})

    result = _run_at(repo, force_band=True)

    assert result.returncode == 1
    assert "more than the" in result.stderr
    total = int(re.search(r"check-preamble-budget: (\d+) /", result.stderr).group(1))
    assert f"CEILING_BYTES={total + band // 2}" in result.stderr
