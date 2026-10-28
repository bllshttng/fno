#!/usr/bin/env python3
"""Live oracle for the config-writer port (`fno config set`, `unset`, `rm`).

Modes, the shape of config_cli_driver.py:

  --case NAME ROOT     build case NAME's fixture under ROOT, run the base
                       commit's Python writer through the real top-level app,
                       and relay (stdout, stderr, exit code) byte-verbatim.
  --fixture NAME ROOT  build the fixture only (the Rust replay's first step).
  --state ROOT         print the after-state both legs compare: each config
                       file parsed, and the config_write receipt rows.
  --capture DIR        run every case in a fresh tempdir and freeze the
                       normalized triple plus the after-state into DIR.

The oracle legs (config/writer.py, claims/optout_lease.py and
config_writer_cli.py) load from git: the nearest ancestor of HEAD that still
holds writer.py. Capture refuses and names the leg when no ancestor within
reach holds it.

Two facts belong to the host, not the contract, and both legs read them from
the fixture: the claim doors and the pr-watch doors answer from
fake_fno_agents.py, and the attester identity is the FNO_FIXTURE_ATTESTER
stamp with an `env_only` witness (the native leg asks the fake's
`attest identity`; this leg patches the Python resolver to the same answer),
because a real ancestry walk reads whichever harness launched the run.
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
HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(REPO / "cli" / "src"))

ORACLE_LEGS = (
    ("fno.config.writer", "cli/src/fno/config/writer.py"),
    ("fno.claims.optout_lease", "cli/src/fno/claims/optout_lease.py"),
    ("fno.config_writer_cli", "cli/src/fno/config_writer_cli.py"),
)


@dataclass
class Case:
    name: str
    argv: list[str]
    fixture: str = "empty"
    env: dict[str, str] = field(default_factory=dict)


def write_base(root: Path) -> Path:
    project = root / "project"
    (project / ".fno").mkdir(parents=True, exist_ok=True)
    (root / "fake_home" / ".fno").mkdir(parents=True, exist_ok=True)
    state = root / "state"
    state.mkdir(parents=True, exist_ok=True)
    (state / ".path-migration-done").touch()
    bin_dir = root / "fake_bin"
    bin_dir.mkdir(parents=True, exist_ok=True)
    fake = bin_dir / "fno-agents"
    shutil.copyfile(HERE / "fake_fno_agents.py", fake)
    fake.chmod(0o755)
    return project


def global_toml(root: Path) -> Path:
    return root / "fake_home" / ".fno" / "config.toml"


def project_toml(root: Path) -> Path:
    return root / "project" / ".fno" / "config.toml"


def seed_claim(root: Path, key: str, holder: str, metadata: dict) -> None:
    claims = root / "fake_claims"
    claims.mkdir(parents=True, exist_ok=True)
    record = {
        "schema_version": 1, "key": key, "holder": holder,
        "acquired_at": 1700000000000, "pid": 4242, "host": "fixture-host",
        "expires_at": 1700003600000, "reason": "merge-gating opt-out",
        "metadata": metadata, "state": "live",
    }
    (claims / (key.replace(":", "__") + ".json")).write_text(json.dumps(record), encoding="utf-8")


def fixture(name: str, root: Path) -> Path:
    project = write_base(root)
    g, p = global_toml(root), project_toml(root)
    if name == "scalar":
        g.write_text("# operator notes\n[review]\nmax_rounds = 5\n", encoding="utf-8")
        p.write_text("[review]\nmax_rounds = 3\n", encoding="utf-8")
    elif name == "malformed":
        g.write_text("[review\nmax_rounds = 5\n", encoding="utf-8")
    elif name == "symlink":
        real = root / "shared" / "config.toml"
        real.parent.mkdir(parents=True, exist_ok=True)
        real.write_text("[review]\nmax_rounds = 5\n", encoding="utf-8")
        g.symlink_to(real)
    elif name == "yaml":
        (root / "fake_home" / ".fno" / "settings.yaml").write_text(
            "config:\n  review:\n    max_rounds: 6\n", encoding="utf-8"
        )
    elif name == "accounts":
        g.write_text(
            '[[accounts.records]]\nid = "work"\nname = "Work"\nharness = "claude"\nauth = "managed"\n',
            encoding="utf-8",
        )
    elif name == "providers":
        g.write_text('[providers]\nactive = "old"\n', encoding="utf-8")
    elif name == "retired":
        g.write_text('[dispatch]\nallowed_verbs = ["a"]\n', encoding="utf-8")  # verb-wording: allow
    elif name == "optout_live":
        g.write_text("[review]\nself_review_required = false\n", encoding="utf-8")
        seed_claim(root, "config-optout:review.self_review_required", "sess-fixture", {
            "config_key": "review.self_review_required",
            "config_path": str(g.resolve()), "scope": "global",
            "prior_present": False, "prior_value": None,
        })
    elif name == "optout_other":
        g.write_text("[review]\nself_review_required = false\n", encoding="utf-8")
        seed_claim(root, "config-optout:review.self_review_required", "sess-other", {
            "config_key": "review.self_review_required",
            "config_path": str(g.resolve()), "scope": "global",
            "prior_present": False, "prior_value": None,
        })
    return project


SESSION = {"FNO_FIXTURE_ATTESTER": "sess-fixture"}

CASES: list[Case] = [
    Case("set_int", ["config", "set", "review.max_rounds", "4"]),
    Case("set_int_bad", ["config", "set", "review.max_rounds", "abc"]),
    Case("set_bool", ["config", "set", "post_merge.enabled", "off"]),
    Case("set_bool_bad", ["config", "set", "post_merge.enabled", "maybe"]),
    Case("set_float", ["config", "set", "agents.min_free_gb", "2.5"]),
    Case("set_float_bad", ["config", "set", "agents.min_free_gb", "lots"]),
    Case("set_null", ["config", "set", "test.max_net_new", "null"]),
    Case("set_list_json", ["config", "set", "review.nonblocking_categories", '["style","docs"]']),
    Case("set_list_csv", ["config", "set", "review.nonblocking_categories", "style, docs,"]),
    Case("set_list_scalar", ["config", "set", "review.nonblocking_categories", '"style"']),
    Case("set_block_json", ["config", "set", "obsidian", '{"enabled": false}']),
    Case("set_block_yaml", ["config", "set", "obsidian", "{enabled: false}"]),
    Case("set_block_list", ["config", "set", "obsidian", "[1]"]),
    Case("set_block_invalid", ["config", "set", "obsidian", '{"enabled": "maybe"}']),
    Case("set_bound", ["config", "set", "review.max_rounds", "0"]),
    Case("set_cross_field", ["config", "set", "obsidian.enabled=true", "obsidian.vault=/tmp/vault"]),
    Case("set_unknown", ["config", "set", "no.such.key", "1"]),
    Case("set_bare_config", ["config", "set", "config", "1"]),
    Case("set_prefix", ["config", "set", "config.review.max_rounds", "4"]),
    Case("set_dict_hop", ["config", "set", "loops.nightly.level", "assisted"]),
    Case("set_multi", ["config", "set", "review.max_rounds=4", "post_merge.enabled=false"]),
    Case("set_dedup", ["config", "set", "review.max_rounds=4", "review.max_rounds=6"]),
    Case("set_batch_atomic", ["config", "set", "review.max_rounds=4", "post_merge.enabled=maybe"], "scalar"),
    Case("set_bad_form", ["config", "set", "a", "b", "c"]),
    Case("set_value_eq", ["config", "set", "store.remote_url", "https://x.invalid/?a=b"]),
    Case("set_missing", ["config", "set"]),
    Case("set_unknown_opt", ["config", "set", "--nope", "a", "b"]),
    Case("set_help", ["config", "set", "--help"]),
    Case("set_local", ["config", "set", "-l", "review.max_rounds", "4"], "scalar"),
    Case("set_keeps_layers", ["config", "set", "post_merge.enabled", "false"], "scalar"),
    Case("set_overridden", ["config", "set", "review.max_rounds", "9"], "scalar"),
    Case("set_malformed", ["config", "set", "review.max_rounds", "4"], "malformed"),
    Case("set_symlink", ["config", "set", "review.max_rounds", "4"], "symlink"),
    Case("set_yaml_convert", ["config", "set", "post_merge.enabled", "false"], "yaml"),
    Case("set_secret", ["config", "set", "store.remote_token", "hunter2"]),
    Case("set_redacted", ["config", "set", "loops.token.level", "assisted"]),
    Case("set_account_ok", ["config", "set", "accounts.active", "work"], "accounts"),
    Case("set_account_bad", ["config", "set", "accounts.active", "nope"], "accounts"),
    Case("set_account_alias", ["config", "set", "providers.active", "nope"], "accounts"),
    Case("set_pr_watch_on", ["config", "set", "pr_watch.enabled", "true"]),
    Case("set_pr_watch_off", ["config", "set", "pr_watch.enabled", "false"]),
    Case("set_optout_no_session", ["config", "set", "review.self_review_required", "false"]),
    Case("set_optout_lease", ["config", "set", "review.self_review_required", "false"], env=SESSION),
    Case("set_optout_held", ["config", "set", "review.self_review_required", "false"], "optout_other", SESSION),
    Case("set_optout_release", ["config", "set", "review.self_review_required", "true"], "optout_live", SESSION),
    Case("unset_present", ["config", "unset", "-l", "review.max_rounds"], "scalar"),
    Case("unset_absent", ["config", "unset", "review.max_rounds"]),
    Case("unset_no_key", ["config", "unset", "post_merge.enabled"], "scalar"),
    Case("unset_unknown", ["config", "unset", "no.such.key"]),
    Case("unset_retired", ["config", "unset", "dispatch.allowed_verbs"], "retired"),  # verb-wording: allow
    Case("unset_providers", ["config", "unset", "accounts.active"], "providers"),
    Case("unset_dict_hop", ["config", "unset", "loops.nightly.level"], "scalar"),
    Case("unset_optout_lease", ["config", "unset", "review.self_review_required"], "optout_live", SESSION),
    Case("unset_optout_held", ["config", "unset", "review.self_review_required"], "optout_other", SESSION),
    Case("unset_help", ["config", "unset", "--help"]),
    Case("unset_missing", ["config", "unset"]),
    Case("rm_alias", ["config", "rm", "review.max_rounds"], "scalar"),
    Case("rm_help", ["config", "rm", "--help"]),
]


def scrubbed_env(root: Path, case_env: dict[str, str]) -> dict[str, str]:
    """Every case runs on the same floored env, then the case's stamps."""
    home = root / "fake_home"
    env = {
        "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
        "HOME": str(home),
        "FNO_GLOBAL_SETTINGS_PATH": str(home / ".fno" / "settings.yaml"),
        "FNO_NO_CANONICAL_CONFIG": "1",
        "FNO_STATE_DIR": str(root / "state"),
        "FNO_AGENTS_HOME": str(home / ".fno" / "agents"),
        "FNO_EVENTS_PATH": str(root / "state" / "project-events.jsonl"),
        "FNO_AGENTS_BIN": str(root / "fake_bin" / "fno-agents"),
    }
    # The temp dir rides through: without it the CLI's temp guard reads the
    # fixture's fake fno-agents as a durable path and points the real gh
    # shim at it.
    for name in ("TMPDIR", "TEMPDIR"):
        if name in os.environ:
            env[name] = os.environ[name]
    env.update(case_env)
    return env


