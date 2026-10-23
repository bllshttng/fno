#!/usr/bin/env python3
"""Capture driver for the `accounts_reconcile` goldens.

The Python leg (`fno.adapters.providers.managed.reconcile_slot`) is the parity
oracle while the usage probe still calls it. Each case lays down a store tree,
Keychain items, a fake profile endpoint and a fake process table, runs one
reconcile, and freezes its result and the store tree it leaves in
`tests/golden/accounts_reconcile/<case>.json`. `accounts_reconcile_parity.rs`
replays the same file against `accounts::reconcile`.

    PYTHONPATH=cli/src cli/.venv/bin/python crates/fno-agents/tests/accounts_reconcile_golden_driver.py

The profile endpoint is faked at `principal_of_bearer`, the seam the Rust
`SlotProbe::profile` mirrors. `flip` names Keychain items the endpoint rewrites
while it answers, which is how a slot that moves mid-proof is staged.
"""

from __future__ import annotations

import json
import os
import shutil
import sys
import tempfile
from datetime import datetime, timezone
from pathlib import Path
from types import SimpleNamespace

sys.path.insert(0, str(Path(__file__).resolve().parent))
from accounts_store_golden_driver import blob, dump_tree, iso, proc  # noqa: E402

GOLDEN = Path(__file__).resolve().parent / "golden" / "accounts_reconcile"
NOW = 1_800_000_000.0

MAKERS = {"account_uuid": "acc-makers", "organization_uuid": "org-1", "email": "makers@x"}
OTHER = {"account_uuid": "acc-other", "organization_uuid": "org-1", "email": "other@x"}
CLAUDE_PROC = ["/usr/local/bin/claude", "--resume"]


def rec(rid, harness="claude", auth="managed", config_dir=None):
    return {"id": rid, "harness": harness, "auth": auth, "config_dir": config_dir}


def bound(principal) -> str:
    return json.dumps(
        {"harness": "claude", "account_id": "x", "captured_at": "2027-01-01T00:00:00Z",
         "kind": "keychain", "principal": principal, "principal_at": "2027-01-01T00:00:00Z"},
        indent=2,
    )


TAINT_LIVE = json.dumps({"pids": [101], "writers": [{"pid": 101, "started": 100.0}]})
TAINT_DEAD = json.dumps({"pids": [102], "writers": [{"pid": 102, "started": 100.0}]})
PAIR = [rec("makers"), rec("other")]
BOUND = {"makers/meta.json": bound(MAKERS), "makers/blob": blob("t-old"), "other/meta.json": bound(OTHER)}

CASES = {
    "unsupported_harness_refuses": dict(cli="gemini", records=PAIR, store=BOUND),
    "no_store_refuses_without_creating_it": dict(records=PAIR, no_store=True),
    "a_live_taint_writer_pins_the_slot": dict(
        records=PAIR, store={**BOUND, ".active-claude.tainted": TAINT_LIVE},
        keychain={"unscoped": blob("t-1")}, profile={"t-1": MAKERS}, started={"101": 100.0},
    ),
    "an_empty_slot_has_nothing_to_prove": dict(records=PAIR, store=BOUND),
    "an_unreachable_endpoint_proves_nothing": dict(
        records=PAIR, store=BOUND, keychain={"unscoped": blob("t-1")}, profile={"t-1": "profile-unavailable"},
    ),
    "a_rejected_bearer_proves_nothing": dict(
        records=PAIR, store=BOUND, keychain={"unscoped": blob("t-1")}, profile={"t-1": "credential-rejected"},
    ),
    "two_accounts_in_the_slot_are_ambiguous": dict(
        records=PAIR, store=BOUND,
        keychain={"scoped:slot": blob("t-1"), "unscoped": blob("t-2")}, profile={"t-1": MAKERS, "t-2": OTHER},
    ),
    "an_unbound_principal_matches_nothing": dict(
        records=PAIR, store={"makers/meta.json": bound(OTHER)},
        keychain={"unscoped": blob("t-1")}, profile={"t-1": MAKERS},
    ),
    "two_records_bound_alike_are_ambiguous": dict(
        records=PAIR, store={"makers/meta.json": bound(MAKERS), "other/meta.json": bound(MAKERS)},
        keychain={"unscoped": blob("t-1")}, profile={"t-1": MAKERS},
    ),
    "a_record_with_its_own_dir_cannot_claim_the_slot": dict(
        records=[rec("makers", config_dir="{TMP}/alt"), rec("other")], store=BOUND,
        keychain={"unscoped": blob("t-1")}, profile={"t-1": MAKERS},
    ),
    "an_api_key_record_cannot_claim_the_slot": dict(
        records=[{"id": "makers", "harness": "claude", "auth": "api_key", "config_dir": None}, rec("other")],
        store=BOUND, keychain={"unscoped": blob("t-1")}, profile={"t-1": MAKERS},
    ),
    "a_drifted_stamp_is_repaired": dict(
        records=PAIR, store={**BOUND, ".active-claude": "other", ".active-claude.reconcile-attempt": "1799999000.0"},
        keychain={"unscoped": blob("t-1")}, profile={"t-1": MAKERS},
    ),
    "a_dead_writer_taint_is_cleared": dict(
        records=PAIR, store={**BOUND, ".active-claude.tainted": TAINT_DEAD},
        keychain={"scoped:slot": blob("t-1"), "unscoped": blob("t-1")}, profile={"t-1": MAKERS},
    ),
    "a_slot_that_moves_during_the_proof_writes_nothing": dict(
        records=PAIR, store=BOUND, keychain={"unscoped": blob("t-1")}, profile={"t-1": MAKERS},
        flip={"unscoped": blob("t-2")},
    ),
    "a_held_switch_lock_times_out": dict(
        records=PAIR, store=BOUND, keychain={"unscoped": blob("t-1")}, profile={"t-1": MAKERS}, hold_lock=True,
    ),
}


