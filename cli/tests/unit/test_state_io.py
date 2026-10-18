"""Tests for fno.state.io - frontmatter + atomic write helpers."""

from __future__ import annotations

import multiprocessing
import time
from pathlib import Path


from fno.state.io import atomic_write, read_frontmatter, write_frontmatter


# -- Helpers --

SAMPLE_STATE = """\
---
status: IN_PROGRESS
iteration: 1
session_id: 20260421T093631Z-97817-920dac
nested:
  key: value
---
# Body text here

Some extra content.
"""


# -- AC1-HP: round-trip preserves data --


def test_ac1_hp_roundtrip(tmp_path: Path) -> None:
    """AC1-HP: read_frontmatter -> write_frontmatter -> read_frontmatter is identity."""
    state_file = tmp_path / "target-state.md"
    state_file.write_text(SAMPLE_STATE)

    data, body = read_frontmatter(state_file)
    assert data["status"] == "IN_PROGRESS"
    assert data["iteration"] == 1
    assert "Body text here" in body

    write_frontmatter(state_file, data, body)

    data2, body2 = read_frontmatter(state_file)
    assert data2["status"] == "IN_PROGRESS"
    assert data2["iteration"] == 1
    assert data2["session_id"] == "20260421T093631Z-97817-920dac"
    assert data2["nested"] == {"key": "value"}
    assert "Body text here" in body2


def test_iso_timestamp_preserved_as_string(tmp_path: Path) -> None:
    """REGRESSION: yaml.safe_load coerces `2026-05-21T00:00:00Z` to datetime,
    which then fails schemas declaring updated_at as Optional[str]. The custom
    loader strips the timestamp resolver so ISO strings stay strings.

    Surfacing PR: example-pipeline Wave 7 followup item 6 (fno do state set --field
    pr_number errored with `Input should be a valid string` pointing at the
    untouched updated_at field).
    """
    state_file = tmp_path / "target-state.md"
    state_file.write_text(
        "---\n"
        "updated_at: 2026-05-21T00:00:00Z\n"
        "iteration: 5\n"
        "is_active: true\n"
        "ratio: 0.75\n"
        "---\n"
        "body\n"
    )

    data, _body = read_frontmatter(state_file)

    assert isinstance(data["updated_at"], str), (
        f"updated_at should stay a string, got {type(data['updated_at']).__name__}"
    )
    assert data["updated_at"] == "2026-05-21T00:00:00Z"
    # Other coercions still work
    assert data["iteration"] == 5
    assert isinstance(data["iteration"], int)
    assert data["is_active"] is True
    assert data["ratio"] == 0.75


# -- AC2-ERR: concurrent writes produce valid file (not interleaved bytes) --


def _writer(path: str, content: str, delay: float) -> None:
    """Worker process for concurrency test."""
    time.sleep(delay)
    atomic_write(Path(path), content)


def test_ac2_err_concurrent_writes_safe(tmp_path: Path) -> None:
    """AC2-ERR: two simultaneous atomic_write calls produce a valid file."""
    target = tmp_path / "concurrent.md"
    content_a = "A" * 1000 + "\n"
    content_b = "B" * 1000 + "\n"

    p1 = multiprocessing.Process(target=_writer, args=(str(target), content_a, 0.0))
    p2 = multiprocessing.Process(target=_writer, args=(str(target), content_b, 0.01))
    p1.start()
    p2.start()
    p1.join(timeout=10)
    p2.join(timeout=10)

    assert p1.exitcode == 0, f"writer p1 exited with {p1.exitcode}"
    assert p2.exitcode == 0, f"writer p2 exited with {p2.exitcode}"

    result = target.read_text()
    # Must be entirely one content or the other - no interleaving
    assert result in (content_a, content_b), f"interleaved bytes detected, len={len(result)}"

    # Lock file should be released
    lock_path = Path(str(target) + ".lock")
    # Lock file may or may not exist depending on filelock impl; if exists it must be unlocked
    if lock_path.exists():
        import filelock

        fl = filelock.FileLock(str(lock_path), timeout=1)
        fl.acquire()
        fl.release()
