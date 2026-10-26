from __future__ import annotations

import datetime as dt
import json
import subprocess
from pathlib import Path
from types import SimpleNamespace

import pytest
import yaml

from fno.company.contracts import (
    CompanyWorkRefs,
    DeliverableRef,
    EffectRef,
    EvidenceRef,
    EvidenceResult,
    EvidenceSubjectKind,
    ObservationRef,
    WorkOrderRef,
)
from fno.rust_binary import find_dev_binary

REPO_ROOT = Path(__file__).resolve().parents[3]
PACK = REPO_ROOT / "plugins" / "company-conformance" / "plugin.yaml"
NOW = dt.datetime(2026, 8, 4, 12, 0, tzinfo=dt.UTC)
FOUNDER = "principal:founder"
ROLE_IDS = ("support-response", "sales-outreach", "operations-recurring")
EFFECTS = {
    "support-response": ("external.communication", "support-channel"),
    "sales-outreach": ("external.outreach", "prospect-channel"),
}
OBSERVATION_CADENCE = {
    "sales-outreach": ("14d", "reply-rate"),
    "operations-recurring": ("per-cycle", "cycle-complete"),
}


def _manifest(path: Path):
    # The pack schema is native; the journey reads the roles as the raw
    # documents the pack ships and the native schema validates.
    raw = yaml.safe_load(path.read_text(encoding="utf-8"))
    return SimpleNamespace(
        roles=tuple(raw.get("roles", ())),
        skills=tuple(raw.get("skills", ())),
        agents=tuple(raw.get("agents", ())),
    )


def _plugins(root: Path, *args: str) -> tuple[int, dict]:
    """One native `fno config plugins` command against this role root."""
    proc = subprocess.run(
        [str(find_dev_binary()), "spawn-overlay", "plugins", "--root", str(root), *args, "--json"],
        capture_output=True,
        text=True,
    )
    return proc.returncode, json.loads(proc.stdout)


def _spawn_overlay(payload: dict) -> dict:
    """One native spawn-overlay round-trip: JSON payload in, answer out."""
    proc = subprocess.run(
        [str(find_dev_binary()), "spawn-overlay"],
        input=json.dumps(payload),
        capture_output=True,
        text=True,
    )
    assert proc.returncode == 0, proc.stdout + proc.stderr
    return json.loads(proc.stdout)


def _discover_rows(root: Path) -> list[dict]:
    """The full discovery rows (raw_definition, definition included) the
    retired Python `discover_role_definitions` door parsed."""
    return _spawn_overlay({"kind": "roles-discover", "root": str(root)})["rows"]


# The defaults the retired pydantic models filled in when the pack YAML omits
# a key; the resolution answer carries them either way.
_REVIEW_POLICY_DEFAULTS = {"required": False, "minimum_reviewers": 0, "required_review_kinds": []}
_DELIVERY_POLICY_DEFAULTS = {"required_evidence": [], "require_all_deliverables": True}


def _delivery_eval(
    workdir: Path, company_work: dict, facts: list[dict]
) -> dict:
    """One native delivery evaluation over plan + journal fixtures.

    The delivery family answers through the `fno-agents delivery-evaluate`
    command now; the journey builds the same declaration it used to pass the
    Python evaluator in process and reads the response payload instead.
    """
    workdir.mkdir(parents=True, exist_ok=True)
    plan = workdir / "plan.md"
    frontmatter = {
        "node": company_work["work_order"]["node_id"],
        "status": "ready",
        "created": "2026-08-02",
        "completion": "delivery",
        "company_work": company_work,
    }
    plan.write_text(
        f"---\n{yaml.safe_dump(frontmatter, sort_keys=False)}---\n# Plan\n"
    )
    events = workdir / "events.jsonl"
    events.write_text(
        "".join(json.dumps(fact) + "\n" for fact in facts)
    )
    proc = subprocess.run(
        [
            str(find_dev_binary()),
            "delivery-evaluate",
            "--plan-path", str(plan),
            "--events", str(events),
            "--json",
        ],
        capture_output=True,
        text=True,
    )
    assert proc.returncode == 0, (proc.stdout, proc.stderr)
    return json.loads(proc.stdout)


