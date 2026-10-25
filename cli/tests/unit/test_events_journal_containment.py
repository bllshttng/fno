"""The checkout event journal is not a test fixture's scratch space.

Measured 2026-08-17: `fno agents needs --json` returned 12 rows, 6 of them test
fixtures wearing production shape (`acme-web -> fno-peer: design Q: which
auth?`). They reached a live operator surface because `append_event` resolves
its default journal from `resolve_repo_root()`, and `fno.hermetic.neutralise`
deliberately leaves `FNO_REPO_ROOT` unset, so the resolver falls through to `git
rev-parse` and finds the developer's real checkout.

The containment is at the writer, never at the fold. A fold that recognises
fixture-shaped rows carries an exception list, and the next fixture that does
not match the list refills the queue in silence.

These tests are the guard the fix exists for. Cleaning today's rows is one-time;
without an assertion the next test touching an escalation path refills the
queue and nobody notices, because a polluted panel looks exactly like a busy
one.
"""
from __future__ import annotations

from pathlib import Path

import pytest
from typer.testing import CliRunner

from fno.paths_testing import use_tmpdir


@pytest.fixture
def runner() -> CliRunner:
    return CliRunner()


@pytest.fixture
def pinned(tmp_path: Path, monkeypatch) -> dict[str, Path]:
    """A fake checkout plus the scratch journal `neutralise` pins beside it.

    The checkout is fake rather than real so the assertion is hermetic and
    still means something in CI, where no polluted 51 MB journal exists to
    detect a write against.
    """
    use_tmpdir(monkeypatch, tmp_path)
    monkeypatch.setenv("FNO_INBOX_ROOT", str(tmp_path / "inbox"))
    monkeypatch.setenv("FNO_INBOX_TEST_MODE", "1")  # suppress desktop notify

    checkout = tmp_path / "repo"
    (checkout / ".fno").mkdir(parents=True)
    checkout_journal = checkout / ".fno" / "events.jsonl"
    checkout_journal.write_text("", encoding="utf-8")

    monkeypatch.setenv("FNO_REPO_ROOT", str(checkout))
    # resolve_repo_root freezes the env var at first call (see its docstring),
    # and the autouse conftest clear already ran before this fixture.


    scratch = tmp_path / "scratch" / "events.jsonl"
    monkeypatch.setenv("FNO_EVENTS_PATH", str(scratch))
    return {"checkout": checkout_journal, "scratch": scratch, "root": checkout}


def _rows(journal: Path, event_type: str) -> list[dict]:
    from tests._event_rows import event_rows

    return [row for row in event_rows(journal) if row.get("type") == event_type]


def test_escalation_lands_in_the_scratch_journal_not_the_checkout(pinned, runner) -> None:
    """A `--kind question` send must leave the checkout journal byte-identical
    while the scratch journal gains the row.

    Both halves are load-bearing. The scratch assertion is the positive
    control: without it, a send that never escalated at all (a refactor that
    drops the call, a debounce that swallows it) would satisfy the containment
    half and read as proof of a fix that is not there.
    """
    from fno.mail.cli import mail_app

    before = pinned["checkout"].read_bytes()

    result = runner.invoke(
        mail_app,
        [
            "send",
            "--to-project", "fno-peer",
            "--kind", "question",
            "--from-name", "acme-web",
            "design Q: which auth?",
        ],
    )
    assert result.exit_code == 0, result.output

    escalations = _rows(pinned["scratch"], "mail_escalation")
    assert len(escalations) == 1, f"expected one escalation, got {escalations}"
    assert escalations[0]["data"]["recipient"] == "fno-peer"

    assert pinned["checkout"].read_bytes() == before
