#!/usr/bin/env python3
"""Capture driver for the `accounts_records` goldens.

The Python leg (`fno.adapters.providers.loader`) is the parity oracle until
the adapters package is deleted. This driver owns the case table: for each
case it writes `tests/golden/accounts_records/<case>.json` holding the
fixture (config layers, slot stamps, the op) and the leg's answer under
`expect`. `accounts_records_parity.rs` builds the same fixture from the same
file and asserts the Rust answer equals `expect`.

    python3 crates/fno-agents/tests/accounts_records_golden_driver.py

Run it from the repo root with `cli/src` importable. A refusal freezes the
Python headline only (the text before pydantic's multi-line report), which
is what the Rust leg reproduces exactly. Paths are normalized to `<TMP>`.
"""

from __future__ import annotations

import dataclasses
import json
import os
import re
import shutil
import sys
import tempfile
from pathlib import Path

GOLDEN = Path(__file__).resolve().parent / "golden" / "accounts_records"


def rec(rid, harness="claude", auth="managed", **extra):
    body = {"id": rid, "name": rid.upper(), "harness": harness, "auth": auth}
    body.update(extra)
    lines = ["[[accounts.records]]"]
    for k, v in body.items():
        lines.append(f"{k} = {json.dumps(v)}")
    return "\n".join(lines) + "\n"


A = rec("alpha")
B = rec("beta", harness="codex")

