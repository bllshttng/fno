#!/usr/bin/env python3
"""Fixture builder and after-state reader for the config-writer port
(`fno config set`, `unset`, `rm`), the characterization replay in
config_set_parity.rs.

  --fixture NAME ROOT  build case NAME's fixture under ROOT (the Rust
                       replay's first step).
  --state ROOT         print the after-state the replay compares: each config
                       file parsed, and the config_write receipt rows.
  --list               print the case table as JSON.

The frozen goldens under tests/golden/config_set are the contract; the live
Python leg they captured from died with the config family.
"""
from __future__ import annotations

import json
import os
import shutil
import subprocess
import sys
from dataclasses import dataclass, field
from pathlib import Path

REPO = Path(__file__).resolve().parents[3]
HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(REPO / "cli" / "src"))

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


def main() -> None:
    if len(sys.argv) == 4 and sys.argv[1] == "--fixture":
        fixture(next(c for c in CASES if c.name == sys.argv[2]).fixture, Path(sys.argv[3]))
        return
    if len(sys.argv) == 3 and sys.argv[1] == "--state":
        print(json.dumps(after_state(Path(sys.argv[2])), indent=1, sort_keys=True))
        return
    if len(sys.argv) == 2 and sys.argv[1] == "--list":
        print(json.dumps([{"name": c.name, "argv": c.argv, "fixture": c.fixture, "env": c.env} for c in CASES]))
        return
    sys.exit(
    )


if __name__ == "__main__":
    main()
