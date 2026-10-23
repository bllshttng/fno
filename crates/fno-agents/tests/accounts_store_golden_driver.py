#!/usr/bin/env python3
"""Capture driver for the `accounts_store` goldens.

The Python leg (the store layer of `fno.adapters.providers.managed`) is the
parity oracle until the adapters package is deleted. Each case lays down a
store tree, slot credential files, Keychain items, a fake process table and a
fake `codex login status`, runs one store operation, and freezes its result,
the store tree it leaves and every Keychain write in
`tests/golden/accounts_store/<case>.json`. `accounts_store_parity.rs` replays
the same file against `accounts::store`.

    PYTHONPATH=cli/src cli/.venv/bin/python crates/fno-agents/tests/accounts_store_golden_driver.py

Keychain items are keyed by role (`unscoped`, `scoped:slot`, `scoped:alt`),
never by the per-path scoped service hash, so a golden carries no temp path.
"""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import sys
import tempfile
from datetime import datetime, timezone
from pathlib import Path
from types import SimpleNamespace

GOLDEN = Path(__file__).resolve().parent / "golden" / "accounts_store"
NOW = 1_800_000_000.0


def blob(token: str) -> str:
    return '{"claudeAiOauth": {"accessToken": "%s", "refreshToken": "r"}}' % token


RESIDUE = '{"claudeAiOauth": {"accessToken": "", "refreshToken": "", "scopes": ["user"]}}'
CODEX = json.dumps({"tokens": {"access_token": "a", "id_token": "i", "refresh_token": "r"}})
MAKERS = {"account_uuid": "acc-makers", "organization_uuid": "org-1", "email": "makers@x"}
CLAUDE_REC = {"id": "makers", "harness": "claude", "account_id": "makers@x"}
CODEX_REC = {"id": "cx", "harness": "codex", "account_id": ""}


def meta(**extra) -> str:
    body = {"harness": "claude", "account_id": "makers", "captured_at": "2027-01-15T07:00:00Z", "kind": "keychain"}
    body.update(extra)
    return json.dumps(body, indent=2)


def proc(pid, cmdline, env=None, name=None, started=None):
    return {"pid": pid, "name": name, "cmdline": cmdline, "env": env if env is not None else {}, "started": started}


CLAUDE_PROC = ["/usr/local/bin/claude", "--resume"]
CODEX_PROC = ["/opt/bin/codex", "exec"]

