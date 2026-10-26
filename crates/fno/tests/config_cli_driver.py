#!/usr/bin/env python3
"""Live oracle for the config-cli family port (the port protocol, step 2).

Two modes:

  --case NAME ROOT  build case NAME's fixture under ROOT, run the Python
                    config verbs there through the real top-level app, and
                    relay (stdout, stderr, exit code) byte-verbatim. The
                    parity test invokes this under FNO_CAPTURE_GOLDEN=1 to
                    freeze goldens from the old leg on the same fixture the
                    native side runs.
  --capture DIR     run every case in a fresh tempdir and freeze the
                    normalized (stdout, stderr, exit) triples into DIR.
                    Refuses once the family's Python leg is gone: a golden
                    can only be captured while the leg runs.

The case table below is the single source both legs share; the Rust parity
test rebuilds the same fixtures and folds only the fixture path. The YAML
fold: the native reader parses TOML only, so every fixture is config.toml;
a legacy settings.yaml fixture would fold a parser detail the loader
migrates away on its own first read.
"""
from __future__ import annotations

import json
import os
import shutil
import subprocess
import sys
import tempfile
from dataclasses import dataclass, field
from pathlib import Path

REPO = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(REPO / "cli" / "src"))


@dataclass
class Case:
    name: str
    argv: list[str]
    env: dict[str, str] = field(default_factory=dict)
    fixture: str = "empty_project"


FAKE_RECEIPT_OBJECT_EMPTY = json.dumps(
    {"targets": [], "missions": 0, "skip_reason": "drain_disabled"}
)
FAKE_RECEIPT_OBJECT_ROWS = json.dumps(
    {
        "targets": [
            {
                "scope": "zeta-epic",
                "cwd": "<FIXTURE_CWD>",
                "interval_seconds": 30,
                "failure_limit": 3,
                "mission": "m-1",
            }
        ],
        "missions": 1,
        "skip_reason": None,
    }
)


def write_project(root: Path) -> Path:
    """The minimal checkout every case runs in: a git-less project dir whose
    `.fno/config.toml` the loader reads as the project layer. The state dir
    carries the path-migration sentinel so the first-invocation migration
    never prints its fresh-install line into a golden."""
    project = root / "project"
    (project / ".fno").mkdir(parents=True, exist_ok=True)
    (root / "fake_home" / ".fno").mkdir(parents=True, exist_ok=True)
    state = root / "state"
    state.mkdir(parents=True, exist_ok=True)
    (state / ".path-migration-done").touch()
    return project


def fixture_empty_project(root: Path) -> Path:
    return write_project(root)


def fixture_scalar(root: Path) -> Path:
    project = write_project(root)
    (project / ".fno" / "config.toml").write_text(
        "[review]\nmax_rounds = 5\n", encoding="utf-8"
    )
    return project


def fixture_layering(root: Path) -> Path:
    project = write_project(root)
    (project / ".fno" / "config.toml").write_text(
        '[project]\nid = "mine"\n\n[review]\nmax_rounds = 3\n', encoding="utf-8"
    )
    (project / ".fno" / "config.local.toml").write_text(
        '[project]\nid = "local-id"\n', encoding="utf-8"
    )
    (root / "fake_home" / ".fno" / "config.toml").write_text(
        "[review]\nmax_rounds = 7\n[post_merge]\nenabled = false\n",
        encoding="utf-8",
    )
    return project


def fixture_providers_alias(root: Path) -> Path:
    project = write_project(root)
    (root / "fake_home" / ".fno" / "config.toml").write_text(
        '[providers]\nactive = "zai-main"\n', encoding="utf-8"
    )
    return project


def fixture_budget(root: Path) -> Path:
    project = write_project(root)
    (project / ".fno" / "config.toml").write_text(
        "[agents.provider_limits.zai]\nsubagents = 5\n", encoding="utf-8"
    )
    return project


def fixture_work_map(root: Path) -> Path:
    project = write_project(root)
    (root / "fake_home" / ".fno" / "config.toml").write_text(
        "[[work.workspaces.alpha.projects]]\n"
        'name = "sunk"\n'
        'path = "<FIXTURE_CWD>"\n',
        encoding="utf-8",
    )
    (project / ".fno" / "config.toml").write_text(
        "[[status_sinks]]\n"
        'name = "sink-one"\n'
        'type = "json-webhook"\n'
        'url = "https://example.invalid/hook"\n'
        "enabled = true\n"
        "\n"
        "[status_fanout]\n"
        "interval_secs = 7\n",
        encoding="utf-8",
    )
    return project


