"""Tests for the status-sink fanout.

Layer 2 of the status-breakpoints protocol: a dumb, config-driven dispatcher
that sweeps ``.fno/events.jsonl`` and routes x-dbaf protocol-family events to
external sinks. This file covers all ACs across the six user stories; the
``-k`` filters in the plan's per-task verify lines select the relevant subset.
"""
from __future__ import annotations
from tests.fixtures.graph_seed import seed_graph

import os
import subprocess
import threading

import pytest

from fno.config import ConfigBlock, StatusFanoutConfig, StatusSinkConfig
from fno.graph.store import read_graph_strict


# ── shared fixtures/helpers for tick tests ──────────────────────────────────


def _sinks_dir(root):
    """The status-sinks dir exactly as production resolves it (x-spaces)."""
    from fno.paths import project_log

    return project_log("events.jsonl", project_root=root).parent / "status-sinks"


def _write_events(root, lines: list[dict]) -> None:
    import json as _json

    from fno.paths import project_log

    journal = project_log("events.jsonl", project_root=root)
    journal.parent.mkdir(parents=True, exist_ok=True)
    with journal.open("w", encoding="utf-8") as fh:
        for obj in lines:
            fh.write(_json.dumps(obj) + "\n")


def _ev(ts: str, type_: str, **envelope) -> dict:
    e = {"ts": ts, "v": 1, "type": type_, "source": "target", "data": {}}
    e.update(envelope)
    return e


class _Recorder:
    """A recording dispatch_fn; classifies by a per-name script or delivers."""

    def __init__(self, script=None):
        self.calls: list[tuple[str, str]] = []  # (sink_name, event_ts)
        self._script = script or {}

    def __call__(self, sink, event):
        self.calls.append((sink.name, event["ts"]))
        from fno import status_fanout as sf

        return self._script.get(sink.name, (sf.DELIVERED, ""))


def _seed_cursor(ss, name, ts, n=0):
    import json as _json
    (ss / f"{name}.cursor").write_text(_json.dumps({"ts": ts, "n": n}))


def _cursor(ss, name):
    import json as _json
    return _json.loads((ss / f"{name}.cursor").read_text())


def _utcnow():
    from datetime import datetime, timezone

    return datetime.now(timezone.utc)


def _ms(dt) -> int:
    return int(dt.timestamp() * 1000)


def _capture_read(monkeypatch, rows=None, high=None):
    """Replace the native read fold with a capture; returns the queries list."""
    import fno.events.store_client as sc

    queries: list[dict] = []

    def fake(active, mode, query):
        queries.append(dict(query))
        return (rows if rows is not None else []), 0, high

    monkeypatch.setattr(sc, "read_projection", fake)
    return queries


# ── US1: config model ───────────────────────────────────────────────────────


def test_config_duplicate_sink_name_rejected() -> None:
    with pytest.raises(ValueError, match="[Dd]uplicate"):
        ConfigBlock(
            status_sinks=[
                {"name": "dup", "type": "backlog-progress"},
                {"name": "dup", "type": "backlog-progress"},
            ]
        )


def test_config_bad_match_key_rejected_names_allowed_keys() -> None:
    # AC1-UI: a match key outside the envelope whitelist fails validation with a
    # message naming the allowed keys.
    with pytest.raises(ValueError) as ei:
        StatusSinkConfig(
            name="s",
            type="backlog-progress",
            match={"projct": "fno"},  # typo
        )
    msg = str(ei.value)
    assert "projct" in msg
    assert "project" in msg  # the allowed-keys list is surfaced


def test_config_webhook_requires_exactly_one_of_url_url_env_neither() -> None:
    with pytest.raises(ValueError, match="url"):
        StatusSinkConfig(name="s", type="json-webhook")


def test_config_unknown_type_rejected() -> None:
    with pytest.raises(ValueError, match="type"):
        StatusSinkConfig(name="s", type="carrier-pigeon", url="https://x")


# ── US2: tick core (cursor / rotation / isolation / dry-run) ─────────────────


def _text_sink(name="s", events=("blocked",), **kw):
    return StatusSinkConfig(
        name=name, type="text-webhook", events=list(events),
        url="https://x", template="{data.reason}", **kw,
    )


def test_tick_store_backed_bootstrap_starts_at_wall_clock_floor(tmp_path, monkeypatch):
    # A store answers a read with every retained generation, so a cursor-less
    # bootstrap tick with the file-EOF floor ("", 0) rescanned the whole store.
    # The floor must be now, and the window bounded at SQL level.
    from fno import status_fanout as sf

    from fno.paths import project_log

    ss = _sinks_dir(tmp_path)
    ss.mkdir(parents=True)
    journal = project_log("events.jsonl", project_root=tmp_path)
    journal.parent.mkdir(parents=True, exist_ok=True)
    (journal.parent / "events.db").write_bytes(b"")  # store-backed discriminator
    _write_events(tmp_path, [_ev("2026-07-12T00:00:05Z", "blocked")])
    before = _utcnow()
    queries = _capture_read(monkeypatch, high=42)

    sink = StatusSinkConfig(name="s", type="json-webhook", events=["blocked"],
                            url="https://x")
    res = sf.run_tick(tmp_path, [sink])

    assert res.rows_read == 0
    q = queries[0]
    assert q["after_seq"] is None  # no scan cursor yet
    assert q["since_ts"] != ""  # a real floor, not "from the epoch"
    assert _ms(before) - 60_000 <= q["since_ms"] <= _ms(_utcnow()) + 1_000
    assert _cursor(ss, "s")["n"] == 0
    assert _cursor(ss, ".scan") == {"ts": "", "n": 42}  # high-water seq seeded


