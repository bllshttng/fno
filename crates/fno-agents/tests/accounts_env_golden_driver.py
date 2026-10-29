#!/usr/bin/env python3
"""Capture driver for the `accounts_env` goldens.

The Python leg (`fno.adapters.providers.dispatch.dispatch_env`, with
`staging.verify_staged` and `dispatch.resolve_env_value` under it) is the
parity oracle until the adapters package is deleted. The `overlay_*` goldens
are frozen: their Python leg is deleted, so this driver rewrites only the
`launch_*` cases. Each case lays down an accounts config, staged
links, credential files, store principals, Keychain items and a slot stamp,
runs one leg against a fake `security` and a fake profile endpoint, and
freezes the answer in `tests/golden/accounts_env/<case>.json`.
`accounts_env_parity.rs` replays the same file against `accounts::env`.

    PYTHONPATH=cli/src python3 crates/fno-agents/tests/accounts_env_golden_driver.py

Slot Keychain items are keyed by role (`unscoped`, `scoped:slot`,
`scoped:alt`) and `${KEYCHAIN:...}` items by name, so a golden carries no
temp path.
"""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import sys
import tempfile
import types
from pathlib import Path

GOLDEN = Path(__file__).resolve().parent / "golden" / "accounts_env"
MAKERS = {"account_uuid": "acc-makers", "organization_uuid": "org-1", "email": "makers@x"}
READY = {"account_uuid": "acc-ready", "organization_uuid": "org-1", "email": "ready@x"}


def blob(token: str) -> str:
    return '{"claudeAiOauth": {"accessToken": "%s", "refreshToken": "r"}}' % token


def rec(rid: str, **extra) -> str:
    body = {"id": rid, "name": rid, "harness": "claude", "auth": "managed", **extra}
    lines = []
    env = body.pop("env", None)
    for k, v in body.items():
        lines.append(f"{k} = {json.dumps(v)}\n")
    if env is not None:
        lines.append("[accounts.records.env]\n")
        lines.extend(f"{k} = {json.dumps(v)}\n" for k, v in env.items())
    return "[[accounts.records]]\n" + "".join(lines)


TWO = rec("makers") + rec("ready")
OAUTH = rec("o1", auth="oauth_dir", credentials_source="{TMP}/creds")
CODEX = rec("cx", harness="codex", auth="oauth_dir", credentials_source="{TMP}/creds")
ALT = rec("alt", config_dir="{TMP}/alt")


def key(rid: str, harness: str = "openclaw", **env) -> str:
    return rec(rid, harness=harness, auth="api_key", env=env)


# name -> case fields. `op` is `launch_env` (dispatch_env); `id` is the
# account asked about.
CASES = {
    "launch_claude_oauth_staged": dict(
        op="launch_env", id="o1", config=OAUTH, dirs=["creds"],
        links={"state/providers/o1/.claude": "creds"},
    ),
    "launch_codex_oauth_takes_a_home_override": dict(
        op="launch_env", id="cx", config=CODEX, dirs=["creds"],
        links={"state/providers/cx/home/.codex": "creds"},
    ),
    "launch_oauth_unstaged_refuses": dict(
        op="launch_env", id="o1", config=OAUTH, dirs=["creds"],
    ),
    "launch_oauth_dangling_link_is_unstaged": dict(
        op="launch_env", id="o1", config=OAUTH, dirs=["creds"],
        links={"state/providers/o1/.claude": "gone"},
    ),
    "launch_unknown_id_is_not_found": dict(op="launch_env", id="ghost", config=TWO),
    "launch_api_key_reads_the_keychain": dict(
        op="launch_env", id="k", config=key("k", ANTHROPIC_API_KEY="${KEYCHAIN:item-a}"),
        secrets={"item-a": "sk-a"},
    ),
    "launch_api_key_missing_keychain_item_refuses": dict(
        op="launch_env", id="k", config=key("k", ANTHROPIC_API_KEY="${KEYCHAIN:does-not-exist}"),
    ),
    "launch_api_key_reads_the_environment": dict(
        op="launch_env", id="k", config=key("k", ANTHROPIC_AUTH_TOKEN="${ENV:MY_TOKEN}"),
        env={"MY_TOKEN": "env-token"},
    ),
    "launch_api_key_unset_variable_refuses": dict(
        op="launch_env", id="k", config=key("k", ANTHROPIC_AUTH_TOKEN="${ENV:MISSING_TOKEN}"),
    ),
    "launch_api_key_reads_a_key_file_first_line": dict(
        op="launch_env", id="k", config=key("k", ANTHROPIC_AUTH_TOKEN="${FILE:{TMP}/key.txt}"),
        files={"key.txt": "  sk-file  \nsecond line\n"},
    ),
    "launch_api_key_empty_key_file_refuses": dict(
        op="launch_env", id="k", config=key("k", ANTHROPIC_AUTH_TOKEN="${FILE:{TMP}/key.txt}"),
        files={"key.txt": ""},
    ),
    "launch_api_key_literals_pass_through": dict(
        op="launch_env", id="k",
        config=key("k", ANTHROPIC_API_KEY="${just-a-value}", B="plain", C="${OTHER:x}", D="${open"),
    ),
    "launch_config_dir_wins_over_managed": dict(
        op="launch_env", id="alt", config=ALT, dirs=["alt"],
    ),
    "launch_config_dir_serving_another_account_refuses": dict(
        op="launch_env", id="alt", config=ALT, dirs=["alt"],
        files={"alt/.credentials.json": blob("t-ready")},
        profiles={"t-ready": READY}, meta={"alt": MAKERS},
    ),
    "launch_managed_unbound_rides_the_slot": dict(op="launch_env", id="makers", config=TWO),
    "launch_managed_slot_serving_another_account_refuses": dict(
        op="launch_env", id="makers", config=TWO,
        keychain={"unscoped": blob("t-ready")}, profiles={"t-ready": READY},
        meta={"makers": MAKERS, "ready": READY},
    ),
    "launch_managed_slot_serving_the_record": dict(
        op="launch_env", id="makers", config=TWO,
        keychain={"unscoped": blob("t-makers")}, profiles={"t-makers": MAKERS},
        meta={"makers": MAKERS, "ready": READY},
    ),
    "launch_route_takes_the_routed_env": dict(
        op="launch_env", id="z", config=rec("z", auth="api_key", route="zai/glm-5.3"),
        env={"ZAI_API_KEY": "zai-key"},
    ),
    "launch_keyless_route_refuses": dict(
        op="launch_env", id="z", config=rec("z", auth="api_key", route="zai/glm-5.3"),
    ),
}


