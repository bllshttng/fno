#!/usr/bin/env python3
"""Live oracle for the `fno config doctor` port (the port protocol, step 2).

Three modes:

  --fixture NAME ROOT  build case NAME's fixture under ROOT and exit. The
                       parity test calls this (stdlib only, no fno import),
                       so both legs run on one fixture.
  --case NAME ROOT     build the fixture, run the Python `fno config doctor`
                       leg there through the real top-level app, and relay
                       (stdout, stderr, exit code) byte-verbatim.
  --capture DIR        run every case in a fresh tempdir and freeze the
                       normalized (stdout, stderr, exit) triples into DIR.

The Python leg (cli/src/fno/setup/doctor.py and config_doctor.py) is deleted
by the change that ports it, so `--case` and `--capture` only work on the
base commit; after the deletion they refuse. The case table below is the one
source the Rust parity test shares through `--list`.

Every case pins FNO_AGENTS_BIN to a missing path and PATH to the system
dirs, so the reporters that ask the fno-agents binary (route slots, the
harness roster, review capability) froze their unreachable-binary arm, which
is byte-stable on any machine. `--review` has no golden: with no binary the
Python leg died on an uncaught exception, which is not a contract. The
`cause` of a TOML parse error is the parser's own wording, so the parity
test masks it on both sides.
"""
from __future__ import annotations

import json
import os
import subprocess
import sys
import tempfile
from dataclasses import dataclass, field
from pathlib import Path

REPO = Path(__file__).resolve().parents[3]


@dataclass
class Case:
    name: str
    argv: list[str]
    fixture: str
    env: dict[str, str] = field(default_factory=dict)


def write_project(root: Path) -> Path:
    """A git-less project whose `.fno/` the loader reads as the project
    layer. The state dir carries the path-migration sentinel so the
    first-invocation migration never prints into a golden."""
    project = root / "project"
    (project / ".fno").mkdir(parents=True, exist_ok=True)
    (root / "fake_home" / ".fno").mkdir(parents=True, exist_ok=True)
    state = root / "state"
    state.mkdir(parents=True, exist_ok=True)
    (state / ".path-migration-done").touch()
    return project


def global_toml(root: Path, text: str) -> None:
    (root / "fake_home" / ".fno" / "config.toml").write_text(text, encoding="utf-8")


def project_toml(project: Path, text: str) -> None:
    (project / ".fno" / "config.toml").write_text(text, encoding="utf-8")


def fixture_no_config(root: Path) -> Path:
    return write_project(root)


def fixture_clean(root: Path) -> Path:
    project = write_project(root)
    global_toml(root, "[review]\nmax_rounds = 3\n")
    return project


def fixture_degraded(root: Path) -> Path:
    project = write_project(root)
    global_toml(
        root,
        "[kanban.wip_caps]\n"
        "now = 0\n"
        "next = true\n"
        'later = "five"\n'
        "\n"
        "[worktree]\n"
        'policy = "sometimes"\n'
        "\n"
        "[accounts]\n"
        'quota = "lots"\n',
    )
    return project


def fixture_deprecated(root: Path) -> Path:
    project = write_project(root)
    global_toml(root, "[dispatch]\nauto_merge = true\n")
    project_toml(
        project,
        "[dispatch]\n"
        'harness = "codex"\n'
        "\n"
        "[active_backlog]\n"
        'mission = "m-1"\n',
    )
    return project


def fixture_post_merge_ready(root: Path) -> Path:
    project = write_project(root)
    global_toml(root, "[review]\nmax_rounds = 3\n")
    project_toml(project, '[post_merge]\nparking_lot_path = "notes/parking.md"\n')
    (project / ".fno" / "target-state.md").write_text("x\n", encoding="utf-8")
    return project


def fixture_post_merge_unconfigured(root: Path) -> Path:
    project = write_project(root)
    global_toml(root, "[review]\nmax_rounds = 3\n")
    project_toml(project, '[project]\nid = "proj-a"\n')
    (project / ".fno" / "target-state.md").write_text("x\n", encoding="utf-8")
    return project


def fixture_post_merge_opted_out(root: Path) -> Path:
    project = write_project(root)
    global_toml(root, "[review]\nmax_rounds = 3\n")
    project_toml(project, "[post_merge]\nenabled = false\n")
    return project


def fixture_post_merge_error(root: Path) -> Path:
    project = write_project(root)
    global_toml(root, "[review]\nmax_rounds = 3\n")
    project_toml(project, "[post_merge\nenabled = \n")
    return project


FIXTURES = {
    "no_config": fixture_no_config,
    "clean": fixture_clean,
    "degraded": fixture_degraded,
    "deprecated": fixture_deprecated,
    "post_merge_ready": fixture_post_merge_ready,
    "post_merge_unconfigured": fixture_post_merge_unconfigured,
    "post_merge_opted_out": fixture_post_merge_opted_out,
    "post_merge_error": fixture_post_merge_error,
}

TEST_MODE = {"FNO_TEST_MODE": "1"}