def _fact_event(
    evidence: EvidenceRef,
    *,
    fact_revision: str = "facts-current",
    observed_at: str = "2026-08-04T12:00:00Z",
    fresh_until: str = "2099-08-04T13:00:00Z",
) -> dict:
    """One observed-evidence journal row carrying the fact the fixture means."""
    return {
        "ts": observed_at,
        "type": "delivery_evidence_observed",
        "source": "target",
        "data": {
            "version": "delivery-evidence-fact.v1",
            "evidence": {
                "id": evidence.id,
                "work_order_id": evidence.work_order_id,
                "attempt_id": evidence.attempt_id,
                "subject_kind": evidence.subject_kind.value,
                "subject_id": evidence.subject_id,
                "result": evidence.result.value,
            },
            "producer": "company-conformance-fixture",
            "observed_at": observed_at,
            "source_revision": "source-v1",
            "fresh_until": fresh_until,
            "adapter_version": "fixture-v1",
            "fact_revision": fact_revision,
        },
    }


CONFORMANCE = _manifest(PACK)


def _role_manifest(role_id: str):
    return next(role for role in CONFORMANCE.roles if role["role"]["id"] == role_id)


def _subject_kind(evidence_id: str) -> EvidenceSubjectKind:
    if evidence_id.endswith("review"):
        return EvidenceSubjectKind.REVIEW
    return {
        "acknowledgment": EvidenceSubjectKind.ACKNOWLEDGMENT,
        "approval": EvidenceSubjectKind.APPROVAL,
        "probe": EvidenceSubjectKind.PROBE,
    }[evidence_id]


def _company_work(role_id: str, *, attempt_id: str = "attempt-current") -> CompanyWorkRefs:
    role = _role_manifest(role_id)
    work_order = WorkOrderRef(
        node_id=f"wo-{role_id}",
        attempt_id=attempt_id,
        principal_id=FOUNDER,
        role_id=role_id,
    )
    deliverable_id = f"deliverable-{role_id}"
    evidence = tuple(
        EvidenceRef(
            id=f"{role_id}:{evidence_id}",
            work_order_id=work_order.node_id,
            attempt_id=attempt_id,
            subject_kind=_subject_kind(evidence_id),
            subject_id=evidence_id,
            result=EvidenceResult.PASSED,
        )
        for evidence_id in role["delivery_policy"]["required_evidence"]
    )
    effect = ()
    effect_id = None
    if role_id in EFFECTS:
        effect_class, destination = EFFECTS[role_id]
        effect_id = f"effect-{role_id}"
        effect = (
            EffectRef(
                id=effect_id,
                work_order_id=work_order.node_id,
                attempt_id=attempt_id,
                deliverable_id=deliverable_id,
                effect_class=effect_class,
                destination=destination,
                idempotency_key=f"key-{role_id}",
            ),
        )
    observations = ()
    if role_id in OBSERVATION_CADENCE:
        _, metric = OBSERVATION_CADENCE[role_id]
        observations = (
            ObservationRef(
                node_id=f"observation-{role_id}",
                delivery_work_order_id=work_order.node_id,
                deliverable_id=deliverable_id,
                metric_ids=(metric,),
            ),
        )
    return CompanyWorkRefs(
        function=role["function"],
        role=role["role"],
        work_order=work_order,
        deliverables=(
            DeliverableRef(
                id=deliverable_id,
                kind=role["deliverable_kinds"][0],
                work_order_id=work_order.node_id,
                attempt_id=attempt_id,
                required_evidence_ids=tuple(item.id for item in evidence),
                effect_id=effect_id,
            ),
        ),
        effects=effect,
        evidence=evidence,
        observations=observations,
    )


def _fact(
    evidence: EvidenceRef,
    *,
    fact_revision: str = "facts-current",
    observed_at: str = "2026-08-04T12:00:00Z",
    fresh_until: str = "2099-08-04T13:00:00Z",
) -> dict:
    return _fact_event(
        evidence,
        fact_revision=fact_revision,
        observed_at=observed_at,
        fresh_until=fresh_until,
    )


def _complete_facts(company_work: CompanyWorkRefs) -> tuple[dict, ...]:
    return tuple(_fact(evidence) for evidence in company_work.evidence)


def _company_work_json(company_work: CompanyWorkRefs) -> dict:
    """The declaration as the plan frontmatter carries it."""
    return json.loads(company_work.model_dump_json())


@pytest.fixture()
def role_root(tmp_path: Path) -> Path:
    root = tmp_path / "roles"
    root.mkdir()
    code, outcome = _plugins(root, "activate", str(PACK))
    assert code == 0, outcome
    assert outcome["unconfigured_artifacts"] == []
    assert set(outcome["written_paths"]) == {
        f"plugin/company-conformance/{role_id}.json" for role_id in ROLE_IDS
    }
    return root


