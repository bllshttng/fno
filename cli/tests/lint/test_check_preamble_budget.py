"""Regression tests for scripts/ci/check-preamble-budget.sh.

Gate output is captured directly with subprocess.run, never piped through a
formatter, so each assertion reads the gate's own return code.
"""
from __future__ import annotations

import hashlib
import json
import os
import re
import subprocess
from pathlib import Path

import pytest
import yaml

ROOT = Path(__file__).resolve().parents[3]
GATE = ROOT / "scripts" / "ci" / "check-preamble-budget.sh"
WORKFLOW = ROOT / ".github" / "workflows" / "guards.yml"


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


def test_json_manifest_reports_hashes_without_changing_exit_semantics(
    tmp_path: Path,
) -> None:
    """Task 1.1: the static gate doubles as a machine-readable census seam."""
    _write_fixed_roots(tmp_path, agents_bytes=500)
    result = subprocess.run(
        ["bash", str(GATE), "--json", str(tmp_path)],
        capture_output=True,
        text=True,
        timeout=5,
    )

    assert result.returncode == 0, result.stderr
    payload = json.loads(result.stdout)
    assert payload["total_bytes"] == 610
    assert payload["estimated_tokens"] == 153
    assert payload["ceiling_bytes"] == CEILING_BYTES
    by_path = {record["path"]: record for record in payload["sources"]}
    assert by_path["AGENTS.md"] == {
        "path": "AGENTS.md",
        "bytes": 500,
        "estimated_tokens": 125,
        "content_hash": hashlib.sha256(b"a" * 500).hexdigest(),
        "reach": "every harness",
    }
    assert by_path["skills/using-fno/SKILL.md"]["reach"].startswith("hook:")
    assert by_path["CLAUDE.md"]["reach"] == "claude only"
    assert set(payload["reach_bytes"]) == {"every-harness", "hook", "claude-only"}
    assert sum(payload["reach_bytes"].values()) == payload["total_bytes"]


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


def _committed_fixture(tmp_path: Path, *, under: int) -> Path:
    """A git repo whose committed file set sits `under` bytes below the ceiling."""
    repo = tmp_path / "repo"
    (repo / ".claude" / "rules").mkdir(parents=True)
    (repo / "skills" / "using-fno").mkdir(parents=True)
    (repo / "AGENTS.md").write_bytes(b"a" * (CEILING_BYTES - under - 110))
    (repo / "CLAUDE.md").write_bytes(b"c" * 10)
    (repo / "skills" / "using-fno" / "SKILL.md").write_bytes(b"s" * 100)

    def git(*args: str) -> None:
        subprocess.run(
            ["git", "-C", str(repo), *args],
            check=True,
            capture_output=True,
            text=True,
        )

    git("init", "-q")
    git("add", ".")
    git("-c", "user.email=t@t", "-c", "user.name=t", "commit", "-qm", "base")
    return repo


def test_headroom_names_pre_change_headroom_and_delta(tmp_path: Path) -> None:
    """AC3-HP: the refusal measures what the author had before the change."""
    repo = _committed_fixture(tmp_path, under=3)
    subprocess.run(
        ["git", "-C", str(repo), "update-ref", "refs/remotes/origin/main", "HEAD"],
        check=True,
        capture_output=True,
        text=True,
    )
    (repo / "AGENTS.md").write_bytes(b"a" * (CEILING_BYTES - 3 - 110 + 822))

    result = _run(repo)

    assert result.returncode == 1
    assert re.search(
        r"Headroom before this change: 3 B at [0-9a-f]{7}; this change adds 822 B\.",
        result.stderr,
    ), result.stderr


def test_preamble_budget_step_is_unconditionally_reachable() -> None:
    """AC9-EDGE: no measured path can make the job unreachable.

    preamble-budget.yml used to gate on a `paths:` filter, and this test used
    to assert that filter covered every measured path (`skills/**` subsuming
    the `skills/using-fno/SKILL.md` entry it replaced, since the gate measures
    every `skills/*/SKILL.md` description - a narrower filter would leave the
    descriptions ceiling unreachable from CI). The check was consolidated
    into guards.yml's `guards` job, which the plan's own name-consumer sweep
    requires to carry NO `paths:` filter at all (it runs on every push and
    pull_request unconditionally) - a strictly stronger guarantee than any
    path list could give, so there is nothing left to enumerate against.
    """
    workflow = yaml.safe_load(WORKFLOW.read_text(encoding="utf-8"))
    triggers = workflow[True]
    assert "paths" not in (triggers.get("push") or {}), (
        "guards.yml's push trigger gained a paths filter - the "
        "preamble-budget step (and every other consolidated check) needs "
        "unconditional reachability, not a filtered one"
    )
    assert "paths" not in (triggers.get("pull_request") or {}), (
        "guards.yml's pull_request trigger gained a paths filter"
    )
    step_names = [s.get("name", "") for s in workflow["jobs"]["guards"]["steps"]]
    assert "SessionStart preamble byte budget" in step_names


