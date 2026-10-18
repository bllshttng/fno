"""Tests for fno.worker.blueprint and fno.worker.ship."""

from __future__ import annotations

import json
from pathlib import Path
from unittest.mock import MagicMock, patch

import pytest


@pytest.fixture(autouse=True)
def _trailer_ensure_passthrough(monkeypatch):
    """Answer ship's trailer write in process.

    The closure producer lives in the fno-agents binary now; ship reaches it
    only through `fno.rust_binary.verb_call`. These tests mock subprocess.run
    with a fixed list of side effects, so a real binary call would consume one
    and every later assertion shifts by one call. Passthrough here: this file
    tests ship's PR logic, and the trailer grammar has its own suite.
    """
    import fno.rust_binary as rb

    def _passthrough(verb, payload, unavailable=None, **kwargs):
        if verb == "pr-base-check":
            # The stale-base guard answers from the binary now. Every test in
            # this file wants the clear pass; a refusal case patches this seam
            # itself.
            return {"code": 0, "message": None}
        assert verb == "pr-closure-ensure", verb
        return {"body": payload["body"]}

    monkeypatch.setattr(rb, "verb_call", _passthrough)


@pytest.fixture(autouse=True)
def _verified_head(monkeypatch):
    monkeypatch.setattr(
        "fno.pr._preflight.local_verification_required",
        lambda **_kwargs: (True, "required"),
    )
    monkeypatch.setattr(
        "fno.pr._preflight.check_verification_evidence",
        lambda **_kwargs: {
            "satisfied": True,
            "mode": "full",
            "result": "passed",
        },
    )


# ---- Helpers ----


def _make_state(tmp_path: Path, extra: dict | None = None) -> Path:
    """Create a minimal target-state.md in tmp_path/.fno/."""
    state_dir = tmp_path / ".fno"
    state_dir.mkdir(parents=True, exist_ok=True)
    state = {
        "status": "IN_PROGRESS",
        "session_id": "20260421T120000Z-99999-aabbcc",
        "artifact_shipped": False,
        "auto_merge_approved": False,
        "pr_number": None,
    }
    if extra:
        state.update(extra)
    import yaml

    content = "---\n" + yaml.dump(state, default_flow_style=False) + "---\n# State\n"
    path = state_dir / "target-state.md"
    path.write_text(content)
    return path


# ---- AC3-EDGE: ship is idempotent (no duplicate PR) ----


def test_ac3_edge_ship_no_duplicate_pr(tmp_path, monkeypatch):
    """ship() detects existing PR and does NOT call gh pr create again."""
    monkeypatch.chdir(tmp_path)
    state_path = _make_state(tmp_path)

    existing_pr = [{"number": 55, "url": "https://github.com/owner/repo/pull/55", "state": "OPEN"}]
    mock_run = MagicMock()
    # All subprocess.run calls return the same mock (pr list returns existing PR,
    # git rev-parse also returns OK - use side_effect for ordered calls)
    mock_run.side_effect = [
        MagicMock(returncode=0, stdout="feature/test\n", stderr=""),  # git rev-parse
        MagicMock(returncode=0, stdout=json.dumps(existing_pr), stderr=""),  # gh pr list
    ]

    with patch("subprocess.run", mock_run):
        from importlib import reload
        import fno.worker.ship as ship_mod

        reload(ship_mod)
        result = ship_mod.ship(
            state_path=state_path,
            title="feat: idempotent",
            body="body",
            artifacts_dir=tmp_path / ".fno" / "artifacts",
        )

    # Should return existing PR info
    assert result["pr_number"] == 55
    # Only 2 calls made (git + gh pr list), no gh pr create
    assert mock_run.call_count == 2
    calls = [str(c) for c in mock_run.call_args_list]
    assert not any("create" in c for c in calls)


def test_existing_pr_still_requires_current_verification(tmp_path, monkeypatch):
    monkeypatch.chdir(tmp_path)
    state_path = _make_state(tmp_path)
    existing_pr = [{"number": 55, "url": "https://github.com/owner/repo/pull/55"}]
    mock_run = MagicMock()
    mock_run.side_effect = [
        MagicMock(returncode=0, stdout="feature/test\n", stderr=""),
        MagicMock(returncode=0, stdout=json.dumps(existing_pr), stderr=""),
    ]
    monkeypatch.setattr(
        "fno.pr._preflight.check_verification_evidence",
        lambda **_kwargs: {"satisfied": False, "mode": "subset", "result": "passed"},
    )

    with patch("subprocess.run", mock_run):
        from fno.worker.ship import ship

        result = ship(
            state_path=state_path,
            title="feat: existing",
            body="body",
            artifacts_dir=tmp_path / ".fno" / "artifacts",
        )

    assert result["action"] == "error"
    assert "mode=subset result=passed" in result["error"]
    assert not list((tmp_path / ".fno" / "artifacts").glob("ship-*.md"))


