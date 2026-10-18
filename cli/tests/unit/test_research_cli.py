"""Tests for the `fno do research` CLI ship-step wiring (US3, AC1/AC5).

`run_round` is stubbed (no network); the test exercises the deliver wiring:
default-deliver writes the brief to config.research.output_dir, and an unset
output_dir fails loud (exit 5).
"""

from __future__ import annotations

from pathlib import Path

import pytest
import typer
from typer.testing import CliRunner

from fno.research import cli as research_cli
from fno.research.core import RoundResult, Source


def _app() -> typer.Typer:
    app = typer.Typer()
    app.command()(research_cli.research_command)
    return app


def _stub_round(tmp_path: Path, monkeypatch: pytest.MonkeyPatch, *, note: str = "") -> Path:
    """Point run_round at a cache sidecar with one verified row; return its path."""
    cache = tmp_path / "cache" / "topic-words-here.sources.jsonl"
    cache.parent.mkdir(parents=True, exist_ok=True)
    if not note:
        cache.write_text(
            Source(
                url="https://a.example/p",
                fetched_at="t",
                hash="h",
                extract="finding",
                verified=True,
            ).to_json_line()
            + "\n",
            encoding="utf-8",
        )
    else:
        cache.touch()

    def fake_round(topic, **kw):
        return RoundResult(topic, "topic-words-here", str(cache), 1, 1, 0, note=note)

    monkeypatch.setattr(research_cli, "run_round", fake_round)
    return cache


def test_cli_delivers_to_output_dir(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    """AC1: default ship writes the brief to config.research.output_dir; DoneAdvisory."""
    _stub_round(tmp_path, monkeypatch)
    out = tmp_path / "out"
    monkeypatch.setattr(research_cli, "_output_dir", lambda: str(out))

    res = CliRunner().invoke(_app(), ["topic words here", "--no-claim"])
    assert res.exit_code == 0, res.output
    assert "DoneAdvisory" in res.output
    assert (out / "topic-words-here.md").is_file()
    assert (out / "topic-words-here.sources.jsonl").is_file()


def test_cli_unset_output_dir_fails_loud(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    """AC5: deliver with no output_dir exits 5 (never guesses)."""
    _stub_round(tmp_path, monkeypatch)
    monkeypatch.setattr(research_cli, "_output_dir", lambda: None)

    res = CliRunner().invoke(_app(), ["topic words here", "--no-claim"])
    assert res.exit_code == 5, res.output
    assert "output_dir" in res.output


def test_cli_explicit_node_flag_wins_over_fno_node(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """codex finding: FNO_NODE is not reliably set on the normal /ship doc
    invoking path, so an explicit --node (the reliable channel, passed the
    same way /blueprint passes --node "$CLAIMS_ID") must take precedence."""
    _stub_round(tmp_path, monkeypatch)
    out = tmp_path / "out"
    monkeypatch.setattr(research_cli, "_output_dir", lambda: str(out))
    monkeypatch.setenv("FNO_NODE", "x-wrong")
    captured: dict = {}
    real_deliver = research_cli.deliver

    def spying_deliver(*args, **kwargs):
        captured.update(kwargs)
        return real_deliver(*args, **kwargs)

    monkeypatch.setattr(research_cli, "deliver", spying_deliver)

    res = CliRunner().invoke(_app(), ["topic words here", "--no-claim", "--node", "x-right"])
    assert res.exit_code == 0, res.output
    assert captured.get("node") == "x-right"