def fixture_history(root: Path) -> Path:
    project = write_project(root)
    state = root / "state"
    state.mkdir(parents=True, exist_ok=True)
    rows = [
        {"type": "config_write", "ts": "2026-10-20T10:00:00Z", "data": {
            "key": "review.max_rounds", "old_value": "2", "new_value": "5",
            "scope": "project", "root_kind": "project",
            "config_path": "<FIXTURE_CWD>/.fno/config.toml",
            "present_before": True, "present_after": True,
            "attester_session_id": "sess-1", "attester_witness": "hook"}},
        {"type": "config_write", "ts": "2026-10-21T10:00:00Z", "data": {
            "key": "blueprint.max_prs_per_epic", "new_value": "6",
            "scope": "global", "root_kind": "state",
            "config_path": "global",
            "present_before": False, "present_after": True}},
        {"type": "other_event", "ts": "2026-10-22T10:00:00Z", "data": {"key": "x"}},
    ]
    (state / "events.jsonl").write_text(
        "".join(json.dumps(row) + "\n" for row in rows), encoding="utf-8"
    )
    return project


def fixture_fake_agents(root: Path, receipt: str) -> Path:
    project = write_project(root)
    bin_dir = root / "fake_bin"
    bin_dir.mkdir(parents=True, exist_ok=True)
    fake = bin_dir / "fno-agents"
    script = (
        "#!/bin/sh\n"
        "case \"$*\" in\n"
        "  *--stage-only*)\n"
        "    echo \"codex arm offline\" >&2\n"
        "    exit 7\n"
        "    ;;\n"
        "  *)\n"
        "    cat <<'RECEIPT'\n"
        f"{receipt}\n"
        "RECEIPT\n"
        "    ;;\n"
        "esac\n"
    )
    fake.write_text(script, encoding="utf-8")
    fake.chmod(0o755)
    return project


def build_fixture(case: Case, root: Path) -> Path:
    project = {
        "empty_project": fixture_empty_project,
        "scalar": fixture_scalar,
        "layering": fixture_layering,
        "providers_alias": fixture_providers_alias,
        "budget": fixture_budget,
        "work_map": fixture_work_map,
        "history": fixture_history,
        "fake_agents_empty": lambda root: fixture_fake_agents(
            root, FAKE_RECEIPT_OBJECT_EMPTY
        ),
        "fake_agents_rows": lambda root: fixture_fake_agents(
            root, FAKE_RECEIPT_OBJECT_ROWS
        ),
        "fake_agents_list": lambda root: fixture_fake_agents(root, "[]"),
    }.get(case.fixture, fixture_empty_project)(root)
    # The workspace map and the history receipts carry the project cwd; point
    # the placeholder at the freshly built project before anything reads it.
    for path in [
        root / "fake_home" / ".fno" / "config.toml",
        project / ".fno" / "config.toml",
        state_events(root),
    ]:
        if path.is_file():
            text = path.read_text(encoding="utf-8")
            path.write_text(
                text.replace("<FIXTURE_CWD>", str(project.resolve())),
                encoding="utf-8",
            )
    return project


def state_events(root: Path) -> Path:
    return root / "state" / "events.jsonl"


def scrubbed_env(fake_home: Path, case_env: dict[str, str]) -> dict[str, str]:
    """The identity env, floored to absent, then the case's stamps."""
    env = {
        "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
        "HOME": str(fake_home),
        "FNO_GLOBAL_SETTINGS_PATH": str(fake_home / ".fno" / "settings.yaml"),
        "FNO_NO_CANONICAL_CONFIG": "1",
        "FNO_STATE_DIR": "<FIXTURE>/state",
        "FNO_REPO_ROOT": "",
        "FNO_AGENTS_HOME": str(fake_home / ".fno" / "agents"),
        "FNO_EVENTS_PATH": "",
        "FNO_ROUTE_PROVIDER": "",
        "FNO_AGENTS_BIN": "",
        "FNO_CONFIG": "",
    }
    env.pop("FNO_CAPTURE_GOLDEN", None)
    env.update(case_env)
    return {key: value for key, value in env.items() if value != ""}


