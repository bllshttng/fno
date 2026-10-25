"""The operator-question store behind `fno inbox outstanding`.

The report itself is native (crates/fno-agents/src/outstanding.rs). What
stays here is the question store the ask, clear, and reindex commands and the
lead escalation still read and write.
"""

from __future__ import annotations

import json
import os
import sys
import time
from dataclasses import dataclass, replace
from pathlib import Path
from typing import Any, Callable, Iterable, Iterator, Optional

EVENTS_NAME = "events.jsonl"
QUESTION_EVENT = "operator_question"
QUESTION_CLOSED_EVENT = "operator_question_closed"
# Both question types share this prefix, so a raw line without it cannot be
# one of ours. See the substring prefilter in read_open_questions.
QUESTION_MARKER = "operator_question"
# SessionStart has a 1.5s hook budget. Reachability may scan multi-gigabyte
# harness stores, so the report gives it one bounded slice and renders an
# explicit unknown for anything that cannot finish in time. The slice is a
# ceiling, not a sleep: the pipe answers the moment the forked child sends.
# The child is a fork of a possibly multi-threaded process, which alone can
# cost more than 50ms on a loaded machine, so the ceiling sits at 0.5s --
# under the SessionStart budget, over the worst fork.
LIVENESS_BUDGET_SECONDS = 0.5


class OutstandingError(Exception):
    """A store exists but could not be read.

    Distinct from "nothing outstanding": a failed read reported as an empty
    one is the absence-as-success trap, and here it would tell an operator the
    queue is clear when it is merely unreadable.
    """


class QuestionIndexWriteError(OutstandingError, RuntimeError):
    """A question event is durable but missing from the recall index."""

    def __init__(self, question_id: str, cause: BaseException) -> None:
        super().__init__(str(cause))
        self.question_id = question_id
        self.cause = cause


@dataclass(frozen=True)
class Question:
    id: str
    ts: str
    question: str
    session_id: Optional[str] = None
    cwd: Optional[str] = None
    node: Optional[str] = None
    asker: Optional[str] = None
    ask: Optional[str] = None
    options: tuple[str, ...] = ()
    blocks: tuple[str, ...] = ()
    subject: Optional[str] = None
    live: Optional[bool] = None

    def as_dict(self, *, rank: Optional[int] = None) -> "dict[str, Any]":
        return {
            "id": self.id,
            "ts": self.ts,
            "question": self.question,
            "session_id": self.session_id,
            "cwd": self.cwd,
            "node": self.node,
            "asker": self.asker,
            "ask": self.ask,
            "options": list(self.options),
            "blocks": list(self.blocks),
            "subject": self.subject,
            "live": self.live,
            "rank": rank,
        }


def events_path(root: Path) -> Path:
    """The events journal beside the carve-out ledger under the same root.

    Routes through ``project_log``, the single accessor for ``.fno/<name>``,
    rather than hand-building the path: that keeps the ``.resolve()`` (so a
    symlinked root yields one path string, not two) and keeps this reader on
    the same placement rule as every other writer.

    Deliberately NOT ``paths.project_events_json``, which honors the
    ``FNO_EVENTS_PATH`` pin: that pin stands in for a root nobody resolved, and
    this caller resolves one. Letting it win here would collapse every root the
    reader folds onto one file, and under test it would replace each test's own
    journal with the one sandbox journal the whole pytest process shares.
    """
    from fno.paths import project_log

    return project_log(EVENTS_NAME, project_root=Path(root))


def _iter_question_lines(fh: "Iterable[str]") -> "Iterator[str]":
    """Yield only the lines that can possibly be a question or decision event.

    The substring test runs per line as it is read, so neither the whole file
    nor the whole filtered set is ever held beyond what the fold needs. Our
    rows carry one of two markers, so a line without either cannot be ours
    and skipping it changes no outcome. Callers that read only asks and
    closes ignore decision rows by type.
    """
    for line in fh:
        if QUESTION_MARKER in line or "operator_decision" in line:
            yield line


def questions_path() -> Path:
    """Resolve the machine-wide question index through the configured state dir."""
    from fno import paths

    return paths.questions_jsonl()


