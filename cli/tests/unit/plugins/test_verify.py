from __future__ import annotations

import subprocess
from pathlib import Path

import pytest
import yaml

from fno.company.contracts import EvidenceResult, RoleRef
from fno.plugins.manifest import AgentDeclaration, EvaluatorDeclaration, PackManifest
from fno.plugins.verify import (
    Condition,
    ConditionFamily,
    VerificationReport,
    verify_pack,
)
from tests.unit.plugins.test_manifest import _full_pack, _materialize_declared_sources

# Role resolution runs in fno-agents (spawn-overlay kind roles-resolve).
pytestmark = pytest.mark.dev_build

REPO_ROOT = Path(__file__).resolve().parents[4]
_PACK = REPO_ROOT / "plugins" / "growth-studio"


def _write_pack(tmp_path: Path, manifest: PackManifest, name: str = "growth-studio") -> Path:
    pack_dir = tmp_path / name
    pack_dir.mkdir()
    payload = manifest.model_dump(mode="json")
    (pack_dir / "plugin.yaml").write_text(yaml.safe_dump(payload), encoding="utf-8")
    _materialize_declared_sources(pack_dir, manifest)
    return pack_dir


def _results_by_name(report: VerificationReport) -> dict[str, Condition]:
    return {condition.name: condition for condition in report.conditions}


def test_valid_pack_is_all_checked_and_passed(tmp_path):
    pack_dir = _write_pack(tmp_path, _full_pack())
    # An empty installed index lets the dependency family evaluate (the pack has
    # no deps), so every family is checked rather than left unknown.
    report = verify_pack(pack_dir, installed={})
    assert report.ok, [c.name for c in report.conditions if not c.ok]
    for condition in report.conditions:
        assert condition.checked, condition.name
        assert condition.result is EvidenceResult.PASSED, (condition.name, condition.result)


def test_verify_writes_activates_and_mutates_nothing(tmp_path):
    pack_dir = _write_pack(tmp_path, _full_pack())
    snapshot = {p: p.read_bytes() for p in pack_dir.rglob("*") if p.is_file()}
    verify_pack(pack_dir)
    after = {p: p.read_bytes() for p in pack_dir.rglob("*") if p.is_file()}
    assert snapshot == after


def test_unevaluated_condition_reports_unchecked_unknown_not_passed(tmp_path):
    # No installed-pack index => dependency family is unchecked/unknown, not passed.
    pack_dir = _write_pack(tmp_path, _full_pack())
    report = verify_pack(pack_dir)
    dependency = next(c for c in report.conditions if c.family is ConditionFamily.DEPENDENCY)
    assert dependency.checked is False
    assert dependency.result is EvidenceResult.UNKNOWN
    # and the report is not ok precisely because of that unknown
    assert report.ok is False


def test_dependency_absent_blocks_when_index_supplied(tmp_path):
    pack_dict = _full_pack().model_dump(mode="json")
    pack_dict["depends_on"] = [{"pack_id": "missing-core", "version_range": {"minimum": "0.1.0"}}]
    pack_dir = tmp_path / "growth-studio"
    pack_dir.mkdir()
    (pack_dir / "plugin.yaml").write_text(yaml.safe_dump(pack_dict), encoding="utf-8")
    report = verify_pack(pack_dir, installed={})
    dependency = next(c for c in report.conditions if c.name == "dependency:missing-core")
    assert dependency.checked is True
    assert dependency.result is EvidenceResult.BLOCKED
    assert report.ok is False


def test_scenario_with_absent_command_blocks(tmp_path):
    pack = _full_pack().model_copy(
        update={
            "scenarios": (
                _full_pack().scenarios[0].model_copy(
                    update={"command": "no-such-binary --dry-run"}
                ),
            )
        }
    )
    pack_dir = _write_pack(tmp_path, pack)
    report = verify_pack(pack_dir)
    scenario = next(c for c in report.conditions if c.name.startswith("scenario:"))
    assert scenario.checked is True
    assert scenario.result is EvidenceResult.BLOCKED
    assert "absent or not executable" in (scenario.detail or "")


# AC1-HP / AC2-ERR / AC3-SEC: agent declarations bind a role and a bounded tool list.

_AGENT_ROLE = RoleRef(id="marketing", function_id="growth-studio")


def _write_agent_file(pack_dir: Path, source: str, *, tools: list[str]) -> None:
    target = pack_dir / source
    target.parent.mkdir(parents=True, exist_ok=True)
    frontmatter = yaml.safe_dump(
        {"name": Path(source).stem, "pack": "growth-studio", "role": "marketing", "tools": tools},
        sort_keys=False,
    )
    target.write_text(f"---\n{frontmatter}---\n\nagent body\n", encoding="utf-8")