def test_non_regular_rule_fails_instead_of_blocking(tmp_path: Path) -> None:
    """A discovered device or pipe must never leave wc waiting for EOF."""
    _write_fixed_roots(tmp_path, agents_bytes=100)
    (tmp_path / ".claude" / "rules" / "stall.md").symlink_to("/dev/zero")

    result = _run(tmp_path)

    assert result.returncode != 0
    assert "not a regular file" in result.stderr
    assert ".claude/rules/stall.md" in result.stderr


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


def test_descriptions_match_an_independent_reader_per_file() -> None:
    """Cross-check the gate's regex reader against PyYAML, file by file.

    Per-file rather than on the total, so two errors cannot cancel out. Files
    PyYAML rejects are skipped and asserted separately below: three agent
    frontmatters in this repo are not valid YAML today.
    """
    payload = json.loads(
        subprocess.run(
            ["bash", str(GATE), "--json"],
            cwd=ROOT,
            capture_output=True,
            text=True,
            timeout=30,
        ).stdout
    )
    measured = {row["skill"]: row["bytes"] for row in payload["descriptions"]["skills"]}

    compared = 0
    for path in sorted((ROOT / "skills").glob("*/SKILL.md")) + sorted(
        (ROOT / "agents").glob("*.md")
    ):
        text = path.read_text(encoding="utf-8")
        match = re.match(r"^---\n(.*?)\n---\n", text, re.S)
        if not match:
            continue
        try:
            front = yaml.safe_load(match.group(1)) or {}
        except yaml.YAMLError:
            continue
        if front.get("disable-model-invocation") or not front.get("description"):
            continue
        label = (
            path.parent.name
            if path.name == "SKILL.md"
            else f"agent:{path.stem}"
        )
        expected = len(str(front["description"]).rstrip("\n").encode("utf-8"))
        assert measured.get(label) == expected, f"{label}: gate says {measured.get(label)}, yaml says {expected}"
        compared += 1

    # Positive control: the loop above must actually have compared something.
    assert compared > 25, f"only {compared} files compared; the cross-check is not running"

    assert payload["measured_always_loaded_bytes"] == (
        payload["total_bytes"] + payload["descriptions"]["total_bytes"]
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


def test_description_growth_past_the_ceiling_fails(tmp_path: Path) -> None:
    """A fatter pointer is a real per-turn cost and has to be paid for."""
    ceiling = _gate_constant("DESCRIPTIONS_CEILING_BYTES")
    repo = _measured_tree(tmp_path, descriptions={"padding": "x" * (ceiling + 1)})

    result = _run_at(repo)

    assert result.returncode == 1
    assert "descriptions are" in result.stderr
    assert "over the" in result.stderr


def test_unbanked_description_cut_fails_and_names_the_value(tmp_path: Path) -> None:
    """The band is two-sided here too: a cut is banked or it is not a cut."""
    band = _gate_constant("DESCRIPTIONS_BAND_BYTES")
    ceiling = _gate_constant("DESCRIPTIONS_CEILING_BYTES")
    tiny = ceiling - band - 100
    repo = _measured_tree(tmp_path, descriptions={"small": "s" * tiny})

    result = _run_at(repo, force_band=True)

    assert result.returncode == 1
    # The suggestion parks the ceiling mid-band, so a later cut has room too.
    assert f"DESCRIPTIONS_CEILING_BYTES={tiny + band // 2}" in result.stderr


def test_a_fence_the_reader_cannot_open_is_caught(tmp_path: Path) -> None:
    """A description the measuring reader skips must not pass as measured.

    The Python reader needs a newline after the closing fence, because its
    regex ends `\\n---\\n`. A file whose frontmatter closes at EOF satisfies the
    cross-check's scan and not the reader's, so its description would go
    unmeasured while the file looks ordinary. That is the live shape of "the
    reader broke": not a total failure, one file silently dropped.

    An earlier version of this test wrote a bare `description:` line with no
    fence at all. Anchoring both scans to the frontmatter slice made that file
    invisible to BOTH, correctly, and the test then passed for the wrong
    reason.
    """
    repo = _measured_tree(tmp_path)
    skipped = repo / "skills" / "unterminated" / "SKILL.md"
    skipped.parent.mkdir(parents=True, exist_ok=True)
    skipped.write_text(
        '---\nname: unterminated\ndescription: "real, and unmeasured"\n---',
        encoding="utf-8",
    )

    result = _run_at(repo)

    assert result.returncode == 1
    assert "the two readers disagree" in result.stderr


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