def append_question_event(event: dict[str, Any], root: Path) -> None:
    """Write one event to project durability first, then machine-wide recall."""
    from fno.events import append_event

    data = event.get("data")
    question_id = data.get("question_id") if isinstance(data, dict) else None
    if not question_id:
        raise ValueError("question event has no question_id")
    try:
        append_event(event, events_path=events_path(root))
    except Exception as exc:
        raise OutstandingError(f"failed to append question to project journal: {exc}") from exc
    try:
        append_event(event, events_path=questions_path())
    except Exception as exc:  # noqa: BLE001 - caller needs the durable event id
        raise QuestionIndexWriteError(str(question_id), exc) from exc


def _read_question_events(path: Path, *, missing_hint: bool) -> "list[dict[str, Any]]":
    """Read valid question envelopes, distinguishing absent from unreadable."""
    # The store commit is the write boundary: with a store beside the index,
    # committed rows are the whole history and the raw check never runs.
    from fno.events.store_client import native_rows, store_db_path

    committed = native_rows(path, include_rejected=True)
    if committed is not None:
        events: "list[dict[str, Any]]" = []
        for line in committed:
            try:
                rec = json.loads(line)
            except (json.JSONDecodeError, ValueError):
                continue
            if not isinstance(rec, dict) or rec.get("type") not in {
                QUESTION_EVENT,
                QUESTION_CLOSED_EVENT,
                "operator_decision",
            }:
                continue
            data = rec.get("data")
            if isinstance(data, dict) and data.get("question_id"):
                events.append(rec)
        if not events and not store_db_path(path).exists() and not path.exists():
            if missing_hint:
                print(
                    f"outstanding: question index {path} is missing; run "
                    "`fno inbox outstanding reindex` to recover project questions.",
                    file=sys.stderr,
                )
        return events
    try:
        path.stat()
    except FileNotFoundError:
        try:
            path.lstat()
        except FileNotFoundError:
            if missing_hint:
                print(
                    f"outstanding: question index {path} is missing; run "
                    "`fno inbox outstanding reindex` to recover project questions.",
                    file=sys.stderr,
                )
            return []
        except OSError as exc:
            raise OutstandingError(f"cannot read question index {path}: {exc}") from exc
        raise OutstandingError(f"cannot read question index {path}: dangling symlink")
    except OSError as exc:
        raise OutstandingError(f"cannot read question index {path}: {exc}") from exc

    events = []
    try:
        for line in _iter_question_lines(path.read_text(encoding="utf-8").splitlines()):
            try:
                rec = json.loads(line)
            except (json.JSONDecodeError, ValueError):
                continue
            if not isinstance(rec, dict) or rec.get("type") not in {
                QUESTION_EVENT,
                QUESTION_CLOSED_EVENT,
                "operator_decision",
            }:
                continue
            data = rec.get("data")
            if isinstance(data, dict) and data.get("question_id"):
                events.append(rec)
    except (OSError, UnicodeDecodeError) as exc:
        raise OutstandingError(f"cannot read question index {path}: {exc}") from exc
    return events


def _resolve_question_liveness(
    askers: "list[str]",
    *,
    budget_seconds: float,
    clock: "Callable[[], float]",
    resolver: "Callable[[str], tuple[Any, list[str]]] | None",
) -> "dict[str, bool]":
    """Resolve unique askers within one wall-clock slice.

    The resolver runs in a child process that is terminated and reaped at the
    deadline, so a store scan cannot survive until interpreter shutdown. Only
    answers completed by the deadline land in the result; every absent key is
    explicitly unknown, never guessed stale.
    """
    import multiprocessing

    if not askers or budget_seconds <= 0:
        return {}
    if resolver is None:
        from fno.agents.discover import resolve_reachable

        resolver = resolve_reachable
    context = multiprocessing.get_context("fork")
    receive, send = context.Pipe(duplex=False)
    _ = clock

    def resolve_all() -> None:
        answers: "dict[str, bool]" = {}
        try:
            for asker in dict.fromkeys(askers):
                try:
                    reachable, ambiguous = resolver(asker)
                    answer = reachable is not None and not ambiguous
                except Exception:  # noqa: BLE001 - a completed refusal is stale
                    answer = False
                answers[asker] = answer
        finally:
            try:
                send.send(answers)
            except (BrokenPipeError, OSError):
                pass
            send.close()

    process = context.Process(target=resolve_all, name="fno-question-liveness", daemon=True)
    process.start()
    send.close()
    answers: "dict[str, bool]" = {}
    try:
        if receive.poll(budget_seconds):
            payload = receive.recv()
            if isinstance(payload, dict):
                answers = payload
    except (EOFError, OSError):
        pass
    finally:
        receive.close()
        if process.is_alive():
            process.terminate()
        process.join(timeout=budget_seconds)
        if process.is_alive():
            process.kill()
            process.join()
    return answers


