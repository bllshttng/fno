"""CLI tests for `fno do think dispatch` (x-0a9c, Wave C).

The command is thin glue over dispatch_conversational; these cover the glue's real
logic: live-session-id resolution, node resolution, exit codes, and that the
LIVE (session_id, cwd) pointer is what flows into the dispatch core.
"""
from __future__ import annotations
from tests.fixtures.graph_seed import seed_graph

import json
import os
from pathlib import Path

import pytest
from typer.testing import CliRunner

from fno.provenance import cli as think_cli
from fno.provenance.spawn_think import ThinkSpawnResult
from fno.provenance.resolver import ResolvedTranscript

runner = CliRunner()


@pytest.fixture(autouse=True)
def _clear_harness_markers(monkeypatch):
    for marker in (
        "CODEX_THREAD_ID",
        "CLAUDE_CODE_SESSION_ID",
        "CODEX_SESSION_ID",
        "GEMINI_SESSION_ID",
    ):
        monkeypatch.delenv(marker, raising=False)


@pytest.fixture
def graph(monkeypatch, tmp_path):
    """A one-node graph + a captured dispatch seam. Returns the capture dict."""
    entries = [{"id": "x-0a9c", "slug": "conv-think", "title": "conversational command",
                "source_session_id": "stored", "source_cwd": "/birth"}]
    monkeypatch.setattr("fno.graph.api.wire_rows", lambda path=None, **k: entries)
    monkeypatch.setattr("fno.graph.cli._graph_path", lambda: tmp_path / "graph.json")
    cap: dict = {}

    def fake_dispatch(node, *, session_id, cwd, harness="claude", **kw):
        cap.update(node=node, session_id=session_id, cwd=cwd, harness=harness)
        return ThinkSpawnResult("spawned", "think_spawned", node_id=node["id"],
                                presence="attended", resolved=True, think_session="abc123")

    monkeypatch.setattr("fno.provenance.spawn_think.dispatch_conversational", fake_dispatch)
    return cap


def test_no_live_session_exits_2(graph, monkeypatch):
    for v in (
        "CODEX_THREAD_ID", "CLAUDE_CODE_SESSION_ID", "CODEX_SESSION_ID", "GEMINI_SESSION_ID"
    ):
        monkeypatch.delenv(v, raising=False)
    r = runner.invoke(think_cli.think_app, ["dispatch", "x-0a9c"])
    assert r.exit_code == 2
    assert "no live session id" in r.output
    assert "node" not in graph  # never reached the dispatch


def test_node_not_found_exits_2(graph, monkeypatch):
    monkeypatch.setenv("CLAUDE_CODE_SESSION_ID", "live-sid")
    r = runner.invoke(think_cli.think_app, ["dispatch", "no-such-node-xyz"])
    assert r.exit_code == 2
    assert "no node matches" in r.output


def test_happy_path_passes_live_pointer(graph, monkeypatch):
    """AC5-HP at the CLI boundary: the LIVE session id + cwd reach the core."""
    monkeypatch.setenv("CLAUDE_CODE_SESSION_ID", "live-sid")
    r = runner.invoke(think_cli.think_app,
                      ["dispatch", "x-0a9c", "--cwd", "/live/here"])
    assert r.exit_code == 0
    assert "think dispatched: x-0a9c" in r.output
    assert graph["session_id"] == "live-sid"
    assert graph["cwd"] == "/live/here"
    assert graph["node"]["id"] == "x-0a9c"


def test_empty_model_rejected_exits_2(graph, monkeypatch):
    """AC2-ERR at this command: an empty --model is a usage error, nothing dispatches."""
    monkeypatch.setenv("CLAUDE_CODE_SESSION_ID", "live-sid")
    r = runner.invoke(think_cli.think_app,
                      ["dispatch", "x-0a9c", "--model", "   "])
    assert r.exit_code == 2
    assert "--model must not be empty" in r.output
    assert "node" not in graph  # never reached the dispatch


def _subprocess_journey(tmp_path: Path, monkeypatch):
    graph_path = tmp_path / "graph.json"
    seed_graph(graph_path, json.dumps(
            {
                "entries": [
                    {
                        "id": "x-0a9c",
                        "slug": "conv-think",
                        "title": "conversational command",
                        "cwd": str(tmp_path),
                    }
                ]
            }
        ))
    # The think-dispatch resolver reads through the guarded metadata seam,
    # which resolves paths.graph_json at call time (not graph.cli._graph_path).
    monkeypatch.setattr("fno.paths.graph_json", lambda: graph_path)
    monkeypatch.setattr(
        "fno.provenance.spawn_think.resolve_transcript",
        lambda harness, sid, cwd, **kw: ResolvedTranscript(
            harness,
            sid,
            cwd,
            True,
            transcript_path=str(tmp_path / "transcript.jsonl"),
        ),
    )
    monkeypatch.setenv("CODEX_THREAD_ID", "codex-subprocess-thread")
    monkeypatch.setenv("FNO_CLAIMS_ROOT", str(tmp_path / "claims-root"))
    monkeypatch.setenv("FNO_HOME", str(tmp_path / "fno-home"))
    monkeypatch.setenv("HOME", str(tmp_path))

    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    capture = tmp_path / "spawn-argv"
    fake = bin_dir / "fno-py"
    fake.write_text(
        "#!/usr/bin/env bash\nprintf '%s\\0' \"$@\" > \"$SPAWN_CAPTURE\"\n"
        "printf '%s\\n' '{\"short_id\":\"real123\"}'\n"
    )
    fake.chmod(0o755)
    monkeypatch.setenv("PATH", f"{bin_dir}:{os.environ['PATH']}")
    monkeypatch.setenv("SPAWN_CAPTURE", str(capture))
    return capture


def test_codex_json_dispatch_reaches_spawn_subprocess_and_parses_receipt(
    tmp_path, monkeypatch
):
    capture = _subprocess_journey(tmp_path, monkeypatch)

    result = runner.invoke(
        think_cli.think_app,
        ["dispatch", "x-0a9c", "--json"],
    )

    assert result.exit_code == 0, result.output
    payload = json.loads(result.stdout)
    assert payload["decision"] == "spawned"
    assert payload["think_session"] == "real123"
    argv = capture.read_bytes().decode().rstrip("\0").split("\0")
    assert argv[:4] == ["agents", "spawn", "--harness", "claude"]
    assert "--substrate" not in argv
    assert "codex posture:" not in result.stdout
    assert "claude-bg-fallback" in result.stderr


def test_unsupported_provider_never_launches_spawn_subprocess(tmp_path, monkeypatch):
    capture = _subprocess_journey(tmp_path, monkeypatch)

    result = runner.invoke(
        think_cli.think_app,
        ["dispatch", "x-0a9c", "--provider", "codex", "--json"],
    )

    assert result.exit_code == 2
    assert not capture.exists()
    assert "dispatch=unsupported" in result.stderr
