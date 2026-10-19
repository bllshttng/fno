from __future__ import annotations

import json
import sys
from pathlib import Path

import typer
import typer.main
from click.testing import CliRunner

from fno.agents.registry import AgentEntry
from fno.lint_cli import lint


def _live_lint_command():
    sub = typer.Typer(add_completion=False)
    sub.command(name="lint")(lint)
    return typer.main.get_command(sub)


app = _live_lint_command()
runner = CliRunner()


def _invoke(monkeypatch, repo: Path, *args: str):
    monkeypatch.setenv("FNO_REPO_ROOT", str(repo))
    return runner.invoke(app, ["field-coverage", *args])


def _write_source_fixture(repo: Path, *, required: list[str], extra: list[str]) -> None:
    registry = repo / "cli" / "src" / "fno" / "agents" / "registry.py"
    registry.parent.mkdir(parents=True)
    fields = "\n".join(f"    {name}: str | None = None" for name in required + extra)
    registry.write_text(
        "from dataclasses import dataclass\n\n@dataclass\nclass AgentEntry:\n" + fields + "\n",
        encoding="utf-8",
    )
    schema = repo / "schemas" / "agents-list-row.json"
    schema.parent.mkdir(parents=True)
    schema.write_text(
        json.dumps(
            {
                "required": required,
                "projection_omissions": [],
                "removed": {},
                "stored": {},
                "derived": {},
                "rust_only": {"keys": []},
                "python_only": {"keys": []},
                "storage_only": [],
                "known_gaps": {},
            }
        ),
        encoding="utf-8",
    )


def test_source_coverage_rediscovers_node_projection_omission_and_choices(
    monkeypatch, tmp_path: Path
) -> None:
    required = [f"field_{index}" for index in range(40)]
    _write_source_fixture(tmp_path, required=required, extra=["node"])
    schema_path = tmp_path / "schemas" / "agents-list-row.json"
    schema = json.loads(schema_path.read_text(encoding="utf-8"))
    schema["stored"] = {"node": "Descriptive metadata is not a disposition."}
    schema_path.write_text(json.dumps(schema), encoding="utf-8")

    result = _invoke(monkeypatch, tmp_path)

    assert result.exit_code == 1
    assert "unaccounted field: node" in result.output
    assert "required and project it" in result.output
    assert "storage_only" in result.output
    assert "known_gaps" in result.output


def test_source_coverage_refuses_empty_accounting(monkeypatch, tmp_path: Path) -> None:
    _write_source_fixture(
        tmp_path,
        required=[],
        extra=[f"field_{index}" for index in range(40)],
    )

    result = _invoke(monkeypatch, tmp_path, "--live")

    assert result.exit_code == 2
    assert "UNMEASURED" in result.output
    assert "clean" not in result.output.lower()


def test_live_coverage_classifies_contract_zeroes_and_keeps_real_dead(
    monkeypatch,
) -> None:
    import fno.agents.registry as registry

    entry = AgentEntry(
        name="positive-control",
        cwd="/tmp/project",
        log_path="/tmp/session.log",
        harness="codex",
    )
    monkeypatch.setattr(registry, "load_registry", lambda: [entry])
    repo = Path(__file__).resolve().parents[3]

    result = _invoke(monkeypatch, repo, "--live", "--json")

    assert result.exit_code == 1, result.output
    payload = json.loads(result.output)
    assert {"name", "created_at", "harness", "status"} <= set(
        payload["anchors"]
    )
    assert payload["contract_errors"] == []
    contracted = {
        "delivery_policy",
        "forked_from_session_id",
        "predecessor_session_ids",
        "live_status",
        "live_status_basis",
    }
    persisted = payload["persisted"]
    projected = payload["projected"]
    # contract-covered zeroes are measured, never dead-field findings
    assert not contracted & set(persisted["dead_fields"])
    assert not contracted & set(projected["dead_fields"])
    # lineage fields are conditional and persisted_and_projected
    assert {"forked_from_session_id", "predecessor_session_ids"} <= set(
        persisted["conditional_zero"]
    )
    assert {"forked_from_session_id", "predecessor_session_ids"} <= set(
        projected["conditional_zero"]
    )
    forked = persisted["conditional_zero"]["forked_from_session_id"]
    assert forked["count"] == 0
    assert forked["writer"]
    assert forked["test"] == "cli/tests/agents/test_session_lineage.py"
    # delivery_policy is transient on both readings
    assert persisted["transient_zero"]["delivery_policy"]["mode"] == "transient"
    assert projected["transient_zero"]["delivery_policy"]["mode"] == "transient"
    # liveness fields are projected-only enrichment
    assert {"live_status", "live_status_basis"} <= set(
        projected["conditional_zero"]
    )
    # role fields have no population contract: still dead findings
    assert {"role", "role_grantor", "role_level", "role_scope"} <= set(
        projected["dead_fields"]
    )


