"""Read-only discovery over the backlog's two recall lanes.

FTS5 supplies vocabulary recall.  Relatedness supplies the calibrated score
and the domain signal used by filing.  This module joins both observations
without creating another store or changing graph state.
"""
from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path
from typing import Any, Iterable

from fno.graph import relatedness


class SearchUnavailableError(RuntimeError):
    """The keeper, and with it the store's FTS index, is unreachable."""


def _fts_search(query: str, graph_path: Path, limit: int | None = 20) -> list[str]:
    """Node ids matching ``query``, ranked by the keeper's FTS5 index."""
    from fno.graph.store import _client_for

    try:
        result = _client_for(Path(graph_path)).request(
            "api", {"op": "search", "q": query, "limit": limit}
        )
    except FileNotFoundError:
        raise
    except RuntimeError as exc:
        raise SearchUnavailableError(str(exc)) from exc
    return [str(row) for row in result.get("rows", [])]


@dataclass(frozen=True)
class Candidate:
    """One possible match and the recall lanes that found it."""

    node_id: str
    score: float
    lanes: frozenset[str]
    reason: str = ""

    @property
    def id(self) -> str:
        """Short alias used by renderers that already call ids ``id``."""
        return self.node_id

    def as_dict(self) -> dict[str, Any]:
        return {
            "id": self.node_id,
            "node_id": self.node_id,
            "score": self.score,
            "lanes": sorted(self.lanes),
            "evidence": self.reason,
        }


class CandidateResults(list[Candidate]):
    """List-compatible result carrying an explicit FTS degradation receipt."""

    def __init__(
        self,
        values: Iterable[Candidate] = (),
        *,
        degraded: bool = False,
        warning: str | None = None,
    ) -> None:
        super().__init__(values)
        self.degraded = degraded
        self.warning = warning


@dataclass(frozen=True)


def _graph_path() -> Path:
    from fno.graph._constants import GRAPH_JSON

    return GRAPH_JSON


def _entries_for(graph_path: Path) -> list[dict[str, Any]]:
    from fno.graph.api import wire_rows

    return [entry for entry in wire_rows(path=graph_path) if isinstance(entry, dict)]


def candidates(
    title: str,
    details: str,
    *,
    limit: int | None = 20,
    entries: list[dict[str, Any]] | None = None,
    graph_path: Path | None = None,
    exclude_id: str | None = None,
    token_cache: dict[str, frozenset[str]] | None = None,
    domain: str = "code",
    floor: float | None = None,
    fts_enabled: bool = True,
) -> CandidateResults:
    """Union FTS5 and relatedness recall, ranked by relatedness score.

    ``limit=None`` returns every row above the relatedness lane's floor: the
    floor is the threshold, and a cap is a second one that re-hides exactly
    the low-score family the fts lane exists to recover.

    Full contract: docs/architecture/backlog-graph-verb-contracts.md
    """
    if limit is not None and limit < 1:
        return CandidateResults()
    path = graph_path or _graph_path()
    pool = entries if entries is not None else _entries_for(path)
    pool = [
        entry
        for entry in pool
        if isinstance(entry.get("id"), str) and entry.get("id") != exclude_id
    ]
    by_id = {entry["id"]: entry for entry in pool}
    incoming = {
        "id": "__incoming__",
        "title": title or "",
        "details": details or "",
        "domain": domain,
    }

    fts_ids: list[str] = []
    degraded = False
    warning: str | None = None
    if not fts_enabled:
        degraded = True
        warning = (
            "graph entries were injected without the graph file; "
            "the fts lane is unavailable"
        )
    else:
        try:
            fts_ids = [node_id for node_id in _fts_search(
                " ".join(part for part in (title, details) if part), path, limit=None
            ) if node_id in by_id]
        except (SearchUnavailableError, FileNotFoundError) as exc:
            degraded = True
            warning = str(exc)

    related_kwargs: dict[str, Any] = {"k": None, "floor": floor}
    if token_cache is not None:
        related_kwargs["token_cache"] = token_cache
    related_rows = relatedness.similar_nodes(incoming, pool, **related_kwargs)
    related_by_id = {
        node_id: (score, reason)
        for node_id, score, reason in related_rows
        if node_id in by_id
    }
    fts_set = set(fts_ids)
    related_set = set(related_by_id)
    all_ids = fts_set | related_set
    result: list[Candidate] = []
    for node_id in all_ids:
        if node_id in related_by_id:
            score, reason = related_by_id[node_id]
        else:
            score, reason = relatedness.score_pair(
                incoming, by_id[node_id], include_epic=False
            )
        lanes = frozenset(
            lane
            for lane, present in (("fts", node_id in fts_set), ("relatedness", node_id in related_set))
            if present
        )
        result.append(Candidate(node_id, score, lanes, reason))
    result.sort(key=lambda candidate: (-candidate.score, candidate.node_id))
    return CandidateResults(result[:limit], degraded=degraded, warning=warning)


def positive_control(
    query: str,
    *,
    graph_path: Path | None = None,
    entries: list[dict[str, Any]] | None = None,
) -> dict[str, Any]:
    """Run a known-positive query so an empty result is not trusted blindly."""
    path = graph_path or _graph_path()
    pool = entries if entries is not None else _entries_for(path)
    try:
        matches = [node_id for node_id in _fts_search(query, path, limit=None)]
        return {"query": query, "matches": matches, "lane": "fts", "degraded": False}
    except SearchUnavailableError as exc:
        lowered = query.casefold()
        matches = [
            entry["id"]
            for entry in pool
            if isinstance(entry.get("id"), str)
            and lowered in " ".join(
                str(entry.get(field) or "") for field in ("title", "slug", "details")
            ).casefold()
        ]
        return {
            "query": query,
            "matches": matches,
            "lane": "substring",
            "degraded": True,
            "warning": str(exc),
        }


