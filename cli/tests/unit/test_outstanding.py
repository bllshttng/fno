"""Tests for `fno outstanding` - the operator-facing read of what is piled up.

Two legs, both over stores that already exist: unharvested carve-outs and open
operator questions. The command exists because the carve-out ledger reached 39 rows
over 29 days with nothing ever asking a human to look at it - `consume_carveouts`
was wired the whole time, but a wire nobody is told to pull is not a surface.

Every assertion here names a POSITIVE marker rather than an absence. An
absence has two explanations (the real outcome, or the instrument never ran)
and a test built on one cannot tell them apart; test_hook_block_positive_control
is the pair that closes that gap for the silent-on-zero case.
"""

from __future__ import annotations
from tests.fixtures.graph_seed import seed_graph

import json
import time
from datetime import datetime, timedelta, timezone
from pathlib import Path

import pytest

from fno.paths import project_log
from typer.testing import CliRunner

from fno.harness_identity import OwnedHarnessIdentity
from fno.lead.lane import LaneItem
from fno.outstanding.cli import outstanding_app
from fno.outstanding.core import Outstanding, Question, VerdictRow, render

# Same loaded-worker hazard as test_done.py: liveness forks a child per
# report, and the captures break under interleaving. One dedicated loadgroup
# worker keeps this module off the crowded workers.
pytestmark = [pytest.mark.xdist_group("done-audit-serial")]

runner = CliRunner()

requires_rust = pytest.mark.dev_build


def _write_carveouts(root: Path, rows: list[dict]) -> Path:
    ledger = root / ".fno" / "carveouts.jsonl"
    ledger.parent.mkdir(parents=True, exist_ok=True)
    ledger.write_text(
        "".join(json.dumps(r) + "\n" for r in rows), encoding="utf-8"
    )
    return ledger


def _carveout(cid: str, kind: str, ts: str = "2026-07-14T04:27:55Z") -> dict:
    return {
        "id": cid,
        "ts": ts,
        "session_id": "sess-a",
        "kind": kind,
        "priority": None,
        "need": None,
        "description": f"{cid} description",
        "truncated": False,
    }


