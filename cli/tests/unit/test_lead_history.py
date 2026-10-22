"""Tests for `fno agents lead history` - the role-scope term readback.

The journal scan and the caller-role scope resolution are the Rust
lead-history command (tested there); Python is the relay. Covers the relay
wiring and the refusal taxonomy.
"""
from __future__ import annotations


SCOPE = "x-a792/fleet"


def _patch_binary(monkeypatch, path) -> None:
    from fno import rust_binary

    monkeypatch.setattr(rust_binary, "resolve_binary", lambda: path)


def test_command_relays_the_native_read(tmp_path, monkeypatch) -> None:
    """The command hands scope, EVERY resolved journal path, and format to the binary."""
    from typer.testing import CliRunner

    from fno.lead.cli import agents_lead_app

    journal = tmp_path / "events.jsonl"
    journal.write_text("", encoding="utf-8")
    seen: dict = {}
    stub = tmp_path / "stub-fno-agents"
    stub.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
    stub.chmod(0o755)

    def fake_run(argv, **_kwargs):
        seen["argv"] = argv

        class Proc:
            returncode = 0
            stdout = '{"scope": "%s", "matched": 0}' % SCOPE
            stderr = ""

        return Proc()

    from fno.lead import history as history_module

    _patch_binary(monkeypatch, str(stub))
    monkeypatch.setattr(history_module.subprocess, "run", fake_run)
    monkeypatch.setenv("FNO_EVENTS_PATH", str(journal))

    result = CliRunner().invoke(agents_lead_app, ["history", "--scope", SCOPE, "--json"])

    assert result.exit_code == 0, result.output
    assert seen["argv"][1] == "lead-history"
    assert "--scope" in seen["argv"] and SCOPE in seen["argv"]
    assert seen["argv"].count("--events-path") >= 1
    assert str(journal) in seen["argv"]
    assert "--json" in seen["argv"]
    assert '"matched": 0' in result.output


def test_verdict_cmd_relays_a_native_failure(monkeypatch) -> None:
    # A failed read exits nonzero naming the cause and prints no verdict word
    # (AC3-ERR): the wrapper never substitutes defaults after a failed read.
    from typer.testing import CliRunner

    from fno.lead import history as history_module
    from fno.lead.cli import agents_lead_app

    def fake_run(argv, **_kwargs):
        class Proc:
            returncode = 1
            stdout = ""
            stderr = "fno-agents lead-verdict: scope x unreadable: graph corrupt"

        return Proc()

    monkeypatch.setattr(history_module.subprocess, "run", fake_run)

    result = CliRunner().invoke(agents_lead_app, ["verdict"])

    assert result.exit_code != 0
    assert "verdict: converging" not in result.output
    assert "graph corrupt" in result.output