def test_tick_nanosecond_fractions_key_monotonically(tmp_path, monkeypatch):
    # Rust emitters stamp nanosecond fractions, and one row the shared parser
    # could not key crashed the tick before any cursor persisted. The pass
    # must complete over such rows.
    from fno import status_fanout as sf

    ss = _sinks_dir(tmp_path)
    ss.mkdir(parents=True)
    _capture_read(
        monkeypatch,
        rows=[
            _ev("2026-10-10T03:39:48.693592000Z", "blocked", node="a"),
            _ev("2026-10-10T03:39:48.693592500Z", "blocked", node="b"),
        ],
        high=7,
    )
    _seed_cursor(ss, "s", "2026-10-10T03:39:00Z")
    rec = _Recorder()
    res = sf.run_tick(tmp_path, [_text_sink()], dispatch_fn=rec)
    assert res.rows_read == 2
    assert [ts for _, ts in rec.calls] == [
        "2026-10-10T03:39:48.693592000Z", "2026-10-10T03:39:48.693592500Z"]
    assert _cursor(ss, "s")["n"] == 0  # advanced past the window, count zeroed


def test_tick_fresh_cursor_starts_at_eof_no_backfill(tmp_path):
    from fno import status_fanout as sf

    _write_events(tmp_path, [
        _ev("2026-07-12T00:00:01Z", "blocked", **{"node": "x-1"}),
        _ev("2026-07-12T00:00:02Z", "blocked", **{"node": "x-2"}),
    ])
    rec = _Recorder()
    res = sf.run_tick(tmp_path, [_text_sink()], dispatch_fn=rec)
    # Fresh sink initializes at EOF -> nothing historical replayed.
    assert rec.calls == []
    assert res.sinks[0].dispatched == 0
    # Cursor persisted at EOF (ts, count-at-ts) so the next tick has a floor and
    # a later same-second event is still delivered (idx >= n).
    import json as _json
    cur = _json.loads(
        (_sinks_dir(tmp_path) / "s.cursor").read_text())
    assert cur == {"ts": "2026-07-12T00:00:02Z", "n": 1}


def test_tick_match_filter_and_events_filter(tmp_path):
    from fno import status_fanout as sf

    ss = _sinks_dir(tmp_path)
    ss.mkdir(parents=True)
    _seed_cursor(ss, "s", "2026-07-12T00:00:00Z")
    _write_events(tmp_path, [
        _ev("2026-07-12T00:00:01Z", "task_started", **{"project": "fno"}),   # wrong kind
        _ev("2026-07-12T00:00:02Z", "blocked", **{"project": "other"}),      # wrong match
        _ev("2026-07-12T00:00:03Z", "blocked", **{"project": "fno"}),        # hit
    ])
    sink = _text_sink(events=("blocked",), match={"project": "fno"})
    rec = _Recorder()
    res = sf.run_tick(tmp_path, [sink], dispatch_fn=rec)
    assert [ts for _, ts in rec.calls] == ["2026-07-12T00:00:03Z"]
    assert res.sinks[0].matched == 1


def test_tick_dry_run_matches_but_sends_nothing(tmp_path):
    from fno import status_fanout as sf

    ss = _sinks_dir(tmp_path)
    ss.mkdir(parents=True)
    _seed_cursor(ss, "s", "2026-07-12T00:00:00Z")
    _write_events(tmp_path, [_ev("2026-07-12T00:00:05Z", "blocked")])
    rec = _Recorder()
    res = sf.run_tick(tmp_path, [_text_sink()], dry_run=True, dispatch_fn=rec)
    assert rec.calls == []               # nothing sent
    assert res.sinks[0].matched == 1     # but counted
    assert _cursor(ss, "s")["ts"] == "2026-07-12T00:00:00Z"  # not advanced


def test_tick_malformed_line_skipped_and_counted(tmp_path):
    from fno import status_fanout as sf
    from fno.paths import project_log

    ss = _sinks_dir(tmp_path)
    ss.mkdir(parents=True, exist_ok=True)
    _seed_cursor(ss, "s", "2026-07-12T00:00:00Z")
    journal = project_log("events.jsonl", project_root=tmp_path)
    journal.parent.mkdir(parents=True, exist_ok=True)
    with journal.open("w") as fh:
        fh.write("not json at all\n")
        import json as _json
        fh.write(_json.dumps(_ev("2026-07-12T00:00:05Z", "blocked")) + "\n")
    rec = _Recorder()
    res = sf.run_tick(tmp_path, [_text_sink()], dispatch_fn=rec)
    assert res.skipped_lines == 1
    assert res.sinks[0].dispatched == 1  # tick did not abort


def test_tick_store_backed_reads_only_rows_past_scan_seq(tmp_path):
    # The daemon ticks every few seconds over a store of a million rows: an
    # unchanged store must cost a tick zero rows, a new row must still land
    # even when it shares the last delivered row's ts, and a fresh sink must
    # not replay store history the journal never held.
    from fno import status_fanout as sf
    from fno.events.store_client import emit_envelope
    from fno.paths import project_log

    journal = project_log("events.jsonl", project_root=tmp_path)
    journal.parent.mkdir(parents=True, exist_ok=True)
    ss = _sinks_dir(tmp_path)
    ss.mkdir(parents=True)
    _seed_cursor(ss, "s", "2026-07-12T00:00:00Z")
    sinks = [_text_sink(), _text_sink(name="f")]
    emit_envelope(_ev("2026-07-12T00:00:05Z", "blocked", run="r"), journal)
    rec = _Recorder()

    first = sf.run_tick(tmp_path, sinks, dispatch_fn=rec)
    assert first.rows_read == 1
    assert rec.calls == [("s", "2026-07-12T00:00:05Z")]

    idle = sf.run_tick(tmp_path, sinks, dispatch_fn=rec)
    assert idle.rows_read == 0
    assert len(rec.calls) == 1

    emit_envelope(_ev("2026-07-12T00:00:05Z", "blocked", run="r", node="b"), journal)
    later = sf.run_tick(tmp_path, sinks, dispatch_fn=rec)
    assert later.rows_read == 1
    # s delivered the first row, so its zeroed cursor admits the same-ts peer.
    # f fresh-initialized at wall-clock EOF (the store-backed bootstrap floor),
    # and this peer's ts predates that floor: history to f, not delivery.
    assert rec.calls[1:] == [("s", "2026-07-12T00:00:05Z")]


