"""Guards that survived the raw-send port: the codex dispatch seams.

The raw prompt-line send itself now lives in the Rust mail front,
characterized by the parity goldens under
``crates/fno-agents/tests/golden/mail_send_raw/``. What remains here pins the
codex delivery helpers the dispatch layer owns (``_mail_inject_codex`` and
``_review_start_codex``): binary argv shape, rust refusal reasons preserved
verbatim, malformed receipts not read as success, and a child timeout never
reported as confirmed.
"""
from __future__ import annotations

import json
import subprocess
from pathlib import Path

import pytest

SID_CODEX = "019fc973-3401-4abc-9def-0123456789ab"


@pytest.mark.parametrize(
    ("returncode", "receipt", "reason"),
    [
        (0, {"delivered": "false"}, "rpc-error"),
        (
            1,
            {
                "delivered": True,
                "turn_id": "turn-1",
                "review_thread_id": "review-1",
            },
            "not-confirmed",
        ),
    ],
)
def test_review_start_codex_rejects_malformed_success_receipts(
    monkeypatch, returncode, receipt, reason
):
    from fno.agents import dispatch

    monkeypatch.setattr("fno.rust_binary.resolve_installed_binary", lambda: Path("/bin/fno-agents"))
    monkeypatch.setattr(
        dispatch.subprocess,
        "run",
        lambda argv, **kwargs: subprocess.CompletedProcess(
            argv,
            returncode,
            stdout=json.dumps(receipt),
            stderr="",
        ),
    )
    assert dispatch._review_start_codex(SID_CODEX, "uncommittedChanges") == {
        "delivered": False,
        "reason": reason,
    }


def test_review_start_codex_child_timeout_is_not_confirmed(monkeypatch):
    from fno.agents import dispatch

    monkeypatch.setattr(
        "fno.rust_binary.resolve_installed_binary", lambda: Path("/bin/fno-agents")
    )
    monkeypatch.setattr(
        dispatch.subprocess,
        "run",
        lambda *_a, **_k: (_ for _ in ()).throw(
            subprocess.TimeoutExpired("fno-agents", 20)
        ),
    )
    assert dispatch._review_start_codex(SID_CODEX, "uncommittedChanges") == {
        "delivered": False,
        "reason": "not-confirmed",
    }