def test_live_coverage_refuses_unreadable_registry(monkeypatch) -> None:
    import fno.agents.registry as registry

    def unreadable():
        raise registry.RegistryVersionError("registry schema is unreadable")

    monkeypatch.setattr(registry, "load_registry", unreadable)
    repo = Path(__file__).resolve().parents[3]

    result = _invoke(monkeypatch, repo, "--live")

    assert result.exit_code == 2
    assert "UNMEASURED" in result.output
    assert "registry schema is unreadable" in result.output


# --- closure probe: check-agent-field-population-contract ---


def _repo() -> Path:
    return Path(__file__).resolve().parents[3]


def _load_checker():
    import importlib.util

    path = _repo() / "scripts" / "ci" / "check-agent-field-population-contract.py"
    spec = importlib.util.spec_from_file_location("closure_probe", path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def _closure_report() -> dict:
    forked = {
        "mode": "conditional",
        "surface": "persisted_and_projected",
        "writer": "record_session_observation branch classification",
        "test": "cli/tests/agents/test_session_lineage.py",
    }
    delivery = {
        "mode": "transient",
        "surface": "persisted_and_projected",
        "writer": "fno agents mail hold while a busy-mode hold is active",
        "test": "cli/tests/unit/test_mail_hold.py",
    }
    live = {
        "mode": "conditional",
        "surface": "projected",
        "writer": "read.py Claude supervisor enrichment",
        "test": "cli/tests/agents/test_read.py",
    }
    anchor = {"set": 7, "total": 7}
    return {
        "status": "ok",
        "anchors": {name: dict(anchor) for name in
                    ("name", "created_at", "harness", "status")},
        "contract_errors": [],
        "persisted": {
            "total": 7,
            "counts": {"forked_from_session_id": 0,
                       "predecessor_session_ids": 0,
                       "delivery_policy": 0, "name": 7},
            "dead_fields": [],
            "conditional_zero": {
                "forked_from_session_id": dict(forked),
                "predecessor_session_ids": dict(forked),
            },
            "transient_zero": {"delivery_policy": dict(delivery)},
        },
        "projected": {
            "total": 7,
            "counts": {"forked_from_session_id": 0, "live_status": 0,
                       "live_status_basis": 0, "delivery_policy": 0, "name": 7},
            "dead_fields": [],
            "conditional_zero": {
                "forked_from_session_id": dict(forked),
                "predecessor_session_ids": dict(forked),
                "live_status": dict(live),
                "live_status_basis": dict(live),
            },
            "transient_zero": {"delivery_policy": dict(delivery)},
        },
    }


def test_closure_probe_prints_marker_on_complete_report() -> None:
    import subprocess

    result = subprocess.run(
        [sys.executable,
         str(_repo() / "scripts" / "ci"
             / "check-agent-field-population-contract.py")],
        input=json.dumps(_closure_report()).encode(),
        capture_output=True,
    )

    assert result.returncode == 0, result.stderr
    assert b"agent-field-population-contract: verified" in result.stdout


def test_closure_probe_fails_on_dead_contract_field() -> None:
    checker = _load_checker()
    payload = _closure_report()
    payload["persisted"]["dead_fields"] = ["forked_from_session_id"]

    assert checker.check(payload, checker.FIELDS) != 0