def run_case(case: dict, tmp: Path) -> dict:
    import filelock
    from fno.adapters.providers import managed
    from fno.adapters.providers.model import ProviderRecord

    home, store, alt = tmp / "home", tmp / "state" / "providers", tmp / "alt"
    for d in (home / ".claude", alt):
        d.mkdir(parents=True, exist_ok=True)
    if not case.get("no_store"):
        store.mkdir(parents=True, exist_ok=True)
    for key in [k for k in os.environ if k.startswith(("FNO_", "CLAUDE_", "CODEX_"))]:
        del os.environ[key]
    os.environ.update(HOME=str(home), FNO_STATE_DIR=str(tmp / "state"), USER="tester")
    sys.platform = "darwin"
    for rel, text in case.get("store", {}).items():
        path = store / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text, encoding="utf-8")

    roles = {"Claude Code-credentials": "unscoped", managed._claude_scoped_service(home / ".claude"): "scoped:slot"}
    keychain = dict(case.get("keychain", {}))

    def fake_security(args):
        import subprocess

        value = keychain.get(roles.get(args[args.index("-s") + 1], "other"))
        if value is None:
            return subprocess.CompletedProcess(args, 44, "", "")
        return subprocess.CompletedProcess(args, 0, value + "\n", "")

    profile = case.get("profile", {})
    flip = case.get("flip", {})

    def fake_principal_of_bearer(bearer):
        keychain.update(flip)
        answer = profile.get(bearer, "profile-unavailable")
        return (None, answer) if isinstance(answer, str) else (dict(answer), None)

    started = {int(k): v for k, v in case.get("started", {}).items()}

    class FakeProc:
        def __init__(self, row):
            self.info = {"pid": row["pid"], "name": row["name"], "cmdline": row["cmdline"], "create_time": row["started"]}
            self._env = row["env"]

        def environ(self):
            return dict(self._env)

    procs = [FakeProc(p) for p in case.get("processes", [])]
    managed.psutil = SimpleNamespace(process_iter=lambda attrs=None: iter(procs), pid_exists=lambda pid: pid in started)
    managed._process_started_at = lambda pid: started.get(pid)
    managed._run_security = fake_security
    managed.principal_of_bearer = fake_principal_of_bearer
    managed._utc_now_iso = lambda: iso(NOW)
    managed.time = SimpleNamespace(time=lambda: NOW, monotonic=lambda: 0.0, sleep=lambda s: None)

    by_id = {}
    for r in case["records"]:
        fields = {**r, "name": r["id"]}
        if fields["config_dir"]:
            fields["config_dir"] = fields["config_dir"].replace("{TMP}", str(tmp))
        else:
            fields.pop("config_dir")
        if fields["auth"] == "api_key":
            fields["env"] = {"ANTHROPIC_API_KEY": "k"}
        by_id[r["id"]] = ProviderRecord(**fields)

    held = None
    if case.get("hold_lock"):
        held = filelock.FileLock(str(store / ".switch.lock"))
        held.acquire()
    try:
        got = managed.reconcile_slot(case.get("cli", "claude"), by_id=by_id, root=store, lock_timeout=0.2)
    finally:
        if held is not None:
            held.release()
    detail = got.detail
    # Longest first: the unscoped name prefixes every scoped one.
    for service, role in sorted(roles.items(), key=lambda kv: -len(kv[0])):
        detail = detail.replace(service, f"<{role}>")
    tree = {}
    if store.is_dir():
        tree, _modes = dump_tree(store)
        tree.pop(".switch.lock", None)
    return {
        "result": {"outcome": got.outcome, "record_id": got.record_id, "detail": detail},
        "store_exists": store.is_dir(),
        "store": tree,
    }


def main() -> int:
    GOLDEN.mkdir(parents=True, exist_ok=True)
    for stale in GOLDEN.glob("*.json"):
        stale.unlink()
    platform = sys.platform
    for name, case in CASES.items():
        tmp = Path(os.path.realpath(tempfile.mkdtemp(prefix="acct-reconcile-")))
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
