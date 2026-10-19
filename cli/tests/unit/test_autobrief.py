"""Unit tests for fno.provenance.autobrief (x-d1f4).

The dispatch-brief priority chain + mechanical synthesis + per-harness
transcript tail. Every store is injected so no read ever touches the
developer's real ~/.claude, ~/.codex, or opencode.db.
"""
from __future__ import annotations

import json
from pathlib import Path

import pytest

from fno.provenance.autobrief import _BRIEF_MAX_BYTES, resolve_dispatch_brief


@pytest.fixture(autouse=True)
def _no_ambient_self(monkeypatch):
    """Deterministic envelope: no live dispatcher identity unless a test sets it."""
    monkeypatch.delenv("FNO_AGENT_SELF", raising=False)


# --------------------------------------------------------------------------- #
# Store fixtures
# --------------------------------------------------------------------------- #

def _write_claude_transcript(root: Path, cwd: str, sid: str, records: list[dict]) -> None:
    slug = cwd.replace("/", "-").replace(".", "-")
    proj = root / slug
    proj.mkdir(parents=True, exist_ok=True)
    (proj / f"{sid}.jsonl").write_text(
        "\n".join(json.dumps(r) for r in records) + "\n", encoding="utf-8"
    )


def _claude_msg(role: str, text: str, ts: str) -> dict:
    return {
        "type": role,
        "timestamp": ts,
        "message": {"role": role, "content": [{"type": "text", "text": text}]},
    }


# --------------------------------------------------------------------------- #
# AC3-HP / AC6-ERR: explicit rung
# --------------------------------------------------------------------------- #

def test_ac3_explicit_brief_wins(tmp_path):
    """AC3-HP: dispatch_brief + sidecar + details all present -> explicit wins."""
    (tmp_path / "x-1.md").write_text("SIDECAR", encoding="utf-8")
    node = {
        "id": "x-1",
        "dispatch_brief": "EXPLICIT",
        "has_brief": True,
        "details": "some details",
    }
    brief, tag = resolve_dispatch_brief(node, briefs_dir=tmp_path)
    assert brief == "EXPLICIT"
    assert tag == "explicit"


# --------------------------------------------------------------------------- #
# AC2-HP / AC6-ERR: sidecar rung
# --------------------------------------------------------------------------- #

def test_ac2_sidecar_rides(tmp_path):
    """AC2-HP: has_brief + non-empty sidecar -> its content, tag=sidecar."""
    (tmp_path / "x-2.md").write_text("the sidecar brief body", encoding="utf-8")
    node = {"id": "x-2", "has_brief": True, "details": "ignored when sidecar present"}
    brief, tag = resolve_dispatch_brief(node, briefs_dir=tmp_path)
    assert brief == "the sidecar brief body"
    assert tag == "sidecar"


def test_ac6_oversized_sidecar_clamped_with_marker(tmp_path):
    """AC6-ERR: a 20 KB sidecar clamps to <= 8192 bytes ending in the marker."""
    (tmp_path / "x-2.md").write_text("y" * 20000, encoding="utf-8")
    node = {"id": "x-2", "has_brief": True}
    brief, tag = resolve_dispatch_brief(node, briefs_dir=tmp_path)
    assert tag == "sidecar"
    assert len(brief.encode("utf-8")) <= _BRIEF_MAX_BYTES
    assert brief.endswith("[truncated]")


# --------------------------------------------------------------------------- #
# AC1-HP: details synthesize
# --------------------------------------------------------------------------- #

def test_ac1_details_synthesize(tmp_path):
    """AC1-HP: details set, no dispatch_brief, no sidecar -> id + title + details,
    tag=synth-details."""
    node = {
        "id": "x-1234",
        "slug": "add-retry",
        "title": "Add retry logic",
        "details": "We need exponential backoff on the dispatch path. " * 5,
    }
    brief, tag = resolve_dispatch_brief(node, briefs_dir=tmp_path)
    assert tag == "synth-details"
    assert "x-1234" in brief
    assert "Add retry logic" in brief
    assert "exponential backoff" in brief
    assert brief.startswith("<fno_spawn ")
    assert 'source="synth-details"' in brief


def test_envelope_from_uses_live_dispatcher_identity(tmp_path, monkeypatch):
    """from precedence: a live dispatching session's ambient identity wins."""
    monkeypatch.setenv("FNO_AGENT_SELF", "athens")
    node = {"id": "x-1", "title": "T", "details": "d " * 300,
            "source_session_id": "20260506T213611Z-58489-6764ea"}
    brief, _ = resolve_dispatch_brief(node, briefs_dir=tmp_path)
    assert 'from="athens"' in brief
    assert "reply: fno agents mail send athens" in brief


# --------------------------------------------------------------------------- #
# AC7-EDGE / AC8-EDGE: transcript tail
# --------------------------------------------------------------------------- #

