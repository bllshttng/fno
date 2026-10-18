"""The operator lane's grouped CLI surface and byte-preserving writes."""

from __future__ import annotations
from pathlib import Path

import pytest
from typer.testing import CliRunner

from fno.outstanding.cli import outstanding_app


runner = CliRunner()


@pytest.fixture()
def lane(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Path:
    path = tmp_path / "my-priorities.md"
    monkeypatch.setattr("fno.paths.operator_lane", lambda: path)
    return path


def test_add_creates_then_appends_and_preserves_unrecognized_bytes(lane: Path):
    created = runner.invoke(outstanding_app, ["mine", "do", "add", "first item"])
    assert created.exit_code == 0, created.output
    assert lane.read_bytes() == b"- [ ] first item\n"

    lane.write_bytes(lane.read_bytes() + b"# operator note  \n\n")
    appended = runner.invoke(outstanding_app, ["mine", "do", "add", "second item"])
    assert appended.exit_code == 0, appended.output
    assert lane.read_bytes() == (
        b"- [ ] first item\n# operator note  \n\n- [ ] second item\n"
    )


def test_done_toggles_checkbox_reversibly_without_touching_notes(lane: Path):
    lane.write_bytes(b"heading\r\n- [ ] one\r\n- [ ] two\r\ntrailer\r\n")

    first = runner.invoke(outstanding_app, ["mine", "do", "done", "2"])
    assert first.exit_code == 0, first.output
    assert lane.read_bytes() == b"heading\r\n- [ ] one\r\n- [x] two\r\ntrailer\r\n"

    second = runner.invoke(outstanding_app, ["mine", "do", "done", "2"])
    assert second.exit_code == 0, second.output
    assert lane.read_bytes() == b"heading\r\n- [ ] one\r\n- [ ] two\r\ntrailer\r\n"


def test_invalid_visible_index_fails_loudly(lane: Path):
    lane.write_text("- [ ] only item\n", encoding="utf-8")

    result = runner.invoke(outstanding_app, ["mine", "do", "drop", "9"])

    assert result.exit_code != 0
    assert "item" in result.output.lower()
    assert lane.read_text(encoding="utf-8") == "- [ ] only item\n"


def test_write_failure_is_loud_and_leaves_original(lane: Path, monkeypatch: pytest.MonkeyPatch):
    lane.write_text("- [ ] keep me\n", encoding="utf-8")

    def fail_replace(_src, _dst):
        raise OSError("disk refused replacement")

    monkeypatch.setattr("fno.outstanding.mine.os.replace", fail_replace)
    result = runner.invoke(outstanding_app, ["mine", "do", "done", "1"])

    assert result.exit_code != 0
    assert "disk refused replacement" in result.output
    assert lane.read_text(encoding="utf-8") == "- [ ] keep me\n"
