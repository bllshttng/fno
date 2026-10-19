"""A bounded codex pane parks on the Hooks need review modal and blows the
bind window when the modal paints after the readiness gate's single frame
read (measured live on codex 0.156.1: 60s of `daemon: no new codex session
for this cwd`). The bind probe answers it once from its decline path with the
modal's option 3, continue WITHOUT trusting: untrusted hooks do not run,
which is the posture a bounded pane already implies. Answering lives in the
probe, not the generic wait loop, so the loop stays provider-agnostic and
mux_spawn keeps no delta against main (operator file-budget order). The
pre-payload posture is untouched: the readiness gate still refuses and reaps,
and bypass postures still carry --dangerously-bypass-hook-trust. The answer
is sent exactly once and never retried - a raised send may still have landed,
and a second `3` into a closed modal would type a turn; a still-parked later
tick downgrades to a condition note instead of a second keystroke.
"""

from __future__ import annotations

import subprocess
from pathlib import Path

import fno.agents.mux_spawn as mux_spawn
from fno.agents.codex_pane import _make_codex_bind_probe


def test_codex_hook_modal_probe_answers_once_then_never_again(monkeypatch):
    """The bind probe answers a hooks-review modal exactly once with option 3,
    records the answer in the condition sink, and on a still-parked later tick
    downgrades to a note instead of a second keystroke."""
    monkeypatch.setattr(mux_spawn, "_backfill_codex_session_id", lambda *a, **k: None)

    sends: list[list[str]] = []
    reads = {"modal": True}

    def runner(argv, **kwargs):
        if argv[1:4] == ["mux", "pane", "read"]:
            frame = "Hooks need review\n" if reads["modal"] else "ready\n"
            return subprocess.CompletedProcess(argv, 0, frame, "")
        if argv[1:4] == ["mux", "pane", "send"]:
            sends.append(list(argv))
            return subprocess.CompletedProcess(argv, 0, "", "")
        return subprocess.CompletedProcess(argv, 0, "", "")

    condition: list = []
    probe = _make_codex_bind_probe(
        cwd=Path("/tmp"),
        spawn_started_ms=0,
        child_pid=1,
        codex_sessions_dir=None,
        daemon_baseline_ids=set(),
        mux={"session": "sess", "pane_id": 7},
        runner=runner,
        condition=condition,
    )

    assert probe() is None
    assert len(sends) == 1
    assert sends[0][sends[0].index("--text") + 1] == "3"
    assert "--submit" in sends[0] and "--raw" in sends[0]
    assert "answered once" in condition[0]

    assert probe() is None
    assert len(sends) == 1
    assert "still parked" in condition[0]

    reads["modal"] = False
    assert probe() is None
    assert len(sends) == 1
    assert "still parked" not in condition[0]
