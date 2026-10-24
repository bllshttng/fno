#!/usr/bin/env python3
"""Capture driver for the `accounts_doctor` goldens.

The Python leg (`fno.adapters.providers.cli._doctor_findings`) is the parity
oracle while the list command still reads it. Each case lays down a store
tree, config dirs, Keychain items and a fake profile endpoint, collects the
findings once, and freezes them with the store tree they leave in
`tests/golden/accounts_doctor/<case>.json`. `accounts_doctor_parity.rs`
replays the same file against `accounts::doctor::findings`.

    PYTHONPATH=cli/src cli/.venv/bin/python crates/fno-agents/tests/accounts_doctor_golden_driver.py

Keychain items are keyed by role (`unscoped`, `scoped:slot`, `scoped:alt`),
never by the per-path scoped service hash, so a golden carries no temp path.
"""

from __future__ import annotations

import json
import os
import shutil
import sys
import tempfile
from pathlib import Path
from types import SimpleNamespace

sys.path.insert(0, str(Path(__file__).resolve().parent))
from accounts_store_golden_driver import blob, dump_tree  # noqa: E402

GOLDEN = Path(__file__).resolve().parent / "golden" / "accounts_doctor"
NOW = 1_800_000_000.0

MAKERS = {"account_uuid": "acc-makers", "organization_uuid": "org-1", "email": "makers@x"}
OTHER = {"account_uuid": "acc-other", "organization_uuid": "org-1", "email": "other@x"}
STRANGER = {"account_uuid": "acc-stranger", "organization_uuid": "org-1", "email": "stranger@x"}


def rec(rid, harness="claude", auth="managed", config_dir=None):
    return {"id": rid, "harness": harness, "auth": auth, "config_dir": config_dir}


def bound(principal) -> str:
    return json.dumps(
        {"harness": "claude", "account_id": "x", "captured_at": "2027-01-01T00:00:00Z",
         "kind": "keychain", "principal": principal, "principal_at": "2027-01-01T00:00:00Z"},
        indent=2,
    )


PAIR = [rec("makers"), rec("other")]
BOUND = {"makers/meta.json": bound(MAKERS), "other/meta.json": bound(OTHER)}
TAINT = json.dumps({"pids": [101], "writers": [{"pid": 101, "started": 100.0}]})

CASES = {
    "a_bound_pair_is_quiet": dict(
        records=PAIR, store={**BOUND, "makers/blob": blob("t-a"), "other/blob": blob("t-b")},
    ),
    "one_credential_under_two_ids_is_named_both_ways": dict(
        records=PAIR, store={**BOUND, "makers/blob": blob("t-a"), "other/blob": blob("t-a")},
    ),
    "a_shared_slot_record_with_no_principal_is_unbound": dict(
        records=PAIR, store={"makers/meta.json": bound(MAKERS)},
    ),
    "a_principal_without_its_organization_still_counts_as_bound": dict(
        records=PAIR, store={"makers/meta.json": bound({"account_uuid": "acc-makers"}), "other/meta.json": bound(OTHER)},
    ),
    "api_key_and_codex_records_are_never_unbound": dict(
        records=[rec("key", auth="api_key"), rec("cx", harness="codex")],
    ),
    "a_missing_config_dir_is_named": dict(
        records=[rec("alt", config_dir="{TMP}/gone")],
    ),
    "a_config_dir_with_no_login_is_named": dict(
        records=[rec("alt", config_dir="{TMP}/alt")],
    ),
    "a_credentials_file_is_a_login": dict(
        records=[rec("alt", config_dir="{TMP}/alt")], files={"alt/.credentials.json": "{}"},
    ),
    "a_scoped_keychain_item_is_a_login": dict(
        records=[rec("alt", config_dir="{TMP}/alt")], keychain={"scoped:alt": blob("t-alt")},
    ),
    "a_tainted_slot_names_the_repair": dict(
        records=[*PAIR, rec("cx", harness="codex")],
        store={**BOUND, ".active-claude.tainted": TAINT, ".active-codex.tainted": TAINT},
    ),
    "a_stamp_on_an_unbound_record_asks_nothing": dict(
        records=PAIR, store={"other/meta.json": bound(OTHER), ".active-claude": "makers"},
        keychain={"unscoped": blob("t-1")}, profile={"t-1": OTHER},
    ),
    "a_matching_slot_is_quiet": dict(
        records=PAIR, store={**BOUND, ".active-claude": "makers"},
        keychain={"unscoped": blob("t-1")}, profile={"t-1": MAKERS},
    ),
    "an_out_of_band_login_is_drift": dict(
        records=PAIR, store={**BOUND, ".active-claude": "makers"},
        keychain={"unscoped": blob("t-1")}, profile={"t-1": OTHER},
    ),
    "a_stranger_in_the_slot_is_drift": dict(
        records=PAIR, store={**BOUND, ".active-claude": "makers"},
        keychain={"unscoped": blob("t-1")}, profile={"t-1": STRANGER},
    ),
    "a_fresh_cached_proof_names_no_label": dict(
        records=PAIR,
        store={**BOUND, ".active-claude": "makers",
               ".active-claude.principal": "CACHE:t-1:acc-other/org-1"},
        keychain={"unscoped": blob("t-1")},
    ),
    "two_accounts_in_the_slot_are_ambiguous": dict(
        records=PAIR, store={**BOUND, ".active-claude": "makers"},
        keychain={"scoped:slot": blob("t-1"), "unscoped": blob("t-2")}, profile={"t-1": MAKERS, "t-2": OTHER},
    ),
    "an_unreachable_endpoint_is_named_unknown": dict(
        records=PAIR, store={**BOUND, ".active-claude": "makers"},
        keychain={"unscoped": blob("t-1")}, profile={"t-1": "profile-unavailable"},
    ),
    "an_empty_slot_is_named_unknown": dict(
        records=PAIR, store={**BOUND, ".active-claude": "makers"},
    ),
}