CASES = {
    # Snapshot
    "snapshot_keychain_writes_private_store": dict(
        platform="darwin", op="snapshot_current", args={"record": CLAUDE_REC},
        keychain={"unscoped": blob("t-live")},
    ),
    "snapshot_prefers_the_scoped_item": dict(
        platform="darwin", op="snapshot_current", args={"record": CLAUDE_REC},
        keychain={"scoped:slot": blob("t-scoped"), "unscoped": blob("t-unscoped")},
    ),
    "snapshot_skips_a_logged_out_residue": dict(
        platform="darwin", op="snapshot_current", args={"record": CLAUDE_REC},
        keychain={"scoped:slot": RESIDUE, "unscoped": blob("t-live")},
    ),
    "snapshot_refuses_with_no_login": dict(
        platform="darwin", op="snapshot_current", args={"record": CLAUDE_REC},
        keychain={"scoped:slot": RESIDUE},
    ),
    "snapshot_keychain_timeout_is_a_keychain_error": dict(
        platform="darwin", op="snapshot_current", args={"record": CLAUDE_REC},
        keychain={"scoped:slot": "TIMEOUT"},
    ),
    "snapshot_reads_the_credentials_file_off_darwin": dict(
        platform="linux", op="snapshot_current", args={"record": CLAUDE_REC},
        files={"slot": blob("t-file")},
    ),
    "snapshot_codex_reads_auth_json": dict(
        platform="darwin", op="snapshot_current", args={"record": CODEX_REC},
        env={"CODEX_HOME": "{TMP}/codex"}, files={"codex": CODEX},
    ),
    "write_snapshot_keeps_the_proven_principal": dict(
        platform="darwin", op="write_snapshot", args={"record": CLAUDE_REC, "blob": blob("t-new")},
        store={"makers/blob": blob("t-old"), "makers/meta.json": meta(principal=MAKERS, principal_at="2027-01-01T00:00:00Z")},
    ),
    # Duplicate credential
    "duplicate_holder_found": dict(
        op="duplicate_holder", args={"blob": blob("t-1"), "exclude_id": "new"},
        store={"a/blob": blob("t-2"), "b/blob": blob("t-1"), "c/blob": blob("t-1")},
    ),
    "duplicate_distinct_credential": dict(
        op="duplicate_holder", args={"blob": blob("t-9"), "exclude_id": "new"},
        store={"a/blob": blob("t-2")},
    ),
    "duplicate_same_id_is_not_a_duplicate": dict(
        op="duplicate_holder", args={"blob": blob("t-1"), "exclude_id": "a"},
        store={"a/blob": blob("t-1")},
    ),
    "duplicate_codex_compares_whole_payload": dict(
        op="duplicate_holder", args={"blob": CODEX + "\n", "exclude_id": "new"},
        store={"cx/blob": CODEX},
    ),
    "duplicate_empty_blob_has_nothing_to_compare": dict(
        op="duplicate_holder", args={"blob": "  ", "exclude_id": "new"},
        store={"a/blob": "  "},
    ),
    # Age labels
    "age_label_ladder": dict(op="age_labels", args={"seconds": [-5, 59, 61, 3599, 3600, 86399, 86400, 3 * 86400 + 5]}),
    "snapshot_age_hours": dict(op="snapshot_age_label", args={"id": "makers"}, store={"makers/meta.json": meta()}),
    "snapshot_age_unregistered": dict(op="snapshot_age_label", args={"id": "ghost"}),
    "snapshot_age_unparsable": dict(op="snapshot_age_label", args={"id": "makers"}, store={"makers/meta.json": meta(captured_at="yesterday")}),
    # Stamp and taint
    "stamp_active_slot": dict(op="stamp", args={"cli": "claude", "id": "makers"}),
    "taint_records_writers": dict(
        op="set_taint", args={"cli": "claude", "tainted": True, "pids": [[101, 1799999000.25], 102]},
        started={"102": 1799999100.5},
    ),
    "taint_clears": dict(op="set_taint", args={"cli": "claude", "tainted": False, "pids": []}, store={".active-claude.tainted": '{"pids": [1], "writers": []}'}),
    "tainting_writers_reads_writers": dict(
        op="tainting_writers", args={"cli": "claude"},
        store={".active-claude.tainted": '{"pids": [1, 2], "writers": [{"pid": 1, "started": 5.5}, {"pid": 2, "started": null}, {"pid": "x"}]}'},
    ),
    "tainting_writers_legacy_pid_only": dict(op="tainting_writers", args={"cli": "claude"}, store={".active-claude.tainted": '{"pids": [7, "x", 8]}'}),
    "tainting_writers_unparsable": dict(op="tainting_writers", args={"cli": "claude"}, store={".active-claude.tainted": "not json"}),
    "still_live_untainted_is_empty": dict(op="still_live", args={"cli": "claude"}, processes=[proc(500, CLAUDE_PROC)]),
    "still_live_alive_dead_and_recycled": dict(
        op="still_live", args={"cli": "claude"},
        store={".active-claude.tainted": json.dumps({"pids": [1, 2, 3, 4, 5], "writers": [
            {"pid": 1, "started": 100.0}, {"pid": 2, "started": 100.0}, {"pid": 3, "started": 100.0},
            {"pid": 4, "started": None}, {"pid": 5, "started": 100.0}]})},
        started={"1": 100.4, "2": 250.0, "4": 9.0}, alive=[5],
    ),
    "still_live_legacy_marker_scans": dict(
        op="still_live", args={"cli": "claude"},
        store={".active-claude.tainted": "[]"},
        processes=[proc(600, CLAUDE_PROC), proc(601, ["/bin/zsh"])],
    ),
    # Principal writes
    "write_record_principal_keeps_meta": dict(op="write_principal", args={"id": "makers", "principal": MAKERS}, store={"makers/meta.json": meta()}),
    "write_record_principal_creates_meta": dict(op="write_principal", args={"id": "fresh", "principal": MAKERS}),
    "clear_record_principal": dict(op="clear_principal", args={"id": "makers"}, store={"makers/meta.json": meta(principal=MAKERS, principal_at="2027-01-01T00:00:00Z")}),
    "clear_record_principal_unbound_is_untouched": dict(op="clear_principal", args={"id": "makers"}, store={"makers/meta.json": '{"harness": "claude"}'}),
    "clear_slot_principal_cache": dict(op="clear_cache", args={"cli": "claude"}, store={".active-claude.principal": "{}", ".active-codex.principal": "{}"}),
    # Slot write and verify
    "write_slot_keychain_unscoped_then_scoped": dict(platform="darwin", op="write_slot_blob", args={"cli": "claude", "blob": blob("t-new")}),
    "write_slot_keychain_failure_names_the_service": dict(
        platform="darwin", op="write_slot_blob", args={"cli": "claude", "blob": blob("t-new")},
        keychain_fail={"scoped:slot": {"code": 45, "stderr": "denied\n"}},
    ),
    "write_slot_file_off_darwin": dict(platform="linux", op="write_slot_blob", args={"cli": "claude", "blob": blob("t-new")}),
    "write_slot_codex_file": dict(platform="darwin", op="write_slot_blob", args={"cli": "codex", "blob": CODEX}, env={"CODEX_HOME": "{TMP}/codex"}),
    "read_slot_alternate_dir_skips_unscoped": dict(
        platform="darwin", op="read_slot_blob", args={"cli": "claude", "config_dir": "alt"},
        keychain={"unscoped": blob("t-shared")},
    ),
    "read_slot_ambient_override_is_the_shared_slot": dict(
        platform="darwin", op="read_slot_blob", args={"cli": "claude"},
        env={"CLAUDE_CONFIG_DIR": "{TMP}/alt"}, keychain={"scoped:alt": blob("t-alt")},
    ),
    "verify_slot_match": dict(platform="darwin", op="verify_slot", args={"record": CLAUDE_REC, "expected": blob("t-1") + "\n"}, keychain={"unscoped": blob("t-1")}),
    "verify_slot_mismatch": dict(platform="darwin", op="verify_slot", args={"record": CLAUDE_REC, "expected": blob("t-2")}, keychain={"unscoped": blob("t-1")}),
    "verify_slot_codex_needs_auth": dict(
        platform="darwin", op="verify_slot", args={"record": CODEX_REC, "expected": '{"tokens": {}}'},
        env={"CODEX_HOME": "{TMP}/codex"}, files={"codex": '{"tokens": {}}'},
    ),
    # Credential checks
    "codex_auth_modes": dict(op="codex_auth", args={"blobs": [
        CODEX,
        json.dumps({"tokens": {"access_token": "a", "id_token": "i", "refresh_token": ""}}),
        json.dumps({"auth_mode": "chatgptAuthTokens", "tokens": {"access_token": "a", "id_token": "i", "refresh_token": ""}}),
        json.dumps({"OPENAI_API_KEY": "sk"}),
        json.dumps({"OPENAI_API_KEY": " "}),
        json.dumps({"personal_access_token": "p"}),
        json.dumps({"bedrock_api_key": {"api_key": "k", "region": "us"}}),
        json.dumps({"bedrock_api_key": {"api_key": "k"}}),
        json.dumps({"auth_mode": "agentIdentity", "agent_identity": "opaque"}),
        json.dumps({"auth_mode": "agentIdentity", "agent_identity": {
            "agent_runtime_id": "r", "agent_private_key": "k", "account_id": "a", "chatgpt_user_id": "u",
            "plan_type": "pro", "chatgpt_account_is_fedramp": False}}),
        json.dumps({"auth_mode": "agentIdentity", "agent_identity": {
            "agent_runtime_id": "r", "agent_private_key": "k", "account_id": "a", "chatgpt_user_id": "u",
            "plan_type": "pro"}}),
        json.dumps({"auth_mode": "mystery"}),
        json.dumps(["not", "a", "dict"]),
        "not json",
        "{}",
        json.dumps({"foo": "bar"}),
        json.dumps({"tokens": {"access_token": "only"}}),
        json.dumps({"agent_identity": "header.payload.signature"}),
        json.dumps({"agent_identity": {"agent_runtime_id": "only"}}),
        json.dumps({"auth_mode": "chatgpt", "OPENAI_API_KEY": "sk-inactive", "tokens": None}),
        json.dumps({"personal_access_token": "", "OPENAI_API_KEY": "sk-inactive"}),
        json.dumps({"auth_mode": "headers", "tokens": json.loads(CODEX)["tokens"]}),
        json.dumps({"OPENAI_API_KEY": None, "tokens": json.loads(CODEX)["tokens"]}),
        json.dumps({"personal_access_token": None, "bedrock_api_key": {"api_key": "k", "region": "us"}}),
        json.dumps({"personal_access_token": None, "bedrock_api_key": None, "OPENAI_API_KEY": "sk-fallback"}),
    ]}),
    "codex_login_ok": dict(op="codex_login", args={}, codex_login="ok", env={"CODEX_HOME": "{TMP}/codex"}),
    "codex_login_rejected": dict(op="codex_login", args={}, codex_login="fail"),
    "codex_login_missing": dict(op="codex_login", args={}, codex_login="missing"),
    "codex_login_timeout": dict(op="codex_login", args={}, codex_login="timeout"),
    # Pin gate
    "matchers": dict(op="matchers", args={"rows": [
        [None, ["/usr/local/bin/claude"]],
        ["claude", []],
        [None, ["  "]],
        [None, ["node", "/x/claude"]],
        [None, ["/usr/bin/codex --x"]],
        [None, ["git", "commit", "-m", "codex fix"]],
        ["codex", ["zsh"]],
        [None, []],
    ]}),
    "pinning_claude_default_slot": dict(
        op="pinning", args={"cli": "claude"},
        processes=[
            proc(10, CLAUDE_PROC, started=1799990000.5),
            proc(11, CLAUDE_PROC, env={"CLAUDE_CONFIG_DIR": "{TMP}/alt"}),
            proc(12, CLAUDE_PROC, env="UNREADABLE"),
            proc(13, ["/bin/zsh"]),
            proc(14, CLAUDE_PROC, env={"CLAUDE_CONFIG_DIR": "{TMP}/home/.claude"}),
        ],
    ),
    "pinning_codex_home": dict(
        op="pinning", args={"cli": "codex"}, env={"CODEX_HOME": "{TMP}/codex"},
        processes=[
            proc(20, CODEX_PROC, env={"CODEX_HOME": "{TMP}/codex"}),
            proc(21, CODEX_PROC),
            proc(22, CODEX_PROC, env="UNREADABLE"),
            proc(23, ["git", "commit", "-m", "codex"], env={"CODEX_HOME": "{TMP}/codex"}),
        ],
    ),
    "pinning_unsupported_cli_refuses": dict(op="pinning", args={"cli": "gemini"}),
}


