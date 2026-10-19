"""The target ship review is the in-session fno lane; the mail round trip is gone.

The review step is exactly `/fno:review <size> --comment` (Codex
`$fno:review <size> --comment`), invoked by the model in its own context on
the final local HEAD before `/fno:ship pr create`. No mail, no paste, no turn
boundary. These tests pin that direction across every reachable target
surface and guard against the prose regrowing the retired
`request-self-review` round trip or the deleted pre-ship decision helper.
"""
from __future__ import annotations

from pathlib import Path
import json

REPO_ROOT = Path(__file__).resolve().parents[3]


def test_request_self_review_pins_pr_head_and_uses_the_raw_self_route(
    monkeypatch,
):
    import fno.target_cli as target_cli
    from typer.testing import CliRunner

    calls = []
    monkeypatch.setattr(
        target_cli,
        "_git_out",
        lambda _cwd, *args: "abc1234" if args == ("rev-parse", "HEAD") else None,
    )
    monkeypatch.setattr(
        target_cli,
        "_read_pr_metadata",
        lambda _pr, _cwd: {
            "number": 123,
            "headRefOid": "abc1234",
            "baseRefName": "main",
        },
    )
    monkeypatch.setattr(
        target_cli,
        "_resolve_self_review_identity",
        lambda: ("codex", "codex-session"),
    )
    monkeypatch.setattr(
        target_cli,
        "_send_self_review_payload",
        lambda **kwargs: calls.append(kwargs) or {"outcome": "started", "transport": "codex-daemon"},
    )

    result = CliRunner().invoke(target_cli.target_app, ["request-self-review", "--pr", "123"])

    assert result.exit_code == 0, result.output
    receipt = json.loads(result.output)
    assert receipt["outcome"] == "started"
    # The inline fno lane is rendered in the active harness's spelling.
    assert calls[0]["payload"].startswith("$fno:review ")
    # The bare PR number leads the target slot; HEAD and base are trailing
    # context a strict reader never mistakes for the target.
    assert calls[0]["payload"].endswith(
        "123 HEAD abc1234 against origin/main"
    )
    assert calls[0]["harness"] == "codex"


def test_request_self_review_refuses_when_local_head_does_not_match_pr(
    monkeypatch,
):
    import fno.target_cli as target_cli
    from typer.testing import CliRunner

    monkeypatch.setattr(
        target_cli,
        "_git_out",
        lambda _cwd, *args: "local999" if args == ("rev-parse", "HEAD") else None,
    )
    monkeypatch.setattr(
        target_cli,
        "_read_pr_metadata",
        lambda _pr, _cwd: {
            "number": 123,
            "headRefOid": "abc1234",
            "baseRefName": "main",
        },
    )
    result = CliRunner().invoke(target_cli.target_app, ["request-self-review", "--pr", "123"])

    assert result.exit_code != 0
    receipt = json.loads(result.output)
    assert receipt["outcome"] == "refused"
    assert "HEAD" in receipt["reason"]


def test_request_self_review_without_pr_refuses_detached_head(
    monkeypatch,
):
    import fno.target_cli as target_cli
    from typer.testing import CliRunner

    monkeypatch.setattr(
        target_cli,
        "_git_out",
        lambda _cwd, *args: "abc1234" if args == ("rev-parse", "HEAD") else "HEAD",
    )

    result = CliRunner().invoke(target_cli.target_app, ["request-self-review"])

    assert result.exit_code == 2
    receipt = json.loads(result.output)
    assert receipt["outcome"] == "refused"
    assert "no branch" in receipt["reason"]
