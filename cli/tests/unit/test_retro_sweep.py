"""PR-independent carve-out sweep: dedup dispositions and the apply contract.

The load-bearing assertion here is that the fuzzy matcher FIRES (a positive
control). A dedup step that silently never matches looks identical to a working
one on a clean ledger, and would file a duplicate for every already-tracked
carve-out the first time it met one.
"""
from __future__ import annotations

from pathlib import Path
from fno.retro.land import MODE_INTERACTIVE
from fno.retro.sweep import (
    DISPOSITION_FILE,
    DISPOSITION_RESOLVE,
    DISPOSITION_REVIEW,
    plan_sweep,
    sweep_carveouts,
)


def _cv(cid: str, description: str, kind: str = "deferred", **extra) -> dict:
    return {"id": cid, "kind": kind, "description": description, **extra}


def test_a_bare_cv_id_mention_parks_rather_than_resolving():
    """x-6c67 shape: a node whose prose merely NAMES a carve-out id (a specimen
    section, a leak report) is not tracking that work. Resolving on a mention
    let `--apply` consume rows whose work never shipped; the row now parks in
    review naming the mentioning node."""
    cv = _cv("cv-11112222", "migrate the spawn adapters")
    nodes = [{"id": "x-1234", "title": "spawn seam", "details": "tracks cv-11112222"}]

    (item,) = plan_sweep([cv], nodes)

    assert item.disposition == DISPOSITION_REVIEW
    assert item.node_id == "x-1234"
    assert "citation is not ownership" in (item.match_reason or "")


def test_fuzzy_subject_match_parks_for_review_and_never_files():
    """Positive control for the fuzzy matcher. A near-identical title must NOT
    file (that would duplicate) and must NOT resolve (a guess that consumes the
    row would lose the work): it parks."""
    cv = _cv("cv-55556666", "consolidate the duplicated stub_exec test fixture")
    nodes = [{"id": "x-9999", "title": "consolidate the duplicated stub_exec test fixtures"}]

    (item,) = plan_sweep([cv], nodes)

    assert item.disposition == DISPOSITION_REVIEW
    assert item.node_id == "x-9999"


def test_an_unrelated_graph_does_not_match():
    cv = _cv("cv-77778888", "wire clippy into CI and clear the two pre-existing sites")
    nodes = [{"id": "x-0001", "title": "rewrite the onboarding docs for new contributors"}]

    (item,) = plan_sweep([cv], nodes)

    assert item.disposition == DISPOSITION_FILE
    assert item.candidate is not None
    # A carve-out's cite is its own ledger id; requiring a PR rejected every
    # swept row as uncited, which is what made the ledger unharvestable.
    assert not item.candidate.uncited


def test_backfill_is_never_swept():
    rows = [_cv("cv-aaaa1111", "backfill the ranks", kind="backfill")]

    assert plan_sweep(rows, []) == []


def test_dry_run_writes_nothing():
    rows = [_cv("cv-cccc1111", "something nothing tracks")]
    consumed: list = []

    report = sweep_carveouts(
        repo_root=Path("/nonexistent"),
        carveout_root=Path("/nonexistent"),
        nodes=[],
        apply=False,
        read_fn=lambda root, kind=None: rows,
        create_fn=lambda **kw: (_ for _ in ()).throw(AssertionError("dry run created a node")),
        consume_fn=lambda root, ids: consumed.extend(ids) or len(ids),
    )

    assert report.by_disposition(DISPOSITION_FILE)
    assert consumed == []
    assert report.consumed == 0


def test_apply_consumes_a_filed_row_but_never_a_review_row():
    rows = [
        _cv("cv-dddd1111", "nothing tracks this one"),
        _cv("cv-dddd2222", "consolidate the duplicated stub_exec test fixture"),
    ]
    nodes = [{"id": "x-9999", "title": "consolidate the duplicated stub_exec test fixtures"}]
    consumed: list = []

    report = sweep_carveouts(
        repo_root=Path("/nonexistent"),
        carveout_root=Path("/nonexistent"),
        nodes=nodes,
        apply=True,
        mode=MODE_INTERACTIVE,
        read_fn=lambda root, kind=None: rows,
        create_fn=lambda **kw: "x-new1",
        consume_fn=lambda root, ids: consumed.extend(ids) or len(ids),
    )

    assert consumed == ["cv-dddd1111"]
    assert report.by_disposition(DISPOSITION_FILE)[0].node_id == "x-new1"
    assert report.by_disposition(DISPOSITION_REVIEW)[0].node_id == "x-9999"


