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

def test_mail_inject_codex_preserves_rust_refusal_reason(monkeypatch):
    from fno.agents import dispatch

    expected = (
        "native-command: use fno mux command <selector> --text <verb> "
        "--proof <compact|goal-active|screen>"
    )
    monkeypatch.setattr(
        "fno.rust_binary.resolve_installed_binary", lambda: Path("/bin/fno-agents")
    )
    monkeypatch.setattr(dispatch, "_delivery_policy_refusal", lambda *_a: None)
    monkeypatch.setattr(
        dispatch.subprocess,
        "run",
        lambda argv, **kwargs: subprocess.CompletedProcess(
            argv,
            1,
            stdout=json.dumps({"delivered": False, "reason": expected}),
            stderr="",
        ),
    )
    reason = []

    assert not dispatch._mail_inject_codex(SID_CODEX, "/compact", reason_out=reason)
    assert reason == [expected]


def test_review_start_codex_uses_structured_binary_argv(monkeypatch):
    from fno.agents import dispatch

    monkeypatch.setattr(
        "fno.rust_binary.resolve_installed_binary", lambda: Path("/bin/fno-agents")
    )
    seen = {}

    def fake_run(argv, **kwargs):
        seen.update(argv=argv, kwargs=kwargs)
        return subprocess.CompletedProcess(
            argv,
            0,
            stdout=json.dumps(
                {
                    "delivered": True,
                    "turn_id": "turn-4",
                    "review_thread_id": "review-4",
                }
            ),
            stderr="",
        )

    monkeypatch.setattr(dispatch.subprocess, "run", fake_run)
    receipt = dispatch._review_start_codex(
        SID_CODEX,
        "baseBranch:origin/main",
        audit_payload="/review --base origin/main",
        audit_sender="sender-1",
        audit_target_cwd="/repo",
    )
    assert receipt["delivered"] is True
    assert seen["argv"] == [
        "/bin/fno-agents",
        "review-start",
        "--session",
        SID_CODEX,
        "--target",
        "baseBranch:origin/main",
        "--delivery",
        "inline",
        "--audit-payload",
        "/review --base origin/main",
        "--audit-sender",
        "sender-1",
        "--audit-target-cwd",
        "/repo",
    ]
    assert seen["kwargs"]["timeout"] == dispatch._MAIL_INJECT_TIMEOUT_S


def test_review_start_codex_preserves_no_daemon_reason(monkeypatch):
    from fno.agents import dispatch

    monkeypatch.setattr(
        "fno.rust_binary.resolve_installed_binary", lambda: Path("/bin/fno-agents")
    )
    monkeypatch.setattr(
        dispatch.subprocess,
        "run",
        lambda argv, **kwargs: subprocess.CompletedProcess(
            argv,
            1,
            stdout='{"delivered":false,"reason":"no-daemon"}',
            stderr="",
        ),
    )
    assert dispatch._review_start_codex(SID_CODEX, "uncommittedChanges") == {
        "delivered": False,
        "reason": "no-daemon",
    }


@pytest.mark.parametrize(
    ("returncode", "receipt", "reason"),
    [
        (0, {"delivered": "false"}, "rpc-error"),
        (0, {"delivered": True, "turn_id": "turn-only"}, "not-confirmed"),
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

    monkeypatch.setattr(
        "fno.rust_binary.resolve_installed_binary", lambda: Path("/bin/fno-agents")
    )
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
