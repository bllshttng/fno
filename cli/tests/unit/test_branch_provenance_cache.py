"""Unit tests for the per-repo branch-provenance cache.

The cache is the join between the pr-watch tick's stranded sweep (writer)
and the Kanban board (reader), so the contract here is the render
contract: only non-CLEAN rows are stored, and every read failure degrades
to "no section".
"""
import json
from pathlib import Path


from fno.branch_provenance_cache import CACHE_RELPATH, read_cache, write_cache
from fno.worktree_stranded import CLEAN, STRANDED, Row


def cache_path(repo):
    return Path(repo) / CACHE_RELPATH


def _row(klass, node, unpushed, age, **facts):
    name = node or "unmapped"
    base = {"path": f"/wt/{name}", "branch": f"feature/{name}"}
    base.update(facts)
    return Row(klass, node, unpushed, age, base)


def _stranded():
    return _row(
        STRANDED,
        "x-abcd",
        23,
        "33 hours ago",
        has_remote=False,
        pr_number=None,
        live=False,
    )


def test_write_skips_clean_rows_and_carries_the_full_shape(tmp_path):
    clean = _row(CLEAN, "x-ok", 0, "1 hour ago", has_remote=True, live=True)
    assert write_cache(tmp_path, [_stranded(), clean]) is True

    rows = json.loads((tmp_path / CACHE_RELPATH).read_text())
    assert len(rows) == 1  # a caught-up worktree has nothing to report
    assert rows[0] == {
        "branch": "feature/x-abcd",
        "node": "x-abcd",
        "klass": "STRANDED",
        "unpushed": 23,
        "has_remote": False,
        "age": "33 hours ago",
        "pr_number": None,
        "live": False,
    }


def test_write_returns_changed_not_written(tmp_path):
    """True flags a CONTENT change, so the tick can re-render the board only
    when the cache moved; an identical rewrite writes nothing."""
    assert write_cache(tmp_path, [_stranded()]) is True
    assert write_cache(tmp_path, [_stranded()]) is False
    assert not list(tmp_path.glob("*.tmp"))  # unchanged: no write at all


def test_read_cache_fails_open(tmp_path):
    assert read_cache(tmp_path) == []  # missing file
    cache_path(tmp_path).parent.mkdir()
    cache_path(tmp_path).write_text("{not json")
    assert read_cache(tmp_path) == []  # malformed
    cache_path(tmp_path).write_text('{"a": 1}')
    assert read_cache(tmp_path) == []  # wrong shape entirely
    cache_path(tmp_path).write_text("[1, 2]")
    assert read_cache(tmp_path) == []  # a list of non-objects is not rows


def test_write_failure_never_raises(tmp_path):
    # A repo path that cannot hold the file (a regular file where the
    # directory belongs) must log, answer False, and leave the tick leg up.
    blocker = tmp_path / "not-a-dir"
    blocker.write_text("occupied")
    assert write_cache(blocker, [_stranded()]) is False
    assert read_cache(blocker) == []
