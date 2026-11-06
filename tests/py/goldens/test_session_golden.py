"""Golden receipts: blueprint session open / close (the lease lifecycle)."""
from __future__ import annotations

from goldens._door import SESSION_ID, door, make_sandbox, seed_node, warm


def test_session_close_prints_the_completion_receipt_and_launch_line(tmp_path):
    root = make_sandbox(tmp_path, [seed_node("x-ccc33000", "design")])
    warm(root, "x-ccc33000")
    door(root, ["session", "open", "x-ccc33000", "--harness", "claude", "--session-id", SESSION_ID])
    code, out, err = door(
        root,
        [
            "session", "close", "x-ccc33000", "--harness", "claude", "--session-id", SESSION_ID,
            "--summary", "blueprint done",
            "--launch", "claude /fno:target x-ccc33000",
        ],
    )
    assert code == 0, err
    assert out.startswith(f"blueprint closed x-ccc33000 (claude:{SESSION_ID})"), out
    assert "summary: blueprint done\n" in out
    assert "launch: claude /fno:target x-ccc33000\n" in out


def test_session_add_ending_another_sessions_row_names_the_reap_door(tmp_path, monkeypatch):
    owner = "sess-owner-00000000"
    # The ambient identity is NOT the row owner: only the owning session ends
    # its own row, so this synthesized close refuses and names the reap door.
    monkeypatch.setenv("CLAUDE_CODE_SESSION_ID", "sess-other-000000")
    root = make_sandbox(
        tmp_path,
        [
            seed_node(
                "x-bbb22000",
                sessions=[{
                    "phase": "execute",
                    "harness": "claude",
                    "session_id": owner,
                    "started_at": "2026-09-20T00:00:00Z",
                }],
            )
        ],
    )
    warm(root, "x-bbb22000")
    code, out, err = door(
        root,
        [
            "session", "add", "x-bbb22000",
            "--phase", "execute",
            "--harness", "claude", "--session-id", owner,
            "--ended-at", "2026-09-27T00:00:00Z",
        ],
    )
    assert code == 2, out
    assert "owns an open execute row on x-bbb22000" in err
    assert "only that session ends it here" in err
    assert "fno backlog session reap-open x-bbb22000 --phase execute" in err