def load_oracle_modules() -> None:
    """Load the family's pre-port modules from git into sys.modules.

    The port deleted (or moved) cli/src/fno/config_cli.py and
    plugin_install_cli.py in the same change that landed the native doors, so
    the live tree can no longer answer for them. The oracle is the BASE
    commit's leg, extracted read-only; every module it imports (the config
    hub, paths, rust_binary, the setup codex engine) still lives unchanged.
    The base is the nearest ancestor that still holds the leg: HEAD before
    the port commit exists, HEAD~N after (fix pushes shift it).
    """
    import subprocess as sp
    import types

    def oracle_ref(repo_path: str) -> str:
        for depth in range(0, 10):
            rev = "HEAD" if depth == 0 else f"HEAD~{depth}"
            probe = sp.run(
                ["git", "cat-file", "-e", f"{rev}:{repo_path}"],
                capture_output=True,
                cwd=REPO,
            )
            if probe.returncode == 0:
                return rev
        raise SystemExit(
            f"no ancestor of HEAD within 10 commits holds {repo_path}; "
            "the base-commit leg is out of reach for capture"
        )

    for module_name, repo_path in (
        ("fno.config_cli", "cli/src/fno/config_cli.py"),
        ("fno.plugin_install_cli", "cli/src/fno/plugin_install_cli.py"),
    ):
        source = sp.run(
            ["git", "show", f"{oracle_ref(repo_path)}:{repo_path}"],
            capture_output=True,
            text=True,
            check=True,
            cwd=REPO,
        ).stdout
        module = types.ModuleType(module_name)
        module.__file__ = f"<git-show:{repo_path}>"
        # Registered BEFORE exec: the dataclass decorator resolves the class's
        # module through sys.modules while the body runs.
        sys.modules[module_name] = module
        exec(compile(source, f"<git-show:{repo_path}>", "exec"), module.__dict__)
    # The live verb table points `config` at the moved shell; capture answers
    # from the base-commit leg, so the oracle module aliases under the name
    # the lazy import resolves.
    sys.modules["fno.config.write_cli"] = sys.modules["fno.config_cli"]


def run_case(case: Case, root: Path) -> tuple[str, str, int]:
    """Run one case in this process: build the fixture, load the base-commit
    leg, chdir, invoke the real app, return (stdout, stderr, exit code)."""
    project = build_fixture(case, root)
    env = scrubbed_env(root / "fake_home", case.env)
    env = {
        key: value.replace("<FIXTURE>", str(root))
        for key, value in env.items()
    }
    os.environ.clear()
    os.environ.update(env)
    load_oracle_modules()
    os.chdir(project)
    from typer.testing import CliRunner

    from fno.cli import app

    # The real front door's wheel exec runs with argv[0] spelled `fno`; the
    # usage lines click renders name the program, so pin it here too.
    sys.argv[0] = "fno"
    result = CliRunner().invoke(app, list(case.argv))
    return result.stdout or "", result.stderr or "", result.exit_code


def normalize(text: str, root: Path) -> str:
    """Fold what the host owns, never what the contract owns: the fixture
    path in resolved and raw spellings, plus macOS's /private prefix."""
    resolved = str(root.resolve())
    for token in (f"/private{resolved}", resolved, str(root)):
        text = text.replace(token, "<FIXTURE>")
    return text


STATE_PIN = {"FNO_STATE_DIR": "<FIXTURE>/state"}
EVENTS_PIN = {"FNO_EVENTS_PATH": "<FIXTURE>/state/events.jsonl"}
AGENTS_PIN = {"FNO_AGENTS_HOME": "<FIXTURE>/fake_home/.fno/agents"}
NO_BIN = {"FNO_AGENTS_BIN": "/nonexistent/fno-agents"}
FAKE_BIN = {"FNO_AGENTS_BIN": "<FIXTURE>/fake_bin/fno-agents"}

