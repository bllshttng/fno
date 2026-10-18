from __future__ import annotations

from pathlib import Path


def _receipt(monkeypatch, tmp_path: Path, seed: str, fake_decisions):
    from fno.think_inspect import build_receipt

    # The fakes keep the old list_decisions shape (subject, rows, damaged);
    # the adapter unwraps the rows the decisions door answers with.
    monkeypatch.setattr(
        "fno.rust_binary.call_front_json",
        lambda payload: {"decisions": fake_decisions(payload["argv"][0])[1]},
    )
    repo = tmp_path / "repo"
    repo.mkdir()
    graph = [
        {
            "id": "x-38d3",
            "slug": "spike-keep-fno-mail",
            "title": "SPIKE",
            "status": "done",
            "domain": "code",
        },
        {
            "id": "x-953b",
            "slug": "no-decisions",
            "title": "No decisions",
            "status": "ready",
            "domain": "code",
        },
    ]

    def run(argv, cwd=None, timeout=10):
        import subprocess

        return subprocess.CompletedProcess(argv, 1, "", "unavailable")

    return build_receipt(
        seed,
        repo=repo,
        graph_entries=graph,
        archive_entries=[],
        plans_path=tmp_path / "plans",
        home=tmp_path,
        run=run,
    )


def test_superseded_ruling_is_dropped_by_the_derived_field(monkeypatch, tmp_path: Path) -> None:
    def fake(subject, limit=None, lane=None, state=None):
        withdrawn = {
            "decision_id": "d-85352ee7",
            "ts": "2026-08-22T12:12:00Z",
            "lane": "coord",
            "subject": "x-38d3",
            # Prose claims supersession, but the derived field is what the reader
            # trusts. The decisions door derives superseded_by from `supersedes`
            # ACROSS the index, so a withdrawn row carries it here even though
            # its own text never says so.
            "decision": "HYBRID",
            "supersedes": None,
            "superseded_by": "d-62cd9d80",
        }
        winner = {
            "decision_id": "d-62cd9d80",
            "ts": "2026-08-22T12:27:04Z",
            "lane": "coord",
            "subject": "x-38d3",
            "decision": "CORRECTION, SUPERSEDES d-85352ee7. WITHDRAWN.",
            "supersedes": "d-85352ee7",
            "superseded_by": None,
        }
        return subject, [winner, withdrawn], 0

    receipt = _receipt(monkeypatch, tmp_path, "x-38d3", fake)

    ids = [row["decision_id"] for row in receipt["graph"]["decisions"]]
    assert "d-62cd9d80" in ids
    assert "d-85352ee7" not in ids


def test_unreadable_decision_index_reports_error_not_empty(monkeypatch, tmp_path: Path) -> None:
    def fake(subject, limit=None, lane=None, state=None):
        raise OSError("index corrupt")

    receipt = _receipt(monkeypatch, tmp_path, "x-38d3", fake)

    assert receipt["graph"]["decisions"] == []
    assert receipt["graph"]["decisions_status"] == "error"
    assert (
        receipt["graph"]["decisions_detail"]
        and "index corrupt" in receipt["graph"]["decisions_detail"]
    )


def test_receipt_reports_unknown_lifecycle_as_error(monkeypatch, tmp_path: Path) -> None:
    reason = "the graph could not be read (database is locked)"

    def fake(subject, limit=None, lane=None, state=None):
        return (
            subject,
            [
                {
                    "decision_id": "d-unknown",
                    "lane": "coord",
                    "subject": subject,
                    "decision": "coord ruling",
                    "lifecycle": "unknown",
                    "lifecycle_reason": reason,
                }
            ],
            0,
        )

    receipt = _receipt(monkeypatch, tmp_path, "x-38d3", fake)
    graph = receipt["graph"]

    assert graph["decisions_status"] == "error"
    assert graph["decisions"][0]["lifecycle"] == "unknown"
    assert graph["decisions_detail"] == (
        f"{reason}; 1 coord ruling(s) listed with an unknown lifecycle"
    )