def test_ac7_thin_details_pull_tail_near_created_at(tmp_path):
    """AC7-EDGE: thin details + a resolvable claude transcript -> tail near
    created_at rides; records after created_at+120s are excluded."""
    cwd = "/Users/bb16/code/footnote"
    sid = "abcd1234-0000-0000-0000-000000000000"
    _write_claude_transcript(
        tmp_path,
        cwd,
        sid,
        [
            _claude_msg("user", "the motivating question", "2026-07-21T00:00:00Z"),
            _claude_msg("assistant", "the motivating answer", "2026-07-21T00:01:00Z"),
            # Well after created_at + 120s: an unrelated later topic.
            _claude_msg("user", "UNRELATED_LATER_TOPIC", "2026-07-21T02:00:00Z"),
        ],
    )
    node = {
        "id": "x-1",
        "title": "T",
        "details": "one-liner",  # < 400 bytes -> thin -> pull tail
        "source_harness": "claude",
        "source_session_id": sid,
        "source_cwd": cwd,
        "created_at": "2026-07-21T00:00:00Z",
    }
    brief, tag = resolve_dispatch_brief(node, briefs_dir=tmp_path, projects_root=tmp_path)
    assert tag == "synth-details+tail"
    assert "one-liner" in brief
    assert "the motivating question" in brief
    assert "UNRELATED_LATER_TOPIC" not in brief


def test_ac8_ambiguous_resolution_skips_tail(tmp_path):
    """AC8-EDGE: an ambiguous claude prefix-glob skips the tail rung entirely."""
    cwd = "/Users/bb16/code/footnote"
    slug = cwd.replace("/", "-").replace(".", "-")
    proj = tmp_path / slug
    proj.mkdir(parents=True, exist_ok=True)
    # Two files under the same 8-hex prefix -> ambiguous.
    for suffix in ("aaaa", "bbbb"):
        (proj / f"abcd1234-{suffix}-0000-0000-000000000000.jsonl").write_text(
            json.dumps(_claude_msg("user", "wrong-session-text", "2026-07-21T00:00:00Z")) + "\n",
            encoding="utf-8",
        )
    node = {
        "id": "x-1", "title": "T", "details": "one-liner",
        "source_harness": "claude", "source_session_id": "abcd1234",
        "source_cwd": cwd, "created_at": "2026-07-21T00:00:00Z",
    }
    brief, tag = resolve_dispatch_brief(node, briefs_dir=tmp_path, projects_root=tmp_path)
    assert tag == "synth-details"  # tail skipped
    assert "wrong-session-text" not in brief


def test_tail_extracts_from_codex_rollout(tmp_path):
    """A thin-details node with a codex source reads the rollout tail."""
    sid = "019f837c-3461-7911-811f-3290b8b34934"
    cwd = "/Users/bb16/code/footnote"
    day = tmp_path / "2026" / "07" / "21"
    day.mkdir(parents=True, exist_ok=True)
    (day / f"rollout-2026-07-21T00-00-00-{sid}.jsonl").write_text(
        "\n".join(
            json.dumps(r)
            for r in [
                {"type": "session_meta", "payload": {"id": sid, "cwd": cwd}},
                {
                    "type": "response_item",
                    "timestamp": "2026-07-21T00:00:10Z",
                    "payload": {
                        "type": "message", "role": "user",
                        "content": [{"type": "input_text", "text": "codex question"}],
                    },
                },
                {
                    "type": "response_item",
                    "timestamp": "2026-07-21T00:00:20Z",
                    "payload": {
                        "type": "message", "role": "assistant",
                        "content": [{"type": "output_text", "text": "codex answer"}],
                    },
                },
            ]
        )
        + "\n",
        encoding="utf-8",
    )
    node = {
        "id": "x-1", "title": "T", "details": "one-liner",
        "source_harness": "codex", "source_session_id": sid, "source_cwd": cwd,
        "created_at": None,
    }
    brief, tag = resolve_dispatch_brief(node, briefs_dir=tmp_path, codex_sessions_dir=tmp_path)
    assert tag == "synth-details+tail"
    assert "codex question" in brief
    assert "codex answer" in brief


# --------------------------------------------------------------------------- #
# AC5-ERR / AC9-FR: degrade + none
# --------------------------------------------------------------------------- #

def test_ac9_nothing_to_synthesize_returns_none(tmp_path):
    """AC9-FR: nothing to synthesize -> (None, 'none')."""
    node = {"id": "x-1"}  # no brief, no sidecar, no details, no provenance
    brief, tag = resolve_dispatch_brief(node, briefs_dir=tmp_path)
    assert brief is None
    assert tag == "none"


def test_tail_omits_fno_mail_turns(tmp_path):
    """fno_mail peer turns in transcripts must not be ingested into synthesized autobriefs."""
    cwd = "/Users/bb16/code/footnote"
    sid = "abcd1234-0000-0000-0000-00000000mail"
    _write_claude_transcript(
        tmp_path, cwd, sid,
        [
            _claude_msg("user", "REAL_OPERATOR_REQUEST", "2026-07-21T00:00:10Z"),
            _claude_msg("user", '<fno_mail from="cc-peer123" harness="claude">PEER_MAIL_SECRET</fno_mail>', "2026-07-21T00:00:20Z"),
            _claude_msg("assistant", "ASSISTANT_RESPONSE", "2026-07-21T00:00:30Z"),
        ],
    )
    node = {
        "id": "x-1", "title": "T", "details": "one-liner",
        "source_harness": "claude", "source_session_id": sid, "source_cwd": cwd,
        "created_at": "2026-07-21T00:01:00Z",
    }
    brief, tag = resolve_dispatch_brief(node, briefs_dir=tmp_path, projects_root=tmp_path)
    assert tag == "synth-details+tail"
    assert "REAL_OPERATOR_REQUEST" in brief
    assert "ASSISTANT_RESPONSE" in brief
    assert "PEER_MAIL_SECRET" not in brief