def test_tick_per_sink_isolation_one_raises(tmp_path):
    from fno import status_fanout as sf

    ss = _sinks_dir(tmp_path)
    ss.mkdir(parents=True)
    _seed_cursor(ss, "bad", "2026-07-12T00:00:00Z")
    _seed_cursor(ss, "good", "2026-07-12T00:00:00Z")
    _write_events(tmp_path, [_ev("2026-07-12T00:00:05Z", "blocked")])

    calls = []

    def dispatch(sink, event):
        calls.append(sink.name)
        if sink.name == "bad":
            raise RuntimeError("boom")
        return sf.DELIVERED, ""

    bad = _text_sink(name="bad")
    good = _text_sink(name="good")
    res = sf.run_tick(tmp_path, [bad, good], dispatch_fn=dispatch)
    names = {sr.name: sr for sr in res.sinks}
    assert names["good"].dispatched == 1          # good sink unaffected
    assert names["bad"].dropped == 1              # bad sink's raise counted as drop
    # bad's error logged.
    assert (ss / "bad.errors.jsonl").exists()


def test_shared_journal_uses_one_fanout_lock_and_cursor(tmp_path):
    import json as _json

    from fno import status_fanout as sf

    canonical = tmp_path / "canonical"
    worktree = tmp_path / "worktree"
    _write_events(canonical, [])
    # One repo, one journal: link the worktree through git so both roots
    # resolve the same space (the old symlink trick retired with .fno state).
    subprocess.run(
        ["git", "init", "-q", str(canonical)],
        check=True, capture_output=True,
        env={**os.environ, "GIT_CONFIG_GLOBAL": "/dev/null", "GIT_CONFIG_SYSTEM": "/dev/null"},
    )
    subprocess.run(
        ["git", "-C", str(canonical), "worktree", "add", "-q", str(worktree)],
        check=True, capture_output=True,
        env={**os.environ, "GIT_CONFIG_GLOBAL": "/dev/null", "GIT_CONFIG_SYSTEM": "/dev/null"},
    )
    sink = _text_sink()

    lock = sf._TickLock(canonical)
    assert lock.acquire()
    try:
        assert sf.run_tick(worktree, [sink], dispatch_fn=_Recorder()).locked_out is True
    finally:
        lock.release()

    sf.run_tick(canonical, [sink], dispatch_fn=_Recorder())
    from fno.paths import project_log

    journal = project_log("events.jsonl", project_root=canonical)
    journal.parent.mkdir(parents=True, exist_ok=True)
    with journal.open("a", encoding="utf-8") as handle:
        handle.write(_json.dumps(_ev("2026-07-12T00:00:05Z", "blocked")) + "\n")

    first = _Recorder()
    second = _Recorder()
    sf.run_tick(canonical, [sink], dispatch_fn=first)
    sf.run_tick(worktree, [sink], dispatch_fn=second)

    assert first.calls == [("s", "2026-07-12T00:00:05Z")]
    assert second.calls == []
    assert sf._cursor_path("s", canonical) == sf._cursor_path("s", worktree)


def test_tick_does_not_publish_cursor_after_lease_loss(tmp_path, monkeypatch):
    from fno import status_fanout as sf

    state_dir = _sinks_dir(tmp_path)
    state_dir.mkdir(parents=True)
    original_cursor = {"ts": "2026-08-11T08:00:00Z", "n": 0}
    _seed_cursor(state_dir, "s", original_cursor["ts"])
    _write_events(tmp_path, [_ev("2026-08-11T09:00:00Z", "blocked")])
    renewal_failed = threading.Event()

    def lose_lease(path, token):
        renewal_failed.set()
        return False

    def slow_dispatch(sink, event):
        assert renewal_failed.wait(1)
        return sf.DELIVERED, ""

    monkeypatch.setattr(sf, "_TICK_LEASE_RENEW_EVERY_S", 0.01)
    monkeypatch.setattr(sf, "renew_dir_mutex", lose_lease)

    result = sf.run_tick(tmp_path, [_text_sink()], dispatch_fn=slow_dispatch)

    assert result.lease_lost is True
    assert result.sinks[0].dispatched == 1
    assert _cursor(state_dir, "s") == original_cursor


# ── US3: json-webhook adapter (failure classes) ─────────────────────────────


def _json_sink(name="j", cloudevents=False, url="https://x", url_env=None):
    return StatusSinkConfig(
        name=name, type="json-webhook", events=["run_summary"],
        url=url, url_env=url_env, cloudevents=cloudevents,
    )


def test_json_webhook_delivers_verbatim(tmp_path, monkeypatch):
    from fno import status_fanout as sf

    posted = {}

    def fake_post(url, body, timeout):
        posted["url"] = url
        posted["body"] = body
        return sf._HttpResult(ok=True, status=200)

    monkeypatch.setattr(sf, "_post_json", fake_post)
    ev = _ev("2026-07-12T00:00:05Z", "run_summary", **{"node": "x-9", "outcome": "SUCCESS"})
    status, _ = sf._dispatch_json_webhook(_json_sink(), ev, StatusFanoutConfig())
    assert status == sf.DELIVERED
    assert posted["url"] == "https://x"
    assert posted["body"] == ev  # verbatim, unwrapped