def read_open_questions(
    root: Path,
    *,
    liveness_budget_seconds: float = LIVENESS_BUDGET_SECONDS,
    clock: "Callable[[], float]" = time.monotonic,
    resolver: "Callable[[str], tuple[Any, list[str]]] | None" = None,
) -> "list[Question]":
    """Fold ``operator_question`` minus ``operator_question_closed``.

    Ranked by liveness lane, then newest first (blocked nodes break same-second
    ties). A malformed line is SKIPPED, never raised, inheriting
    ``read_carveouts``' rule: one bad row must not cost the others. A missing
    index reads as no questions with a recovery hint; an unreadable one fails.
    """
    _ = root  # The index is machine-wide; retain the argument for caller parity.
    asked: "dict[str, Question]" = {}
    closed: "set[str]" = set()
    for rec in _read_question_events(questions_path(), missing_hint=True):
        data = rec.get("data")
        if not isinstance(data, dict):
            continue
        if rec.get("type") == QUESTION_EVENT:
            qid = data.get("question_id")
            if not qid:
                continue
            asked[str(qid)] = Question(
                id=str(qid),
                ts=str(rec.get("ts") or ""),
                question=str(data.get("question") or ""),
                session_id=data.get("session_id") or None,
                cwd=data.get("cwd") or None,
                node=data.get("node") or None,
                asker=data.get("asker") or None,
                ask=data.get("ask") or None,
                options=tuple(data.get("options") or ()),
                blocks=tuple(data.get("blocks") or ()),
                subject=data.get("subject") or None,
            )
        elif rec.get("type") == QUESTION_CLOSED_EVENT:
            qid = data.get("question_id")
            if qid:
                closed.add(str(qid))

    open_qs = [q for qid, q in asked.items() if qid not in closed]
    resolved = _resolve_question_liveness(
        [q.asker for q in open_qs if q.asker],
        budget_seconds=liveness_budget_seconds,
        clock=clock,
        resolver=resolver,
    )
    open_qs = [replace(q, live=False if not q.asker else resolved.get(q.asker)) for q in open_qs]
    # Newest first inside a lane. An ask filed minutes ago is the one a human
    # can still act on, and blocks drops below recency because the 19 rows
    # carrying it were all filed before 09-08 and held the whole 10-row window
    # against every ask filed since. An empty ts sorts last: "" loses under
    # reverse.
    open_qs.sort(key=lambda q: (q.ts, len(q.blocks), q.id), reverse=True)
    open_qs.sort(key=lambda q: q.live is not True)
    return open_qs


def read_answered_questions() -> "list[dict[str, Any]]":
    """Read actionable answers through the native history-aware join."""
    from fno.events.store_client import read_projection

    return read_projection(questions_path(), "--answered-questions", {})


def read_question_events() -> "list[dict[str, Any]]":
    """Every question-journal record (ask, close, decision), oldest-first:
    the positional stream an answer-suppression fold replays."""
    return _read_question_events(questions_path(), missing_hint=False)