@pytest.mark.dev_build
def test_pack_verification_runs_but_the_journey_carries_conformance(tmp_path: Path) -> None:
    code, report = _plugins(tmp_path / "roles", "verify", str(PACK))
    failing = [c["name"] for c in report["conditions"] if not (c["checked"] and c["result"] == "passed")]
    assert code == 0 and report["ok"], failing
    scenario_conditions = [
        condition for condition in report["conditions"] if condition["name"].startswith("scenario:")
    ]
    assert len(scenario_conditions) == len(ROLE_IDS)
    assert all(condition["checked"] for condition in scenario_conditions)


def _assert_resolved_roles_match_every_declared_policy_axis(role_root: Path) -> None:
    rows = _discover_rows(role_root)
    for declared in CONFORMANCE.roles:
        definitions = [
            record["definition"]
            for record in rows
            if record["definition"] is not None and record["role"] == declared["role"]
        ]
        role_id = declared["role"]["id"]
        revision = definitions[0]["snapshot_revision"]
        work_order = {"node_id": f"wo-{role_id}", "attempt_id": "attempt-current",
                      "principal_id": None, "role_id": role_id}
        facts = [
            {
                "capability": capability, "available": True, "source_id": "journey-fixture",
                "snapshot_revision": revision, "work_order_scope": work_order,
            }
            for capability in declared["required_capabilities"]
        ]
        resolved = _spawn_overlay(
            {
                "kind": "roles-resolve",
                "role": declared["role"],
                "definitions": definitions,
                "capability_facts": facts,
                "context_catalog": [],
                "work_order": work_order,
                "clock": NOW.isoformat(),
                "snapshot_revision": revision,
                "bundle_bounds": {
                    "max_references": 8,
                    "max_bytes": 1024,
                    "max_sensitivity": "sensitive",
                    "max_combinations": 4096,
                },
                "unchecked_definitions": [
                    {
                        "layer": record["layer"], "source": record["source"], "status": record["status"],
                        "role": record["role"], "has_definition": record["definition"] is not None,
                        "error": record["error"],
                    }
                    for record in rows
                ],
            }
        )
        assert "reason" not in resolved, (declared["role"]["id"], resolved.get("reason"))
        assert resolved["authority_ceiling"] == declared["authority_ceiling"]
        assert resolved["approval_floor"] == declared.get("approval_floor", "none")
        assert resolved["review_policy"] == {
            **_REVIEW_POLICY_DEFAULTS,
            **declared.get("review_policy", {}),
        }
        assert resolved["delivery_policy"] == {
            **_DELIVERY_POLICY_DEFAULTS,
            **declared.get("delivery_policy", {}),
        }
        assert resolved["manifest"]["default_topology"] == declared["default_topology"]
        assert resolved["required_capabilities"] == declared.get("required_capabilities", [])
        assert all(source["layer"] == "plugin" for source in resolved["source_chain"])

    signatures = {
        (
            role.get("approval_floor", "none"),
            role["default_topology"],
            tuple(role.get("review_policy", {}).get("required_review_kinds", ())),
            tuple(role.get("delivery_policy", {}).get("required_evidence", ())),
        )
        for role in CONFORMANCE.roles
    }
    assert len(signatures) == len(ROLE_IDS)
    assert any(role.get("approval_floor", "none") == "none" for role in CONFORMANCE.roles)
    assert CONFORMANCE.skills == () and CONFORMANCE.agents == ()


def _assert_complete_delivery_evidence_passes(role_id: str, workdir: Path) -> None:
    company_work = _company_work(role_id)
    payload = _delivery_eval(
        workdir,
        _company_work_json(company_work),
        list(_complete_facts(company_work)),
    )
    assert payload["status"] == "evaluated", (role_id, payload)
    assert payload["verdict"]["aggregate"] == "passed", (role_id, payload["verdict"])


@pytest.mark.dev_build
def test_support_response_scenario(role_root: Path) -> None:
    _assert_resolved_roles_match_every_declared_policy_axis(role_root)
    _assert_complete_delivery_evidence_passes("support-response", role_root.parent)


@pytest.mark.dev_build
def test_sales_outreach_scenario(role_root: Path) -> None:
    _assert_resolved_roles_match_every_declared_policy_axis(role_root)
    _assert_complete_delivery_evidence_passes("sales-outreach", role_root.parent)