def test_json_webhook_cloudevents_wrap(tmp_path, monkeypatch):
    from fno import status_fanout as sf

    posted = {}
    monkeypatch.setattr(sf, "_post_json",
                        lambda u, b, t: (posted.update(body=b) or sf._HttpResult(ok=True, status=200)))
    ev = _ev("2026-07-12T00:00:05Z", "run_summary", **{"run": "R1"})
    sf._dispatch_json_webhook(_json_sink(cloudevents=True), ev, StatusFanoutConfig())
    body = posted["body"]
    assert set(body) == {"id", "source", "type", "time", "data"}
    assert body["type"] == "run_summary" and body["time"] == "2026-07-12T00:00:05Z"
    assert body["data"] == ev
    # id = run:ts:type:<8-hex content hash> (x-25d0: distinct same-second events
    # get distinct ids; the prefix is stable, the suffix is a deterministic digest).
    assert body["id"].startswith("R1:2026-07-12T00:00:05Z:run_summary:")
    assert len(body["id"].rsplit(":", 1)[1]) == 8


def test_json_webhook_4xx_drops_immediately_no_retry(tmp_path, monkeypatch):
    from fno import status_fanout as sf

    calls = {"n": 0}

    def fake_post(url, body, timeout):
        calls["n"] += 1
        return sf._HttpResult(ok=False, status=404)

    monkeypatch.setattr(sf, "_post_json", fake_post)
    monkeypatch.setattr(sf, "_sleep", lambda s: None)
    status, detail = sf._dispatch_json_webhook(
        _json_sink(), _ev("2026-07-12T00:00:05Z", "run_summary"), StatusFanoutConfig(retries=2))
    assert status == sf.DROPPED
    assert calls["n"] == 1  # permanent: no retry
    assert "404" in detail


def test_json_webhook_connect_class_retries_then_short_circuits(tmp_path, monkeypatch):
    from fno import status_fanout as sf

    calls = {"n": 0}

    def fake_post(url, body, timeout):
        calls["n"] += 1
        return sf._HttpResult(ok=False, status=None)  # connect-class

    monkeypatch.setattr(sf, "_post_json", fake_post)
    monkeypatch.setattr(sf, "_sleep", lambda s: None)
    status, detail = sf._dispatch_json_webhook(
        _json_sink(), _ev("2026-07-12T00:00:05Z", "run_summary"), StatusFanoutConfig(retries=2))
    assert status == sf.SHORT_CIRCUIT
    assert calls["n"] == 3  # 1 + 2 retries
    assert "connect-class" in detail


def test_json_webhook_429_honors_retry_after(tmp_path, monkeypatch):
    from fno import status_fanout as sf

    slept = []
    seq = [sf._HttpResult(ok=False, status=429, retry_after=2.0),
           sf._HttpResult(ok=True, status=200)]
    monkeypatch.setattr(sf, "_post_json", lambda u, b, t: seq.pop(0))
    monkeypatch.setattr(sf, "_sleep", lambda s: slept.append(s))
    status, _ = sf._dispatch_json_webhook(
        _json_sink(), _ev("2026-07-12T00:00:05Z", "run_summary"), StatusFanoutConfig(retries=2))
    assert status == sf.DELIVERED
    assert slept == [2.0]  # honored Retry-After, not the backoff schedule


# ── US4: text-webhook adapter (custom Formatter, mentions guard) ─────────────


def test_render_missing_field_is_empty_not_crash():
    from fno import status_fanout as sf

    ev = _ev("t", "blocked")  # no 'from', no data.reason
    out = sf._render_template("[{from}] {data.reason}{node}", ev)
    assert out == "[] "  # every missing ref renders empty, no KeyError/AttributeError


def test_text_webhook_raw_body_posts_rendered_text_not_json(monkeypatch):
    """ntfy: with the topic in the URL the body IS the raw message - a JSON
    envelope lands on the phone as literal JSON (measured against ntfy 2.28.0:
    even {"topic", "message"} is shown verbatim unless posted to the root URL).
    raw_body=True posts the rendered template as plain text instead."""
    from fno import status_fanout as sf

    posted = {}
    monkeypatch.setattr(sf, "_post_raw",
                        lambda u, b, t: (posted.update(url=u, body=b) or sf._HttpResult(ok=True, status=200)))
    sink = StatusSinkConfig(name="n", type="text-webhook", events=["blocked"],
                            url="https://tailnet.ts.net:8443/fno-topic",
                            template="fno [{project}] blocked", raw_body=True)
    status, _ = sf._dispatch_text_webhook(sink, _ev("t", "blocked", project="t"), StatusFanoutConfig())
    assert status == sf.DELIVERED
    assert posted["body"] == "fno [t] blocked"  # a str, never a JSON envelope


# ── integration: run_tick through the real adapter router (mocked HTTP) ──────


def test_integration_tick_text_webhook_delivers_and_advances(tmp_path, monkeypatch):
    from fno import status_fanout as sf

    posts = []
    monkeypatch.setattr(sf, "_post_json",
                        lambda u, b, t: (posts.append(b) or sf._HttpResult(ok=True, status=200)))
    ss = _sinks_dir(tmp_path)
    ss.mkdir(parents=True)
    _seed_cursor(ss, "d", "2026-07-12T00:00:00Z")
    ev = _ev("2026-07-12T00:00:05Z", "blocked", **{"node": "x-9"})
    ev["data"] = {"reason": "why"}
    _write_events(tmp_path, [ev])
    sink = StatusSinkConfig(name="d", type="text-webhook", events=["blocked"],
                            url="https://x", template="{node}: {data.reason}", field="content")
    res = sf.run_tick(tmp_path, [sink])  # default dispatch_fn -> real router
    assert len(posts) == 1 and posts[0]["content"] == "x-9: why"
    assert res.sinks[0].dispatched == 1
    assert _cursor(ss, "d")["ts"] == "2026-07-12T00:00:05Z"


