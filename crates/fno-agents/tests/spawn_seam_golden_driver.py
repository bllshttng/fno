#!/usr/bin/env python3
"""Capture driver for the `spawn_seam` goldens.

The Python legs these cases froze are the spawn seam this row deletes:
`spawn_defaults._scan_projection` and `normalize_spawn_args` (the argv facts
the compose transport sent and the normalization it ran first) and the three
pre-route seed refusals (`seedless_thread_refusal`, `lost_command_refusal`,
`cannot_fire_refusal`). They are deleted when the client's in-process
admission takes them over, so this driver re-runs only at a commit that
still has those legs.
`spawn_seam_parity.rs` replays each file against the Rust owners.

    FNO_CAPTURE_GOLDEN=1 PYTHONPATH=cli/src python3 crates/fno-agents/tests/spawn_seam_golden_driver.py
"""

from __future__ import annotations

import json
import random
import sys
from pathlib import Path

sys.path.insert(0, "cli/src")

from fno.agents.harness_map import cannot_fire_refusal, lost_command_refusal  # noqa: E402
from fno.agents.spawn_defaults import (  # noqa: E402
    _scan_projection,
    normalize_spawn_args,
    seedless_thread_refusal,
)

GOLDEN = Path(__file__).resolve().parent / "golden" / "spawn_seam"

# name -> case fields. `op` picks the leg; the rest are its inputs. Argv
# shapes cover the axes the scan projects and the normalization rewrites:
# name minting, the -m shorthand, the fenced payload, each substrate
# spelling, role/route/account/resume, the retired axes and the seed slots.
CASES = {
    "scan_bare_message": dict(
        op="scan", argv=["spawn", "check the failing test"],
    ),
    "scan_named_with_flags": dict(
        op="scan",
        argv=["spawn", "--name", "w1", "-H", "codex", "seed text", "--", "--exec"],
    ),
    "scan_model_shorthand": dict(
        op="scan", argv=["spawn", "-m", "glm-5.2", "go"],
    ),
    "scan_model_behind_fence": dict(
        op="scan", argv=["spawn", "go", "--", "--model", "glm-5.2"],
    ),
    "scan_pane_explicit": dict(
        op="scan", argv=["spawn", "--as", "pane", "--split", "left", "hi"],
    ),
    "scan_headless_once": dict(
        op="scan", argv=["spawn", "--once", "hi"],
    ),
    "scan_cloud": dict(
        op="scan", argv=["spawn", "--as", "cloud", "hi"],
    ),
    "scan_role_lead": dict(
        op="scan", argv=["spawn", "--role", "lead", "hi"],
    ),
    "scan_route_value": dict(
        op="scan", argv=["spawn", "--route", "zai/glm-5.2", "hi"],
    ),
    "scan_account_flag": dict(
        op="scan", argv=["spawn", "--account", "makers", "hi"],
    ),
    "scan_dispatch_account_pane": dict(
        op="scan",
        argv=["spawn", "--dispatch-account", "work", "--as", "pane", "hi"],
    ),
    "scan_resume": dict(
        op="scan", argv=["spawn", "--resume", "abcd1234", "follow up"],
    ),
    "scan_retired_provider": dict(
        op="scan", argv=["spawn", "--provider", "zai", "hi"],
    ),
    "scan_permission_yolo": dict(
        op="scan", argv=["spawn", "-Y", "hi"],
    ),
    "scan_node_flag": dict(
        op="scan", argv=["spawn", "--node", "x-9a6b", "hi"],
    ),
    "scan_node_seed_id": dict(
        op="scan", argv=["spawn", "x-9a6b build the door"],
    ),
    "scan_argv_payload": dict(
        op="scan", argv=["spawn", "hi", "--argv", "some", "payload"],
    ),
    "scan_explicit_thread": dict(
        op="scan", argv=["spawn", "--as", "thread", "hi"],
    ),
    "normalize_mints_name": dict(
        op="normalize", argv=["spawn", "check the failing test"],
    ),
    "normalize_keeps_name": dict(
        op="normalize", argv=["spawn", "--name", "w1", "hi"],
    ),
    "normalize_fence_shorthand": dict(
        op="normalize", argv=["spawn", "-m", "glm-5.2", "hi"],
    ),
    "normalize_second_positional": dict(
        op="normalize", argv=["spawn", "one", "two"],
    ),
    "normalize_payload_untouched": dict(
        op="normalize", argv=["spawn", "hi", "--", "codex", "-m", "x"],
    ),
    "refusal_seedless_thread": dict(
        op="refusal_seedless", harness="claude", substrate="thread",
    ),
    "refusal_seedless_pane_no_seed": dict(
        op="refusal_seedless", harness="claude", substrate="pane",
    ),
    "refusal_seedless_resume": dict(
        op="refusal_seedless", harness="claude", substrate="thread", resume="abcd1234",
    ),
    "refusal_lost_command": dict(
        op="refusal_lost", seed="$fno:target scratch",
    ),
    "refusal_lost_bash_eaten": dict(
        op="refusal_lost", seed=":target x-9a6b",
    ),
    "refusal_lost_zsh_eaten": dict(
        op="refusal_lost", seed="arget x-9a6b",
    ),
    "refusal_seedless_named": dict(
        op="refusal_seedless", harness="claude", substrate="bg", name="w1", node="x-9a6b",
    ),
    "refusal_lost_plain_seed": dict(
        op="refusal_lost", seed="plain text seed",
    ),
    "refusal_cannot_fire_codex": dict(
        op="refusal_cannot", seed="/fno:review medium", harness="codex",
    ),
    "refusal_cannot_fire_claude": dict(
        op="refusal_cannot", seed="/fno:review medium", harness="claude",
    ),
}


def main() -> int:
    GOLDEN.mkdir(parents=True, exist_ok=True)
    for name, case in CASES.items():
        op = case["op"]
        if op == "scan":
            argv = [str(a) for a in case["argv"]]
            out = {"argv": argv, "scan": _scan_projection(argv[1:])}
        elif op == "normalize":
            argv = [str(a) for a in case["argv"]]
            err = NullErr()
            try:
                minted = normalize_spawn_args(
                    argv,
                    resolver=lambda _session: None,
                    existing_names={"w1", "taken-name"},
                    rng=random.Random(7),
                    stderr=err,
                )
                out = {"argv": minted, "stderr": err.lines}
            except SystemExit as exc:
                # A refusal IS the behavior: the exit and the note it printed
                # are the contract the Rust port must replay.
                out = {"exit": exc.code, "stderr": err.lines}
        elif op == "refusal_seedless":
            text = seedless_thread_refusal(
                case["harness"],
                case["substrate"],
                case.get("seed"),
                resume=case.get("resume"),
                role=case.get("role", False),
                name=case.get("name"),
                node=case.get("node"),
            )
            out = {"text": text}
        elif op == "refusal_lost":
            out = {"text": lost_command_refusal(case["seed"])}
        elif op == "refusal_cannot":
            out = {"text": cannot_fire_refusal(case["seed"], case["harness"])}
        else:  # pragma: no cover - the table above is closed
            raise SystemExit(f"unknown op {op}")
        (GOLDEN / f"{name}.json").write_text(json.dumps(out, indent=2, sort_keys=True) + "\n")
        print(f"captured {name}")
    return 0


class NullErr:
    """A stderr sink the normalize battery writes its notes into."""

    def __init__(self) -> None:
        self.lines: list[str] = []

    def write(self, line: str) -> int:
        self.lines.append(line)
        return len(line)


if __name__ == "__main__":
    raise SystemExit(main())