def _capture_project_roots(root: Path) -> "list[Path]":
    """This project plus every project root the machine-wide graph names.

    The graph is the one machine-wide store (``~/.fno/graph.json``), and its
    entries carry the ``cwd``/``source_cwd`` each node was worked from, so it
    is the measured-fact enumeration of sibling projects - no second registry.
    The read goes through the store api, never the file. Dead or missing roots
    degrade to zero captures in the fold below, and a corrupt or absent store
    reads as this project alone.
    """
    roots = {Path(root).resolve()}
    try:
        backend = os.environ.get("FNO_TRACKER_BACKEND") or "graph"
        if backend == "graph":
            from fno import paths
            from fno.graph.store import _client_for

            # The keeper's read op is the store's raw arm: unlike the typed
            # `api.nodes` it never drops a row the node model cannot carry,
            # which this enumeration must not do - a legacy row's cwd is as
            # real a project root as a typed row's. The path resolves at call
            # time so the seam the tests pin (`paths.graph_json`) picks the
            # store up.
            gpath = paths.graph_json()
            payload = _client_for(gpath).read(gpath)
            entries = payload.get("entries", []) if isinstance(payload, dict) else payload
            locations = (
                (entry.get("cwd"), entry.get("source_cwd"))
                for entry in entries
                if isinstance(entry, dict)
            )
        else:
            # External trackers keep footnote-owned checkout fields in their
            # sidecars; graph mode reads those same fields from their owner.
            from fno.tracker import sidecar as sidecar_store

            locations = ((sc.cwd, sc.source_cwd) for sc in sidecar_store.load_all().values())
        for pair in locations:
            for raw in pair:
                if isinstance(raw, str) and raw:
                    p = Path(raw)
                    if p.is_dir():
                        # Graph checkout paths are already absolute in normal
                        # records. Keep relative legacy rows absolute without
                        # resolving every symlink here: the inbox and journal
                        # folds below resolve their physical stores once for
                        # deduplication, which is the authority that matters.
                        roots.add(Path(os.path.abspath(p)))
    except Exception:  # noqa: BLE001 - the graph is advisory here, never fatal
        pass
    return sorted(roots)


def _canonical_checkout_for(root: Path) -> Path:
    """Collapse a linked worktree onto its canonical checkout without git.

    Standard linked-worktree ``.git`` files point into
    ``<canonical>/.git/worktrees/<name>``. Other shapes keep the existing
    root so their established resolver can handle them.
    """
    root = Path(root)
    marker = root / ".git"
    if marker.is_dir():
        return root
    try:
        line = marker.read_text(encoding="utf-8").splitlines()[0]
    except (OSError, UnicodeDecodeError, IndexError):
        return root
    if not line.startswith("gitdir:"):
        return root
    git_dir = Path(line.partition(":")[2].strip())
    if not git_dir.is_absolute():
        git_dir = root / git_dir
    if git_dir.parent.name != "worktrees" or git_dir.parent.parent.name != ".git":
        return root
    canonical = git_dir.parent.parent.parent
    return canonical if (canonical / ".git").is_dir() else root


def _question_journals(root: Path) -> "list[Path]":
    """Every graph-named project journal, deduped by physical file."""
    seen: "set[tuple[int, int]]" = set()
    journals: "list[Path]" = []
    from fno.events.store_client import store_db_path

    for project_root in _capture_project_roots(root):
        path = events_path(project_root)
        # The store commit is the write boundary: a journal whose bytes were
        # never written still owns a readable store beside it.
        probe = path if path.exists() else store_db_path(path)
        try:
            stat = probe.stat()
        except OSError:
            continue
        key = (stat.st_dev, stat.st_ino)
        if key in seen:
            continue
        seen.add(key)
        journals.append(path)
    return journals


def _question_event_identity(event: dict[str, Any]) -> "tuple[str, str] | None":
    event_type = event.get("type")
    data = event.get("data")
    question_id = data.get("question_id") if isinstance(data, dict) else None
    if event_type not in {QUESTION_EVENT, QUESTION_CLOSED_EVENT} or not question_id:
        return None
    return str(event_type), str(question_id)


def reindex_questions(root: Path) -> "dict[str, int]":
    """Append every missing ask/close identity from graph-named projects."""
    from fno.events import append_event

    index = questions_path()
    existing = _read_question_events(index, missing_hint=False)
    known = {
        identity for event in existing if (identity := _question_event_identity(event)) is not None
    }
    preexisting = set(known)
    added = 0
    already: "set[tuple[str, str]]" = set()
    for journal in _question_journals(root):
        try:
            events = _read_question_events(journal, missing_hint=False)
        except OutstandingError:
            continue
        for event in events:
            identity = _question_event_identity(event)
            if identity is None:
                continue
            if identity in known:
                if identity in preexisting:
                    already.add(identity)
                continue
            append_event(event, events_path=index)
            known.add(identity)
            added += 1
    return {"added": added, "already": len(already), "total": len(known)}
