"""Unit tests for `fno do plan stamp`, `graduate`, and `set-expected`.

The wrappers are clients of the keeper-served plan-doc writer (via
``fno.plan._project``): args reach the writer unchanged and its exit code
propagates.
"""
from __future__ import annotations

import pytest
from typer.testing import CliRunner

from fno.cli import app

runner = CliRunner()


def test_plan_validate_execution_refuses_post_gate_no_difficulty(tmp_path):
    """x-baef round-5: validate-plan.sh runs --execution, which never reaches
    PlanFrontmatter, so the difficulty gate must fire in THAT scope - a
    post-gate plan with no difficulty is refused at authoring time, quoting
    the bands; the on-gate twin passes the gate."""
    post_gate = tmp_path / "post-gate.md"
    post_gate.write_text(
        "---\nnode: x-baef\nstatus: ready\ncreated: 2026-08-27\n---\n# T\n\nBody.\n"
    )
    result = runner.invoke(app, ["do", "plan", "validate", str(post_gate), "--execution"])
    assert result.exit_code == 1, result.output
    assert "difficulty is required" in result.output
    assert "low, medium, high" in result.output

    # Round-12: undatable created is LENIENT at the authoring scope -
    # validate-plan.sh dates those plans itself (filename date, tolerant
    # reads), so the gate defers instead of refusing; the refusal the
    # minting lanes raise never appears here.
    no_created = tmp_path / "no-created.md"
    no_created.write_text("---\nnode: x-baef\nstatus: ready\n---\n# T\n\nBody.\n")
    import json as _json

    r3 = runner.invoke(
        app, ["do", "plan", "validate", str(no_created), "--execution", "--json"]
    )
    payload = _json.loads(r3.output)
    assert not any(
        "cannot read created" in v["message"] for v in payload["violations"]
    ), "authoring scope defers undatable created to validate-plan.sh's own dating"

    on_gate = tmp_path / "on-gate.md"
    on_gate.write_text(
        "---\nnode: x-baef\nstatus: ready\ncreated: 2026-08-26\n---\n"
        "# T\n\n## Execution Strategy\n\n```yaml\n"
        "execution_mode: sequential\n"
        "waves:\n  - wave: 1\n    mode: sequential\n    name: w\n    tasks: ['1.1']\n"
        "tasks:\n  - id: '1.1'\n    title: t\n    surface: ['cli/x.py']\n"
        "    verify: pytest cli/x.py -q\n"
        "    acceptance: [AC1-ERR]\n"
        "```\n"
    )
    result2 = runner.invoke(app, ["do", "plan", "validate", str(on_gate), "--execution"])
    assert result2.exit_code == 0, result2.output
    assert "difficulty is required" not in result2.output


def _stub_plan_docs(monkeypatch, exit_code=0):
    """Capture what the command hands the keeper client, without a keeper."""
    import fno.plan._project as project_client

    captured: list = []

    def _stub(op, **params):
        captured.append((op, params))
        return {"exit": exit_code}

    monkeypatch.setattr(project_client, "plan_docs", _stub)
    return captured


@pytest.mark.parametrize(
    "argv",
    [
        [
            "stamp",
            "--plan-path",
            "/tmp/some-plan.md",
            "--session-id",
            "abc-123",
            "--url",
            "https://example.com/pr/42",
            "--expected-url-count",
            "1",
        ],
    ],
)
def test_plan_verbs_forward_args_verbatim(monkeypatch, argv):
    """AC1-HP: every flag reaches the writer unchanged and its exit code propagates."""
    captured = _stub_plan_docs(monkeypatch, exit_code=2)
    result = runner.invoke(app, ["do", "plan", *argv])
    assert result.exit_code == 2
    assert captured == [("argv", {"verb": argv[0], "args": argv[1:]})]