def test_integration_tick_connect_class_short_circuit_holds_batch(tmp_path, monkeypatch):
    # AC1-ERR end-to-end: 3 pending events, connect-class. First exhausts retries
    # and short-circuits; remaining 2 are unattempted; cursor does not advance.
    from fno import status_fanout as sf

    monkeypatch.setattr(sf, "_post_json", lambda u, b, t: sf._HttpResult(ok=False, status=None))
    monkeypatch.setattr(sf, "_sleep", lambda s: None)
    ss = _sinks_dir(tmp_path)
    ss.mkdir(parents=True)
    _seed_cursor(ss, "d", "2026-07-12T00:00:00Z")
    _write_events(tmp_path, [
        _ev("2026-07-12T00:00:01Z", "blocked"),
        _ev("2026-07-12T00:00:02Z", "blocked"),
        _ev("2026-07-12T00:00:03Z", "blocked"),
    ])
    sink = StatusSinkConfig(name="d", type="text-webhook", events=["blocked"],
                            url="https://x", template="hi", field="content")
    res = sf.run_tick(tmp_path, [sink], StatusFanoutConfig(retries=1))
    assert res.sinks[0].short_circuited is True
    assert res.sinks[0].dispatched == 0
    assert _cursor(ss, "d")["ts"] == "2026-07-12T00:00:00Z"  # held
    assert (ss / "d.errors.jsonl").exists()


# ── US5: fno backlog note + backlog-progress adapter ────────────────────────


@pytest.fixture
def tmp_graph(tmp_path, monkeypatch):
    g = tmp_path / "graph.json"
    seed_graph(g, '{"entries": []}\n')
    import fno.graph._constants as gc
    import fno.graph.store as gs
    for mod in (gc, gs):
        monkeypatch.setattr(mod, "GRAPH_JSON", g, raising=False)
    monkeypatch.setattr(gc, "GRAPH_MD", tmp_path / "graph.md", raising=False)
    monkeypatch.setattr(gc, "GRAPH_HTML", tmp_path / "graph.html", raising=False)
    return g


def _seed(graph_path, entry):
    import json as _json
    seed_graph(graph_path, _json.dumps({"entries": [entry]}) + "\n")


def test_backlog_note_cli_verb(tmp_graph, monkeypatch):
    from typer.testing import CliRunner
    from fno.cli import app
    from types import SimpleNamespace

    session_id = "019f48e1-5b09-72a0-9bc8-6b364bcf4ae4"
    monkeypatch.setattr(
        "fno.claims.self_identity.resolve_self_identity",
        lambda: SimpleNamespace(session_id=session_id, harness="codex"),
    )

    _seed(tmp_graph, {"id": "x-9", "title": "t"})
    res = CliRunner().invoke(app, ["backlog", "note", "x-9", "shipped wave 1", "-J", "-q"],
                             catch_exceptions=False)
    assert res.exit_code == 0
    import json as _json
    payload = _json.loads(res.stdout)
    assert payload["id"] == "x-9" and payload["text"] == "shipped wave 1"
    assert payload["routed"] == "thread"
    node = read_graph_strict(tmp_graph)[0]
    notes = node["progress_notes"]
    assert notes[-1]["text"] == "shipped wave 1"
    assert notes[-1]["source_session_id"] == session_id
    # A null harness exports as an absent key (the store strips nulls).
    assert notes[-1].get("source_harness") is None


def test_backlog_progress_adapter_task_done_stamps_node_and_plan(tmp_path, monkeypatch):
    from fno import status_fanout as sf

    calls = []
    plan_doc = tmp_path / "plan.md"
    plan_doc.write_text("---\ntitle: t\n---\n\n# Plan\n")
    monkeypatch.setattr(sf, "_machine_note",
                        lambda path, nid, kind, text: calls.append((nid, text)) or {"routed": "history", "node_id": nid})
    monkeypatch.setattr("fno.graph.api.node",
                        lambda nid: type("R", (), {"plan_path": str(plan_doc)})())
    ev = _ev("2026-07-12T00:00:05Z", "task_done", **{"node": "x-9", "outcome": "SUCCESS"})
    status, _ = sf._dispatch_backlog_progress(
        StatusSinkConfig(name="b", type="backlog-progress"), ev, tmp_path)
    assert status == sf.DELIVERED
    assert calls[0][0] == "x-9"
    assert "## Progress" in plan_doc.read_text()
    # Frontmatter untouched.
    assert plan_doc.read_text().startswith("---\ntitle: t\n---")


def test_plan_progress_creates_then_appends(tmp_path):
    from fno import status_fanout as sf

    p = tmp_path / "plan.md"
    p.write_text("---\ntitle: t\n---\n\n# Plan\n\nbody\n")
    sf._append_plan_progress(str(p), "T1 first", tmp_path)
    sf._append_plan_progress(str(p), "T2 second", tmp_path)
    text = p.read_text()
    assert text.count("## Progress") == 1        # heading created once
    assert "- T1 first" in text and "- T2 second" in text
    assert text.startswith("---\ntitle: t\n---")  # frontmatter intact


# ── US6: daemon host resolver + config emitter ──────────────────────────────