CASES: list[Case] = [
    Case("bare_no_config", ["config", "doctor"], "no_config", dict(TEST_MODE)),
    Case("bare_clean", ["config", "doctor"], "clean", dict(TEST_MODE)),
    Case("bare_clean_probe", ["config", "doctor"], "clean"),
    Case("bare_degraded", ["config", "doctor"], "degraded", dict(TEST_MODE)),
    Case("bare_deprecated", ["config", "doctor"], "deprecated", dict(TEST_MODE)),
    Case("post_merge_dormant", ["config", "doctor", "--post-merge"], "clean"),
    Case("post_merge_dormant_json", ["config", "doctor", "--post-merge", "--json"], "clean"),
    Case("json_alone", ["config", "doctor", "--json"], "clean"),
    Case("post_merge_ready", ["config", "doctor", "--post-merge"], "post_merge_ready"),
    Case(
        "post_merge_ready_json",
        ["config", "doctor", "--post-merge", "--json"],
        "post_merge_ready",
    ),
    Case(
        "post_merge_unconfigured",
        ["config", "doctor", "--post-merge"],
        "post_merge_unconfigured",
    ),
    Case(
        "post_merge_unconfigured_json",
        ["config", "doctor", "--post-merge", "--json"],
        "post_merge_unconfigured",
    ),
    Case(
        "post_merge_opted_out_json",
        ["config", "doctor", "--post-merge", "--json"],
        "post_merge_opted_out",
    ),
    Case(
        "post_merge_error_json",
        ["config", "doctor", "--post-merge", "--json"],
        "post_merge_error",
    ),
    Case("help", ["config", "doctor", "--help"], "clean"),
    Case("unknown_option", ["config", "doctor", "--bogus"], "clean"),
]


def scrubbed_env(root: Path, case_env: dict[str, str]) -> dict[str, str]:
    """The identity env, floored to the fixture, then the case's stamps."""
    fake_home = root / "fake_home"
    env = {
        "PATH": "/usr/bin:/bin",
        "GIT_CEILING_DIRECTORIES": str(root),
        "HOME": str(fake_home),
        "FNO_GLOBAL_SETTINGS_PATH": str(fake_home / ".fno" / "settings.yaml"),
        "FNO_NO_CANONICAL_CONFIG": "1",
        "FNO_STATE_DIR": str(root / "state"),
        "FNO_AGENTS_HOME": str(fake_home / ".fno" / "agents"),
        "FNO_AGENTS_BIN": "/nonexistent/fno-agents",
    }
    env.update(case_env)
    return env


def run_case(case: Case, root: Path) -> tuple[str, str, int]:
    project = FIXTURES[case.fixture](root)
    import fno

    if not (Path(fno.__file__).parent / "setup" / "config_doctor.py").is_file():
        raise SystemExit(
            "the Python `fno config doctor` leg is gone; a golden can only be "
            "captured on the base commit that still holds it"
        )
    os.environ.clear()
    os.environ.update(scrubbed_env(root, case.env))
    os.chdir(project)
    from typer.testing import CliRunner

    from fno.cli import app

    sys.argv[0] = "fno"
    result = CliRunner().invoke(app, list(case.argv))
    return result.stdout or "", result.stderr or "", result.exit_code


def normalize(text: str, root: Path) -> str:
    """Fold what the host owns: the fixture root's space slug (the resolved
    path with `/` swapped for `-`), then the root itself in resolved, raw
    and macOS `/private` spellings."""
    resolved = str(root.resolve())
    for token in (resolved.replace("/", "-"), str(root).replace("/", "-")):
        text = text.replace(token, "<FIXTURE_SLUG>")
    for token in (f"/private{resolved}", resolved, str(root)):
        text = text.replace(token, "<FIXTURE>")
    return text


def capture(golden_dir: Path) -> None:
    golden_dir.mkdir(parents=True, exist_ok=True)
    for case in CASES:
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            proc = subprocess.run(
                [sys.executable, str(Path(__file__).resolve()), "--case", case.name, str(root)],
                capture_output=True,
                text=True,
            )
            out = normalize(proc.stdout, root)
            err = normalize(proc.stderr, root)
        (golden_dir / f"{case.name}.out").write_text(out, encoding="utf-8")
        (golden_dir / f"{case.name}.err").write_text(err, encoding="utf-8")
        (golden_dir / f"{case.name}.exit").write_text(f"{proc.returncode}\n", encoding="utf-8")
        print(f"{case.name}: exit {proc.returncode}, {len(out)}B out, {len(err)}B err")


def main() -> None:
    if len(sys.argv) >= 4 and sys.argv[1] == "--case":
        out, err, code = run_case(
            next(c for c in CASES if c.name == sys.argv[2]), Path(sys.argv[3])
        )
        sys.stdout.write(out)
        sys.stderr.write(err)
        sys.exit(code)
    if len(sys.argv) >= 4 and sys.argv[1] == "--fixture":
        case = next(c for c in CASES if c.name == sys.argv[2])
        FIXTURES[case.fixture](Path(sys.argv[3]))
        return
    if len(sys.argv) == 2 and sys.argv[1] == "--list":
        print(json.dumps([{"name": c.name, "argv": c.argv, "env": c.env} for c in CASES]))
        return
    if len(sys.argv) == 3 and sys.argv[1] == "--capture":
        capture(Path(sys.argv[2]))
        return
    sys.exit("usage: config_doctor_driver.py --case NAME ROOT | --fixture NAME ROOT | --list | --capture DIR")


if __name__ == "__main__":
    main()
