"""The Python emitter asserts the shared row-contradiction truth table."""

import json
from pathlib import Path

import pytest

from fno.agents.row_contradiction import model_substitution, project_row


_FIXTURE = Path(__file__).resolve().parents[3] / "schemas" / "agents-row-contradiction.json"


def test_shared_row_contradiction_fixture() -> None:
    fixture = json.loads(_FIXTURE.read_text())
    for case in fixture["cases"]:
        actual = project_row(case["row"], now=fixture["now"])
        assert "exited_at" not in actual, case["name"]
        for key, expected in case["expected"].items():
            assert actual[key] == expected, case["name"]


@pytest.mark.parametrize(
    ("requested", "observed", "expected"),
    [
        ("glm-5.3[1m]", {"kind": "observed", "model": "glm-5.3"}, "match"),
        (
            "glm-5.3[1m]",
            {"kind": "observed", "model": "glm-5.3-flash"},
            "substituted",
        ),
        # Missing / unreadable sides are UNKNOWN, never a match.
        (None, {"kind": "observed", "model": "glm-5.3"}, "unknown"),
        ("glm-5.3[1m]", {"kind": "no-transcript"}, "unknown"),
        # A bare observed string (the direct-call shape) still compares.
        ("glm-5.3[1m]", "glm-5.3-flash", "substituted"),
    ],
)
def test_model_substitution_specimen_table(requested, observed, expected) -> None:
    assert model_substitution(requested, observed) == expected