def iso(t: float) -> str:
    return datetime.fromtimestamp(t, timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def dump_tree(root: Path) -> tuple[dict, dict]:
    files, modes = {}, {}
    for path in sorted(root.rglob("*")):
        if path.is_file():
            rel = path.relative_to(root).as_posix()
            text = path.read_text(encoding="utf-8")
            try:
                files[rel] = json.loads(text)
            except ValueError:
                files[rel] = text
            # Only the 600 marker: a fixture file's mode is the capture box's umask.
            modes[rel] = path.stat().st_mode & 0o777 == 0o600
    return files, modes


def run_case(case: dict, tmp: Path) -> dict:
    from fno.adapters.providers import managed

    home, store, alt = tmp / "home", tmp / "state" / "providers", tmp / "alt"
    for d in (home / ".claude", store, alt, tmp / "codex"):
        d.mkdir(parents=True, exist_ok=True)
    for key in [k for k in os.environ if k.startswith(("FNO_", "CLAUDE_", "CODEX_"))]:
        del os.environ[key]
    os.environ.update(HOME=str(home), FNO_STATE_DIR=str(tmp / "state"), USER="tester")
    for key, value in case.get("env", {}).items():
        os.environ[key] = value.replace("{TMP}", str(tmp))
    sys.platform = case.get("platform", "darwin")

    for rel, text in case.get("store", {}).items():
        path = store / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text, encoding="utf-8")
    slot_files = {
        "slot": home / ".claude" / ".credentials.json",
        "alt": alt / ".credentials.json",
        "codex": managed._codex_slot_auth_path(),
    }
    for label, text in case.get("files", {}).items():
        slot_files[label].parent.mkdir(parents=True, exist_ok=True)
        slot_files[label].write_text(text, encoding="utf-8")

    roles = {"Claude Code-credentials": "unscoped"}
    roles[managed._claude_scoped_service(home / ".claude")] = "scoped:slot"
    roles[managed._claude_scoped_service(alt)] = "scoped:alt"
    keychain = dict(case.get("keychain", {}))
    fail = case.get("keychain_fail", {})
    writes: list = []

    def fake_security(args):
        role = roles.get(args[args.index("-s") + 1], "other")
        value = keychain.get(role)
        if value == "TIMEOUT":
            raise managed.KeychainError(f"`security {args[0]}` timed out after 5s")
        if args[0] == "add-generic-password":
            if role in fail:
                return subprocess.CompletedProcess(args, fail[role]["code"], "", fail[role]["stderr"])
            new = args[args.index("-w") + 1]
            writes.append([role, new])
            keychain[role] = new
            return subprocess.CompletedProcess(args, 0, "", "")
        if value is None:
            return subprocess.CompletedProcess(args, 44, "", "")
        return subprocess.CompletedProcess(args, 0, value + "\n", "")

    started = {int(k): v for k, v in case.get("started", {}).items()}
    alive = set(case.get("alive", []))

    class FakeProc:
        def __init__(self, row):
            self.info = {"pid": row["pid"], "name": row["name"], "cmdline": row["cmdline"], "create_time": row["started"]}
            self._env = row["env"]

        def environ(self):
            if self._env == "UNREADABLE":
                raise PermissionError("denied")
            return {k: v.replace("{TMP}", str(tmp)) for k, v in self._env.items()}

    procs = [FakeProc(row) for row in case.get("processes", [])]
    managed.psutil = SimpleNamespace(
        process_iter=lambda attrs=None: iter(procs),
        pid_exists=lambda pid: pid in alive,
    )
    managed._process_started_at = lambda pid: started.get(pid)
    managed._run_security = fake_security
    managed._utc_now_iso = lambda: iso(NOW)

    class FixedDatetime(datetime):
        @classmethod
        def now(cls, tz=None):
            return datetime.fromtimestamp(NOW, tz)

    managed.datetime = FixedDatetime
    login = case.get("codex_login")

    login_calls: list = []

    def fake_run(argv, **kwargs):
        login_calls.append(kwargs.get("env", {}).get("CODEX_HOME"))
        if login == "missing":
            raise FileNotFoundError(argv[0])
        if login == "timeout":
            raise subprocess.TimeoutExpired(argv, 5)
        return subprocess.CompletedProcess(argv, 0 if login == "ok" else 1, "", "")

    managed.subprocess = SimpleNamespace(run=fake_run, TimeoutExpired=subprocess.TimeoutExpired)

    def record(r):
        return SimpleNamespace(id=r["id"], harness=r["harness"], account_id=r["account_id"] or None)

    op, a = case["op"], case.get("args", {})
    result, error = None, None
    try:
        if op == "snapshot_current":
            result = str(managed.snapshot_current(record(a["record"]), store))
        elif op == "write_snapshot":
            result = str(managed.write_snapshot(record(a["record"]), a["blob"], store))
        elif op == "duplicate_holder":
            result = managed.duplicate_credential_holder(a["blob"], exclude_id=a["exclude_id"], root=store)
        elif op == "age_labels":
            result = [managed.age_label_from_seconds(s) for s in a["seconds"]]
        elif op == "snapshot_age_label":
            result = managed.snapshot_age_label(a["id"], store)
        elif op == "stamp":
            managed.stamp_active_slot(a["cli"], a["id"], store)
        elif op == "set_taint":
            pids = [tuple(p) if isinstance(p, list) else p for p in a["pids"]]
            managed._set_slot_taint(a["cli"], store, a["tainted"], pids)
        elif op == "tainting_writers":
            got = managed.tainting_writers(a["cli"], store)
            result = None if got is None else [list(w) for w in got]
        elif op == "still_live":
            result = managed.taint_writers_still_live(a["cli"], store)
        elif op == "write_principal":
            managed.write_record_principal(a["id"], a["principal"], store)
        elif op == "clear_principal":
            managed._clear_record_principal(a["id"], store)
        elif op == "clear_cache":
            managed.clear_slot_principal_cache(a["cli"], store)
        elif op == "write_slot_blob":
            managed._write_slot_blob(a["cli"], a["blob"])
        elif op == "read_slot_blob":
            cfg = {"alt": alt}.get(a.get("config_dir"))
            result = managed._read_slot_blob(a["cli"], cfg)
        elif op == "verify_slot":
            result = managed.verify_slot(record(a["record"]), a["expected"])
        elif op == "codex_auth":
            result = [managed._codex_auth_present(b) for b in a["blobs"]]
        elif op == "codex_login":
            got = managed._codex_login_ok()
            result = {"ok": got.ok, "reason": got.reason, "home": login_calls[0]}
        elif op == "matchers":
            result = [[managed._looks_like_claude(n, c), managed._looks_like_codex(n, c)] for n, c in a["rows"]]
        elif op == "pinning":
            result = [{"pid": s.pid, "cmdline": s.cmdline, "started": s.started} for s in managed.pinning_sessions_for(a["cli"])]
        else:
            raise SystemExit(f"unknown op {op}")
    except managed.ManagedStoreError as exc:
        message = str(exc)
        # Longest first: the unscoped name prefixes every scoped one.
        for service, role in sorted(roles.items(), key=lambda kv: -len(kv[0])):
            message = message.replace(service, f"<{role}>")
        error = {"type": type(exc).__name__, "message": message}

    tree, modes = dump_tree(store)
    files = {label: p.read_text(encoding="utf-8") for label, p in slot_files.items() if p.exists()}
    return {
        "result": result,
        "error": error,
        "store": tree,
        "modes": modes,
        "files": files,
        "keychain_writes": writes,
    }


def main() -> int:
    GOLDEN.mkdir(parents=True, exist_ok=True)
    for stale in GOLDEN.glob("*.json"):
        stale.unlink()
    platform = sys.platform
    for name, case in CASES.items():
        tmp = Path(os.path.realpath(tempfile.mkdtemp(prefix="acct-store-")))
        try:
            expect = json.loads(json.dumps(run_case(case, tmp)).replace(str(tmp), "<TMP>"))
        finally:
            sys.platform = platform
            shutil.rmtree(tmp, ignore_errors=True)
        (GOLDEN / f"{name}.json").write_text(
            json.dumps({**case, "now": NOW, "expect": expect}) + "\n", encoding="utf-8"
        )
    print(f"wrote {len(CASES)} cases to {GOLDEN}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
