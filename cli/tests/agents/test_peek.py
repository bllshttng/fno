"""Tests for ``fno agents peek`` (x-05da) — the read-only twin of mail send.

Grouped by the plan's verify slices:
  peek_resolve       — US1: resolver parity + exit-13 miss (AC1-HP, AC1-ERR)
  peek_reader        — US2: recent_records claude/codex/unsupported (AC2-*)
  peek_tail_follow   — US3: --lines / --json / no-activity / follow (AC1-EDGE/FR/UI)
  peek_status_stream — US4: dual-envelope status fast-path (AC3-EDGE)
"""
from __future__ import annotations

import io
import json
import os
import time
from pathlib import Path

from fno.agents.peek import (
    _read_complete_lines,
    peek,
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


def test_peek_resolve_hit_prints_header_and_records(tmp_path):
    """AC1-HP: a resolvable peer shows a header + its recent records."""
    sess = _Session()
    _claude_transcript(tmp_path, sess.cwd, sess.session_id, [_user("hello"), _assistant("hi there")])
    out, err = io.StringIO(), io.StringIO()
    rc = peek(
        "worker-x",
        lines=5,
        stdout=out,
        stderr=err,
        resolve=lambda h: (sess, []),
        projects_root=tmp_path,
    )
    assert rc == 0
    body = out.getvalue()
    assert "peer worker-x: agent=claude short_id=abc12345" in body
    assert "user: hello" in body
    assert "assistant: hi there" in body


def test_peek_resolve_miss_exits_13_with_suggestions(tmp_path):
    """AC1-ERR: an unresolvable handle exits 13, parity with mail send."""
    out, err = io.StringIO(), io.StringIO()
    rc = peek(
        "nope",
        stdout=out,
        stderr=err,
        resolve=lambda h: (None, ["worker-a", "worker-b"]),
        projects_root=tmp_path,
    )
    assert rc == 13
    # The miss names the surface it read. A bare "peer not found" reported a
    # registry absence as a death and nearly cost a live session.
    assert "peer not found in the registry: nope" in err.getvalue()
    assert "fno agents adopt" in err.getvalue()
    assert "worker-a" in err.getvalue()


# --------------------------------------------------------------------------
# peek_mux — pane-substrate workers, the default substrate (x-680d)
# --------------------------------------------------------------------------


def test_peek_mux_pane_worker_read_when_live_resolver_misses(tmp_path):
    """A pane worker resolves via the registry mux ref and is read, even though
    the live-session resolver returns None (its content is a PTY, not a transcript)."""
    out, err = io.StringIO(), io.StringIO()
    rc = peek(
        "worker-x-5f43",
        lines=5,
        stdout=out,
        stderr=err,
        resolve=lambda h: (None, ["board-survey", "gate-b1c3"]),
        projects_root=tmp_path,
        mux_lookup=lambda h: ("mux-sess", 33, "worker-x-5f43"),
        mux_reader=lambda sess, pane, n: (0, "thinking...\nacting now\n"),
    )
    assert rc == 0
    text = out.getvalue()
    assert "mux pane=mux-sess:33" in text
    assert "acting now" in text
    # The misdirecting "did you mean" suggestions must NOT appear for a peer
    # peek can in fact observe.
    assert "did you mean" not in err.getvalue()


def test_peek_mux_pane_refusal_names_working_surface(tmp_path, monkeypatch):
    """When the mux cannot answer, the refusal names `fno mux pane read <id>`
    instead of listing unrelated peers (the 2026-08-04 false-liveness incident
    where an empty resolver result was read as proof three live workers were wedged)."""
    # Keep the refusal deterministic: an unclassifiable read (falsifier None)
    # is the only shape that refuses; a real gone pane falls through instead.
    monkeypatch.setattr(
        "fno.agents.reachability.pane_falsifier", lambda mux: None
    )
    out, err = io.StringIO(), io.StringIO()
    rc = peek(
        "boardsort-b63a",
        lines=5,
        stdout=out,
        stderr=err,
        resolve=lambda h: (None, ["board-survey", "gate-b1c3", "footnote-56"]),
        projects_root=tmp_path,
        mux_lookup=lambda h: ("mux-sess", 25, "boardsort-b63a"),
        mux_reader=lambda sess, pane, n: (1, ""),
    )
    assert rc == 1
    msg = err.getvalue()
    assert "fno mux pane read 25" in msg
    assert "boardsort-b63a" in msg
    # Unrelated peers must not be listed as "did you mean" for a pane worker.
    assert "board-survey" not in msg


def test_peek_gone_pane_names_the_exit_and_the_resume(tmp_path, monkeypatch):
    """A read failure the falsifier classifies as pane-gone is a DEAD pane:
    say so, name the resume command, and fall through to the registry row
    instead of claiming the mux did not answer."""
    monkeypatch.setattr(
        "fno.agents.reachability.pane_falsifier", lambda mux: "pane-gone"
    )
    out, err = io.StringIO(), io.StringIO()
    rc = peek(
        "lead-4d9b-delivery",
        stdout=out,
        stderr=err,
        resolve=lambda h: (None, ["someone-else"]),
        projects_root=tmp_path,
        mux_lookup=lambda h: ("main", 2277, "lead-4d9b-delivery"),
        mux_reader=lambda sess, pane, n: (1, ""),
    )
    msg = err.getvalue()
    assert "pane 2277 is gone" in msg
    assert "fno agents resume lead-4d9b-delivery" in msg
    assert "the mux did not answer" not in msg
    # The fall-through continues past the pane read, so the not-found shape
    # (no registry row in this test) is the expected exit here, not 1.
    assert rc == 13
    assert "peer not found in the registry: lead-4d9b-delivery" in msg


def test_peek_live_row_without_harness_session_id_reports_identity_defect(
    tmp_path, monkeypatch
):
    """AC3-ERR: an existing incomplete row is not misreported as a typo."""
    from fno import paths
    from fno.agents.registry import AgentEntry, write_registry

    registry_path = tmp_path / "registry.json"
    log_path = tmp_path / "incomplete-worker.log"
    log_path.touch()
    write_registry(
        [
            AgentEntry(
                name="incomplete-worker",
                harness="claude",
                harness_session_id=None,
                status="live",
                cwd=str(tmp_path),
                log_path=str(log_path),
                short_id="7c5dcf5d",
            )
        ],
        path=registry_path,
    )
    monkeypatch.setattr(paths, "agents_registry_path", lambda: registry_path)

    out, err = io.StringIO(), io.StringIO()
    rc = peek(
        "incomplete-worker",
        stdout=out,
        stderr=err,
        resolve=lambda h: (None, ["complete-worker"]),
        projects_root=tmp_path,
        mux_lookup=lambda h: None,
    )

    assert rc != 0
    message = err.getvalue()
    assert "registry row incomplete-worker exists" in message
    assert "harness_session_id" in message
    assert "status=live" in message
    assert "peer not found" not in message
    assert "did you mean" not in message


def test_lookup_mux_pane_finds_registry_row(tmp_path):
    """_lookup_mux_pane resolves a registry mux row directly, no live session needed."""
    from fno.agents.peek import _lookup_mux_pane
    from fno.agents.registry import AgentEntry, write_registry

    # A pane worker carries the mux ref and NO transport short_id (the mux ref
    # is its identity) - the one-live-ref invariant mux XOR worker XOR bg.
    entry = AgentEntry(
        name="worker-x-5f43",
        harness="claude",
        cwd=str(tmp_path),
        # x-7bcd: needs a resolvable handle to pass write_registry's guard;
        # log_path stands in for the fallback file mux_spawn.py now touches
        # for an id-less mux row.
        log_path=str(tmp_path / "worker-x-5f43.log"),
        mux={"session": "mux-sess", "pane_id": 33},
    )
    registry_path = tmp_path / ".fno" / "agents" / "registry.json"
    write_registry([entry], path=registry_path)

    assert _lookup_mux_pane("worker-x-5f43", registry_path=registry_path) == (
        "mux-sess",
        33,
        "worker-x-5f43",
    )
    # An unknown handle and a non-mux row both resolve to None.
    assert _lookup_mux_pane("nope", registry_path=registry_path) is None

    plain = AgentEntry(
        name="daemon-worker",
        harness="claude",
        harness_session_id="sess-cafe1234",  # x-7bcd: needs a resolvable handle
        cwd=str(tmp_path),
        log_path="",
        short_id="cafe1234",
    )
    write_registry([plain], path=registry_path)
    assert _lookup_mux_pane("daemon-worker", registry_path=registry_path) is None


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


def test_peek_unsupported_agent_exit_1(tmp_path):
    """AC2-ERR at the command layer: legible message, exit 1 (not 13)."""
    sess = _Session(agent="gemini")
    out, err = io.StringIO(), io.StringIO()
    rc = peek("g", stdout=out, stderr=err, resolve=lambda h: (sess, []), projects_root=tmp_path)
    assert rc == 1
    assert "observe not yet supported for gemini" in err.getvalue()


# --------------------------------------------------------------------------
# peek_tail_follow — US3
# --------------------------------------------------------------------------


def test_peek_no_activity_yet(tmp_path):
    """AC1-EDGE: resolved but no transcript file → 'no activity yet', exit 0."""
    sess = _Session()  # no transcript written
    out, err = io.StringIO(), io.StringIO()
    rc = peek("w", stdout=out, stderr=err, resolve=lambda h: (sess, []), projects_root=tmp_path)
    assert rc == 0
    assert "no activity yet" in out.getvalue()


def test_peek_json_emits_rows(tmp_path):
    """--json emits one JSON object per record (AC1-UI, machine-readable)."""
    sess = _Session()
    _claude_transcript(tmp_path, sess.cwd, sess.session_id, [_assistant("done")])
    out, err = io.StringIO(), io.StringIO()
    rc = peek("w", json_out=True, stdout=out, stderr=err, resolve=lambda h: (sess, []), projects_root=tmp_path)
    assert rc == 0
    rows = [json.loads(line) for line in out.getvalue().splitlines() if line.strip()]
    assert {"role": "assistant", "text": "done"} in rows


def test_peek_follow_exits_when_peer_not_live(tmp_path):
    """AC1-FR: --follow exits (no spin) once is_live() reports the peer gone."""
    sess = _Session()
    _claude_transcript(tmp_path, sess.cwd, sess.session_id, [_user("hi")])
    out, err = io.StringIO(), io.StringIO()
    rc = peek(
        "w",
        follow=True,
        stdout=out,
        stderr=err,
        resolve=lambda h: (sess, []),
        projects_root=tmp_path,
        is_live=lambda: False,
    )
    assert rc == 0
    assert "peer ended" in err.getvalue()


# --------------------------------------------------------------------------
# peek_status_stream — US4
# --------------------------------------------------------------------------


def test_follow_reads_only_complete_lines_no_partial_loss(tmp_path):
    """Concurrency (gemini HIGH): a record split across two writes is emitted
    whole on the next poll, not corrupted into two dropped fragments."""
    p = tmp_path / "t.jsonl"
    p.write_bytes(b'{"a":1}\n{"b":2')  # one complete line + a partial
    with p.open("rb") as fh:
        assert _read_complete_lines(fh) == [b'{"a":1}\n']
        pos = fh.tell()  # positioned at the start of the partial
    with p.open("ab") as fh:
        fh.write(b'}\n')  # writer completes the record
    with p.open("rb") as fh:
        fh.seek(pos)
        assert _read_complete_lines(fh) == [b'{"b":2}\n']


def test_peek_status_stream_dual_envelope(tmp_path):
    """AC3-EDGE: both {type,data} and {kind,..flat} status events render; a
    neither-shape record is skipped (fast-path stays present, not partial)."""
    sess = _Session(short_id="abc12345", session_id="sid-123")
    events = tmp_path / "events.jsonl"
    events.write_text(
        "\n".join(
            [
                json.dumps({"type": "task_started", "data": {"short_id": "abc12345"}}),
                json.dumps({"kind": "task_done", "short_id": "abc12345"}),
                json.dumps({"type": "unrelated_event", "data": {"short_id": "abc12345"}}),
            ]
        )
        + "\n",
        encoding="utf-8",
    )
    out, err = io.StringIO(), io.StringIO()
    rc = peek(
        "w",
        stdout=out,
        stderr=err,
        resolve=lambda h: (sess, []),
        projects_root=tmp_path,
        events_path=events,
    )
    assert rc == 0
    body = out.getvalue()
    assert "[task_started]" in body
    assert "[task_done]" in body
    assert "unrelated_event" not in body


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


def test_peek_reads_adopted_orphan_row_instead_of_lying(tmp_path, monkeypatch):
    """An adopted orphan is addressable: peek reads its transcript, never "not found".

    ``fno agents adopt`` mints a codex row with status="orphaned" and (by
    design, for a non-claude harness) an empty short_id. The live-session
    resolver skips it because it is not alive, so peek answered "peer not found
    in the registry" for a row it had already read, and pointed the caller back
    at adopt: a closed loop that left a 2,830-line conversation unreachable
    through the documented observe verb.
    """
    from fno import paths
    from fno.agents.registry import AgentEntry, write_registry

    session_id = "01a04292-22f0-7501-9967-b96c053b01f2"
    day = tmp_path / "sessions" / "2026" / "08" / "27"
    day.mkdir(parents=True)
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
                            "content": [{"type": "input_text", "text": "orphaned ping"}],
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

    # Addressable by the row name AND by the full session id adopt echoes back.
    for handle in ("01a04292", session_id):
        out, err = io.StringIO(), io.StringIO()
        rc = peek(
            handle,
            stdout=out,
            stderr=err,
            resolve=lambda h: (None, ["someone-else"]),
            projects_root=tmp_path,
            codex_sessions_dir=tmp_path / "sessions",
            mux_lookup=lambda h: None,
        )
        # Positive marker: the conversation itself, never merely a zero exit.
        assert rc == 0, err.getvalue()
        assert "orphaned ping" in out.getvalue()
        assert "peer not found" not in err.getvalue()
        assert "fno agents adopt" not in err.getvalue()


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


def test_peek_empty_short_id_never_matches_other_workers_events(tmp_path, monkeypatch):
    """An empty short_id must not turn into the catch-all token ``worker:``.

    ``wants`` interpolated ``f"worker:{short_id}"`` before filtering falsy
    entries, so an empty short_id (every non-claude adopted orphan) produced the
    bare literal "worker:", which the substring arm then matched against every
    ``worker:<name>`` event source. peek rendered OTHER workers' status lines as
    this session's, on the fast path, before its own transcript was read.
    """
    session_id = "01a04292-22f0-7501-9967-b96c053b01f2"
    _adopted_codex_registry(tmp_path, monkeypatch, session_id)
    events = tmp_path / "events.jsonl"
    events.write_text(
        "\n".join(
            [
                json.dumps({"kind": "task_started", "source": "worker:someone-else"}),
                json.dumps({"kind": "task_done", "source": "worker:another-worker"}),
            ]
        )
        + "\n",
        encoding="utf-8",
    )

    out, err = io.StringIO(), io.StringIO()
    rc = peek(
        "01a04292",
        stdout=out,
        stderr=err,
        resolve=lambda h: (None, []),
        projects_root=tmp_path,
        codex_sessions_dir=tmp_path / "sessions",
        events_path=events,
        mux_lookup=lambda h: None,
    )
    assert rc == 0, err.getvalue()
    # Positive marker: THIS session's own transcript line is what was rendered.
    assert "my own message" in out.getvalue()
    assert "someone-else" not in out.getvalue()
    assert "another-worker" not in out.getvalue()


def test_peek_row_without_a_transcript_is_not_reported_idle(tmp_path, monkeypatch):
    """A dead row whose store is gone must not answer "no activity yet".

    The row resolves, so "peer not found" is wrong; the session ended, so idle
    is wrong too. The module contract forbids a blank exit-0 a caller could read
    as idle, which is exactly what an empty record list produced here.
    """
    session_id = "01a04292-22f0-7501-9967-b96c053b01f2"
    _adopted_codex_registry(tmp_path, monkeypatch, session_id, with_rollout=False)

    out, err = io.StringIO(), io.StringIO()
    rc = peek(
        "01a04292",
        stdout=out,
        stderr=err,
        resolve=lambda h: (None, []),
        projects_root=tmp_path,
        codex_sessions_dir=tmp_path / "sessions",
        mux_lookup=lambda h: None,
    )
    assert rc == 13
    assert "no activity yet" not in out.getvalue()
    message = err.getvalue()
    assert "registry row 01a04292 exists" in message
    assert "no codex transcript resolved" in message


def test_peek_follow_on_a_recovered_row_returns_instead_of_tailing(tmp_path, monkeypatch):
    """--follow must not block forever on a session that cannot write again.

    The follow loop exits only on rotation, a false ``is_live``, or Ctrl-C, and
    ``cmd_peek`` passes no ``is_live``. On a recovered row all three are
    unreachable, so the call hung after printing the tail.
    """
    session_id = "01a04292-22f0-7501-9967-b96c053b01f2"
    _adopted_codex_registry(tmp_path, monkeypatch, session_id)

    out, err = io.StringIO(), io.StringIO()
    rc = peek(
        "01a04292",
        follow=True,
        stdout=out,
        stderr=err,
        resolve=lambda h: (None, []),
        projects_root=tmp_path,
        codex_sessions_dir=tmp_path / "sessions",
        mux_lookup=lambda h: None,
    )
    assert rc == 0
    assert "my own message" in out.getvalue()
    message = err.getvalue()
    assert "no writer to tail" in message
    # Says what was measured, never that the session is dead: `row_derived`
    # only means the live-session resolver missed, and liveness is `truth`'s.
    assert "not a live session" not in message


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


def test_peek_resolves_a_quiet_session_by_short_session_id(tmp_path, monkeypatch):
    """A session the roster, truth and mail all resolve must not read as gone.

    x-f715: peek refused a short-id handle with "peer not found in the
    registry" while three other readers resolved the same session. The refusal
    named an absence the roster contradicted, and the role rerouted a launch
    order on the strength of it.
    """
    session_id = "647b3a9c-6544-43fe-899e-704382f3d973"
    _quiet_claude_fleet(tmp_path, monkeypatch, session_id)

    out, err = io.StringIO(), io.StringIO()
    rc = peek(
        session_id[:8],
        stdout=out,
        stderr=err,
        resolve=lambda h: (None, []),
        projects_root=tmp_path / "projects",
        mux_lookup=lambda h: None,
    )
    assert rc == 0, err.getvalue()
    assert "quiet but present" in out.getvalue()
    assert "peer not found" not in err.getvalue()
    assert "fno agents adopt" not in err.getvalue()


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


def test_grep_returns_only_matching_records(tmp_path):
    sess = _Session()
    _grep_transcript(tmp_path, sess, "x-450f")
    out, err = io.StringIO(), io.StringIO()
    rc = peek(
        "worker-x",
        lines=10,
        grep="x-450f",
        stdout=out,
        stderr=err,
        resolve=lambda h: (sess, []),
        projects_root=tmp_path,
    )
    assert rc == 0
    body = out.getvalue()
    assert "x-450f" in body
    assert "unrelated reply" not in body
    # The positive control: the count names what was searched, on stderr,
    # keeping stdout parseable.
    assert "2 matching record(s)" in err.getvalue()


def test_peek_all_spans_sessions_and_reports_both(tmp_path, monkeypatch):
    """--all reads every registry session with a transcript; the summary names
    sessions READ, so a zero-hit corpus never reads as an unsearched one."""
    from types import SimpleNamespace

    from fno.agents import registry as registry_mod

    rows = [
        SimpleNamespace(
            harness="claude",
            harness_session_id="sid-aaa",
            cwd="/tmp/proj-a",
        ),
        SimpleNamespace(
            harness="codex",
            harness_session_id="sid-bbb",
            cwd="/tmp/proj-b",
        ),
        SimpleNamespace(harness="claude", harness_session_id=None, cwd="/tmp"),
    ]
    monkeypatch.setattr(registry_mod, "load_registry", lambda p=None: rows)
    # claude store for sid-aaa; codex needs a rollout path, so give the second
    # row a claude transcript too by reusing the claude harness shape.
    _claude_transcript(tmp_path, "/tmp/proj-a", "sid-aaa", [_user("hit x-450f here")])
    rows[1].harness = "claude"
    rows[1].cwd = "/tmp/proj-b"
    _claude_transcript(tmp_path, "/tmp/proj-b", "sid-bbb", [_assistant("nothing here")])
    out, err = io.StringIO(), io.StringIO()
    rc = peek(
        "",
        lines=10,
        all_sessions=True,
        grep="x-450f",
        stdout=out,
        stderr=err,
        projects_root=tmp_path,
    )
    assert rc == 0
    assert "sid-aaa" in out.getvalue()
    assert "sid-bbb" not in out.getvalue(), "a session with no hits emits no block"
    assert "1 matching record(s) across 2 session(s)" in err.getvalue()


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