CASES: list[Case] = [
    Case("get_scalar", ["config", "get", "review.max_rounds"], fixture="scalar"),
    Case("get_default", ["config", "get", "blueprint.max_prs_per_epic"], fixture="scalar"),
    Case("get_descends", ["config", "get", "loops.mystery.level"], fixture="scalar"),
    Case("get_block", ["config", "get", "review"], fixture="scalar"),
    Case("get_unknown", ["config", "get", "no.such.key"], fixture="scalar"),
    Case("get_prefix", ["config", "get", "config.review.max_rounds"], fixture="scalar"),
    Case("get_alias", ["config", "get", "providers.active"], fixture="providers_alias"),
    Case("get_json", ["config", "get", "review.max_rounds", "--json"], fixture="scalar"),
    Case("get_layering", ["config", "get", "review.max_rounds"], fixture="layering"),
    Case("get_local_override", ["config", "get", "project.id"], fixture="layering"),
    Case("get_missing", ["config", "get"], fixture="scalar"),
    Case("get_extra", ["config", "get", "a", "b"], fixture="scalar"),
    Case("get_unknown_opt", ["config", "get", "--nope"], fixture="scalar"),
    Case("get_help", ["config", "get", "--help"], fixture="scalar"),
    Case("history_empty", ["config", "history"], {**STATE_PIN, **AGENTS_PIN, **EVENTS_PIN}),
    Case(
        "history_rows",
        ["config", "history"],
        {**STATE_PIN, **AGENTS_PIN, **EVENTS_PIN},
        fixture="history",
    ),
    Case(
        "history_json",
        ["config", "history", "--json"],
        {**STATE_PIN, **AGENTS_PIN, **EVENTS_PIN},
        fixture="history",
    ),
    Case(
        "history_filter",
        ["config", "history", "review.max_rounds"],
        {**STATE_PIN, **AGENTS_PIN, **EVENTS_PIN},
        fixture="history",
    ),
    Case("history_help", ["config", "history", "--help"]),
    Case("build_dir", ["config", "build-dir"], dict(STATE_PIN)),
    Case("builddir_help", ["config", "build-dir", "--help"], fixture="empty_project"),
    Case("budget_nostamp", ["config", "assert-subagent-budget", "--width", "4"], fixture="budget"),
    Case(
        "budget_within",
        ["config", "assert-subagent-budget", "--width", "4"],
        {"FNO_ROUTE_PROVIDER": "zai"},
        fixture="budget",
    ),
    Case(
        "budget_exceeded",
        ["config", "assert-subagent-budget", "--width", "9"],
        {"FNO_ROUTE_PROVIDER": "zai"},
        fixture="budget",
    ),
    Case(
        "budget_no_entry",
        ["config", "assert-subagent-budget", "--width", "3"],
        {"FNO_ROUTE_PROVIDER": "gemini"},
        fixture="budget",
    ),
    Case(
        "budget_badwidth",
        ["config", "assert-subagent-budget", "--width", "abc"],
        {"FNO_ROUTE_PROVIDER": "zai"},
        fixture="budget",
    ),
    Case("budget_help", ["config", "assert-subagent-budget", "--help"]),
    Case("sinks_empty", ["config", "status-sinks"]),
    Case("sinks_json", ["config", "status-sinks", "--json"], fixture="work_map"),
    Case("sinks_help", ["config", "status-sinks", "--help"]),
    Case(
        "backlog_empty_object",
        ["config", "active-backlog"],
        dict(FAKE_BIN),
        fixture="fake_agents_empty",
    ),
    Case(
        "backlog_rows",
        ["config", "active-backlog"],
        dict(FAKE_BIN),
        fixture="fake_agents_rows",
    ),
    Case(
        "backlog_list_shape",
        ["config", "active-backlog"],
        dict(FAKE_BIN),
        fixture="fake_agents_list",
    ),
    Case("backlog_help", ["config", "active-backlog", "--help"]),
    Case("plugin_usage", ["config", "plugin", "install"], dict(NO_BIN)),
    Case("plugin_help", ["config", "plugin", "install", "--help"]),
    Case(
        "plugin_claude_forward",
        ["config", "plugin", "install", "claude"],
        dict(FAKE_BIN),
        fixture="fake_agents_list",
    ),
    Case(
        "plugin_codex",
        ["config", "plugin", "install", "codex"],
        dict(FAKE_BIN),
        fixture="fake_agents_list",
    ),
]


def capture(golden_dir: Path) -> None:
    """Freeze every case from the base commit's leg (the oracle loads from
    git HEAD, so capture works while the leg lives and after it deletes)."""
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
        build_fixture(case, Path(sys.argv[3]))
        return
    if len(sys.argv) == 2 and sys.argv[1] == "--list":
        print(
            json.dumps(
                [
                    {"name": c.name, "argv": list(c.argv), "env": dict(c.env), "fixture": c.fixture}
                    for c in CASES
                ]
            )
        )
        return
    if len(sys.argv) == 3 and sys.argv[1] == "--capture":
        capture(Path(sys.argv[2]))
        return
    sys.exit(
        "usage: config_cli_driver.py --case NAME ROOT | --capture GOLDEN_DIR"
        " | --list | --fixture NAME ROOT"
    )


if __name__ == "__main__":
    main()