# ---- AC1-HP (x-1951): ship never pre-authorizes a merge ----


def test_ac1_hp_ship_does_not_arm_automerge(tmp_path, monkeypatch):
    """Even with auto_merge_approved=true, ship() must NOT call `gh pr merge`.

    Arming at PR-creation time hands the merge timing to GitHub before any gate
    has run, so a reviewer posting a blocking finding after CI greens loses the
    race. The arming lives in `fno-agents finalize`'s DonePRGreen branch now.
    """
    monkeypatch.chdir(tmp_path)
    state_path = _make_state(tmp_path, {"auto_merge_approved": True})

    mock_run = MagicMock()
    # Calls: git rev-parse, gh pr list, gh pr create. The trailing entry is a
    # deliberate slack slot: without it a regression that re-arms would die on
    # StopIteration, and this test would pass-by-accident for the wrong reason.
    # With it, the regression runs to completion and trips the assertion below.
    mock_run.side_effect = [
        MagicMock(returncode=0, stdout="feature/test\n", stderr=""),  # git rev-parse
        MagicMock(returncode=0, stdout="[]", stderr=""),  # gh pr list
        MagicMock(
            returncode=0, stdout="https://github.com/owner/repo/pull/77", stderr=""
        ),  # gh pr create
        MagicMock(returncode=0, stdout="", stderr=""),  # unused slack
    ]

    with (
        patch("subprocess.run", mock_run),
        patch("fno.pr._preflight.check_duplicate_pr", return_value=""),
    ):
        from importlib import reload
        import fno.worker.ship as ship_mod

        reload(ship_mod)
        result = ship_mod.ship(
            state_path=state_path,
            title="feat: automerge",
            body="body",
            artifacts_dir=tmp_path / ".fno" / "artifacts",
        )

    assert result["pr_number"] == 77
    argvs = [call.args[0] for call in mock_run.call_args_list if call.args]
    assert not any(argv[:3] == ["gh", "pr", "merge"] for argv in argvs), (
        f"ship must not arm auto-merge; saw {argvs}"
    )
    # The reporting keys went with the behavior - a permanently-false
    # `auto_merge_armed` would read as "not armed" rather than "not armed here".
    assert "auto_merge_armed" not in result
    assert "auto_merge_error" not in result


# ---- x-a166: ship stamps the backlog node <-> PR link ----


def _make_state_with_node(tmp_path: Path, node_id: str) -> Path:
    """target-state.md with graph_node_id appended to the BODY (below the
    frontmatter), exactly as init-target-state.sh writes it."""
    path = _make_state(tmp_path)
    with path.open("a", encoding="utf-8") as fh:
        fh.write(f"graph_node_id: {node_id}\n")
    return path


def test_ac2_err_stamp_failure_reports_incomplete_delivery(tmp_path, monkeypatch):
    """AC2-ERR: a created PR with no graph binding is incomplete delivery."""
    monkeypatch.chdir(tmp_path)
    state_path = _make_state_with_node(tmp_path, "x-1a2b")

    mock_run = MagicMock()
    mock_run.side_effect = [
        MagicMock(returncode=0, stdout="feature/test\n", stderr=""),  # git rev-parse
        MagicMock(returncode=0, stdout="[]", stderr=""),  # gh pr list
        MagicMock(
            returncode=0, stdout="https://github.com/owner/repo/pull/42", stderr=""
        ),  # gh pr create
        MagicMock(returncode=1, stdout="", stderr="graph locked"),  # fno backlog update FAILS
    ]

    with (
        patch("subprocess.run", mock_run),
        patch("fno.pr._preflight.check_duplicate_pr", return_value=""),
    ):
        from fno.worker.ship import ship

        result = ship(
            state_path=state_path,
            title="feat: test",
            body="body",
            artifacts_dir=tmp_path / ".fno" / "artifacts",
        )

    assert result["action"] == "incomplete_delivery"
    assert result["pr_number"] == 42
    assert result["binding_error"] == "graph locked"
    assert result["repair_command"] == (
        "fno do pr bind-created --url "
        "https://github.com/owner/repo/pull/42 --repo "
        f"{tmp_path} --node x-1a2b"
    )


# ---- AC1-UI: a guard refusal short-circuits before gh pr create ----