def lay_down(case: dict, tmp: Path) -> tuple[Path, dict]:
    """The fixture both legs read; returns the store root and slot dirs."""
    home, store, alt = tmp / "home", tmp / "state" / "providers", tmp / "alt"
    for d in (home / ".claude", store):
        d.mkdir(parents=True, exist_ok=True)
    for rel in case.get("dirs", []):
        (tmp / rel).mkdir(parents=True, exist_ok=True)
    for rel, text in case.get("files", {}).items():
        path = tmp / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text, encoding="utf-8")
    for rel, target in case.get("links", {}).items():
        link = tmp / rel
        link.parent.mkdir(parents=True, exist_ok=True)
        link.symlink_to(tmp / target)
    if "stamp" in case:
        (store / ".active-claude").write_text(case["stamp"], encoding="utf-8")
    (tmp / "config.toml").write_text(
        case["config"].replace("{TMP}", str(tmp)), encoding="utf-8"
    )
    return store, {"slot": home / ".claude", "alt": alt}


def run_case(case: dict, tmp: Path) -> dict:
    from fno.adapters.providers import dispatch, managed
    from fno.adapters.providers.model import (
        ProviderConfigError,
        ProviderNotFoundError,
        ProviderUnavailableError,
    )
    store, dirs = lay_down(case, tmp)
    for k in [k for k in os.environ if k.startswith(("FNO_", "CLAUDE_", "ZAI_"))]:
        del os.environ[k]
    for k in ("MY_TOKEN", "MISSING_TOKEN"):
        os.environ.pop(k, None)
    os.environ.update(
        HOME=str(tmp / "home"),
        FNO_CONFIG=str(tmp / "config.toml"),
        FNO_STATE_DIR=str(tmp / "state"),
        USER="tester",
        **case.get("env", {}),
    )
    for rid, principal in case.get("meta", {}).items():
        managed.write_record_principal(rid, principal, store)

    services = {"Claude Code-credentials": "unscoped"}
    services.update(
        {managed._claude_scoped_service(d): f"scoped:{label}" for label, d in dirs.items()}
    )
    keychain = case.get("keychain", {})

    def fake_security(args):
        value = keychain.get(services.get(args[2], ""))
        if value is None:
            return subprocess.CompletedProcess(args, 44, "", "")
        return subprocess.CompletedProcess(args, 0, value + "\n", "")

    profiles = case.get("profiles", {})

    def fake_profile(bearer):
        got = profiles.get(bearer)
        return (dict(got), None) if got else (None, "profile-unavailable")

    secrets = case.get("secrets", {})

    def fake_run(cmd, **_kwargs):
        if cmd[-1] not in secrets:
            raise subprocess.CalledProcessError(44, cmd)
        return subprocess.CompletedProcess(cmd, 0, secrets[cmd[-1]] + "\n", "")

    managed._run_security = fake_security
    managed.principal_of_bearer = fake_profile
    dispatch.subprocess = types.SimpleNamespace(
        run=fake_run, CalledProcessError=subprocess.CalledProcessError
    )
    sys.platform = "darwin"
    os.chdir(tmp)

    try:
        return {"env": dispatch.dispatch_env(case["id"], root=store)}
    except (ProviderConfigError, ProviderNotFoundError, ProviderUnavailableError) as exc:
        # args[0]: str() of a KeyError subclass wraps the text in quotes.
        return {"error": type(exc).__name__, "message": exc.args[0]}


def main() -> int:
    GOLDEN.mkdir(parents=True, exist_ok=True)
    for stale in GOLDEN.glob("launch_*.json"):
        stale.unlink()
    for name, case in CASES.items():
        tmp = Path(os.path.realpath(tempfile.mkdtemp(prefix="acct-env-")))
        try:
            expect = json.loads(json.dumps(run_case(case, tmp)).replace(str(tmp), "<TMP>"))
        finally:
            os.chdir("/")
            shutil.rmtree(tmp, ignore_errors=True)
        (GOLDEN / f"{name}.json").write_text(
            json.dumps({**case, "expect": expect}) + "\n", encoding="utf-8"
        )
    print(f"wrote {len(CASES)} cases to {GOLDEN}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
