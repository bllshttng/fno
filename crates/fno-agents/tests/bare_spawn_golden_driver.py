#!/usr/bin/env python3
"""Capture driver for the `bare_spawn` goldens.

The Python leg these cases froze is `cmd_spawn`'s bare-spawn admission
(`fno.agents.cli`): the thread seat and fence refusal, the `--wait` grammar
and the gate's waitable reasons, the sandbox write policy refusals and the
prompt-file reader. Every case refuses or answers before anything launches.
`bare_spawn_parity.rs` replays each file against `spawn_bare.rs`. Re-run only
at a commit that still has `cmd_spawn`; run it under a scratch HOME so no
operator override reaches the capability table.

    HOME=$(mktemp -d) PYTHONPATH=cli/src python3 crates/fno-agents/tests/bare_spawn_golden_driver.py
"""

from __future__ import annotations

import io
import json
import sys
import tempfile
from contextlib import redirect_stderr
from pathlib import Path

sys.path.insert(0, "cli/src")

from fno.agents.cli import _parse_wait_seconds  # noqa: E402
from fno.agents.harness_map import (  # noqa: E402
    DispatchResolveError,
    capabilities,
    fence_uncarried,
    known_harnesses,
    thread_seatable,
)
from fno.agents.model_routing import load_sandbox_write_policy  # noqa: E402
from fno.agents.spawn_gate import WAITABLE_REFUSAL_REASONS  # noqa: E402

GOLDEN = Path(__file__).resolve().parent / "golden" / "bare_spawn"


def thread_line(harness: str, fenced: list[str]) -> str | None:
    """cmd_spawn's thread refusal (cli.py, `if substrate == "thread":`)."""
    try:
        capabilities(harness)
    except DispatchResolveError:
        # An unknown harness has no lane at all; the roster is the remediation.
        roster = ", ".join(known_harnesses())
        return (
            f"fno agents spawn: unknown harness {harness!r}; "
            f"the capability contract knows: {roster}"
        )
    uncarried = fence_uncarried(harness, fenced)
    seatable = thread_seatable(harness)
    why = f"has no carrier for {uncarried}" if uncarried else ""
    if why or not seatable:
        why = why or "is not seatable"
        # A seatable harness's pane lane carries what its fence cannot; a
        # not-seatable one (footnote, muse) seats headless, its resolver
        # fallback. The thread_footnote/thread_muse goldens were added by
        # hand after the capture driver lost its cmd_spawn oracle.
        tail = "add --as pane" if seatable else "add --as headless"
        return f"fno agents spawn: the {harness} thread lane {why}; {tail}"
    return None


def refusal(call) -> dict:
    """Run a leg that exits; the exit and its stderr are the contract."""
    err = io.StringIO()
    try:
        with redirect_stderr(err):
            call()
    except SystemExit as exc:
        return {"exit": exc.code, "stderr": err.getvalue()}
    raise SystemExit(f"{call} did not refuse")


def main() -> int:
    GOLDEN.mkdir(parents=True, exist_ok=True)
    cases: dict[str, dict] = {}
    for harness in ("claude", "codex", "opencode", "pi", "agy", "gemini"):
        cases[f"thread_{harness}"] = {"harness": harness, "fenced": [], "line": thread_line(harness, [])}
    for name, harness, fenced in (
        ("fence_codex_bogus", "codex", ["--bogus-flag"]),
        ("fence_codex_carried", "codex", ["-c", "model_reasoning_effort=high"]),
        ("fence_opencode_bogus", "opencode", ["--bogus-flag"]),
    ):
        cases[name] = {"harness": harness, "fenced": fenced, "line": thread_line(harness, fenced)}
    waits = {}
    for raw in ("90", "1.5m", "2H", " 5m ", "0", "", "m", "1.", ".5m", "1d", "5 m", "-5"):
        try:
            waits[raw] = _parse_wait_seconds(raw)
        except ValueError:
            waits[raw] = None
    cases["wait_grammar"] = {"waits": waits}
    cases["waitable_reasons"] = {"reasons": sorted(WAITABLE_REFUSAL_REASONS)}
    with tempfile.TemporaryDirectory() as tmp:
        for name, body in (
            ("sandbox_disabled", {"sandbox": {"enabled": False}}),
            ("sandbox_missing", {"deny_edit": []}),
        ):
            path = Path(tmp) / "policy.json"
            path.write_text(json.dumps(body))
            out = refusal(lambda: load_sandbox_write_policy(str(path)))
            out["stderr"] = out["stderr"].replace(str(path), "<path>")
            cases[name] = {"policy": body, **out}
    for name, out in cases.items():
        (GOLDEN / f"{name}.json").write_text(json.dumps(out, indent=2, sort_keys=True) + "\n")
        print(f"captured {name}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
