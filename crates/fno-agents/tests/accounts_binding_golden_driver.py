#!/usr/bin/env python3
"""Capture driver for the `accounts_binding` goldens.

The Python leg (`fno.adapters.providers.binding.resolve_account_binding`) is
the parity oracle until the adapters package is deleted. Each case lays down
an accounts config, Keychain items, credential files, store principals and a
proof cache, runs the leg against a fake `security` and a fake profile
endpoint, and freezes the binding plus the proof cache it leaves behind in
`tests/golden/accounts_binding/<case>.json`. `accounts_binding_parity.rs`
replays the same file against `accounts::binding::resolve`.

    PYTHONPATH=cli/src python3 crates/fno-agents/tests/accounts_binding_golden_driver.py

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
from pathlib import Path

GOLDEN = Path(__file__).resolve().parent / "golden" / "accounts_binding"
NOW = 1_800_000_000.0
MAKERS = {
    "account_uuid": "acc-makers",
    "organization_uuid": "org-1",
    "email": "makers@x",
}
READY = {"account_uuid": "acc-ready", "organization_uuid": "org-1", "email": "ready@x"}
OTHER_ORG = {
    "account_uuid": "acc-makers",
    "organization_uuid": "org-2",
    "email": "makers@x",
}


def blob(token: str) -> str:
    return '{"claudeAiOauth": {"accessToken": "%s", "refreshToken": "r"}}' % token


def rec(rid: str, **extra) -> str:
    body = {"id": rid, "name": rid, "harness": "claude", "auth": "managed", **extra}
    return "[[accounts.records]]\n" + "".join(
        f"{k} = {json.dumps(v)}\n" for k, v in body.items()
    )


TWO = rec("makers") + rec("ready")
ALT = rec("alt", config_dir="{TMP}/alt")
BOUND = {"makers": MAKERS, "ready": READY}

# name -> case fields; `request` defaults to the unpinned shared slot.
CASES = {
    "unpinned_names_the_live_account": dict(
        config=TWO,
        keychain={"unscoped": blob("t-ready")},
        profiles={"t-ready": READY},
        meta=BOUND,
    ),
    "unpinned_two_records_claim_one_identity": dict(
        config=TWO,
        keychain={"unscoped": blob("t-ready")},
        profiles={"t-ready": READY},
        meta={"makers": READY, "ready": READY},
    ),
    "unpinned_no_record_claims_it": dict(
        config=TWO, keychain={"unscoped": blob("t-ready")}, profiles={"t-ready": READY}
    ),
    "pinned_slot_serves_another_account": dict(
        config=TWO,
        request={"record": "makers"},
        keychain={"unscoped": blob("t-ready")},
        profiles={"t-ready": READY},
        meta=BOUND,
    ),
    "pinned_slot_serves_the_record": dict(
        config=TWO,
        request={"record": "makers"},
        keychain={"scoped:slot": blob("t-makers")},
        profiles={"t-makers": MAKERS},
        meta=BOUND,
    ),
    "pinned_other_organization_is_a_mismatch": dict(
        config=TWO,
        request={"record": "makers"},
        keychain={"unscoped": blob("t-org2")},
        profiles={"t-org2": OTHER_ORG},
        meta=BOUND,
    ),
    "unbound_record_needs_no_profile_call": dict(
        config=TWO,
        request={"record": "makers"},
        keychain={"unscoped": blob("t-makers")},
    ),
    "slot_holding_two_accounts_is_ambiguous": dict(
        config=TWO,
        keychain={"scoped:slot": blob("t-makers"), "unscoped": blob("t-ready")},
        profiles={"t-makers": MAKERS, "t-ready": READY},
        meta=BOUND,
    ),
    "same_account_in_both_items_is_one_identity": dict(
        config=TWO,
        keychain={"scoped:slot": blob("t-makers"), "unscoped": blob("t-makers-2")},
        profiles={"t-makers": MAKERS, "t-makers-2": MAKERS},
        meta=BOUND,
    ),
    "unreadable_keychain_is_unknown": dict(
        config=TWO, keychain={"unscoped": {"error": True}}, meta=BOUND
    ),
    "api_key_record_is_not_a_subscription_question": dict(
        config=rec("key", auth="api_key", route="zai/glm"), request={"record": "key"}
    ),
    "codex_record_is_unsupported": dict(
        config=rec("cx", harness="codex"), request={"record": "cx"}
    ),
    "own_config_dir_reads_only_its_scoped_item": dict(
        config=TWO + ALT,
        request={"record": "alt"},
        keychain={"scoped:alt": blob("t-makers"), "unscoped": blob("t-ready")},
        profiles={"t-makers": MAKERS, "t-ready": READY},
        meta={"alt": MAKERS, "ready": READY},
    ),
    "own_config_dir_reads_its_credentials_file": dict(
        config=ALT,
        request={"record": "alt"},
        files={"alt": blob("t-makers")},
        profiles={"t-makers": MAKERS},
        meta={"alt": MAKERS},
    ),
    "credentials_file_dedups_against_the_keychain": dict(
        config=TWO,
        keychain={"unscoped": blob("t-ready")},
        files={"slot": blob("t-ready")},
        profiles={"t-ready": READY},
        meta=BOUND,
    ),
    "oauth_dir_record_reads_its_source_dir": dict(
        config=rec("od", auth="oauth_dir", credentials_source="{TMP}/alt"),
        request={"record": "od"},
        files={"alt": blob("t-makers")},
        profiles={"t-makers": MAKERS},
        meta={"od": MAKERS},
    ),
    "no_slot_credential": dict(config=TWO, meta=BOUND),
    "logged_out_residue_is_no_credential": dict(
        config=TWO,
        keychain={
            "unscoped": '{"claudeAiOauth": {"accessToken": "", "refreshToken": ""}}'
        },
        meta=BOUND,
    ),
    "refresh_only_blob_cannot_prove": dict(
        config=TWO,
        keychain={"unscoped": '{"claudeAiOauth": {"refreshToken": "r"}}'},
        meta=BOUND,
    ),
    "profile_rejected": dict(
        config=TWO,
        keychain={"unscoped": blob("t-dead")},
        profiles={"t-dead": "rejected"},
        meta=BOUND,
    ),
    "profile_unavailable": dict(
        config=TWO, keychain={"unscoped": blob("t-ready")}, meta=BOUND
    ),
    "profile_malformed": dict(
        config=TWO,
        keychain={"unscoped": blob("t-ready")},
        profiles={"t-ready": "malformed"},
        meta=BOUND,
    ),
    "fresh_cached_proof_is_reused": dict(
        config=TWO,
        keychain={"unscoped": blob("t-ready")},
        meta=BOUND,
        cache={"credential": blob("t-ready"), "identity": "acc-ready/org-1", "age": 60},
    ),
    "stale_cached_proof_is_reproven": dict(
        config=TWO,
        keychain={"unscoped": blob("t-ready")},
        profiles={"t-ready": READY},
        meta=BOUND,
        cache={
            "credential": blob("t-ready"),
            "identity": "acc-makers/org-1",
            "age": 2000,
        },
    ),
    "cached_proof_for_another_credential_is_ignored": dict(
        config=TWO,
        keychain={"unscoped": blob("t-ready")},
        profiles={"t-ready": READY},
        meta=BOUND,
        cache={"credential": blob("t-old"), "identity": "acc-makers/org-1", "age": 60},
    ),
    "pinned_cached_mismatch_names_the_record": dict(
        config=TWO,
        request={"record": "makers"},
        keychain={"unscoped": blob("t-ready")},
        meta=BOUND,
        cache={"credential": blob("t-ready"), "identity": "acc-ready/org-1", "age": 60},
    ),
    "bearer_needs_a_record": dict(config=TWO, request={"bearer": "t-ready"}),
    "bearer_matches_its_record": dict(
        config=TWO,
        request={"record": "ready", "bearer": "t-ready"},
        keychain={"unscoped": blob("t-ready")},
        profiles={"t-ready": READY},
        meta=BOUND,
    ),
    "bearer_belongs_to_another_account": dict(
        config=TWO,
        request={"record": "makers", "bearer": "t-ready"},
        keychain={"unscoped": blob("t-ready")},
        profiles={"t-ready": READY},
        meta=BOUND,
    ),
    "bearer_on_an_unbound_record_is_unprovable": dict(
        config=TWO,
        request={"record": "makers", "bearer": "t-ready"},
        keychain={"unscoped": blob("t-ready")},
        profiles={"t-ready": READY},
    ),
    "bearer_refuses_a_slot_holding_two_principals": dict(
        config=TWO,
        request={"record": "ready", "bearer": "t-ready"},
        keychain={"scoped:slot": blob("t-makers"), "unscoped": blob("t-ready")},
        profiles={"t-makers": MAKERS, "t-ready": READY},
        meta=BOUND,
    ),
    "bearer_reports_an_unprovable_slot_as_unknown": dict(
        config=TWO,
        request={"record": "ready", "bearer": "t-ready"},
        keychain={"unscoped": blob("t-x")},
        meta=BOUND,
    ),
    "bearer_on_an_own_dir_skips_the_shared_slot": dict(
        config=TWO + ALT,
        request={"record": "alt", "bearer": "t-makers"},
        profiles={"t-makers": MAKERS},
        meta={"alt": MAKERS},
    ),
    "bearer_uses_a_fresh_cached_proof": dict(
        config=TWO,
        request={"record": "ready", "bearer": "t-ready"},
        keychain={"unscoped": blob("t-ready")},
        profiles={"t-ready": READY},
        meta=BOUND,
        cache={"credential": "t-ready", "identity": "acc-makers/org-1", "age": 60},
    ),
}


def run_case(case: dict, tmp: Path) -> dict:
    from fno.adapters.providers import binding, loader, managed

    home, store, alt = tmp / "home", tmp / "state" / "providers", tmp / "alt"
    for d in (home, store, alt, home / ".claude"):
        d.mkdir(parents=True, exist_ok=True)
    config = tmp / "config.toml"
    config.write_text(
        case.get("config", "").replace("{TMP}", str(tmp)), encoding="utf-8"
    )
    for key in [k for k in os.environ if k.startswith(("FNO_", "CLAUDE_"))]:
        del os.environ[key]
    os.environ.update(
        HOME=str(home),
        FNO_CONFIG=str(config),
        FNO_STATE_DIR=str(tmp / "state"),
        USER="tester",
    )
    dirs = {"slot": home / ".claude", "alt": alt}
    for label, text in case.get("files", {}).items():
        (dirs[label] / ".credentials.json").write_text(text, encoding="utf-8")
    for rid, principal in case.get("meta", {}).items():
        managed.write_record_principal(rid, principal, store)
    if "cache" in case:
        c = case["cache"]
        body = {
            "account_uuid": c["identity"],
            "credential": c["digest"],
            "at": NOW - c["age"],
        }
        (store / ".active-claude.principal").write_text(
            json.dumps(body), encoding="utf-8"
        )
    services = {"Claude Code-credentials": "unscoped"}
    services.update(
        {
            managed._claude_scoped_service(d): f"scoped:{label}"
            for label, d in dirs.items()
        }
    )
    keychain = case.get("keychain", {})

    def fake_security(args):
        value = keychain.get(services.get(args[2], ""))
        if isinstance(value, dict):
            return subprocess.CompletedProcess(args, 1, "", "denied")
        if value is None:
            return subprocess.CompletedProcess(args, 44, "", "")
        return subprocess.CompletedProcess(args, 0, value + "\n", "")

    profiles = case.get("profiles", {})

    def fake_profile(bearer):
        got = profiles.get(bearer, "unavailable")
        if isinstance(got, dict):
            return dict(got), None
        return None, {
            "rejected": "credential-rejected",
            "malformed": "malformed-profile",
        }.get(got, "profile-unavailable")

    managed._run_security = fake_security
    managed.principal_of_bearer = fake_profile
    sys.platform = "darwin"
    request = case.get("request", {})
    accounts = loader.load_providers()
    record = accounts.by_id.get(request["record"]) if request.get("record") else None
    got = binding.resolve_account_binding(
        record,
        harness="claude",
        bearer=request.get("bearer"),
        root=store,
        by_id=accounts.by_id,
        now=NOW,
    )
    cache = store / ".active-claude.principal"
    return {
        "binding": {
            "harness": got.harness,
            "status": got.status,
            "credential_root": str(got.credential_root)
            if got.credential_root
            else None,
            "requested_record": got.requested_record,
            "observed_principal": got.observed_principal,
            "observed_label": got.observed_label,
            "matched_record": got.matched_record,
            "observed_at": got.observed_at,
            "reason": got.reason,
            "receipt": got.receipt,
        },
        "cache_after": json.loads(cache.read_text()) if cache.exists() else None,
    }


def main() -> int:
    from fno.adapters.providers import managed

    GOLDEN.mkdir(parents=True, exist_ok=True)
    for stale in GOLDEN.glob("*.json"):
        stale.unlink()
    for name, case in CASES.items():
        if "cache" in case:
            case["cache"]["digest"] = managed.credential_digest(
                case["cache"].pop("credential")
            )
        tmp = Path(os.path.realpath(tempfile.mkdtemp(prefix="acct-bind-")))
        try:
            expect = json.loads(
                json.dumps(run_case(case, tmp)).replace(str(tmp), "<TMP>")
            )
        finally:
            shutil.rmtree(tmp, ignore_errors=True)
        (GOLDEN / f"{name}.json").write_text(
            json.dumps({**case, "now": NOW, "expect": expect}) + "\n", encoding="utf-8"
        )
    print(f"wrote {len(CASES)} cases to {GOLDEN}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
