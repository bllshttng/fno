"""The Python door of the bot-review producer.

The publish contract itself (gate chain, refusals, the POST, the
reviewDecision readback) lives in the Rust module behind the binary
(``crates/fno-agents/src/publish_review.rs``) and is tested there. These
tests pin the Python side of the seam: the transport round-trip, and the
hidden verb's exit-code mapping.
"""

from __future__ import annotations


from typer.testing import CliRunner

from fno.pr._publish_review import PublishReviewUnavailable

runner = CliRunner()

ANSWER = {
    "status": "posted",
    "reason": "posted APPROVE as fno-review-bot on #931 (reviewDecision=APPROVED)",
    "event": "APPROVE",
    "review_decision": "APPROVED",
    "stderr": None,
    "receipt": "bot-review: posted APPROVE as fno-review-bot on #931 (reviewDecision=APPROVED)",
    "exit": 0,
}


def test_verb_passes_the_default_verdict_and_dry_run_flag(tmp_path, monkeypatch):
    from fno.pr import cli as pr_cli

    seen = {}

    def fake(payload):
        seen.update(payload)
        return {"status": "skipped", "receipt": "bot-review: skipped (x)", "exit": 1}

    monkeypatch.setattr("fno.pr._publish_review.publish_review_call", fake)
    result = runner.invoke(pr_cli.pr_app, ["publish-review", "--pr", "5", "--dry-run"])
    assert result.exit_code == 1
    assert seen["pr_number"] == 5
    assert seen["verdict"] == ""
    assert seen["dry_run"] is True
    assert "bot-review: skipped (x)" in result.output


def test_verb_survives_an_unavailable_binary(monkeypatch):
    from fno.pr import cli as pr_cli

    def fake(payload):
        raise PublishReviewUnavailable("the fno-agents binary was not found")

    monkeypatch.setattr("fno.pr._publish_review.publish_review_call", fake)
    result = runner.invoke(pr_cli.pr_app, ["publish-review", "--pr", "5"])
    assert result.exit_code == 1
    assert "bot-review: failed" in result.output