# name -> (op, files, extra fields)
CASES = {
    "empty_no_files": ("load", {}, {}),
    "global_only": ("load", {"global": A}, {}),
    "project_overlays_global_by_id": (
        "load",
        {"global": A + B, "project": rec("alpha", priority=5, description="local")},
        {},
    ),
    "empty_local_records_keep_global": (
        "load",
        {"global": A, "project": "[accounts]\nrecords = []\n"},
        {},
    ),
    "legacy_providers_block": (
        "load",
        {"global": A.replace("accounts.records", "providers.records")},
        {},
    ),
    "legacy_config_wrapper": (
        "load",
        {"global": A.replace("accounts.records", "config.accounts.records")},
        {},
    ),
    "yaml_fallback": (
        "load",
        {
            "global_yaml": "accounts:\n  records:\n    - id: y\n      name: Y\n      harness: codex\n      auth: managed\n      description: null\n"
        },
        {},
    ),
    "toml_wins_over_yaml": (
        "load",
        {
            "global": A,
            "global_yaml": "accounts:\n  records:\n    - {id: y, name: Y, harness: codex, auth: managed}\n",
        },
        {},
    ),
    "unparseable_layer_reads_empty": (
        "load",
        {"global": A, "project": "[accounts\nbroken"},
        {},
    ),
    "cli_alias": (
        "load",
        {
            "global": '[[accounts.records]]\nid = "a"\nname = "A"\ncli = "codex"\nauth = "managed"\n'
        },
        {},
    ),
    "cli_beside_harness_is_metadata": ("load", {"global": rec("a", cli="codex")}, {}),
    "lax_coercion_and_strip": (
        "load",
        {
            "global": rec(
                "a",
                name="  Padded  ",
                priority=5.0,
                window_minutes="300",
                tags=[" t1 ", "t2"],
                account_id=" acct ",
            )
        },
        {},
    ),
    "unknown_metadata_kept": (
        "load",
        {
            "global": rec("a", foo=1, bar=["x"])
            + "[accounts.records.nested]\nk = true\n"
        },
        {},
    ),
    "pricing_and_cost_cap": (
        "load",
        {
            "global": rec("a", cost_cap_usd_per_session=5)
            + "[accounts.records.pricing]\ninput_per_million_usd = 3\noutput_per_million_usd = 15.5\n"
        },
        {},
    ),
    "config_dir_expands_home": (
        "load",
        {"global": rec("a", config_dir="~/.claude-alt/")},
        {},
    ),
    "oauth_dir_source_expands_home": (
        "load",
        {"global": rec("a", auth="oauth_dir", credentials_source="~/creds")},
        {},
    ),
    "api_key_env_stripped": (
        "load",
        {
            "global": rec("a", auth="api_key")
            + '[accounts.records.env]\nANTHROPIC_API_KEY = "  keychain:x  "\n'
        },
        {},
    ),
    "api_key_route": (
        "load",
        {"global": rec("a", auth="api_key", route=" zai/glm-5.3 ")},
        {},
    ),
    "active_and_auto_switch_word": (
        "load",
        {
            "global_yaml": "accounts:\n  active: a\n  auto_switch: 'yes'\n  records:\n    - {id: a, name: A, harness: claude, auth: managed}\n"
        },
        {},
    ),
    "agent_pin_and_reserved_keys": (
        "load",
        {
            "global": A
            + '[agents]\nmax_live = 15\n[agents.reviewer]\nprovider = "alpha"\n[agents.defaults]\nprovider = "claude"\n'
        },
        {},
    ),
    "agent_pins_several_and_blank_defaults": (
        "load",
        {
            "global": A
            + B
            + '[agents.reviewer]\nprovider = "alpha"\n[agents.planner]\nprovider = "beta"\n'
            + '[agents.defaults]\nprovider = ""\n'
        },
        {},
    ),
    "all_harness_literals": (
        "load",
        {
            "global": "".join(
                rec(f"h-{h}", harness=h)
                for h in [
                    "claude",
                    "codex",
                    "opencode",
                    "agy",
                    "trae",
                    "openclaw",
                    "hermes",
                    "gemini",
                ]
            )
        },
        {},
    ),
    "api_key_gemini_and_openai_env": (
        "load",
        {
            "global": rec("g", auth="api_key")
            + '[accounts.records.env]\nGEMINI_API_KEY = "x"\n'
            + rec("o", auth="api_key")
            + '[accounts.records.env]\nOPENAI_API_KEY = "x"\n'
        },
        {},
    ),
    "zero_priority": ("load", {"global": rec("a", priority=0)}, {}),
    "err_pricing_negative_cache_rate": (
        "load",
        {
            "global": rec("a")
            + "[accounts.records.pricing]\ninput_per_million_usd = 1\noutput_per_million_usd = 1\ncache_read_per_million_usd = -1\n"
        },
        {},
    ),
    "err_pricing_missing_output": (
        "load",
        {
            "global": rec("a")
            + "[accounts.records.pricing]\ninput_per_million_usd = 1\n"
        },
        {},
    ),
    "err_bad_id": ("load", {"global": rec("Bad")}, {}),
    "err_unknown_harness": ("load", {"global": rec("a", harness="cursor")}, {}),
    "err_padded_harness": ("load", {"global": rec("a", harness=" claude ")}, {}),
    "err_missing_name": (
        "load",
        {
            "global": '[[accounts.records]]\nid = "a"\nharness = "claude"\nauth = "managed"\n'
        },
        {},
    ),
    "err_oauth_without_source": ("load", {"global": rec("a", auth="oauth_dir")}, {}),
    "err_api_key_env_and_route": (
        "load",
        {
            "global": rec("a", auth="api_key", route="zai/glm")
            + '[accounts.records.env]\nANTHROPIC_API_KEY = "x"\n'
        },
        {},
    ),
    "err_api_key_neither": ("load", {"global": rec("a", auth="api_key")}, {}),
    "err_api_key_unrecognized_env": (
        "load",
        {
            "global": rec("a", auth="api_key")
            + '[accounts.records.env]\nOTHER_KEY = "x"\n'
        },
        {},
    ),
    "err_managed_with_env": (
        "load",
        {"global": rec("a") + '[accounts.records.env]\nANTHROPIC_API_KEY = "x"\n'},
        {},
    ),
    "err_route_on_managed": ("load", {"global": rec("a", route="zai/glm")}, {}),
    "err_route_unparseable": (
        "load",
        {"global": rec("a", auth="api_key", route="zai glm")},
        {},
    ),
    "err_duplicate_ids_within_file": ("load", {"global": A + A}, {}),
    "err_active_not_found": (
        "load",
        {"global": '[accounts]\nactive = "ghost"\n' + A},
        {},
    ),
    "err_dangling_agent_pin": (
        "load",
        {"global": A + '[agents.reviewer]\nprovider = "ghost"\n'},
        {},
    ),
    "err_agent_binding_extra_key": (
        "load",
        {"global": A + '[agents.reviewer]\nprovider = "alpha"\nmodel = "x"\n'},
        {},
    ),
    "err_priority_negative": ("load", {"global": rec("a", priority=-1)}, {}),
    "err_priority_fractional": ("load", {"global": rec("a", priority=1.5)}, {}),
    "err_window_zero": ("load", {"global": rec("a", window_minutes=0)}, {}),
    "err_pricing_extra_key": (
        "load",
        {
            "global": rec("a")
            + "[accounts.records.pricing]\ninput_per_million_usd = 1\noutput_per_million_usd = 1\nsurprise = 1\n"
        },
        {},
    ),
    "err_config_dir_relative": ("load", {"global": rec("a", config_dir="rel/dir")}, {}),
    "err_tags_not_list": ("load", {"global": rec("a", tags="x")}, {}),
    "err_model_name_blank": ("load", {"global": rec("a", model_name="  ")}, {}),
    "err_record_not_mapping": ("load", {"global": '[accounts]\nrecords = ["x"]\n'}, {}),
    "err_active_not_string": ("load", {"global": "[accounts]\nactive = 5\n" + A}, {}),
    "err_auto_switch_garbage": (
        "load",
        {"global": '[accounts]\nauto_switch = "garbage"\n' + A},
        {},
    ),
    "combos_local_wins_per_name": (
        "combos",
        {
            "global": A
            + B
            + '[accounts.combos.main]\nproviders = ["alpha"]\n[accounts.combos.both]\nproviders = ["alpha", "beta"]\nstrategy = "round_robin"\n',
            "project": '[accounts.combos.main]\nproviders = ["beta"]\nsticky_limit = 3\n',
        },
        {},
    ),
    "combos_unknown_provider": (
        "combos",
        {"global": A + '[accounts.combos.main]\nproviders = ["ghost"]\n'},
        {},
    ),
    "combos_malformed_top": (
        "combos",
        {"global": '[accounts]\ncombos = ["x"]\n' + A},
        {},
    ),
    "combos_empty_providers": (
        "combos",
        {"global": A + "[accounts.combos.main]\nproviders = []\n"},
        {},
    ),
    "combos_bad_strategy": (
        "combos",
        {
            "global": A
            + '[accounts.combos.main]\nproviders = ["alpha"]\nstrategy = "random"\n'
        },
        {},
    ),
    "combos_sticky_clamped": (
        "combos",
        {
            "global": A
            + '[accounts.combos.main]\nproviders = ["alpha"]\nsticky_limit = 0\n'
        },
        {},
    ),
    "active_combo_local_wins": (
        "active_combo",
        {
            "global": '[accounts]\nactive_combo = "g"\n',
            "project": '[accounts]\nactive_combo = "p"\n',
        },
        {},
    ),
    "quota_defaults": ("quota", {"global": A}, {}),
    "quota_leaf_merge_and_salvage": (
        "quota",
        {
            "global": "[accounts.quota]\nobserve = true\ndefer_threshold_pct = 50\nprobe_ttl_seconds = 120\n",
            "project": "[accounts.quota]\ndefer_threshold_pct = 80\nprobe_ttl_seconds = 0\npick_on_launch = true\n",
        },
        {},
    ),
    "known_ids_across_layers": (
        "known_ids",
        {"global": A + rec("Bad-id"), "project": B},
        {},
    ),
    "effective_active_managed_slot": (
        "effective_active",
        {"global": '[accounts]\nactive = "alpha"\n' + A + rec("gamma")},
        {"stamps": {"claude": "gamma\n"}},
    ),
    "effective_active_unstamped": (
        "effective_active",
        {"global": '[accounts]\nactive = "alpha"\n' + A},
        {},
    ),
    "effective_active_api_key": (
        "effective_active",
        {
            "global": '[accounts]\nactive = "k"\n'
            + rec("k", auth="api_key", route="zai/glm")
        },
        {"stamps": {"claude": "other"}},
    ),
    "scope_project_cross_layer_active": (
        "scope_project",
        {"global": A, "project": '[accounts]\nactive = "alpha"\n' + B},
        {},
    ),
    "scope_global": ("scope_global", {"global": A + B, "project": rec("delta")}, {}),
    "record_harness_hit": ("record_harness", {"global": A + B}, {"id": "beta"}),
    "record_harness_bad_config": (
        "record_harness",
        {"global": A + rec("Bad")},
        {"id": "alpha"},
    ),
}