def test_fanout_resolver_includes_only_projects_with_enabled_sinks(monkeypatch):
    import fno.active_backlog as ab
    from fno.config import SettingsModel

    monkeypatch.setattr(ab, "_workspace_paths",
                        lambda: {"withsinks": "/w", "nosinks": "/n", "disabled": "/d"})

    def fake_load(root):
        r = str(root)
        if r == "/w":
            return SettingsModel(status_sinks=[{"name": "s", "type": "backlog-progress"}])
        if r == "/d":
            return SettingsModel(status_sinks=[
                {"name": "s", "type": "backlog-progress", "enabled": False}])
        return SettingsModel()  # /n: no sinks

    monkeypatch.setattr("fno.config.load_settings_for_repo", fake_load)
    targets = ab.resolve_fanout_targets()
    # Only the project with an ENABLED sink; active_backlog enablement is never
    # consulted (none is configured here, yet withsinks still ticks).
    assert [t.project for t in targets] == ["withsinks"]
    assert targets[0].cwd == "/w"
    assert targets[0].interval_seconds == 5  # default status_fanout.interval_secs


def test_config_status_sinks_json_command(monkeypatch):
    from typer.testing import CliRunner
    from fno.cli import app
    import fno.active_backlog as ab
    from fno.config import SettingsModel

    monkeypatch.setattr(ab, "_workspace_paths", lambda: {"p": "/p"})
    monkeypatch.setattr("fno.config.load_settings_for_repo",
                        lambda root: SettingsModel(
                            status_sinks=[{"name": "s", "type": "backlog-progress"}]))
    res = CliRunner().invoke(app, ["config", "status-sinks", "--json"], catch_exceptions=False)
    assert res.exit_code == 0
    import json as _json
    payload = _json.loads(res.stdout)
    assert payload == [{"project": "p", "cwd": "/p", "interval_seconds": 5}]


# ── review follow-ups: same-second tiebreak, tick_cmd surface, new validators ─


def test_tick_same_second_events_both_delivered(tmp_path):
    # Finding #1 (the critical one): two matched events sharing one second must
    # BOTH deliver. A bare-ts cursor dropped the second forever.
    from fno import status_fanout as sf

    ss = _sinks_dir(tmp_path)
    ss.mkdir(parents=True)
    _seed_cursor(ss, "s", "2026-07-12T00:00:00Z")
    _write_events(tmp_path, [
        _ev("2026-07-12T00:00:05Z", "blocked", **{"node": "a"}),
        _ev("2026-07-12T00:00:05Z", "blocked", **{"node": "b"}),  # SAME second
    ])
    rec = _Recorder()
    res = sf.run_tick(tmp_path, [_text_sink()], dispatch_fn=rec)
    assert [ts for _, ts in rec.calls] == [
        "2026-07-12T00:00:05Z", "2026-07-12T00:00:05Z"]
    assert res.sinks[0].dispatched == 2
    assert _cursor(ss, "s") == {"ts": "2026-07-12T00:00:05Z", "n": 2}


def test_tick_orders_mixed_precision_timestamps_by_instant(tmp_path):
    from fno import status_fanout as sf

    ss = _sinks_dir(tmp_path)
    ss.mkdir(parents=True)
    _seed_cursor(ss, "s", "2026-07-12T00:00:00Z")
    _write_events(tmp_path, [
        _ev("2026-07-12T00:00:00.100000Z", "blocked", node="later"),
        _ev("2026-07-12T00:00:00Z", "blocked", node="earlier"),
    ])
    rec = _Recorder()
    sf.run_tick(tmp_path, [_text_sink()], dispatch_fn=rec)
    assert [ts for _, ts in rec.calls] == [
        "2026-07-12T00:00:00Z",
        "2026-07-12T00:00:00.100000Z",
    ]

    _write_events(tmp_path, [
        _ev("2026-07-12T00:00:00.100000Z", "blocked", node="later"),
        _ev("2026-07-12T00:00:00Z", "blocked", node="earlier"),
        _ev("2026-07-12T00:00:00.200000Z", "blocked", node="latest"),
    ])
    rec = _Recorder()
    sf.run_tick(tmp_path, [_text_sink()], dispatch_fn=rec)
    assert rec.calls == [("s", "2026-07-12T00:00:00.200000Z")]


def test_tick_cmd_exits_0_and_logs_counts_on_drop(tmp_path, monkeypatch):
    # AC1-ERR (tick exits 0) + AC1-UI (real tick logs counts), at the CLI surface.
    from typer.testing import CliRunner
    from fno.cli import app
    from fno.config import SettingsModel
    from fno import paths as _paths
    from fno import status_fanout as sf

    ss = _sinks_dir(tmp_path)
    ss.mkdir(parents=True)
    _seed_cursor(ss, "d", "2026-07-12T00:00:00Z")
    _write_events(tmp_path, [_ev("2026-07-12T00:00:05Z", "blocked")])
    monkeypatch.setattr(_paths, "resolve_repo_root", lambda *a, **k: tmp_path)
    monkeypatch.setattr("fno.config.load_settings", lambda: SettingsModel(status_sinks=[
        {"name": "d", "type": "text-webhook", "events": ["blocked"],
         "url": "https://x", "template": "hi", "field": "content"}]))
    monkeypatch.setattr(sf, "_post_json", lambda u, b, t: sf._HttpResult(ok=False, status=404))
    monkeypatch.setattr(sf, "_sleep", lambda s: None)
    res = CliRunner().invoke(app, ["doctor", "event", "fanout", "tick"], catch_exceptions=False)
    assert res.exit_code == 0
    assert "matched=1" in res.stdout and "dropped=1" in res.stdout


def test_tick_unknown_dispatch_status_drops_not_shortcircuits(tmp_path):
    # Finding #5: a typo'd dispatcher return must not silently retry forever.
    from fno import status_fanout as sf

    ss = _sinks_dir(tmp_path)
    ss.mkdir(parents=True)
    _seed_cursor(ss, "s", "2026-07-12T00:00:00Z")
    _write_events(tmp_path, [
        _ev("2026-07-12T00:00:01Z", "blocked"),
        _ev("2026-07-12T00:00:02Z", "blocked"),
    ])
    rec = _Recorder(script={"s": ("typo-status", "oops")})
    res = sf.run_tick(tmp_path, [_text_sink()], dispatch_fn=rec)
    assert res.sinks[0].dropped == 2 and res.sinks[0].short_circuited is False
    assert _cursor(ss, "s")["ts"] == "2026-07-12T00:00:02Z"


