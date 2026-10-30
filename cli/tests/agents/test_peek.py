"""Tests for ``fno agents peek`` (x-05da) — the read-only twin of mail send.

Grouped by the plan's verify slices:
  peek_resolve       — US1: resolver parity + exit-13 miss (AC1-HP, AC1-ERR)
  peek_reader        — US2: recent_records claude/codex/unsupported (AC2-*)
  peek_tail_follow   — US3: --lines / --json / no-activity / follow (AC1-EDGE/FR/UI)
  peek_status_stream — US4: dual-envelope status fast-path (AC3-EDGE)
"""
from __future__ import annotations

import json
import os
import time
from pathlib import Path

from fno.agents.peek import (
    recent_records,
)


class _Session:
    """Minimal DiscoveredSession stand-in (peek reads attrs by getattr)."""

    def __init__(self, agent="claude", session_id="sid-123", short_id="abc12345", cwd="/tmp/proj"):
        self.agent = agent
        self.session_id = session_id
        self.short_id = short_id
        self.cwd = cwd


def _claude_transcript(root: Path, cwd: str, session_id: str, lines: list[str]) -> Path:
    slug = cwd.replace("/", "-").replace(".", "-")
    d = root / slug
    d.mkdir(parents=True, exist_ok=True)
    p = d / f"{session_id}.jsonl"
    p.write_text("\n".join(lines) + "\n", encoding="utf-8")
    return p


def _user(text):
    return json.dumps({"type": "user", "message": {"role": "user", "content": text}})


def _assistant(text):
    return json.dumps(
        {"type": "assistant", "message": {"role": "assistant", "content": [{"type": "text", "text": text}]}}
    )


# --------------------------------------------------------------------------
# peek_resolve — US1
# --------------------------------------------------------------------------



# --------------------------------------------------------------------------
# peek_mux — pane-substrate workers, the default substrate (x-680d)
# --------------------------------------------------------------------------



# --------------------------------------------------------------------------
# peek_reader — US2
# --------------------------------------------------------------------------


def test_peek_reader_claude_tails_and_skips_torn(tmp_path):
    """AC2-HP + AC2-EDGE: claude arm parses records, skips a torn trailing line."""
    sess = _Session()
    lines = [_user("first"), _assistant("second"), '{"type":"assistant","message":{"role":"assist']
    _claude_transcript(tmp_path, sess.cwd, sess.session_id, lines)
    recs = recent_records("claude", sess.session_id, sess.cwd, 10, projects_root=tmp_path)
    assert [r.text for r in recs] == ["first", "second"]


def test_peek_distinguishes_fno_mail_as_peer(tmp_path):
    """fno_mail user turns must be labelled with role 'peer', not 'user'."""
    sess = _Session()
    lines = [
        _user("operator prompt"),
        _user('<fno_mail from="cc-12345678" harness="claude">peer mail content</fno_mail>'),
        _assistant("assistant response"),
    ]
    _claude_transcript(tmp_path, sess.cwd, sess.session_id, lines)
    recs = recent_records("claude", sess.session_id, sess.cwd, 10, projects_root=tmp_path)
    assert [(r.role, r.text) for r in recs] == [
        ("user", "operator prompt"),
        ("peer", '<fno_mail from="cc-12345678" harness="claude">peer mail content</fno_mail>'),
        ("assistant", "assistant response"),
    ]



# --------------------------------------------------------------------------
# peek_tail_follow — US3
# --------------------------------------------------------------------------



# --------------------------------------------------------------------------
# peek_status_stream — US4
# --------------------------------------------------------------------------



# --------------------------------------------------------------------------
# peek_reader — US6 opencode arm
# --------------------------------------------------------------------------


def _opencode_message(
    storage: Path,
    session_id: str,
    *,
    msg_id: str,
    role: str,
    created: int,
    parts: list[dict] | None,
) -> None:
    """Write one message + its parts into an opencode storage tree.

    Mirrors the real 1.0.223 layout: the message JSON carries NO text (only
    role + ``time.created``); the text lives in ``part/<msg_id>/``.
    """
    mdir = storage / "message" / session_id
    mdir.mkdir(parents=True, exist_ok=True)
    (mdir / f"{msg_id}.json").write_text(
        json.dumps({"id": msg_id, "role": role, "time": {"created": created}}),
        encoding="utf-8",
    )
    if parts is None:
        return
    pdir = storage / "part" / msg_id
    pdir.mkdir(parents=True, exist_ok=True)
    for i, p in enumerate(parts):
        (pdir / f"prt_{i:03d}.json").write_text(json.dumps(p), encoding="utf-8")