def _pack_with_agent(tmp_path: Path, agent: AgentDeclaration) -> Path:
    manifest = _full_pack().model_copy(update={"agents": (agent,)})
    pack_dir = _write_pack(tmp_path, manifest)
    return pack_dir


def test_agent_unbounded_tools_fail(tmp_path):
    agent = AgentDeclaration(id="growth-marketer", source="agents/growth-marketer.md", role=_AGENT_ROLE)
    pack_dir = _pack_with_agent(tmp_path, agent)
    _write_agent_file(
        pack_dir,
        "agents/growth-marketer.md",
        tools=["Read", "Bash", "Task", "WebSearch", "WebFetch", "mcp__evil__steal"],
    )
    report = verify_pack(pack_dir, installed={})
    by_name = _results_by_name(report)
    cond = by_name["agent-tools-bounded:growth-marketer"]
    assert cond.result is EvidenceResult.FAILED
    detail = cond.detail or ""
    for offender in ("Bash", "Task", "WebSearch", "WebFetch", "mcp__evil__steal"):
        assert offender in detail


def test_agent_with_no_tools_frontmatter_fails(tmp_path):
    # An omitted tools field inherits the harness default tool set, so it is
    # unbounded, not vacuously bounded.
    agent = AgentDeclaration(id="growth-marketer", source="agents/growth-marketer.md", role=_AGENT_ROLE)
    pack_dir = _pack_with_agent(tmp_path, agent)
    target = pack_dir / "agents" / "growth-marketer.md"
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_text(
        "---\nname: growth-marketer\npack: growth-studio\nrole: marketing\n---\n\nbody\n",
        encoding="utf-8",
    )
    report = verify_pack(pack_dir, installed={})
    by_name = _results_by_name(report)
    cond = by_name["agent-tools-bounded:growth-marketer"]
    assert cond.result is EvidenceResult.FAILED
    assert "inherit the harness default" in (cond.detail or "")


# AC7-ERR: a declared evaluator whose command does not resolve blocks verify.


def test_evaluator_command_not_runnable_blocks(tmp_path):
    bad = _full_pack().model_copy(
        update={
            "evaluators": (
                EvaluatorDeclaration(id="factual-review", command="definitely-not-a-command-xyz"),
            )
        }
    )
    pack_dir = _write_pack(tmp_path, bad)
    report = verify_pack(pack_dir, installed={})
    by_name = _results_by_name(report)
    cond = by_name["evaluator-runnable:factual-review"]
    assert cond.checked is True
    assert cond.result is EvidenceResult.BLOCKED
    assert not report.ok


# AC8-HP: factual-check.sh passes a cited draft and fails an uncited one.


def _run_evaluator(script: str, draft: Path, *args: str) -> int:
    return subprocess.run(
        ["bash", str(_PACK / "evaluators" / script), str(draft), *map(str, args)],
        capture_output=True,
    ).returncode


def test_factual_check_fails_an_uncited_claim(tmp_path):
    truth = _PACK / "assets" / "product-truth.md"
    draft = tmp_path / "draft.md"
    draft.write_text(
        "# Plan\n\n## Claims\n\n"
        "- An assertion with no citation at all.\n",
        encoding="utf-8",
    )
    assert _run_evaluator("factual-check.sh", draft, truth) != 0


def test_resolver_binary_absent_reports_unchecked_unknown(monkeypatch, tmp_path):
    """A checkout that never built the Rust leg has no fno-agents binary: the
    resolution family degrades to unchecked/unknown, the dependency family's
    own shape, instead of crashing the verify."""
    import fno.plugins.verify as verify_mod
    from fno.rust_binary import CommandUnavailable

    def _raise(**kwargs):
        raise CommandUnavailable("the fno-agents binary was not found")

    monkeypatch.setattr(verify_mod, "resolve_role", _raise)
    pack_dir = _write_pack(tmp_path, _full_pack())
    report = verify_pack(pack_dir, installed={})
    resolve_conditions = [c for c in report.conditions if c.name.startswith("resolve:")]
    assert resolve_conditions
    for condition in resolve_conditions:
        assert condition.checked is False, condition.name
        assert condition.result is EvidenceResult.UNKNOWN, condition.name
    assert not any(c.result is EvidenceResult.FAILED for c in report.conditions)