def test_config_unsafe_sink_name_rejected():
    with pytest.raises(ValueError, match="name"):
        StatusSinkConfig(name="../evil", type="backlog-progress")
    with pytest.raises(ValueError, match="name"):
        StatusSinkConfig(name="a/b", type="backlog-progress")


def test_post_json_malformed_url_is_permanent_drop():
    # gemini HIGH: a URL with no scheme raises ValueError from urlopen (not
    # URLError/OSError); classify as a permanent 4xx so it drops, not retries.
    from fno import status_fanout as sf

    r = sf._post_json("not-a-url", {"x": 1}, 1.0)  # fails on URL parse, no network
    assert r.ok is False and r.status == 400


def test_tick_same_second_split_across_rotation_boundary(tmp_path):
    # codex peer P1: a single second split across .1 and the active file. Cursor is
    # (T, 2) - two T-events already processed (both in .1). The 2 active-file
    # T-events are occurrences 2 and 3 and MUST deliver. With the old `>=` guard,
    # .1 was skipped, the occurrence index reset to 0, and both were dropped forever.
    from fno import status_fanout as sf
    import json as _json
    from fno.paths import project_log

    ss = _sinks_dir(tmp_path)
    ss.mkdir(parents=True, exist_ok=True)
    T = "2026-07-12T00:00:05Z"
    _seed_cursor(ss, "s", T, n=2)
    with project_log("events.jsonl.1", project_root=tmp_path).open("w") as fh:
        fh.write(_json.dumps(_ev(T, "blocked", **{"node": "t1"})) + "\n")
        fh.write(_json.dumps(_ev(T, "blocked", **{"node": "t2"})) + "\n")
    with project_log("events.jsonl", project_root=tmp_path).open("w") as fh:
        fh.write(_json.dumps(_ev(T, "blocked", **{"node": "t3"})) + "\n")
        fh.write(_json.dumps(_ev(T, "blocked", **{"node": "t4"})) + "\n")
    rec = _Recorder()
    res = sf.run_tick(tmp_path, [_text_sink()], dispatch_fn=rec)
    assert res.sinks[0].dispatched == 2  # t3, t4 delivered, not lost
    assert _cursor(ss, "s") == {"ts": T, "n": 4}


# ── hardening: rotation-stable read, transient 4xx, broadcast defang, plan lock ─


def test_stream_since_discards_inflated_pass_no_same_ts_overcount(tmp_path, monkeypatch):
    # The loss path this fix closes: a racing rotation makes a pass double-count
    # same-ts events, inflating the occurrence index n and skipping real same-second
    # events next tick. The retry discards that pass and returns the consistent one.
    from fno import status_fanout as sf

    active = tmp_path / "events.jsonl"
    active.write_text("")
    T = "2026-07-12T00:00:05Z"
    inflated = ([_ev(T, "blocked"), _ev(T, "blocked"), _ev(T, "blocked")], 0, None)
    clean = ([_ev(T, "blocked")], 0, None)
    results = iter([inflated, clean])
    monkeypatch.setattr(sf, "_stream_pass", lambda a, s, q=None: next(results))
    seq = iter([1, 2, 3, 3])  # pass1 rotated, pass2 stable
    monkeypatch.setattr(sf, "_active_inode", lambda p: next(seq))
    events, _, _ = sf._stream_since(active, None)
    assert len(events) == 1  # the clean pass won; n not inflated


# 401/403/408 join the transient (retry -> short-circuit) class.


@pytest.mark.parametrize("code", [401])
def test_json_webhook_transient_4xx_holds_cursor_short_circuits(monkeypatch, code):
    from fno import status_fanout as sf

    calls = {"n": 0}
    monkeypatch.setattr(sf, "_post_json",
                        lambda u, b, t: (calls.__setitem__("n", calls["n"] + 1)
                                         or sf._HttpResult(ok=False, status=code)))
    monkeypatch.setattr(sf, "_sleep", lambda s: None)
    status, detail = sf._dispatch_json_webhook(
        _json_sink(), _ev("2026-07-12T00:00:05Z", "run_summary"), StatusFanoutConfig(retries=2))
    assert status == sf.SHORT_CIRCUIT   # transient, not an immediate drop
    assert calls["n"] == 3              # retried (1 + 2), like 429/connect-class
    assert str(code) in detail


# Slack broadcast defang on non-Discord fields.


def _capture_post(monkeypatch, sf, posted):
    monkeypatch.setattr(sf, "_post_json",
                        lambda u, b, t: (posted.update(body=b) or sf._HttpResult(ok=True, status=200)))


def test_text_webhook_slack_field_defangs_broadcast(monkeypatch):
    from fno import status_fanout as sf

    posted = {}
    _capture_post(monkeypatch, sf, posted)
    ev = _ev("t", "blocked")
    ev["data"] = {"reason": "<!channel> ship it"}
    sink = StatusSinkConfig(name="s", type="text-webhook", events=["blocked"],
                            url="https://slack", template="{data.reason}", field="text")
    sf._dispatch_text_webhook(sink, ev, StatusFanoutConfig())
    assert posted["body"]["text"] == "&lt;!channel> ship it"  # Slack renders &lt; as <
    assert "<!" not in posted["body"]["text"]                 # no live broadcast token