def oracle_ref(repo_path: str) -> str:
    for depth in range(0, 40):
        rev = "HEAD" if depth == 0 else f"HEAD~{depth}"
        probe = subprocess.run(
            ["git", "cat-file", "-e", f"{rev}:{repo_path}"], capture_output=True, cwd=REPO
        )
        if probe.returncode == 0:
            return rev
    raise SystemExit(
        f"capture refused: no ancestor of HEAD within 40 commits holds {repo_path}, "
        "the Python writer leg; a golden can only be frozen while the leg is in reach"
    )


def load_oracle_modules() -> None:
    import types

    import fno.harness_identity as harness_identity

    def fixture_attester(env=None):  # noqa: ANN001 - the resolver's signature
        return (os.environ.get("FNO_FIXTURE_ATTESTER", ""), "env_only")

    harness_identity.resolve_attester_identity = fixture_attester
    for module_name, repo_path in ORACLE_LEGS:
        source = subprocess.run(
            ["git", "show", f"{oracle_ref(repo_path)}:{repo_path}"],
            capture_output=True, text=True, check=True, cwd=REPO,
        ).stdout
        module = types.ModuleType(module_name)
        module.__file__ = f"<git-show:{repo_path}>"
        sys.modules[module_name] = module
        parent, _, leaf = module_name.rpartition(".")
        exec(compile(source, module.__file__, "exec"), module.__dict__)
        setattr(sys.modules[parent], leaf, module)