def run_case(case: dict, tmp: Path) -> dict:
    from fno.adapters.providers import binding, cli, managed
    from fno.adapters.providers.model import ProviderRecord

    home, store, alt = tmp / "home", tmp / "state" / "providers", tmp / "alt"
    for d in (home / ".claude", store, alt):
        d.mkdir(parents=True, exist_ok=True)
    for key in [k for k in os.environ if k.startswith(("FNO_", "CLAUDE_", "CODEX_"))]:
        del os.environ[key]
    os.environ.update(HOME=str(home), FNO_STATE_DIR=str(tmp / "state"), USER="tester")
    sys.platform = "darwin"
    for rel, text in case.get("store", {}).items():
        if text.startswith("CACHE:"):
            # The proof cache keys on the credential digest, which only the
            # Python leg can compute at capture time.
            _, token, identity = text.split(":", 2)
            text = json.dumps({"account_uuid": identity,
                               "credential": managed.credential_digest(blob(token)), "at": NOW - 60})
        path = store / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text, encoding="utf-8")
    for rel, text in case.get("files", {}).items():
        (tmp / rel).write_text(text, encoding="utf-8")

    roles = {
        "Claude Code-credentials": "unscoped",
        managed._claude_scoped_service(home / ".claude"): "scoped:slot",
        managed._claude_scoped_service(alt): "scoped:alt",
    }
    keychain = dict(case.get("keychain", {}))

    def fake_security(args):
        import subprocess

        value = keychain.get(roles.get(args[args.index("-s") + 1], "other"))
        if value is None:
            return subprocess.CompletedProcess(args, 44, "", "")
        return subprocess.CompletedProcess(args, 0, value + "\n", "")

    profile = case.get("profile", {})

    def fake_principal_of_bearer(bearer):
        answer = profile.get(bearer, "profile-unavailable")
        return (None, answer) if isinstance(answer, str) else (dict(answer), None)

    managed._run_security = fake_security
    managed.principal_of_bearer = fake_principal_of_bearer
    managed.time = SimpleNamespace(time=lambda: NOW, monotonic=lambda: 0.0, sleep=lambda s: None)
    binding.time = SimpleNamespace(time=lambda: NOW)

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
    cli._load = lambda scope="global": SimpleNamespace(records=list(by_id.values()), by_id=by_id)

    findings = cli._doctor_findings()
    for f in findings:
        # Longest first: the unscoped name prefixes every scoped one.
        for service, role in sorted(roles.items(), key=lambda kv: -len(kv[0])):
            f["detail"] = f["detail"].replace(service, f"<{role}>")
    tree, _modes = dump_tree(store)
    return {"findings": findings, "store": tree}


def main() -> int:
    GOLDEN.mkdir(parents=True, exist_ok=True)
    for stale in GOLDEN.glob("*.json"):
        stale.unlink()
    platform = sys.platform
    for name, case in CASES.items():
        tmp = Path(os.path.realpath(tempfile.mkdtemp(prefix="acct-doctor-")))
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