def test_peek_reader_opencode_orders_by_created_and_joins_parts(tmp_path):
    """AC-HP: messages render in time.created order with their parts joined,
    even though the filenames sort the other way."""
    storage = tmp_path / "opencode"
    sid = "ses_abc123"
    # Filenames sort z, m, a; time.created orders them a, m, z.
    _opencode_message(
        storage, sid, msg_id="zzz", role="user", created=1,
        parts=[{"type": "text", "text": "first"}],
    )
    _opencode_message(
        storage, sid, msg_id="mmm", role="assistant", created=2,
        parts=[{"type": "text", "text": "second"}, {"type": "text", "text": "half"}],
    )
    _opencode_message(
        storage, sid, msg_id="aaa", role="user", created=3,
        parts=[{"type": "text", "text": "third"}],
    )
    recs = recent_records("opencode", sid, "/tmp/proj", 10, opencode_storage_dir=storage)
    assert [(r.role, r.text) for r in recs] == [
        ("user", "first"),
        ("assistant", "second half"),
        ("user", "third"),
    ]


# --------------------------------------------------------------------------
# peek_reader — opencode SQLite store (current opencode)
# --------------------------------------------------------------------------


def _opencode_db(storage: Path, session_id: str, turns) -> None:
    """Build an opencode.db transcript. `turns` = [(msg_id, role, created, parts)].

    Message and part payloads are the same JSON shapes the legacy files hold,
    which is why the rendering policy is shared between the two readers.
    """
    import sqlite3

    storage.mkdir(parents=True, exist_ok=True)
    con = sqlite3.connect(storage.parent / "opencode.db")
    con.execute("CREATE TABLE session (id TEXT, directory TEXT, time_created INTEGER, time_updated INTEGER)")
    con.execute("CREATE TABLE message (id TEXT, session_id TEXT, time_created INTEGER, data TEXT)")
    con.execute("CREATE TABLE part (id TEXT, message_id TEXT, session_id TEXT, time_created INTEGER, data TEXT)")
    for mid, role, created, parts in turns:
        con.execute(
            "INSERT INTO message VALUES (?,?,?,?)",
            (mid, session_id, created, json.dumps({"id": mid, "role": role})),
        )
        for i, p in enumerate(parts):
            con.execute(
                "INSERT INTO part VALUES (?,?,?,?,?)",
                (f"prt_{mid}_{i}", mid, session_id, i, json.dumps(p)),
            )
    con.commit()
    con.close()


def test_peek_reader_opencode_db_orders_and_joins(tmp_path):
    """Ordering comes from the time_created column, not a filename or mtime."""
    storage = tmp_path / "opencode" / "storage"
    sid = "ses_db"
    _opencode_db(
        storage,
        sid,
        [
            ("zzz", "user", 1, [{"type": "text", "text": "first"}]),
            ("mmm", "assistant", 2, [
                {"type": "reasoning", "text": "hidden"},
                {"type": "text", "text": "second"},
                {"type": "tool", "tool": "bash"},
            ]),
            ("aaa", "user", 3, [{"type": "text", "text": "third"}]),
        ],
    )
    recs = recent_records("opencode", sid, "/x", 10, opencode_storage_dir=storage)
    assert [(r.role, r.text) for r in recs] == [
        ("user", "first"),
        ("assistant", "second [tool_use: bash]"),
        ("user", "third"),
    ]


def test_peek_opencode_idle_db_session_does_not_serve_legacy_transcript(tmp_path):
    """A session with no messages yet reads empty from the database. Falling
    back would serve the legacy tree's stale transcript for that same id as if
    it were current."""
    storage = tmp_path / "opencode" / "storage"
    sid = "ses_both"
    _opencode_message(
        storage, sid, msg_id="old", role="user", created=1,
        parts=[{"type": "text", "text": "six months stale"}],
    )
    _opencode_db(storage, "ses_other", [("m1", "user", 1, [{"type": "text", "text": "x"}])])
    assert recent_records(
        "opencode", sid, "/x", 10, opencode_storage_dir=storage
    ) == []



def _adopted_codex_registry(tmp_path, monkeypatch, session_id, *, with_rollout=True):
    """An adopted codex orphan: empty short_id, optional rollout on disk."""
    from fno import paths
    from fno.agents.registry import AgentEntry, write_registry

    if with_rollout:
        day = tmp_path / "sessions" / "2026" / "08" / "27"
        day.mkdir(parents=True, exist_ok=True)
        (day / f"rollout-2026-08-27T02-34-28-{session_id}.jsonl").write_text(
            "\n".join(
                [
                    json.dumps(
                        {"type": "session_meta", "payload": {"id": session_id, "cwd": "/tmp/proj"}}
                    ),
                    json.dumps(
                        {
                            "type": "response_item",
                            "payload": {
                                "type": "message",
                                "role": "user",
                                "content": [{"type": "input_text", "text": "my own message"}],
                            },
                        }
                    ),
                ]
            )
            + "\n",
            encoding="utf-8",
        )
    registry_path = tmp_path / "registry.json"
    write_registry(
        [
            AgentEntry(
                name="01a04292",
                harness="codex",
                harness_session_id=session_id,
                status="orphaned",
                origin="adopted",
                cwd="/tmp/proj",
                log_path="",
                short_id="",
            )
        ],
        path=registry_path,
    )
    monkeypatch.setattr(paths, "agents_registry_path", lambda: registry_path)