def run_case(case: Case, root: Path) -> tuple[str, str, int]:
    project = fixture(case.fixture, root)
    env = scrubbed_env(root, case.env)
    os.environ.clear()
    os.environ.update(env)
    load_oracle_modules()
    os.chdir(project)
    from typer.testing import CliRunner

    from fno.cli import app

    sys.argv[0] = "fno"
    result = CliRunner().invoke(app, list(case.argv))
    if result.exception is not None and not isinstance(result.exception, SystemExit):
        import traceback

        traceback.print_exception(result.exception)
    return result.stdout or "", result.stderr or "", result.exit_code


def read_receipts(root: Path) -> list[dict]:
    """The config_write rows in both pinned journals, payload only: the
    envelope's ts and ids are minted per run. Both legs commit through the
    native store, so the native reader (FNO_BIN, else `fno` on PATH) reads
    them back for both."""
    binary = os.environ.get("FNO_BIN") or shutil.which("fno")
    if binary is None:
        raise SystemExit("--state needs the native fno binary (FNO_BIN or PATH) to read receipts")
    rows: list[dict] = []
    for name in ("events.jsonl", "project-events.jsonl"):
        journal = root / "state" / name
        out = subprocess.run(
            [binary, "doctor", "event", "rows", "--events", str(journal)],
            capture_output=True, text=True, check=False,
            env={**os.environ, "FNO_STATE_DIR": str(root / "state")},
        ).stdout
        try:
            lines = json.loads(out) if out.strip() else []
        except ValueError:
            lines = []
        for line in lines:
            try:
                event = json.loads(line)
            except (TypeError, ValueError):
                continue
            if isinstance(event, dict) and event.get("type") == "config_write":
                rows.append({"journal": name, **(event.get("data") or {})})
    return rows