LAYER_PATHS = {
    "global": "home/.fno/config.toml",
    "global_yaml": "home/.fno/settings.yaml",
    "project": "repo/.fno/config.toml",
    "project_yaml": "repo/.fno/settings.yaml",
}


def headline(msg: str) -> str:
    return re.split(r": \d+ validation errors? for ", msg)[0]


def run_case(op, files, extra, tmp: Path):
    from fno.adapters.providers import loader

    for layer, text in files.items():
        path = tmp / LAYER_PATHS[layer]
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text, encoding="utf-8")
    (tmp / "repo").mkdir(exist_ok=True)
    (tmp / "home").mkdir(exist_ok=True)
    for harness, text in extra.get("stamps", {}).items():
        stamp = tmp / "state" / "providers" / f".active-{harness}"
        stamp.parent.mkdir(parents=True, exist_ok=True)
        stamp.write_text(text, encoding="utf-8")
    for key in list(os.environ):
        if key.startswith("FNO_"):
            del os.environ[key]
    os.environ.update(
        HOME=str(tmp / "home"),
        PWD=str(tmp / "repo"),
        FNO_REPO_ROOT=str(tmp / "repo"),
        FNO_STATE_DIR=str(tmp / "state"),
    )
    os.chdir(tmp / "repo")
    loader._LEGACY_BLOCK_WARNED = True
    try:
        if op == "load":
            return {"ok": loader.load_providers().model_dump(mode="json")}
        if op == "combos":
            return {
                "ok": {
                    n: dataclasses.asdict(c) for n, c in loader.load_combos().items()
                }
            }
        if op == "active_combo":
            return {"ok": loader.load_active_combo()}
        if op == "quota":
            return {"ok": loader.load_quota_config().model_dump(mode="json")}
        if op == "known_ids":
            return {"ok": sorted(loader.known_account_ids())}
        if op == "effective_active":
            return {"ok": loader.effective_active(loader.load_providers())}
        if op == "scope_project":
            return {"ok": loader.load_scope_config("project").model_dump(mode="json")}
        if op == "scope_global":
            return {"ok": loader.load_scope_config("global").model_dump(mode="json")}
        if op == "record_harness":
            return {"ok": loader.record_harness(extra["id"])}
    except loader.ProviderConfigError as exc:
        return {"error": headline(str(exc))}
    raise SystemExit(f"unknown op {op}")


def main() -> int:
    GOLDEN.mkdir(parents=True, exist_ok=True)
    for stale in GOLDEN.glob("*.json"):
        stale.unlink()
    cwd = os.getcwd()
    for name, (op, files, extra) in CASES.items():
        tmp = Path(os.path.realpath(tempfile.mkdtemp(prefix="acct-golden-")))
        try:
            expect = run_case(op, files, extra, tmp)
        finally:
            os.chdir(cwd)
        text = json.dumps(expect).replace(str(tmp), "<TMP>")
        case = {"op": op, "files": files, **extra, "expect": json.loads(text)}
        (GOLDEN / f"{name}.json").write_text(
            json.dumps(case, indent=2) + "\n", encoding="utf-8"
        )
        shutil.rmtree(tmp, ignore_errors=True)
    print(f"wrote {len(CASES)} cases to {GOLDEN}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
