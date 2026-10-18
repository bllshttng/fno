"""Tests for the `doc` deliverable (US3): brief + sidecar -> output_dir.

Covers AC1 (cited brief + sidecar written, DoneAdvisory), AC3 (no-sources brief
stamped, not a crash), AC4 (round-cap stop stamped), AC5 (output_dir unset =>
fail loud, never guess).
"""

from __future__ import annotations

import json
import subprocess
from pathlib import Path

import pytest

from fno.research import deliverable as deli
from fno.research.core import Source


def _write_sources(path: Path, rows: list[Source]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("w", encoding="utf-8") as fh:
        for r in rows:
            fh.write(r.to_json_line() + "\n")


def _verified(url: str, extract: str) -> Source:
    return Source(
        url=url, fetched_at="2026-06-23T00:00:00+00:00", hash="abc", extract=extract, verified=True
    )


# --------------------------------------------------------------------------- #
# build_brief - claims cite sources, frontmatter stamps the stop reason
# --------------------------------------------------------------------------- #


def test_build_brief_cites_every_verified_source() -> None:
    srcs = [
        _verified("https://a.example/x", "alpha finding text"),
        _verified("https://b.example/y", "beta finding text"),
    ]
    md = deli.build_brief("my research topic", "my-research-topic", srcs, stopped="declared")
    # frontmatter records topic + stop reason + sidecar name
    assert "stopped: declared" in md
    assert "my-research-topic.sources.jsonl" in md
    # one cited claim per verified source; citations resolve in a Sources section
    assert "[S1]" in md and "[S2]" in md
    assert "[S1]: https://a.example/x" in md
    assert "[S2]: https://b.example/y" in md


def test_build_brief_skips_unverified_sources() -> None:
    srcs = [
        _verified("https://a.example/x", "alpha"),
        Source(
            url="https://dead.example",
            fetched_at="t",
            hash="",
            extract="",
            verified=False,
            reason="http 404",
        ),
    ]
    md = deli.build_brief("topic two words", "topic-two-words", srcs, stopped="declared")
    # the dead source is not cited as a claim (only verified rows become claims)
    assert "https://dead.example" not in md
    assert md.count("[S1]:") == 1


# --------------------------------------------------------------------------- #
# deliver - the end-to-end ship step
# --------------------------------------------------------------------------- #


def test_deliver_writes_brief_and_sidecar(tmp_path: Path) -> None:
    """AC1: deliver writes <slug>.md + <slug>.sources.jsonl to output_dir and
    reports DoneAdvisory."""
    cache = tmp_path / "cache" / "my-topic-words.sources.jsonl"
    _write_sources(cache, [_verified("https://a.example/p", "finding p")])
    out = tmp_path / "out"

    res = deli.deliver(
        "my topic words",
        sources_path=cache,
        stopped="declared",
        output_dir=str(out),
    )

    assert res.terminated == "DoneAdvisory"
    brief = out / "my-topic-words.md"
    sidecar = out / "my-topic-words.sources.jsonl"
    assert brief.is_file() and sidecar.is_file()
    assert Path(res.brief_path) == brief
    # sidecar beside the brief carries the same rows as the cache
    assert json.loads(sidecar.read_text().splitlines()[0])["url"] == "https://a.example/p"


def test_deliver_sidecar_preserves_failed_rows(tmp_path: Path) -> None:
    """The graded sidecar carries the FULL store (incl. verified=false rows) so
    the eval's dead-URL assertion stays meaningful (codex P1 on PR #21)."""
    cache = tmp_path / "cache" / "mixed-topic-words.sources.jsonl"
    _write_sources(
        cache,
        [
            _verified("https://a.example/p", "finding p"),
            Source(
                url="https://dead.example",
                fetched_at="t",
                hash="",
                extract="",
                verified=False,
                reason="http 404",
            ),
        ],
    )
    out = tmp_path / "out"
    deli.deliver("mixed topic words", sources_path=cache, stopped="declared", output_dir=str(out))
    lines = (out / "mixed-topic-words.sources.jsonl").read_text().splitlines()
    urls = {json.loads(ln)["url"] for ln in lines}
    assert "https://dead.example" in urls  # failed row preserved, not dropped
    assert len(lines) == 2


def test_deliver_unset_output_dir_fails_loud(tmp_path: Path) -> None:
    """AC5: deliver with no output_dir raises OutputDirUnset (non-zero upstream)."""
    cache = tmp_path / "c" / "t.sources.jsonl"
    _write_sources(cache, [_verified("https://a.example", "x")])
    with pytest.raises(deli.OutputDirUnset):
        deli.deliver("topic words here", sources_path=cache, stopped="declared", output_dir=None)


def test_deliver_no_sources_still_ships(tmp_path: Path) -> None:
    """AC3: a missing/empty cache still ships a stamped brief, DoneAdvisory."""
    cache = tmp_path / "c" / "none.sources.jsonl"
    cache.parent.mkdir(parents=True)
    cache.touch()
    out = tmp_path / "out"
    res = deli.deliver(
        "nothing found topic", sources_path=cache, stopped="declared", output_dir=str(out)
    )
    assert res.terminated == "DoneAdvisory"
    assert (out / "nothing-found-topic.md").is_file()
    assert "no sources found" in (out / "nothing-found-topic.md").read_text().lower()


# --------------------------------------------------------------------------- #
# deliver(node=...) - x-953b Change 4: bind the shipped doc to its node
# --------------------------------------------------------------------------- #


def test_deliver_with_node_binds_plan_path(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    """A node in scope: deliver shells out to `fno backlog update --plan-path`."""
    cache = tmp_path / "c" / "topic.sources.jsonl"
    _write_sources(cache, [_verified("https://a.example/p", "finding p")])
    calls: list[list[str]] = []

    def fake_run(argv, **kwargs):
        calls.append(argv)
        return subprocess.CompletedProcess(argv, 0, "", "")

    monkeypatch.setattr(deli.subprocess, "run", fake_run)

    res = deli.deliver(
        "topic words", sources_path=cache, stopped="declared", output_dir=str(tmp_path / "out"), node="x-abcd"
    )

    assert res.bind_warning is None
    assert calls == [["fno", "backlog", "update", "x-abcd", "--plan-path", res.brief_path]]