def parsed(path: Path) -> object:
    if not path.is_file():
        return None
    import tomllib

    try:
        return tomllib.loads(path.read_text(encoding="utf-8"))
    except (tomllib.TOMLDecodeError, UnicodeDecodeError):
        return {"<unparsed>": path.read_text(encoding="utf-8")}


def after_state(root: Path) -> dict:
    home = root / "fake_home" / ".fno"
    log = root / "fake_agents.log"
    return {
        "global": parsed(home / "config.toml"),
        "global_yaml_left": (home / "settings.yaml").exists(),
        "project": parsed(root / "project" / ".fno" / "config.toml"),
        "shared": parsed(root / "shared" / "config.toml"),
        "claims": sorted(p.name for p in (root / "fake_claims").glob("*.json")) if (root / "fake_claims").is_dir() else [],
        "agents_calls": log.read_text(encoding="utf-8").splitlines() if log.is_file() else [],
        "receipts": read_receipts(root),
    }


def normalize(text: str, root: Path) -> str:
    resolved = str(root.resolve())
    for token in (f"/private{resolved}", resolved, str(root)):
        text = text.replace(token, "<FIXTURE>")
    return text


def capture(golden_dir: Path) -> None:
    for _, repo_path in ORACLE_LEGS:
        oracle_ref(repo_path)
    golden_dir.mkdir(parents=True, exist_ok=True)
    for case in CASES:
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            proc = subprocess.run(
                [sys.executable, str(Path(__file__).resolve()), "--case", case.name, str(root)],
                capture_output=True, text=True,
            )
            state = subprocess.run(
                [sys.executable, str(Path(__file__).resolve()), "--state", str(root)],
                capture_output=True, text=True, check=True,
            ).stdout
            out = normalize(proc.stdout, root)
            err = normalize(proc.stderr, root)
            state = normalize(state, root)
        (golden_dir / f"{case.name}.out").write_text(out, encoding="utf-8")
        (golden_dir / f"{case.name}.err").write_text(err, encoding="utf-8")
        (golden_dir / f"{case.name}.exit").write_text(f"{proc.returncode}\n", encoding="utf-8")
        (golden_dir / f"{case.name}.state.json").write_text(state, encoding="utf-8")
        print(f"{case.name}: exit {proc.returncode}, {len(out)}B out, {len(err)}B err")


def main() -> None:
    if len(sys.argv) == 4 and sys.argv[1] == "--case":
        out, err, code = run_case(next(c for c in CASES if c.name == sys.argv[2]), Path(sys.argv[3]))
        sys.stdout.write(out)
        sys.stderr.write(err)
        sys.exit(code)
    if len(sys.argv) == 4 and sys.argv[1] == "--fixture":
        fixture(next(c for c in CASES if c.name == sys.argv[2]).fixture, Path(sys.argv[3]))
        return
    if len(sys.argv) == 3 and sys.argv[1] == "--state":
        print(json.dumps(after_state(Path(sys.argv[2])), indent=1, sort_keys=True))
        return
    if len(sys.argv) == 2 and sys.argv[1] == "--list":
        print(json.dumps([{"name": c.name, "argv": c.argv, "fixture": c.fixture, "env": c.env} for c in CASES]))
        return
    if len(sys.argv) == 3 and sys.argv[1] == "--capture":
        capture(Path(sys.argv[2]))
        return
    sys.exit(
        "usage: config_set_driver.py --case NAME ROOT | --fixture NAME ROOT"
        " | --state ROOT | --list | --capture GOLDEN_DIR"
    )


if __name__ == "__main__":
    main()
