#!/usr/bin/env python3
"""Capture driver for the `accounts_launch` goldens.

The Python legs these cases froze were `fno.agents.account_env`
(`compose_worker_credentials`, `seal_state_root`), the `--dispatch-account`
block of `agents/cli.py` `cmd_spawn` and `agents/dispatch.py`
`_account_id_for_env`. They were deleted when `accounts::credentials` and
`accounts::launch` took them over, so the goldens are frozen and this driver
only re-runs at a commit that still has those legs.
`accounts_launch_parity.rs` replays each file against the Rust owners.

    FNO_CAPTURE_GOLDEN=1 PYTHONPATH=cli/src python3 crates/fno-agents/tests/accounts_launch_golden_driver.py

The fixture helpers are the `accounts_env` driver's, so both case tables lay
down a config the same way.
"""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from accounts_env_golden_driver import key, lay_down, rec  # noqa: E402

GOLDEN = Path(__file__).resolve().parent / "golden" / "accounts_launch"
ROUTE = {
    "ANTHROPIC_BASE_URL": "https://api.example.test/anthropic",
    "ANTHROPIC_AUTH_TOKEN": "route-token",
    "ANTHROPIC_MODEL": "m-route",
}
AMBIENT = {
    "PATH": "/bin",
    "ANTHROPIC_API_KEY": "ambient-key",
    "CLAUDE_CODE_OAUTH_TOKEN": "ambient-oauth",
    "ANTHROPIC_DEFAULT_OPUS_MODEL": "glm-ambient",
    "FNO_ROUTE_PROVIDER": "zai",
}
ALT = rec("alt", config_dir="{TMP}/alt")
LIT = key("lit", "claude", ANTHROPIC_BASE_URL="https://lit.example.test", ANTHROPIC_API_KEY="lit-key")
CODEX_KEY = key("cx", "codex", OPENAI_API_KEY="cx-key")

# name -> case fields. `op` is `credentials`, `seal`, `dispatch` or
# `account_for_env`.
CASES = {
    "credentials_account_only": dict(
        op="credentials", account_env={"CLAUDE_CONFIG_DIR": "/acct/alt"},
        base=AMBIENT, account_id="alt",
    ),
    "credentials_route_only": dict(op="credentials", route_env=ROUTE, base=AMBIENT),
    "credentials_self_authed_route_drops_the_account_key": dict(
        op="credentials",
        account_env={"ANTHROPIC_API_KEY": "acct-key", "ANTHROPIC_BASE_URL": "https://acct.example.test"},
        route_env=ROUTE, base=AMBIENT, account_id="k",
    ),
    "credentials_keyless_route_over_an_account": dict(
        op="credentials", account_env={"CLAUDE_CONFIG_DIR": "/acct/alt"},
        route_env={"ANTHROPIC_BASE_URL": "https://api.example.test", "ANTHROPIC_DEFAULT_SONNET_MODEL": "s-route"},
        base={}, account_id="alt",
    ),
    "credentials_neither_reads_ambient": dict(op="credentials", base=AMBIENT),
    "credentials_api_key_account_bills_its_endpoint_host": dict(
        op="credentials",
        account_env={"ANTHROPIC_BASE_URL": "https://User@API.Example.test:8443/v1", "ANTHROPIC_API_KEY": "k",
                     "ANTHROPIC_DEFAULT_HAIKU_MODEL": "h-acct"},
        base={}, account_id="k",
    ),
    "seal_moved_home_pins_every_root": dict(op="seal", env={"HOME": "/acct/home", "X": "1"}),
    "seal_keeps_a_root_the_overlay_names": dict(
        op="seal", env={"HOME": "/acct/home", "FNO_STATE_DIR": "/meant/state"},
    ),
    "seal_claims_root_falls_back_to_home": dict(
        op="seal", env={"HOME": "/acct/home"}, unset=["FNO_CLAIMS_ROOT"],
    ),
    "seal_without_home_is_unchanged": dict(op="seal", env={"CODEX_HOME": "/acct/codex"}),
    "seal_same_home_is_unchanged": dict(op="seal", env={"HOME": "{HOME}"}),
    "dispatch_record_derived": dict(op="dispatch", id="lit", harness="claude", config=LIT),
    "dispatch_config_dir_record": dict(op="dispatch", id="alt", harness="claude", config=ALT, dirs=["alt"]),
    "dispatch_unknown_record_refuses": dict(op="dispatch", id="ghost", harness="claude", config=LIT),
    "dispatch_harness_mismatch_refuses": dict(op="dispatch", id="cx", harness="claude", config=CODEX_KEY),
    "dispatch_empty_carrier_derives_from_the_record": dict(
        op="dispatch", id="lit", harness="claude", config=LIT, carrier="",
    ),
    "dispatch_non_map_carrier_refuses": dict(
        op="dispatch", id="cx", harness="codex", config=CODEX_KEY, carrier='["a"]',
    ),
    "dispatch_empty_map_carrier_refuses": dict(
        op="dispatch", id="cx", harness="codex", config=CODEX_KEY, carrier="{}",
    ),
    "dispatch_proven_carrier_wins": dict(
        op="dispatch", id="cx", harness="codex", config=CODEX_KEY, carrier='{"OPENAI_API_KEY": "proven", "B": "2"}',
    ),
    "dispatch_carrier_without_a_record_refuses": dict(
        op="dispatch", id=None, harness="claude", config=LIT, carrier='{"A": "1"}',
    ),
    "dispatch_unresolvable_record_refuses": dict(
        op="dispatch", id="k", harness="claude", config=key("k", "claude", ANTHROPIC_API_KEY="${ENV:MISSING_TOKEN}"),
    ),
    "account_for_env_matches_a_config_dir": dict(
        op="account_for_env", config=ALT + LIT + CODEX_KEY, dirs=["alt"],
        files={"alt/.credentials.json": "{}"}, env={"CLAUDE_CONFIG_DIR": "{TMP}/alt"},
    ),
    "account_for_env_matches_an_api_key_record": dict(
        op="account_for_env", config=ALT + LIT, env={"ANTHROPIC_BASE_URL": "https://lit.example.test", "ANTHROPIC_API_KEY": "lit-key"},
    ),
    "account_for_env_no_match": dict(op="account_for_env", config=LIT + CODEX_KEY, env={"OPENAI_API_KEY": "cx-key"}),
}