def test_a_failed_mint_leaves_the_row_in_the_ledger():
    """Consuming a row whose node never landed is the exact failure the close
    gate exists to prevent: the ledger goes green and the work is tracked
    nowhere."""
    rows = [_cv("cv-eeee1111", "nothing tracks this one either")]
    consumed: list = []

    def _boom(**kwargs):
        raise RuntimeError("graph lock timeout")

    report = sweep_carveouts(
        repo_root=Path("/nonexistent"),
        carveout_root=Path("/nonexistent"),
        nodes=[],
        apply=True,
        read_fn=lambda root, kind=None: rows,
        create_fn=_boom,
        consume_fn=lambda root, ids: consumed.extend(ids) or len(ids),
    )

    assert consumed == []
    assert report.failed
    assert report.by_disposition(DISPOSITION_FILE)[0].error


def test_two_rows_with_the_same_text_do_not_both_file():
    """One blocker carved out from two sessions. Filing both mints two
    permanent nodes for one piece of work, and there is no delete verb."""
    rows = [
        _cv("cv-dup00001", "the same blocker, written twice"),
        _cv("cv-dup00002", "the same blocker, written twice"),
    ]

    first, second = plan_sweep(rows, [])

    assert first.disposition == DISPOSITION_FILE
    assert second.disposition == DISPOSITION_REVIEW
    assert "cv-dup00001" in second.match_reason


def test_an_unreadable_ledger_reports_failed_so_the_verb_can_exit_nonzero():
    """A present-but-unreadable ledger printing '0 unharvested' and exiting 0
    is the silent success read_carveouts raises specifically to prevent."""

    def _boom(root, kind=None):
        raise OSError("ledger is unreadable")

    report = sweep_carveouts(
        repo_root=Path("/nonexistent"),
        carveout_root=Path("/nonexistent"),
        nodes=[],
        apply=False,
        read_fn=_boom,
    )

    assert report.read_failed
    assert report.failed  # drives the CLI exit code


def test_cv_id_scan_requires_the_structured_cite_not_a_bare_mention():
    """The helper counts OWNERSHIP (the ``source `cv-...` `` cite a filing
    writes), not presence. A bare mention - the x-6c67 citations - must not
    make the PR-scoped harvest skip filing either."""
    from fno.retro.dedup import cv_ids_cited_in_nodes

    mention = {"id": "x-6c67", "title": "ambient state", "details": "cv-cross001 leaked into the sandbox"}
    owner = {"id": "x-home", "title": "work", "details": "Source: source `cv-cross001`"}

    assert cv_ids_cited_in_nodes([mention], ["cv-cross001"]) == set()
    assert cv_ids_cited_in_nodes([owner], ["cv-cross001"]) == {"cv-cross001"}


# -- the x-2d34 shape: the carveout text names the node that owns the work --


def test_a_done_owner_named_in_the_text_resolves_instead_of_filing():
    """The false-filing shape: the sweep minted a live p1 for work its own
    carveout text said was carried by a node that had already shipped. The
    evidence was inside the text being read. Resolving on the named owner
    consumes the row and names the covering node instead of spending a
    dispatch lane."""
    cv = _cv("cv-f0f00001", "re-rank the board columns; carried by x-87fb")
    nodes = [{"id": "x-87fb", "title": "board rank suffix", "status": "done"}]

    (item,) = plan_sweep([cv], nodes)

    assert item.disposition == DISPOSITION_RESOLVE
    assert item.node_id == "x-87fb"
    assert "x-87fb" in (item.match_reason or "")


def test_a_live_owner_named_in_the_text_files_but_links():
    """The carveout splits work OUT of a live node: file it, but the minted
    node carries the related edge so the two stay joined on the graph."""
    cv = _cv("cv-f0f00004", "split out of x-2222, which is still open")
    nodes = [{"id": "x-2222", "title": "parent epic", "status": "in_progress"}]

    (item,) = plan_sweep([cv], nodes)
    assert item.disposition == DISPOSITION_FILE
    assert item.link_to == "x-2222"

    links: list = []
    consumed: list = []
    report = sweep_carveouts(
        repo_root=Path("/nonexistent"),
        carveout_root=Path("/nonexistent"),
        nodes=nodes,
        apply=True,
        read_fn=lambda root, kind=None: [cv],
        create_fn=lambda **kw: "x-new1",
        consume_fn=lambda root, ids: consumed.extend(ids) or len(ids),
        link_fn=lambda node_id, owner: links.append((node_id, owner)),
    )

    assert links == [("x-new1", "x-2222")]
    assert consumed == ["cv-f0f00004"]
    assert not report.failed