# One missing requirement per role: every row exercises the same refusal
# branch, so further requirements of the same role add no distinct case.
MISSING_CASES = tuple(
    (role["role"]["id"], role["delivery_policy"]["required_evidence"][0])
    for role in CONFORMANCE.roles
)


@pytest.mark.dev_build
@pytest.mark.parametrize(("role_id", "missing_name"), MISSING_CASES)
def test_each_missing_requirement_never_reports_passed(
    role_id: str, missing_name: str, tmp_path: Path
) -> None:
    company_work = _company_work(role_id)
    missing_id = f"{role_id}:{missing_name}"
    facts = tuple(
        fact for fact in _complete_facts(company_work) if fact["data"]["evidence"]["id"] != missing_id
    )
    payload = _delivery_eval(
        tmp_path, _company_work_json(company_work), list(facts)
    )
    aggregate = payload["verdict"]["aggregate"]
    assert aggregate in {"unknown", "blocked"}
    assert aggregate != "passed"


@pytest.mark.dev_build
@pytest.mark.parametrize(
    ("role_id", "result"),
    (("operations-recurring", EvidenceResult.PASSED),),
)
def test_observation_facts_cannot_satisfy_delivery_or_change_its_verdict(
    role_id: str, result: EvidenceResult, tmp_path: Path
) -> None:
    company_work = _company_work(role_id)
    cadence, metric = OBSERVATION_CADENCE[role_id]
    assert cadence in {"14d", "per-cycle"}
    assert company_work.observations[0].metric_ids == (metric,)
    missing_id = company_work.deliverables[0].required_evidence_ids[0]
    facts = tuple(
        fact
        for fact in _complete_facts(company_work)
        if fact["data"]["evidence"]["id"] != missing_id
    )
    baseline = _delivery_eval(tmp_path / "baseline", _company_work_json(company_work), list(facts))
    observation_fact = _fact(
        EvidenceRef(
            id=missing_id,
            work_order_id=company_work.work_order.node_id,
            attempt_id=company_work.work_order.attempt_id,
            subject_kind=EvidenceSubjectKind.OBSERVATION,
            subject_id=company_work.observations[0].node_id,
            result=result,
        )
    )
    with_observation = _delivery_eval(
        tmp_path / "with-observation",
        _company_work_json(company_work),
        [*facts, observation_fact],
    )
    assert with_observation["verdict"]["aggregate"] == "unknown"
    assert baseline["verdict"]["aggregate"] == "unknown"
    # The evidence revision hashes the verdict minus its journal fact
    # revision, so equal hashes mean the observation fact changed nothing.
    assert with_observation["evidence_revision"] == baseline["evidence_revision"]


@pytest.mark.dev_build
def test_cadence_rejects_previous_cycle_residue_but_accepts_it_in_its_own_cycle(
    tmp_path: Path,
) -> None:
    current = _company_work("operations-recurring", attempt_id="attempt-current")
    previous = _company_work("operations-recurring", attempt_id="attempt-previous")
    # The command reads freshness against the wall clock, so the residue leg
    # carries facts whose freshness window closed (still after observed_at,
    # which the fact contract requires) and the own-cycle leg carries the
    # same reads inside a live window.
    stale_facts = tuple(
        _fact(
            evidence,
            fact_revision="facts-previous",
            observed_at="2026-08-04T12:00:00Z",
            fresh_until="2026-08-04T13:00:00Z",
        )
        for evidence in previous.evidence
    )
    fresh_facts = tuple(
        _fact(
            evidence,
            fact_revision="facts-previous",
            observed_at="2026-08-04T12:00:00Z",
            fresh_until="2099-08-04T13:00:00Z",
        )
        for evidence in previous.evidence
    )

    residue = _delivery_eval(tmp_path / "residue", _company_work_json(current), list(stale_facts))
    diagnostics = "\n".join(
        diagnostic
        for requirement in residue["verdict"]["requirements"]
        for diagnostic in requirement["diagnostics"]
    )
    assert residue["verdict"]["aggregate"] == "unknown"
    assert "attempt attempt-previous" in diagnostics
    assert "stale after" in diagnostics

    own_cycle = _delivery_eval(tmp_path / "own", _company_work_json(previous), list(fresh_facts))
    assert own_cycle["verdict"]["aggregate"] == "passed"
    assert all(
        requirement["result"] == "passed"
        for requirement in own_cycle["verdict"]["requirements"]
    )