def dispatch_block(case: dict, workdir: Path) -> dict:
    """The `--dispatch-account` block of `cmd_spawn` (agents/cli.py), its refusals as text."""
    from fno.adapters.providers.dispatch import dispatch_env
    from fno.adapters.providers.loader import load_providers

    raw, account, harness = case.get("carrier"), case["id"], case["harness"]
    if raw is not None and account is None:
        return {"refusal": "refusing: the proven-credential carrier is set but no "
                "--dispatch-account was given; no worker launched"}
    proven = None
    if raw:
        try:
            parsed = json.loads(raw)
            if not isinstance(parsed, dict) or not all(
                isinstance(k, str) and isinstance(v, str) for k, v in parsed.items()
            ):
                raise ValueError("carrier payload must be a flat string map")
            if not parsed:
                raise ValueError("carrier payload is empty")
            proven = parsed
        except (TypeError, ValueError) as exc:
            return {"refusal": f"refusing --dispatch-account {account!r}: the proven-credential "
                    f"carrier is unreadable ({exc}); no worker launched"}
    try:
        record = load_providers(repo_root=workdir).by_id.get(account)
        if record is None:
            raise ValueError("not a registered provider record")
        got = (getattr(record, "harness", "") or "").strip()
        if got != harness:
            raise ValueError(f"record is a {got or '<no harness>'} account but the spawn resolves {harness}")
        env = proven if proven is not None else dispatch_env(account, repo_root=workdir)
    except Exception as exc:  # noqa: BLE001 - the block's own catch
        return {"refusal": f"refusing --dispatch-account {account!r}: {exc}; no worker launched"}
    return {
        "env": env,
        "credential_source": "canary-proven carrier" if proven is not None else "record-derived",
        "credential_env_keys": sorted(env),
    }


def run_case(case: dict, tmp: Path) -> dict:
    from fno.adapters.providers import managed
    from fno.agents import account_env, dispatch

    for k in [k for k in os.environ if k.startswith(("FNO_", "CLAUDE_"))]:
        del os.environ[k]
    os.environ.pop("MISSING_TOKEN", None)
    if case["op"] == "credentials":
        (tmp / "config.toml").write_text("", encoding="utf-8")
        os.environ.update(HOME=str(tmp / "home"), FNO_CONFIG=str(tmp / "config.toml"))
        os.chdir(tmp)
        env, decision = account_env.compose_worker_credentials(
            case.get("account_env"), case.get("route_env"), case["base"],
            account_id=case.get("account_id"),
        )
        return {"env": env, "decision": vars(decision)}
    if case["op"] == "seal":
        roots = {"HOME": str(tmp / "home"), "FNO_AGENTS_HOME": str(tmp / "agents"),
                 "FNO_STATE_DIR": str(tmp / "state"), "FNO_CLAIMS_ROOT": str(tmp / "claims")}
        for name in case.get("unset", []):
            roots.pop(name)
        os.environ.update(roots)
        env = {k: v.replace("{HOME}", roots["HOME"]) for k, v in case["env"].items()}
        return {"env": account_env.seal_state_root(env)}
    store, _ = lay_down(case, tmp)
    os.environ.update(HOME=str(tmp / "home"), FNO_CONFIG=str(tmp / "config.toml"),
                      FNO_STATE_DIR=str(tmp / "state"), USER="tester")
    managed._run_security = lambda args: subprocess.CompletedProcess(args, 44, "", "")
    sys.platform = "darwin"
    os.chdir(tmp)
    if case["op"] == "dispatch":
        return dispatch_block(case, tmp)
    env = {k: v.replace("{TMP}", str(tmp)) for k, v in case["env"].items()}
    return {"account": dispatch._account_id_for_env(env)}


def main() -> int:
    if os.environ.get("FNO_CAPTURE_GOLDEN") != "1":
        print("set FNO_CAPTURE_GOLDEN=1 to rewrite the frozen goldens", file=sys.stderr)
        return 2
    GOLDEN.mkdir(parents=True, exist_ok=True)
    for stale in GOLDEN.glob("*.json"):
        stale.unlink()
    for name, case in CASES.items():
        tmp = Path(os.path.realpath(tempfile.mkdtemp(prefix="acct-launch-")))
        saved = dict(os.environ)
        try:
            expect = json.loads(json.dumps(run_case(case, tmp)).replace(str(tmp), "<TMP>"))
        finally:
            os.environ.clear()
            os.environ.update(saved)
            os.chdir("/")
            shutil.rmtree(tmp, ignore_errors=True)
        (GOLDEN / f"{name}.json").write_text(
            json.dumps({**case, "expect": expect}, sort_keys=True) + "\n", encoding="utf-8"
        )
    print(f"wrote {len(CASES)} cases to {GOLDEN}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
