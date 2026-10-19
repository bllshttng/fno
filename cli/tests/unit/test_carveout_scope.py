"""Tests for the carveout `scope` field (x-7685, US8/AC15 prerequisite).

The lead orphan check (hooks/context-nudge.sh) must match a carved-out
orphaning by STRUCTURED scope, not by grepping `description` free text (a
reworded sentence would silently silence it). So the Carveout record carries an
optional `scope`, written via `fno carveout add --scope`, and the field is the
match key. Optional + last so existing records and the retro-triage harvest
parse unchanged.
"""
from __future__ import annotations

import json
from pathlib import Path

from typer.testing import CliRunner

from fno.cli import app


def test_cli_scope_round_trips_through_list_jsonl(tmp_path: Path, monkeypatch):
    monkeypatch.setenv("FNO_REPO_ROOT", str(tmp_path))
    monkeypatch.chdir(tmp_path)
    monkeypatch.delenv("CLAUDECODE_SESSION_ID", raising=False)
    runner = CliRunner()
    add = runner.invoke(
        app,
        ["carveout", "add", "--kind", "deferred", "--scope", "zzz-probe",
         "--need", "review-orphaned: no lead to trigger review",
         "workers exit review-orphaned; advisory self-review"],
    )
    assert add.exit_code == 0, add.output + add.stderr
    listing = runner.invoke(app, ["carveout", "list", "--json"])
    assert listing.exit_code == 0, listing.output
    rows = [json.loads(ln) for ln in listing.stdout.splitlines() if ln.strip()]
    matched = [r for r in rows if r.get("scope") == "zzz-probe"]
    assert len(matched) == 1
    assert "review-orphaned" in matched[0]["description"]