def test_guard_refusal_blocks_pr_create(tmp_path, monkeypatch):
    """A guard refusal (stale base, or a duplicate PR) makes ship() error out
    with the refusal text and never reach gh pr create."""
    monkeypatch.chdir(tmp_path)
    state_path = _make_state(tmp_path)

    mock_run = MagicMock()
    mock_run.side_effect = [
        MagicMock(returncode=0, stdout="feature/test\n", stderr=""),  # git rev-parse
        MagicMock(returncode=0, stdout="[]", stderr=""),  # gh pr list
    ]
    refusal = "stale base: your branch's merge-base with origin/main is 3.0 days behind"

    with (
        patch("subprocess.run", mock_run),
        patch(
            "fno.rust_binary.verb_call",
            lambda verb, payload, **k: {"code": 3, "message": refusal},
        ),
        patch("fno.pr._preflight.check_duplicate_pr", return_value=""),
    ):
        from importlib import reload
        import fno.worker.ship as ship_mod

        reload(ship_mod)
        result = ship_mod.ship(
            state_path=state_path,
            title="feat: stale",
            body="body",
            artifacts_dir=tmp_path / ".fno" / "artifacts",
        )

    assert result["action"] == "error"
    assert refusal in result["error"]
    # gh pr create was never invoked (only git rev-parse + gh pr list ran).
    assert mock_run.call_count == 2
    assert not any("create" in str(c) for c in mock_run.call_args_list)

    # The duplicate guard's refusal maps to the same error dict and the same
    # never-create posture - the check that would have stopped the 2026-10-04
    # twins on this path.
    mock_run = MagicMock()
    mock_run.side_effect = [
        MagicMock(returncode=0, stdout="feature/test\n", stderr=""),  # git rev-parse
        MagicMock(returncode=0, stdout="[]", stderr=""),  # gh pr list
    ]
    dup_refusal = "pr-create: REFUSED: an open PR already touches the same changed files"

    with (
        patch("subprocess.run", mock_run),
        patch(
            "fno.rust_binary.verb_call",
            lambda verb, payload, **k: (
                {"code": 0, "message": None}
                if verb == "pr-base-check"
                else {"body": payload["body"]}
            ),
        ),
        patch("fno.pr._preflight.check_duplicate_pr", return_value=dup_refusal),
    ):
        from fno.worker.ship import ship

        result = ship(
            state_path=state_path,
            title="feat: twin",
            body="body",
            artifacts_dir=tmp_path / ".fno" / "artifacts",
        )

    assert result["action"] == "error"
    assert dup_refusal in result["error"]
    assert mock_run.call_count == 2
    assert not any("create" in str(c) for c in mock_run.call_args_list)


def test_non_full_verification_refuses_pr_create(tmp_path, monkeypatch):
    monkeypatch.chdir(tmp_path)
    state_path = _make_state(tmp_path)
    mock_run = MagicMock()
    mock_run.side_effect = [
        MagicMock(returncode=0, stdout="feature/test\n", stderr=""),
        MagicMock(returncode=0, stdout="[]", stderr=""),
    ]
    monkeypatch.setattr(
        "fno.pr._preflight.check_verification_evidence",
        lambda **_kwargs: {"satisfied": False, "mode": "subset", "result": "passed"},
    )

    with (
        patch("subprocess.run", mock_run),
        patch("fno.pr._preflight.check_duplicate_pr", return_value=""),
    ):
        from fno.worker.ship import ship

        result = ship(
            state_path=state_path,
            title="feat: unverified",
            body="body",
            artifacts_dir=tmp_path / ".fno" / "artifacts",
        )

    assert result["action"] == "error"
    assert "mode=subset result=passed" in result["error"]
    assert mock_run.call_count == 2


# ---- F2: incarnation fence blocks PR creation ----


def test_incarnation_fence_blocks_pr_create(tmp_path, monkeypatch):
    """F2: a losing incarnation (its session claim held by another) never reaches
    gh pr create / gh pr list / git rev-parse - the fence refuses first."""
    monkeypatch.chdir(tmp_path)
    state_path = _make_state(tmp_path)

    mock_run = MagicMock()  # any subprocess.run call means an outward action slipped through

    with (
        patch("subprocess.run", mock_run),
        patch("fno.claims.incarnation.resolve_fence_session_uuid", return_value="transcript-uuid"),
        patch(
            "fno.claims.incarnation.incarnation_fence_blocks",
            return_value=(True, "incarnation-fence: session:transcript-uuid held by other"),
        ),
    ):
        from fno.worker.ship import ship

        result = ship(
            state_path=state_path,
            title="feat: fenced",
            body="body",
            artifacts_dir=tmp_path / ".fno" / "artifacts",
        )

    assert result["action"] == "blocked"
    assert "incarnation-fence" in result["error"]
    assert mock_run.call_count == 0  # no outward action ran
