"""`fno setup post-merge` scaffold.

Drives the interactive-agnostic core ``scaffold_post_merge`` with fake
prompt/confirm callables so the write path is exercised without real stdin.
The scaffold is the ONLY writer of ``config.post_merge.parking_lot_path`` /
``config.project.id``; the oracle stays read-only.
"""

from __future__ import annotations

from pathlib import Path
from typing import List, Optional


from fno.config_cli import post_merge_readiness
from fno.setup_cli import scaffold_post_merge


class FakePrompter:
    """Pops queued responses; None signals a cancel (Ctrl-C / EOF)."""

    def __init__(self, responses: List[Optional[str]]):
        self.responses = list(responses)
        self.calls: List[str] = []

    def __call__(self, message: str, default: str) -> Optional[str]:
        self.calls.append(message)
        return self.responses.pop(0) if self.responses else None


def _repo(tmp_path: Path, body: Optional[str] = None, *, active: bool = False) -> Path:
    fno = tmp_path / ".fno"
    fno.mkdir(parents=True, exist_ok=True)
    if body is not None:
        (fno / "settings.yaml").write_text(body, encoding="utf-8")
    if active:
        (fno / "target-state.md").write_text("session\n", encoding="utf-8")
    return tmp_path


def _settings_text(repo: Path) -> Optional[str]:
    p = repo / ".fno" / "settings.yaml"
    return p.read_text(encoding="utf-8") if p.is_file() else None


# --- AC2-ERR: a rejected path re-prompts ------------------------------------


def test_rejects_absolute_path_then_reprompts(tmp_path: Path) -> None:
    repo = _repo(tmp_path, "config:\n  post_merge:\n    enabled: true\n")
    prompter = FakePrompter(["/etc/escape.md", "internal/ok/parking-lot.md", "ok"])
    result = scaffold_post_merge(
        repo,
        prompt_fn=prompter,
        confirm_fn=lambda _m: True,
    )
    assert result["changed"] is True
    assert result["parking_lot_path"] == "internal/ok/parking-lot.md"
    # The bad path was re-prompted, not written.
    assert len([c for c in prompter.calls if "parking_lot_path" in c]) == 2
    assert post_merge_readiness(repo).parking_lot_path == "internal/ok/parking-lot.md"


# --- AC2-FR: a cancelled prompt writes nothing partial ----------------------


def test_cancel_writes_nothing(tmp_path: Path) -> None:
    repo = _repo(tmp_path, "config:\n  post_merge:\n    enabled: true\n")
    before = _settings_text(repo)
    prompter = FakePrompter([None])  # cancel at the parking_lot_path prompt
    result = scaffold_post_merge(
        repo,
        prompt_fn=prompter,
        confirm_fn=lambda _m: True,
    )
    assert result["changed"] is False
    assert result["reason"] == "cancelled"
    assert _settings_text(repo) == before  # no partial write
    assert post_merge_readiness(repo).parking_lot_path is None


def test_malformed_settings_aborts_without_prompt_loop(tmp_path: Path) -> None:
    # A malformed existing settings.yaml makes every set_config_value fail on
    # the same file; re-prompting would loop forever. The scaffold must refuse
    # up front (verdict == error) and never enter the prompt loop.
    repo = _repo(tmp_path, "config:\n  post_merge:\n    bad: [unterminated\n")
    prompter = FakePrompter(["internal/x/parking-lot.md"])
    result = scaffold_post_merge(repo, prompt_fn=prompter, confirm_fn=lambda _m: True)
    assert result["changed"] is False
    assert result["reason"] == "settings-error"
    assert prompter.calls == []  # never prompted