def test_text_webhook_discord_content_not_defanged(monkeypatch):
    from fno import status_fanout as sf

    posted = {}
    _capture_post(monkeypatch, sf, posted)
    ev = _ev("t", "blocked")
    ev["data"] = {"reason": "<!channel>"}
    sink = StatusSinkConfig(name="d", type="text-webhook", events=["blocked"],
                            url="https://discord", template="{data.reason}", field="content")
    sf._dispatch_text_webhook(sink, ev, StatusFanoutConfig())
    assert posted["body"]["content"] == "<!channel>"          # Discord path byte-for-byte
    assert posted["body"]["allowed_mentions"] == {"parse": []}  # its own guard intact


# Shared plan-doc lock serializes stamp vs progress append.


def test_plan_doc_lock_serializes_concurrent_stamp_and_append(tmp_path):
    import os as _os
    import threading
    from fno import status_fanout as sf
    from fno.plan.locking import plan_doc_lock

    plan = tmp_path / "plan.md"
    plan.write_text("---\ntitle: t\n---\n\n# Plan\n")
    barrier = threading.Barrier(2)

    def stamp_writer():
        barrier.wait()
        with plan_doc_lock(plan):
            content = plan.read_text()
            new = content.replace("title: t", "title: t\nstatus: shipped")
            tmp = plan.with_suffix(".md.tmp")
            tmp.write_text(new)
            _os.replace(tmp, plan)

    def append_writer():
        barrier.wait()
        sf._append_plan_progress(str(plan), "T1 progress", tmp_path)

    threads = [threading.Thread(target=stamp_writer), threading.Thread(target=append_writer)]
    for t in threads:
        t.start()
    for t in threads:
        t.join()

    final = plan.read_text()
    assert "status: shipped" in final   # stamp survived
    assert "- T1 progress" in final     # append survived (neither clobbered the other)


def test_append_plan_progress_preserves_file_mode(tmp_path):
    # Review finding (codex P2): the atomic tmp+replace must carry the plan's
    # original mode, else a private 0600 vault note is loosened to the umask
    # default. Reusing _stamp._atomic_write preserves it.
    import os as _os
    import stat as _stat
    from fno import status_fanout as sf

    p = tmp_path / "plan.md"
    p.write_text("# Plan\n")
    _os.chmod(p, 0o600)
    sf._append_plan_progress(str(p), "T1", tmp_path)
    assert _stat.S_IMODE(p.stat().st_mode) == 0o600  # not widened by the replace
    assert "- T1" in p.read_text()


def test_backlog_progress_delivered_even_when_plan_lock_times_out(tmp_path, monkeypatch):
    import contextlib
    from fno import status_fanout as sf

    plan_doc = tmp_path / "plan.md"
    plan_doc.write_text("# Plan\n")
    monkeypatch.setattr(sf, "_machine_note",
                        lambda path, nid, kind, text: {"routed": "history", "node_id": nid})

    @contextlib.contextmanager
    def timing_out(path, timeout=2.0):
        raise TimeoutError("busy")
        yield  # pragma: no cover

    monkeypatch.setattr(sf, "plan_doc_lock", timing_out)
    ev = _ev("2026-07-12T00:00:05Z", "task_done", **{"node": "x-9"})
    status, _ = sf._dispatch_backlog_progress(
        StatusSinkConfig(name="b", type="backlog-progress"), ev, tmp_path)
    assert status == sf.DELIVERED  # plan-doc append is best-effort; delivery unaffected


def test_post_json_sends_explicit_user_agent(monkeypatch):
    """Discord 403s the stdlib default `Python-urllib` UA, so `_post_json` must
    send an explicit User-Agent on every webhook POST (regression: a sink to a
    real Discord webhook short-circuited forever without it)."""
    from fno import status_fanout as sf

    captured = {}

    class _Resp:
        def __enter__(self):
            return self

        def __exit__(self, *a):
            return False

        def getcode(self):
            return 204

    def _fake_urlopen(req, timeout=None):
        captured["ua"] = req.get_header("User-agent")
        return _Resp()

    monkeypatch.setattr("urllib.request.urlopen", _fake_urlopen)
    result = sf._post_json("https://example.test/hook", {"content": "hi"}, 5.0)

    assert result.ok
    assert captured["ua"] == sf._USER_AGENT
    assert not captured["ua"].lower().startswith("python-urllib")


def test_resolve_url_falls_back_to_env_file(tmp_path, monkeypatch):
    """US1: a url_env sink with no process-env var resolves the secret from
    ~/.fno/.env (the daemon-invisible-secret fix, x-0128)."""
    from fno import status_fanout as sf

    env_file = tmp_path / ".env"
    env_file.write_text("FNO_STATUS_DISCORD=https://discord.example/hook\n")
    monkeypatch.delenv("FNO_STATUS_DISCORD", raising=False)
    monkeypatch.setattr(sf, "_secrets_env_path", lambda: env_file)

    sink = StatusSinkConfig(name="d", type="text-webhook", url_env="FNO_STATUS_DISCORD")
    url, err = sf._resolve_url(sink)
    assert url == "https://discord.example/hook"
    assert err is None


def test_resolve_url_missing_everywhere_short_circuits_secret_safe(tmp_path, monkeypatch):
    """US3: absent in both env and file -> short-circuit-worthy, the error names
    the var but never a value, and no crash on a missing file."""
    from fno import status_fanout as sf

    monkeypatch.delenv("FNO_STATUS_DISCORD", raising=False)
    monkeypatch.setattr(sf, "_secrets_env_path", lambda: tmp_path / "absent.env")

    sink = StatusSinkConfig(name="d", type="text-webhook", url_env="FNO_STATUS_DISCORD")
    url, err = sf._resolve_url(sink)
    assert url is None
    assert "FNO_STATUS_DISCORD" in err
    assert "discord.example" not in err  # never leaks a value