def _quiet_claude_fleet(tmp_path, monkeypatch, session_id):
    """A row named for its agent holding a full uuid, plus its stale transcript.

    The x-f715 shape: the live listing is liveness-gated, so nothing here
    answers a live resolver, and the exact row match takes only the name
    ("lead-harness-pi") and the full session id - the 8-hex handle hits
    neither. The durable-store rung is the only reader left.
    """
    from fno import paths
    from fno.agents.registry import AgentEntry, write_registry

    proj = tmp_path / "projects" / "-tmp-proj"
    proj.mkdir(parents=True)
    t = proj / f"{session_id}.jsonl"
    t.write_text(
        json.dumps(
            {
                "type": "user",
                "message": {
                    "role": "user",
                    "content": [{"type": "text", "text": "quiet but present"}],
                },
                "timestamp": "2026-09-13T17:06:00Z",
            }
        )
        + "\n",
        encoding="utf-8",
    )
    old = time.time() - 7200
    os.utime(t, (old, old))

    registry_path = tmp_path / "registry.json"
    write_registry(
        [
            AgentEntry(
                name="lead-harness-pi",
                harness="claude",
                harness_session_id=session_id,
                status="orphaned",
                origin="adopted",
                cwd="/tmp/proj",
                log_path="",
                short_id="",
            )
        ],
        path=registry_path,
    )
    monkeypatch.setattr(paths, "agents_registry_path", lambda: registry_path)



# --------------------------------------------------------------------------
# peek_grep — rank 9: transcript search with a reported count
# --------------------------------------------------------------------------


def _grep_transcript(tmp_path, sess, needle):
    _claude_transcript(
        tmp_path,
        sess.cwd,
        sess.session_id,
        [
            _user(f"working {needle} today"),
            _assistant("unrelated reply"),
            _user(f"again {needle} and again"),
        ],
    )



# --------------------------------------------------------------------------
# the record reader's bounded tail (x-5f26)
# --------------------------------------------------------------------------


def _num(rec):
    return rec["n"]


def _numbered_jsonl(tmp_path, records):
    path = tmp_path / "t.jsonl"
    path.write_text(
        "\n".join(json.dumps(r, ensure_ascii=False) for r in records) + "\n",
        encoding="utf-8",
    )
    return path


def test_records_reader_tail_returns_the_records_a_full_pass_returns(
    tmp_path, monkeypatch
):
    # AC4-HP: a transcript larger than the tail window answers identically
    # while parsing only the window.
    from fno.agents import peek as peek_mod

    monkeypatch.setattr(peek_mod, "_TAIL_BYTES", 4096)
    path = _numbered_jsonl(tmp_path, [{"n": i, "pad": "x" * 64} for i in range(500)])
    assert path.stat().st_size > 4096

    seen: list = []

    def parse(rec):
        seen.append(rec["n"])
        return rec["n"]

    tail = peek_mod._records_from_jsonl(path, 40, parse)
    full = peek_mod._records_from_jsonl(path, 40, _num, tail=False)
    assert tail == full
    assert len(tail) == 40 and tail[-1] == 499
    assert len(seen) < 500, "the tail read must not parse every record"


def test_records_reader_tolerates_a_seek_inside_a_line_or_character(tmp_path, monkeypatch):
    # AC4-ERR: the seek lands mid-line and mid-multibyte-character; the
    # fragment parses as nothing and the answer is unchanged.
    from fno.agents import peek as peek_mod

    records = [{"n": i, "pad": "x" * 32} for i in range(50)]
    records.insert(25, {"n": -1, "pad": "é" * 40})
    path = _numbered_jsonl(tmp_path, records)
    raw = path.read_bytes()
    # Seek onto the SECOND byte of the two-byte é: a torn, undecodable fragment.
    landing = raw.index("é".encode()) + 1
    monkeypatch.setattr(peek_mod, "_TAIL_BYTES", len(raw) - landing)

    tail = peek_mod._records_from_jsonl(path, 40, _num)
    full = peek_mod._records_from_jsonl(path, 40, _num, tail=False)
    assert tail == full
    assert len(tail) == 40