@pytest.fixture()
def root(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Path:
    """A hermetic canonical root; both legs resolve through it."""
    monkeypatch.setenv("FNO_REPO_ROOT", str(tmp_path))
    monkeypatch.delenv("CLAUDECODE_SESSION_ID", raising=False)
    monkeypatch.delenv("FNO_AGENT_SELF", raising=False)
    monkeypatch.delenv("FNO_BG", raising=False)
    # Presence resolves through classify_presence, whose documented default is
    # "away" when no human signal is present - correct for a cron/script, and
    # what a bare pytest process looks like. Declare the attended case here so
    # each test states the presence it is exercising instead of inheriting it.
    monkeypatch.setenv("FNO_THINK_SPAWN_PRESENCE", "attended")
    monkeypatch.setattr(
        "fno.agents.self_stamp.resolve_self_identity",
        lambda: OwnedHarnessIdentity(None, None, (), "empty"),
    )

    # The provenance resolver is a native door now; its hermetic default here
    # is a resolved provenance that claims no authority, the shape a relayed
    # name or a sub-operator origin produces at a terminal. The
    # listing read answers an empty law set, so the ask gate stays inert
    # unless a test stubs its own rows.
    def _door(payload):
        if payload.get("mode") == "resolve-provenance":
            return {
                "decided_by": "operator",
                "authority_source": None,
                "attested_by": "operator",
                "relayed_by": None,
                "origin": None,
            }
        assert payload.get("mode") == "decisions", payload
        return {"decisions": [], "damaged": 0}

    monkeypatch.setattr("fno.rust_binary.call_front_json", _door)
    # resolve_repo_root is @cache'd and is warmed with the REAL worktree root
    # before this fixture runs. Without the clear, session resolution reads the
    # live target-state.md instead of this sandbox, and the ownership tests
    # would pass on the real session id rather than the one they set.
    # The clear path projects decisions onto the subject node's graph entry,
    # resolving GRAPH_JSON through the module attribute. Pin it to a
    # nonexistent path so these tests never read or write the real machine
    # graph when a --node happens to resolve there.
    import fno.graph._constants as gc
    import fno.graph.store as gs

    monkeypatch.setattr(gc, "GRAPH_JSON", tmp_path / "graph.json")
    monkeypatch.setattr(gs, "GRAPH_JSON", tmp_path / "graph.json")
    # Sidecar/metadata seam readers resolve through paths.graph_json at call
    # time; pin the resolver too or the seam reads the real machine graph.
    monkeypatch.setattr("fno.paths.graph_json", lambda: tmp_path / "graph.json")
    monkeypatch.setattr(
        "fno.paths.questions_jsonl",
        lambda: tmp_path / "questions.jsonl",
        raising=False,
    )
    # Every test in this module that calls `collect()` without an explicit
    # `lane=` must never touch the real machine's ~/.fno/my-priorities.md -
    # pin it to a sandbox path that starts out missing (an empty lane).
    monkeypatch.setattr(
        "fno.paths.operator_lane",
        lambda: tmp_path / "my-priorities.md",
        raising=False,
    )
    # The verdict leg shells the fno-agents binary; pin it to a clean read so
    # these tests never touch the machine's real FAIL rows (tests that assert
    # the leg patch this seam themselves).
    monkeypatch.setattr(
        "fno.outstanding.core._read_open_verdicts",
        lambda: ([], None),
    )
    (tmp_path / ".fno").mkdir(parents=True, exist_ok=True)
    return tmp_path


# --- 2.1 both legs -----------------------------------------------------------


@requires_rust
def test_both_legs_report_a_count_each(root: Path):
    _write_carveouts(
        root,
        [
            _carveout("cv-1", "oos-bug"),
            _carveout("cv-2", "oos-bug"),
            _carveout("cv-3", "deferred"),
        ],
    )
    assert runner.invoke(outstanding_app, ["ask", "should the gate refuse?", "--ask", "finish the lane"]).exit_code == 0
    assert runner.invoke(outstanding_app, ["ask", "which base do we rebase on?", "--ask", "finish the lane"]).exit_code == 0

    result = runner.invoke(outstanding_app, [])
    assert result.exit_code == 0, result.output

    # The carve-out leg names the split, not just a total: only `deferred`
    # blocks a node close, so a bare total hides which rows have a gate.
    assert "3 carve-out" in result.output
    assert "2 oos-bug" in result.output
    assert "1 deferred" in result.output
    assert "2 open question" in result.output


@requires_rust
def test_a_plain_ask_is_refused_for_context_and_records_nothing(root: Path):
    """AC9-HP: the ask port refuses a context-free question. Exit 2, the
    refusal naming the missing parts, and no row in either store."""
    result = runner.invoke(outstanding_app, ["ask", "which lane?"])
    assert result.exit_code == 2, result.output
    assert "a question needs" in result.output
    assert "--question-file" in result.output
    assert "--ask" in result.output, "the refusal names the pin door"
    assert len(_question_rows(root)) == 0, "the refused ask recorded nothing"


def test_carveout_leg_names_the_command_that_clears_it(root: Path):
    _write_carveouts(root, [_carveout("cv-1", "oos-bug")])
    result = runner.invoke(outstanding_app, [])
    assert result.exit_code == 0
    # The whole fix for "nothing clears them": the ledger sat 29 days because
    # nobody was ever told what to run.
    assert "fno backlog retro sweep-carveouts" in result.output


# --- prove-it verdict leg (x-6d64) -------------------------------------------


def _open_fail_row() -> VerdictRow:
    return VerdictRow(
        node="x-70e1",
        report="/plans/a.md.artifacts/coverage-audit-20260908/REPORT.md",
        verdict="FAIL",
        claim="The retirement done probe rejects incomplete evidence and requires the outcome",
        status="done",
        mtime="2026-09-08T20:56:56Z",
    )


def test_an_open_fail_verdict_renders_with_node_claim_and_ruling_command(
    root: Path, monkeypatch: pytest.MonkeyPatch
):
    monkeypatch.setattr(
        "fno.outstanding.core._read_open_verdicts", lambda: ([_open_fail_row()], None)
    )
    result = runner.invoke(outstanding_app, [])
    assert result.exit_code == 0, result.output
    assert "1 prove-it FAIL verdict with no ruling." in result.output
    assert "x-70e1 (done):" in result.output
    assert "The retirement done probe rejects incomplete evidence" in result.output
    assert "/plans/a.md.artifacts/coverage-audit-20260908/REPORT.md" in result.output
    assert "fno inbox decide" in result.output


@requires_rust
def test_a_failed_verdict_read_names_itself_and_keeps_the_questions_leg(
    root: Path, monkeypatch: pytest.MonkeyPatch
):
    """AC2-ERR: the verdict reader failing must not blank the other legs."""
    monkeypatch.setattr(
        "fno.outstanding.core._read_open_verdicts", lambda: ([], "the binary refused")
    )
    assert runner.invoke(outstanding_app, ["ask", "should the gate refuse?", "--ask", "finish the lane"]).exit_code == 0
    result = runner.invoke(outstanding_app, [])
    assert result.exit_code == 0, result.output
    assert "prove-it verdicts could not be read (the binary refused)." in result.output
    assert "1 open question" in result.output


# --- 2.2 an unreadable ledger is a stated failure ----------------------------


def test_unreadable_ledger_is_a_stated_failure_not_silence(root: Path):
    ledger = root / ".fno" / "carveouts.jsonl"
    ledger.parent.mkdir(parents=True, exist_ok=True)
    # Present but undecodable. read_carveouts raises rather than returning []
    # for exactly this case; the new command must not re-introduce the masquerade.
    ledger.write_bytes(b"\xff\xfe not utf-8 at all\n")

    result = runner.invoke(outstanding_app, [])

    assert result.exit_code != 0
    # Positive marker in the FAILURE text, pinned to the thing measured.
    assert "cannot read" in result.output or "failed to read" in result.output
    # And never the success line: a failed read reported as "nothing
    # outstanding" is the exact absence-as-success trap.
    assert "nothing outstanding" not in result.output.lower()


# --- 2.3 ask then clear ------------------------------------------------------


@requires_rust
def test_ask_clear_round_trip_and_idempotence(root: Path):
    asked = runner.invoke(outstanding_app, ["ask", "do we widen the fold window?", "--ask", "finish the lane"])
    assert asked.exit_code == 0, asked.output
    qid = asked.stdout.strip().splitlines()[-1].strip()
    assert qid, "ask must print the new question id on stdout"

    listed = runner.invoke(outstanding_app, ["--json"])
    assert listed.exit_code == 0
    payload = json.loads(listed.stdout)
    assert [q["id"] for q in payload["questions"]] == [qid]

    cleared = runner.invoke(outstanding_app, ["clear", qid, "--answer", "yes, widen it"])
    assert cleared.exit_code == 0, cleared.output
    assert f"outstanding: closed {qid} (decision d-" in cleared.stdout
    assert "recorded)" in cleared.stdout

    after = json.loads(runner.invoke(outstanding_app, ["--json"]).stdout)
    assert after["questions"] == []

    # Idempotent clears name each id instead of returning an unlabeled count.
    again = runner.invoke(outstanding_app, ["clear", qid])
    assert again.exit_code == 0
    assert f"{qid} was already closed; nothing written" in again.stdout

    unknown = runner.invoke(outstanding_app, ["clear", "q-neverexisted"])
    assert unknown.exit_code == 4
    assert "q-neverexisted is not a question id this machine knows" in unknown.stdout


def _journal_last(events_path):
    """The newest committed row for one journal."""
    return _journal_events(events_path)[-1]


def _journal_events(events_path):
    """Committed rows for one journal (store rows, raw fallback)."""
    from tests._event_rows import event_rows

    return event_rows(events_path)


@requires_rust
def test_asker_ask_field_options_and_blocks_are_recorded(
    root: Path, monkeypatch: pytest.MonkeyPatch
):
    monkeypatch.setattr("fno.outstanding.cli._session_id", lambda: "ledger-run-id")
    # The asker handle is stamped on a durable question event, so it resolves
    # through the owned-identity seam rather than raw precedence.
    monkeypatch.setattr(
        "fno.claims.self_identity.resolve_self_identity",
        lambda *a, **k: OwnedHarnessIdentity(
            "01234567-full-session", "codex", (), "single"
        ),
    )

    asked = runner.invoke(
        outstanding_app,
        [
            "ask",
            "which implementation should land?",
            "--ask",
            "pick one",
            "--node",
            "x-one",
            "--blocks",
            "x-one",
            "--blocks",
            "x-two",
        ],
    )

    assert asked.exit_code == 0, asked.output
    event = _journal_events(project_log("events.jsonl", project_root=root))[-1]
    assert event["data"]["asker"] == "01234567"
    assert event["data"]["ask"] == "pick one"
    assert event["data"]["blocks"] == ["x-one", "x-two"]
    assert event["data"]["session_id"] == "ledger-run-id"
    assert "live" not in event["data"], "liveness is computed, never stored"

    monkeypatch.setattr(
        "fno.agents.discover.resolve_reachable",
        lambda asker: (object(), []) if asker == "01234567" else (None, []),
    )
    question = json.loads(runner.invoke(outstanding_app, ["--json"]).stdout)["questions"][0]
    assert question == {
        "id": event["data"]["question_id"],
        "ts": event["ts"],
        "question": "which implementation should land?",
        "session_id": "ledger-run-id",
        "cwd": str(Path.cwd()),
        "node": "x-one",
        "asker": "01234567",
        "ask": "pick one",
        "options": [],
        "blocks": ["x-one", "x-two"],
        "subject": None,
        "live": True,
        "rank": 1,
    }


# --- the ask gate: live law on the subject refuses the question ---------------


def _fake_law_rows(*a, **k):
    """The decisions door's answer for one live law row."""
    return {
        "decisions": [
            {
                "decision_id": "d-0fa92eb9",
                "subject": "review-coverage",
                "lane": "law",
                "lifecycle": "live",
            }
        ],
        "damaged": 0,
    }


_PR1717_QUESTION = (
    "PR 1717 is stuck at the review cap: coverage reads uncovered and the "
    "attestation is stale. Approve, or set the override label?"
)


def _question_rows(root: Path) -> list[dict]:
    return [
        e
        for e in _journal_events(project_log("events.jsonl", project_root=root))
        if e["type"] == "operator_question"
    ]


class TestAskReceiptNamesVisibility:
    """The receipt must say whether the ask renders (x-0dc5): a bare
    `recorded q-...` shipped two fleet blockers into the invisible 98 percent.
    CliRunner merges streams here, so the stream guarantee is asserted as
    ORDER: the stdout qid is the final line of the invocation."""

    def _seed(self, root: Path, count: int, *, ts: str = "2026-08-19T00:00:00Z") -> None:
        _write_indexed_questions(
            root,
            [
                _indexed_question(
                    f"q-{i:02d}", ts, asker="gone", blocks=[]
                )
                for i in range(count)
            ],
        )

    @requires_rust
    def test_an_ask_below_the_window_says_so_and_names_its_position(self, root: Path):
        """AC9-ERR (x-0dc5): the honest branch - queued behind a full window,
        nothing will show it, and the receipt says exactly that."""
        # ts far in the future: every seeded row outranks the fresh ask, so it
        # sorts last regardless of the id the receipt mints.
        self._seed(root, 10, ts="2099-01-01T00:00:00Z")
        result = runner.invoke(outstanding_app, ["ask", "buried on arrival", "--ask", "finish the lane"])

        assert result.exit_code == 0, result.output
        rows = [row["data"]["question_id"] for row in _question_rows(root)]
        qid = next(q for q in rows if q not in {f"q-{i:02d}" for i in range(10)})
        assert f"{qid} does NOT render: position 11 of 11" in result.output
        assert "Nothing will show it to the operator" in result.output


class TestAskRefusedWhenLiveLawRules:
    # These four route the exact tier through the real binary (the law read
    # inside question-intake), so they need the compiled dev build, not just
    # the patched lifecycle read.
    @requires_rust
    def test_the_pr1717_question_exits_2_and_records_nothing(
        self, root: Path, monkeypatch: pytest.MonkeyPatch
    ):
        monkeypatch.setattr("fno.rust_binary.call_front_json", _fake_law_rows)
        # Positive control: a plain ask on a different subject records a row,
        # so the absence below is the gate's doing and not a broken journal.
        plain = runner.invoke(outstanding_app, ["ask", "which base do we rebase on?", "--ask", "finish the lane"])
        assert plain.exit_code == 0, plain.output
        assert len(_question_rows(root)) == 1

        refused = runner.invoke(outstanding_app, ["ask", _PR1717_QUESTION, "--ask", "finish the lane"])
        assert refused.exit_code == 2, refused.output
        assert "d-0fa92eb9" in refused.output
        assert "review-coverage" in refused.output
        assert len(_question_rows(root)) == 1, "the refused ask recorded nothing new"

    @requires_rust
    def test_a_failing_law_lookup_fails_open_and_records(
        self, root: Path, monkeypatch: pytest.MonkeyPatch
    ):
        def broken(*a, **k):
            raise RuntimeError("index unreadable")

        monkeypatch.setattr("fno.rust_binary.call_front_json", broken)
        allowed = runner.invoke(outstanding_app, ["ask", _PR1717_QUESTION, "--ask", "finish the lane"])
        assert allowed.exit_code == 0, allowed.output
        assert "live-law lookup failed" in allowed.output
        rows = _question_rows(root)
        assert len(rows) == 1, "a broken index must not eat the question"


def test_live_is_computed_for_json_and_missing_asker_is_stale(
    root: Path,
):
    from fno.outstanding.core import read_open_questions

    def resolve(asker: str):
        return (object(), []) if asker == "live-ask" else (None, [])

    _write_indexed_questions(
        root,
        [
            _indexed_question(
                "q-live", "2026-08-19T00:00:00Z", asker="live-ask", blocks=[]
            ),
            _indexed_question(
                "q-stale", "2026-08-18T00:00:00Z", asker="gone-ask", blocks=[]
            ),
            {
                "ts": "2026-08-17T00:00:00Z",
                "type": "operator_question",
                "source": "test",
                "data": {"question_id": "q-legacy", "question": "legacy?"},
            },
        ],
    )
    questions = read_open_questions(
        root,
        liveness_budget_seconds=1.0,
        clock=lambda: 0.0,
        resolver=resolve,
    )
    report = Outstanding(
        carveout_total=0,
        carveout_by_kind={},
        carveout_oldest_ts=None,
        questions=questions,
        captures=[],
    )

    payload = report.as_dict()["questions"]

    assert {row["id"]: row["live"] for row in payload} == {
        "q-live": True,
        "q-stale": False,
        "q-legacy": False,
    }


def test_liveness_budget_expiry_is_unknown_and_does_not_block_report(
    root: Path,
):
    """AC-ERR: a blocked durable-store scan expires as explicit unknown."""
    import multiprocessing
    import threading

    from fno.outstanding.core import read_open_questions

    _write_indexed_questions(
        root,
        [
            _indexed_question(
                "q-slow", "2026-08-19T00:00:00Z", asker="slow-ask", blocks=[]
            )
        ],
    )
    process_context = multiprocessing.get_context("fork")
    started = process_context.Event()

    def slow_resolver(_asker):
        started.set()
        time.sleep(1)
        return object(), []

    questions = read_open_questions(
        root, liveness_budget_seconds=0.1, resolver=slow_resolver
    )
    lingering = [
        thread.name
        for thread in threading.enumerate()
        if thread.name == "fno-question-liveness" and thread.is_alive()
    ]

    assert started.is_set()
    # The resolver's 1s sleep bounds the read: if the budget ever waits for
    # the answer, live lands True and the assert below fails.
    assert lingering == []
    assert questions[0].as_dict()["live"] is None
    output = render(
        Outstanding(0, {}, None, questions, [])
    )
    assert "Questions with unknown liveness" in output
    assert "Stale questions" not in output


def _write_indexed_questions(root: Path, rows: list[dict]) -> None:
    (root / "questions.jsonl").write_text(
        "".join(json.dumps(row) + "\n" for row in rows), encoding="utf-8"
    )


def _indexed_question(
    qid: str,
    ts: str,
    *,
    asker: str,
    blocks: list[str],
) -> dict:
    return {
        "ts": ts,
        "type": "operator_question",
        "source": "test",
        "data": {
            "question_id": qid,
            "question": f"question {qid}",
            "asker": asker,
            "blocks": blocks,
        },
    }


def test_question_rank_orders_newest_first_within_a_liveness_lane(
    root: Path, monkeypatch: pytest.MonkeyPatch
):
    """Rewrites the old blocks-then-oldest rank (x-0dc5): the 19 rows carrying
    blocks were all filed before 09-08 and held the whole 10-row window against
    every ask filed since, so recency outranks blocks. Liveness still lanes
    first; blocks breaks same-second ties."""
    monkeypatch.setattr(
        "fno.agents.discover.resolve_reachable",
        lambda asker: (object(), []) if asker == "live" else (None, []),
    )
    _write_indexed_questions(
        root,
        [
            _indexed_question(
                "q-zero", "2026-08-19T03:00:00Z", asker="live", blocks=[]
            ),
            _indexed_question(
                "q-two", "2026-08-19T02:00:00Z", asker="live", blocks=["x-a", "x-b"]
            ),
            _indexed_question(
                "q-one", "2026-08-19T01:00:00Z", asker="live", blocks=["x-a"]
            ),
            _indexed_question(
                "q-stale", "2026-08-19T00:00:00Z", asker="stale", blocks=["x-a", "x-b", "x-c"]
            ),
        ],
    )

    payload = json.loads(runner.invoke(outstanding_app, ["--json"]).stdout)["questions"]

    assert [row["id"] for row in payload] == ["q-zero", "q-two", "q-one", "q-stale"]
    assert [row["rank"] for row in payload] == [1, 2, 3, 4]


def test_question_render_cap_shows_ten_and_names_true_total(
    root: Path, monkeypatch: pytest.MonkeyPatch
):
    monkeypatch.setattr("fno.agents.discover.resolve_reachable", lambda _asker: (None, []))
    rows = [
        _indexed_question(
            f"q-{i:02d}", f"2026-08-19T{i:02d}:00:00Z", asker="stale", blocks=[]
        )
        for i in range(11)
    ]
    _write_indexed_questions(root, rows)

    out = runner.invoke(outstanding_app, []).stdout

    assert sum(row["data"]["question_id"] in out for row in rows) == 10
    assert "Showing 10 of 11 open questions" in out


def test_a_long_body_renders_bounded_with_the_trim_footer():
    """AC6-HP (x-0dc5): a 2000-character body prints as one bounded line that
    ends in the ellipsis, and the footer names the way to the full text."""
    from fno.outstanding.core import QUESTION_BODY_CAP

    body = "x" * 2000
    report = Outstanding(
        carveout_total=0,
        carveout_by_kind={},
        carveout_oldest_ts=None,
        questions=[Question("q-long", "2026-08-19T00:00:00Z", body, live=False)],
        captures=[],
    )

    output = render(report)

    body_line = next(line for line in output.splitlines() if "q-long" in line)
    printed_body = body_line.split("  ", 2)[2].split("  (")[0]
    assert len(printed_body) == QUESTION_BODY_CAP
    assert printed_body.endswith("…")
    assert "Rows are trimmed to one line" in output
    assert "fno inbox outstanding -J" in output


def test_stale_questions_render_under_visible_heading_with_age(
):
    stale_ts = (datetime.now(timezone.utc) - timedelta(days=2)).strftime("%Y-%m-%dT%H:%M:%SZ")
    report = Outstanding(
        carveout_total=0,
        carveout_by_kind={},
        carveout_oldest_ts=None,
        questions=[
            Question(
                "q-stale",
                stale_ts,
                "which lane?",
                asker="gone-ask",
                ask="pick a lane",
                live=False,
            )
        ],
        captures=[],
    )

    output = render(report)

    assert "Stale questions" in output
    assert "2 days old" in output
    assert "records the decision but reaches nobody" in output


def test_askerless_questions_have_their_own_human_header():
    report = Outstanding(
        carveout_total=0,
        carveout_by_kind={},
        carveout_oldest_ts=None,
        questions=[
            Question("q-unaddressed", "2026-08-19T00:00:00Z", "triage fleet", live=False),
            Question(
                "q-stale",
                "2026-08-19T00:00:01Z",
                "choose lane",
                asker="gone-ask",
                live=False,
            ),
        ],
        captures=[],
    )

    output = render(report)

    assert "Questions for you (no agent is waiting):" in output
    assert output.index("q-unaddressed") < output.index("Stale questions:")
    assert output.count("records the decision but reaches nobody") == 1


@requires_rust
def test_clear_with_answer_emits_operator_decision(root: Path):
    """An answered close records the decision, not just the closure.

    The closed event stays; the decision event lands beside it carrying the
    recovery fields. A close with NO answer is a withdrawal and must not
    mint a decision.
    """
    asked = runner.invoke(
        outstanding_app, ["ask", "fold or migrate?", "--node", "x-7d94", "--ask", "finish the lane"]
    )
    qid = asked.stdout.strip().splitlines()[-1]
    cleared = runner.invoke(outstanding_app, ["clear", qid, "--answer", "fold"])
    assert cleared.exit_code == 0, cleared.output

    lines = [
        json.loads(line)
        for line in [json.dumps(e) for e in _journal_events(project_log('events.jsonl', project_root=root))]
        if line.strip()
    ]
    decisions = [e for e in lines if e["type"] == "operator_decision"]
    assert len(decisions) == 1, "an answered close must emit exactly one decision"
    data = decisions[0]["data"]
    assert data["decision"] == "fold"
    assert data["question_id"] == qid
    assert data["question"] == "fold or migrate?"
    assert data["subject"] == "x-7d94"
    assert data["decision_id"].startswith("d-")
    assert data["asked_at"], "asked_at must inherit the question's timestamp"
    assert data["decided_by"]
    # NOT authority_source == "operator". This runs with no session identity
    # and no terminal, which is the fail-closed third state: the answer is
    # recorded and claims no authority, rather than inheriting law by silence.
    # Answering a question does not by itself prove the operator answered it.
    assert "authority_source" not in data

    # A withdrawal (no --answer) decides nothing: positive control is the
    # closed event itself, the decision count stays at one from the ask above.
    qid2 = runner.invoke(outstanding_app, ["ask", "second question?", "--ask", "finish the lane"]).stdout.strip().splitlines()[-1]
    runner.invoke(outstanding_app, ["clear", qid2])
    lines = [
        json.loads(line)
        for line in [json.dumps(e) for e in _journal_events(project_log('events.jsonl', project_root=root))]
        if line.strip()
    ]
    assert len([e for e in lines if e["type"] == "operator_decision"]) == 1
    assert len([e for e in lines if e["type"] == "operator_question_closed"]) == 2


@requires_rust
def test_clear_with_answer_records_operator_authority_when_stated_at_a_terminal(
    root: Path, monkeypatch
):
    """The superuser lane stays reachable on this path. An agent clearing a
    question on the operator's behalf must not be indistinguishable from the
    operator answering it."""
    def operator_at_a_terminal(payload):
        assert payload["mode"] == "resolve-provenance"
        return {
            "decided_by": "operator",
            "authority_source": "operator",
            "attested_by": "operator",
            "relayed_by": None,
            "origin": None,
        }

    monkeypatch.setattr("fno.rust_binary.call_front_json", operator_at_a_terminal)

    asked = runner.invoke(
        outstanding_app, ["ask", "fold or migrate?", "--node", "x-7d94", "--ask", "finish the lane"]
    )
    qid = asked.stdout.strip().splitlines()[-1]
    cleared = runner.invoke(
        outstanding_app,
        ["clear", qid, "--answer", "fold", "--authority", "operator"],
    )
    assert cleared.exit_code == 0, cleared.output

    lines = _journal_events(project_log("events.jsonl", project_root=root))
    data = [e for e in lines if e["type"] == "operator_decision"][0]["data"]
    assert data["decided_by"] == "operator"
    assert data["attested_by"] == "operator", "a person was at the terminal"
    # The assertion the name promises. Without --authority on `clear`, this
    # path could not produce law at all, and the operator answering a question
    # they were formally asked filed into the same lane as the pre-cutover rows
    # the cutover exists to quarantine.
    assert data["authority_source"] == "operator"


@requires_rust
def test_a_refused_answer_leaves_the_question_open(root: Path, monkeypatch):
    """Closing on a refused answer would retire the question with nothing on
    record, which is worse than refusing the close. The refusal covers both."""
    def unattributed(payload):
        assert payload["mode"] == "resolve-provenance"
        return {
            "refusal_kind": "unattributed",
            "refusal": "no session identity and no terminal, so nothing here marks a decider",
        }

    monkeypatch.setattr("fno.rust_binary.call_front_json", unattributed)

    asked = runner.invoke(
        outstanding_app, ["ask", "fold or migrate?", "--node", "x-7d94", "--ask", "finish the lane"]
    )
    qid = asked.stdout.strip().splitlines()[-1]
    refused = runner.invoke(
        outstanding_app,
        ["clear", qid, "--answer", "fold", "--authority", "operator"],
    )
    assert refused.exit_code != 0, refused.output
    assert "Nothing was closed" in refused.output

    still_open = runner.invoke(outstanding_app, [])
    assert still_open.exit_code == 0, still_open.output
    assert qid in still_open.output, "the question must survive its refused close"

    # A batch refuses whole, never halfway. The refusal turns on the ambient
    # identity and the one --authority value, both invocation-wide, so it
    # fires on the first id before anything closes. Asserted rather than
    # reasoned, because a partial batch is a data-integrity bug and the
    # argument for why it cannot happen is exactly the kind that stops being
    # true when someone moves the record_decision call.
    qid2 = runner.invoke(
        outstanding_app, ["ask", "second question?", "--node", "x-7d94", "--ask", "finish the lane"]
    ).stdout.strip().splitlines()[-1]
    batch = runner.invoke(
        outstanding_app,
        ["clear", qid, qid2, "--answer", "fold", "--authority", "operator"],
    )
    assert batch.exit_code != 0, batch.output
    after = runner.invoke(outstanding_app, [])
    assert qid in after.output and qid2 in after.output, "neither may close"


# --- 2.2 machine-wide question index ---------------------------------------


@requires_rust
def test_question_index_dual_writes_ask_and_close(root: Path):
    asked = runner.invoke(outstanding_app, ["ask", "which lane ships first?", "--ask", "finish the lane"])
    assert asked.exit_code == 0, asked.output
    qid = asked.stdout.strip().splitlines()[-1]

    project_path = project_log("events.jsonl", project_root=root)
    index_path = root / "questions.jsonl"
    project_ask = _journal_last(project_path)
    index_ask = _journal_last(index_path)
    assert project_ask == index_ask
    assert project_ask["data"]["question_id"] == qid

    cleared = runner.invoke(outstanding_app, ["clear", qid])
    assert cleared.exit_code == 0, cleared.output
    project_close = _journal_last(project_path)
    index_close = _journal_last(index_path)
    assert project_close == index_close
    assert project_close["type"] == "operator_question_closed"
    assert project_close["data"]["question_id"] == qid


@requires_rust
def test_question_index_failure_names_id_and_reindex(root: Path, monkeypatch: pytest.MonkeyPatch):
    from fno.events.store_client import store_db_path

    # The SQLite sidecar is the Rust append boundary.
    blocked = root / "index-blocked.jsonl"
    store_db_path(blocked).mkdir()
    monkeypatch.setattr("fno.paths.questions_jsonl", lambda: blocked)

    result = runner.invoke(outstanding_app, ["ask", "which index?", "--ask", "finish the lane"])

    assert result.exit_code == 1
    assert "fno inbox outstanding reindex" in result.output
    durable = _journal_last(project_log("events.jsonl", project_root=root))
    assert durable["data"]["question_id"] in result.output


def test_missing_question_index_reports_empty_with_reindex_hint(root: Path):
    result = runner.invoke(outstanding_app, ["--json"])

    assert result.exit_code == 0, result.output
    assert json.loads(result.stdout)["questions"] == []
    assert "fno inbox outstanding reindex" in result.stderr


def test_unreadable_question_index_is_a_failed_read(root: Path):
    index_path = root / "questions.jsonl"
    index_path.mkdir()

    result = runner.invoke(outstanding_app, ["--json"])

    assert result.exit_code == 1
    assert "failed to read" in result.output
    assert "questions.jsonl" in result.output


def test_reindex_is_idempotent_and_machine_wide(
    root: Path, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
):
    from fno.events import append_event, operator_question, operator_question_closed
    from fno.outstanding.core import events_path

    sibling = tmp_path / "sibling"
    sibling.mkdir()
    first = operator_question(
        question_id="q-first",
        question="first project question",
        session_id="s-first",
        cwd=str(root),
    )
    first["ts"] = "2026-08-19T00:00:00Z"
    second = operator_question(
        question_id="q-second",
        question="second project question",
        session_id="s-second",
        cwd=str(sibling),
    )
    second["ts"] = "2026-08-19T00:01:00Z"
    closed = operator_question_closed(question_id="q-first", closed_by="operator")
    closed["ts"] = "2026-08-19T00:02:00Z"
    append_event(first, events_path=events_path(root))
    append_event(second, events_path=events_path(sibling))
    append_event(closed, events_path=events_path(root))
    monkeypatch.setattr(
        "fno.outstanding.core._capture_project_roots", lambda _root: [root, sibling]
    )

    indexed = runner.invoke(outstanding_app, ["reindex"])
    assert indexed.exit_code == 0, indexed.output
    assert "3 event(s) added" in indexed.output
    indexed_again = runner.invoke(outstanding_app, ["reindex"])
    assert indexed_again.exit_code == 0, indexed_again.output
    assert "0 event(s) added" in indexed_again.output

    monkeypatch.setattr("fno.outstanding.cli._storage_root", lambda: root)
    from_first = json.loads(runner.invoke(outstanding_app, ["--json"]).stdout)["questions"]
    monkeypatch.setattr("fno.outstanding.cli._storage_root", lambda: sibling)
    from_second = json.loads(runner.invoke(outstanding_app, ["--json"]).stdout)["questions"]
    assert from_first == from_second
    assert [row["id"] for row in from_first] == ["q-second"]

    indexed_events = _journal_events(root / "questions.jsonl")
    assert indexed_events == [first, closed, second]


# --- 3rd leg: the capture fold -----------------------------------------------


def _write_inbox(root: Path, lines: list[str]) -> Path:
    inbox = root / "internal" / "fno" / "backlog" / "inbox.md"
    inbox.parent.mkdir(parents=True, exist_ok=True)
    inbox.write_text("\n".join(lines) + "\n", encoding="utf-8")
    return inbox


def _capture_event(fu_id: str, ts: str) -> str:
    return json.dumps(
        {
            "ts": ts,
            "type": "capture_add",
            "source": "backlog",
            "data": {"session_id": "manual", "fu_id": fu_id, "title": "t"},
        }
    )


def test_capture_project_roots_preserves_external_backend_sidecars(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
):
    """AC-EDGE: non-graph trackers still discover roots through sidecars."""
    from types import SimpleNamespace

    from fno.outstanding.core import _capture_project_roots
    from fno.tracker import sidecar as sidecar_store

    this = tmp_path / "this"
    sibling = tmp_path / "sibling"
    this.mkdir()
    sibling.mkdir()
    monkeypatch.setenv("FNO_TRACKER_BACKEND", "github")
    monkeypatch.setattr(
        sidecar_store,
        "load_all",
        lambda: {
            "external-1": SimpleNamespace(cwd=None, source_cwd=str(sibling)),
        },
    )

    assert _capture_project_roots(this) == [sibling.resolve(), this.resolve()]


@pytest.fixture()
def capture_roots(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> "tuple[Path, Path]":
    """A hermetic THIS project plus one sibling project, both known to the
    machine-wide graph so the fold finds the sibling's inbox."""
    this = tmp_path / "this"
    other = tmp_path / "other"
    for p in (this, other):
        p.mkdir(parents=True)
    monkeypatch.setenv("FNO_REPO_ROOT", str(this))
    (this / ".fno").mkdir(exist_ok=True)
    graph = tmp_path / "graph.json"
    seed_graph(graph, json.dumps(
            {
                "entries": [
                    {"id": "x-0001", "title": "a", "cwd": str(this)},
                    {"id": "x-0002", "title": "b", "cwd": str(other)},
                ]
            }
        )
        + "\n")
    import fno.graph._constants as gc
    import fno.graph.store as gs

    monkeypatch.setattr(gc, "GRAPH_JSON", graph)
    monkeypatch.setattr(gs, "GRAPH_JSON", graph)
    monkeypatch.setattr("fno.paths.graph_json", lambda: graph)
    return this, other


def test_capture_leg_folds_every_project_and_renders_true_count(capture_roots):
    """Three legs; the capture leg renders Showing N of M, oldest D
    days, with M folded across projects."""
    this, other = capture_roots
    _write_inbox(this, ["- [ ] fu-aaaaaa - alpha (p1)", "- [ ] fu-bbbbbb - beta"])
    _write_inbox(other, ["- [ ] fu-cccccc - gamma", "- [x] fu-dddddd - done -> ab-1"])

    res = runner.invoke(outstanding_app, [])
    assert res.exit_code == 0, res.output
    assert "3 captures" in res.output, res.output
    assert "Showing 3 of 3" in res.output, res.output

    as_json = runner.invoke(outstanding_app, ["--json"])
    payload = json.loads(as_json.stdout)
    assert payload["captures"]["total"] == 3
    assert payload["captures"]["resolved_files"] == 2
    assert payload["captures"]["parsed_open_rows"] == 3
    assert payload["captures"]["repeated_ids"] == 0
    assert payload["captures"]["by_project"][this.name] == 2
    assert payload["captures"]["by_project"][other.name] == 2 - 1


def test_capture_leg_reports_oldest_age_from_capture_add_events(capture_roots):
    """The inbox markdown carries no dates; age comes from capture_add in the
    events journal."""
    this, other = capture_roots
    _write_inbox(this, ["- [ ] fu-aaaaaa - alpha", "- [ ] fu-bbbbbb - beta"])
    events = this / ".fno" / "events.jsonl"
    events.parent.mkdir(exist_ok=True)
    old = (datetime.now(timezone.utc) - timedelta(days=62)).strftime(
        "%Y-%m-%dT%H:%M:%SZ"
    )
    recent = (datetime.now(timezone.utc) - timedelta(days=1)).strftime(
        "%Y-%m-%dT%H:%M:%SZ"
    )
    events.write_text(
        _capture_event("fu-aaaaaa", old) + "\n" + _capture_event("fu-bbbbbb", recent) + "\n",
        encoding="utf-8",
    )

    res = runner.invoke(outstanding_app, [])
    assert res.exit_code == 0, res.output
    assert "oldest 62 days" in res.output, res.output


def test_unreadable_inbox_contributes_zero_and_never_raises(capture_roots):
    """A missing or unreadable sibling inbox contributes zero captures
    and the other projects still report."""
    this, other = capture_roots
    _write_inbox(this, ["- [ ] fu-aaaaaa - alpha"])
    # other/ has no inbox at all; also plant a graph cwd that does not exist.
    import fno.graph._constants as gc

    seed_graph(
        gc.GRAPH_JSON,
        [{"id": "x-dead", "title": "d", "cwd": "/nonexistent/project"}],
    )

    res = runner.invoke(outstanding_app, [])
    assert res.exit_code == 0, res.output
    assert "1 capture" in res.output, res.output


def test_hook_block_positive_control(root: Path):
    """The pair for the test above.

    An empty-output assertion alone cannot distinguish a genuinely clean state
    from a block that is permanently broken and renders nothing either way.
    This is the control that makes the silence meaningful.
    """
    assert runner.invoke(outstanding_app, []).stdout.strip() == ""
    _write_carveouts(root, [_carveout("cv-1", "oos-bug")])
    assert runner.invoke(outstanding_app, []).stdout.strip() != ""


# --- 2.6 the superuser lane --------------------------------------------------


def _lane_item(text, **kw):
    return LaneItem(text=text, node=None, parked=None, done=False, line=1, **kw)


def test_lane_leads_the_block_above_carveouts(root: Path):
    """AC1-HP: any session sees the lane count and top item first."""
    (root / "my-priorities.md").write_text(
        "- [ ] ship 981 and 971 tonight\n- [ ] call the dentist\n", encoding="utf-8"
    )
    out = runner.invoke(outstanding_app, []).stdout
    assert out.startswith("## Outstanding for you\n\n2 items on your lane, top first.")
    assert "ship 981 and 971 tonight" in out


def test_render_hides_the_action_line_when_unpromoted():
    """AC6-HP: an unpromoted session gets the count and top item, no action line."""
    outstanding = Outstanding(0, {}, None, [], [], lane=[_lane_item("ship it")])
    block = render(outstanding, promoted=False)
    assert "1 item on your lane, top first." in block
    assert "ship it" in block
    assert "File one with:" not in block


# --- 5.2 the asking session's own questions lead -----------------------------


@requires_rust
def test_own_rows_are_labelled_and_rank_is_newest_first(
    root: Path, monkeypatch: pytest.MonkeyPatch
):
    """Rewrites the old own-first order assert (x-0dc5): rank is newest-first
    inside a lane, so the newer row outranks this session's. Ownership shows
    as the [this session] label, never as rank."""
    monkeypatch.setenv("CLAUDECODE_SESSION_ID", "sess-mine")
    runner.invoke(outstanding_app, ["ask", "MINE: do we ship the fold arm?", "--ask", "finish the lane"])
    monkeypatch.setenv("CLAUDECODE_SESSION_ID", "sess-other")
    runner.invoke(outstanding_app, ["ask", "THEIRS: which base branch?", "--ask", "finish the lane"])

    monkeypatch.setenv("CLAUDECODE_SESSION_ID", "sess-mine")
    out = runner.invoke(outstanding_app, []).stdout
    assert "MINE:" in out
    assert "THEIRS:" in out
    assert out.index("THEIRS:") < out.index("MINE:")
    # Labelled as this session's, so the agent that asked is the one prompted.
    assert "this session" in out.lower()


# --- delivery edge: an answered close reaches its asker -----------------------


class _StoredSession:
    """The one field deliver_answer needs from resolve_reachable's hit."""

    session_id = "89abcdef-full-session-id"


def _asked_question_with_asker(root: Path, monkeypatch: pytest.MonkeyPatch) -> str:
    # The asker handle is stamped on a durable question event, so `ask` resolves
    # it through the owned path rather than raw precedence.
    monkeypatch.setattr(
        "fno.claims.self_identity.resolve_self_identity",
        lambda *a, **k: OwnedHarnessIdentity(
            "89abcdef-full-session", "codex", (), "single"
        ),
    )
    asked = runner.invoke(outstanding_app, ["ask", "which lane?", "--ask", "finish the lane"])
    return asked.stdout.strip().splitlines()[-1]


def test_delivered_answer_carries_the_verbatim_text(monkeypatch: pytest.MonkeyPatch):
    """AC1: a resolvable asker gets the answer over mail, whole, plus the ids."""
    from fno.outstanding import deliver as deliver_mod

    sent: list[list[str]] = []
    monkeypatch.setattr(deliver_mod, "_resolve_asker", lambda a: (_StoredSession(), []))
    monkeypatch.setattr(deliver_mod, "_asker_is_live", lambda token: True)
    monkeypatch.setattr(
        deliver_mod,
        "_mail_send",
        lambda argv: (
            sent.append(argv),
            (0, "msg-1 delivered (hosted) to 89abcdef"),
        )[1],
    )

    q = Question(id="q-ab12cd34", ts="2026-08-21T00:00:00Z", question="which lane?", asker="89abcdef")
    line = deliver_mod.deliver_answer(q, "coordination lane", "d-1a2b")

    assert sent, "a resolvable asker must produce exactly one mail send"
    argv = sent[0]
    flat = " ".join(argv)
    assert " mail send " in flat
    # Address by the FULL session id: short ids collide (codex ids are
    # time-prefixed), and a truncated address is mail nobody drains.
    assert "89abcdef-full-session-id" in argv
    assert "coordination lane" in flat, "the answer must travel verbatim"
    assert "q-ab12cd34" in flat, "the asker must learn WHICH question was answered"
    assert "--style-exception" in flat, "operator prose is data, not authored relay"
    assert "mail to 89abcdef" in line
    assert "delivered (hosted)" in line, "quote the bus's own evidence, never just exit 0"
    assert "d-1a2b" in line


def test_an_unexpected_resolver_failure_is_a_stated_line_not_a_traceback(
    monkeypatch: pytest.MonkeyPatch,
):
    """clear calls this outside its own try/except, once per id; one resolver
    explosion must not abort a multi-id clear after some ids already closed."""
    from fno.outstanding import deliver as deliver_mod

    def boom(_token):
        raise RuntimeError("store exploded")

    monkeypatch.setattr(deliver_mod, "_resolve_asker", boom)

    q = Question(id="q-0000000b", ts="2026-08-21T00:00:00Z", question="which?", asker="89abcdef")
    line = deliver_mod.deliver_answer(q, "go", "d-2")

    assert "delivery failed" in line
    assert "store exploded" in line
    assert "fno backlog decisions" in line, "the answer stays recoverable"


def test_no_asker_gets_a_stated_posture_not_a_silent_drop(monkeypatch: pytest.MonkeyPatch):
    """AC4: lead escalations carry asker None; the answer still says where it landed."""
    from fno.outstanding import deliver as deliver_mod

    sent: list[list[str]] = []
    monkeypatch.setattr(deliver_mod, "_mail_send", lambda argv: (sent.append(argv), (0, ""))[1])

    q = Question(id="q-00000001", ts="2026-08-21T00:00:00Z", question="lead stalled", asker=None)
    line = deliver_mod.deliver_answer(q, "reap them", "d-9f")

    assert not sent
    assert "no asker on record" in line
    assert "d-9f" in line
    assert "fno backlog decisions" in line, "the posture names the recovery command"


def test_ambiguous_asker_is_never_guessed(monkeypatch: pytest.MonkeyPatch):
    """AC4: two stored sessions under one short id; waking either could wake a stranger."""
    from fno.outstanding import deliver as deliver_mod

    sent: list[list[str]] = []
    monkeypatch.setattr(
        deliver_mod, "_resolve_asker", lambda a: (None, ["uuid-one", "uuid-two"])
    )
    monkeypatch.setattr(deliver_mod, "_mail_send", lambda argv: (sent.append(argv), (0, ""))[1])

    q = Question(id="q-00000002", ts="2026-08-21T00:00:00Z", question="which?", asker="89abcdef")
    line = deliver_mod.deliver_answer(q, "do both", "d-3")

    assert not sent
    assert "ambiguous" in line
    assert "2" in line, "the posture names how many candidate sessions"
    assert "never guess" in line


def test_failed_mail_send_names_the_retry_and_never_raises(monkeypatch: pytest.MonkeyPatch):
    """The close is durable before delivery runs; a mail failure is a stated line."""
    from fno.outstanding import deliver as deliver_mod

    monkeypatch.setattr(deliver_mod, "_resolve_asker", lambda a: (_StoredSession(), []))
    monkeypatch.setattr(deliver_mod, "_asker_is_live", lambda token: True)
    monkeypatch.setattr(deliver_mod, "_mail_send", lambda argv: (11, "agent lock contention"))

    q = Question(id="q-00000004", ts="2026-08-21T00:00:00Z", question="which?", asker="89abcdef")
    line = deliver_mod.deliver_answer(q, "ship it", "d-5")

    assert "delivery failed" in line
    assert "agent lock contention" in line
    assert "fno agents mail send 89abcdef-full-session-id" in line, "the retry command is copy-pasteable"


def test_clear_with_answer_prints_the_delivery_posture(
    root: Path, monkeypatch: pytest.MonkeyPatch
):
    """AC1 wiring: the posture line reaches the operator through clear itself."""
    from fno.outstanding import deliver as deliver_mod

    qid = _asked_question_with_asker(root, monkeypatch)
    sent: list[list[str]] = []
    monkeypatch.setattr(deliver_mod, "_resolve_asker", lambda a: (_StoredSession(), []))
    monkeypatch.setattr(deliver_mod, "_asker_is_live", lambda token: True)
    monkeypatch.setattr(deliver_mod, "_mail_send", lambda argv: (sent.append(argv), (0, ""))[1])

    cleared = runner.invoke(outstanding_app, ["clear", qid, "--answer", "coordination lane"])

    assert cleared.exit_code == 0, cleared.output
    assert sent, "clear must deliver when the asker resolves"
    assert "mail to 89abcdef" in cleared.output


def test_a_stored_asker_that_is_not_live_is_never_mailed(monkeypatch: pytest.MonkeyPatch):
    """AC2: a store hit outside the live listing gets no mail; the wake rung
    would revive the dead session and charge the answerer's spawn share."""
    from fno.outstanding import deliver as deliver_mod

    live_checks: list[str] = []
    sent: list[list[str]] = []

    def fake_live(token):
        live_checks.append(token)
        return False

    def record_run(argv, **kwargs):
        sent.append(argv)
        return deliver_mod.subprocess.CompletedProcess(
            argv, 0, "msg-1 delivered (woken) to 89abcdef", ""
        )

    monkeypatch.setattr(deliver_mod, "_resolve_asker", lambda a: (_StoredSession(), []))
    monkeypatch.setattr(deliver_mod, "_asker_is_live", fake_live, raising=False)
    monkeypatch.setattr(deliver_mod.subprocess, "run", record_run)

    q = Question(id="q-00000005", ts="2026-08-21T00:00:00Z", question="which?", asker="89abcdef")
    line = deliver_mod.deliver_answer(q, "ship it", "d-6")

    assert live_checks == ["89abcdef-full-session-id"]
    assert not sent, "a dead asker must never reach the mail subprocess"
    assert "is not live, so nobody was woken" in line
    assert "d-6" in line
    assert "fno backlog decisions" in line


# --- named roots: every stream states where it read (BREAK 3) ----------------


def test_payload_names_the_root_each_stream_read(root: Path):
    """AC2: a zero has two explanations - clean and misdirected. Name the root."""
    _write_carveouts(root, [_carveout("cv-1", "oos-bug")])

    payload = json.loads(runner.invoke(outstanding_app, ["--json"]).stdout)

    roots = payload["roots"]
    assert roots["carveouts"]["scope"] == "project"
    assert roots["carveouts"]["root"] == str(root)
    assert roots["questions"]["scope"] == "machine"
    assert roots["captures"]["scope"] == "machine"
    assert roots["lane"]["scope"] == "machine"


# --- BREAK 4 read-side pin: render paths never slice --------------------------


@requires_rust
def test_operator_authority_refusal_names_the_drop_flag_remedy(
    root: Path, monkeypatch: pytest.MonkeyPatch
):
    """The superuser lane is not an agent's to claim, and the remedy is the
    flag drop: an agent answer records as agent or role."""

    qid = (
        runner.invoke(outstanding_app, ["ask", "close PR 1157?", "--ask", "finish the lane"])
        .stdout.strip()
        .splitlines()[-1]
    )

    def agent_refused(payload):
        assert payload["mode"] == "resolve-provenance"
        return {
            "refusal_kind": "authority",
            "refusal": "agent cl7420e8f7 cannot record under superuser authority",
        }

    monkeypatch.setattr("fno.rust_binary.call_front_json", agent_refused)

    refused = runner.invoke(
        outstanding_app,
        ["clear", qid, "--answer", "reopen it", "--authority", "operator"],
    )

    assert refused.exit_code == 3, refused.output
    # The refusal itself survives: this is the unforgeable half.
    assert "cannot record under superuser authority" in refused.output
    # The all-or-nothing statement survives too.
    assert "Nothing was closed" in refused.output
    # The remedy is the flag drop, not a second door.
    assert "An agent answers as agent or role" in refused.output
    assert "Drop --authority operator" in refused.output
    assert "/fno:law" not in refused.output

    # The question really is still open: a refused answer must never retire it.
    after = json.loads(runner.invoke(outstanding_app, ["--json"]).stdout)
    assert [q["id"] for q in after["questions"]] == [qid]
