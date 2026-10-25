"""Capture the route family's characterization goldens from the Python leg.

Port protocol step 2, run BEFORE the deletion touches anything:

    cd cli && uv run python \
      ../crates/fno-agents/tests/fixtures/config_route/capture_driver.py

Writes tests/golden/config_route/cases.json (the `config route` command surface,
driven through the typer app in-process) and
tests/golden/dispatch_harness/cases.json (the dispatch_flags decisions,
driven as direct function calls). Every case runs with the config world
pinned to a per-case tmp dir (FNO_CONFIG as the sole candidate, absent global
tier, empty agents home and state), the same pin set the route_gather
fixtures froze under, so the capture is hermetic against the developer's own
config. Cases that need the route-slot or spawn-overlay doors use the
DEPLOYED fno-agents binary; those doors are outside this port and stable.
"""

from __future__ import annotations

import json
import os
import sys
import tempfile
from pathlib import Path

REPO = Path(__file__).resolve().parents[5]
sys.path.insert(0, str(REPO / "cli" / "src"))

FIXTURE_DIR = REPO / "crates/fno-agents/tests/fixtures/config_route"
GOLDEN_DIR = REPO / "crates/fno-agents/tests/golden"

# The z.ai record every fixture config declares (the built-in shape the
# route tests pin): anthropic protocol, key from ZAI_API_KEY.
ZAI_RECORD = """[model_routing.providers.zai]
protocol = "anthropic"
base_url = "https://api.z.ai/api/anthropic"
api_key_env = "ZAI_API_KEY"
api_key_file = "~/.fno/.env"

[model_routing.providers.zai.tier_models]
haiku = "glm-4.7"
"""

INVENTORY_ROWS = """[[routing.models]]
name = "glm-5.3"
harness = "claude"
model = "glm-5.3"
route = "zai/glm-5.3"
band = "medium"

[[routing.models]]
name = "ghost-x"
harness = "ghostharness"
model = "g"
band = "high"
"""


def write_config(tmp: Path, body: str) -> Path:
    path = tmp / "config.toml"
    path.write_text(body, encoding="utf-8")
    return path


def pin_env(tmp: Path) -> None:
    os.environ["FNO_CONFIG"] = str(tmp / "config.toml")
    os.environ["FNO_GLOBAL_SETTINGS_PATH"] = str(tmp / "absent-global.json")
    os.environ["FNO_AGENTS_HOME"] = str(tmp / "agents-home")
    os.environ["FNO_STATE_DIR"] = str(tmp / "state")
    os.environ["FNO_NO_CANONICAL_CONFIG"] = "1"


def unpin_env() -> None:
    for key in (
        "FNO_CONFIG",
        "FNO_GLOBAL_SETTINGS_PATH",
        "FNO_AGENTS_HOME",
        "FNO_STATE_DIR",
        "FNO_NO_CANONICAL_CONFIG",
        "ZAI_API_KEY",
    ):
        os.environ.pop(key, None)


class Case:
    def __init__(self, label: str, config: str = "", home: Path | None = None) -> None:
        self.label = label
        self.config = config
        self.home = home
        self.exit = 0
        self.out = ""
        self.err = ""
        self.args: list[str] = []
        self.env: dict[str, str] = {}

    def run_command(self, args: list[str], env: dict[str, str] | None = None,
                 cwd: Path | None = None) -> "Case":
        self.args = list(args)
        self.env = dict(env or {})
        from typer.testing import CliRunner

        from fno.route_cli import route_app

        old_cwd = os.getcwd()
        if cwd is not None:
            os.chdir(cwd)
        # HOME pins the key-file fallback (~/.fno/.env) and the writer's
        # global target into the case's tmp world: a dev shell carrying a live
        # key file would otherwise leak a real token into the golden and let
        # the fail-closed case pass with exports.
        pin = dict(env or {})
        if self.home is not None:
            pin.setdefault("HOME", str(self.home))
        saved = {k: os.environ.get(k) for k in pin}
        os.environ.update(pin)
        try:
            result = CliRunner().invoke(route_app, args)
            self.exit = int(result.exit_code or 0)
            self.out = result.stdout or ""
            self.err = (result.stderr or "") if self.exit else (result.stderr or "")
            if result.exception is not None and not isinstance(
                result.exception, SystemExit
            ):
                self.err = f"{type(result.exception).__name__}: {result.exception}"
                self.exit = self.exit or 1
        finally:
            os.chdir(old_cwd)
            for k, v in saved.items():
                if v is None:
                    os.environ.pop(k, None)
                else:
                    os.environ[k] = v
        return self


def run_fn(label: str, fn) -> dict:
    try:
        value = fn()
        # Deterministic rendering for whatever the call returns (tuples and
        # dataclasses included), so the golden stays json-comparable.
        out = value if isinstance(value, str) else json.dumps(
            value, sort_keys=True, default=repr
        )
        return {"label": label, "exit": 0, "out": out, "err": ""}
    except Exception as exc:  # noqa: BLE001 - the refusal IS the golden
        return {
            "label": label,
            "exit": getattr(exc, "exit_code", 1) or 1,
            "out": "",
            "err": f"{type(exc).__name__}: {exc}",
        }


def command_cases() -> list[dict]:
    cases: list[Case] = []
    base = ZAI_RECORD
    seeded = base + '[model_routing.roles]\nbuild = "zai/glm-5.2"\ntidy = "zai/glm-4.6"\n'

    tmp_ctx = tempfile.TemporaryDirectory()
    tmp = Path(tmp_ctx.name)
    SCRUBS.append((str(tmp), "<CASE-TMP>"))
    (tmp / "repo" / ".git").mkdir(parents=True)

    # A nonexistent key env everywhere: the key column answers MISSING and the
    # env command answers its fail-closed line, regardless of the dev's shell.
    os.environ.pop("ZAI_API_KEY", None)

    def case(label: str, config: str, args: list[str], env: dict | None = None,
             cwd: Path | None = None) -> None:
        local = tmp / f"cfg-{len(cases)}.toml"
        local.write_text(config, encoding="utf-8")
        home = tmp / f"home-{len(cases)}"
        home.mkdir(exist_ok=True)
        pin = dict(env or {})
        pin.setdefault("FNO_CONFIG", str(local))
        cases.append(Case(label, config=config, home=home).run_command(args, env=pin, cwd=cwd))

    case("ls text renders the table", base, ["ls"])
    case("ls json emits the rows", base, ["ls", "--json"])
    case("set comma form normalizes", base, ["set", "tidy", "zai,glm-4.7"])
    case("set slash form writes", base, ["set", "build", "zai/glm-5.2"])
    case("set protected role refuses", base, ["set", "implement", "zai,glm-5.2"])
    case("set malformed target refuses", base, ["set", "build", "zia-no-slash"])
    case("set unknown provider refuses", base, ["set", "build", "zia/glm-5.2"])
    case("unset configured role reverts", seeded, ["unset", "tidy"])
    case("unset default-routed no-op", base, ["unset", "build"])
    case("unset unknown role no-op", base, ["unset", "random-role"])
    case(
        "env routed prints unset then exports",
        base,
        ["env", "zai/glm-5.3"],
        env={"ZAI_API_KEY": "zk-golden"},
    )
    case("env missing key fails closed", base, ["env", "zai/glm-5.3"])
    case("env role unrouted fails", base, ["env", "never-routed-role"])
    case("env malformed target refuses", base, ["env", "zai/"])
    case("init appends the sample", "", ["init"])
    # Idempotency needs the SAME home twice: the second run must find the
    # marker the first run appended.
    idem_home = tmp / "home-idem"
    idem_home.mkdir(exist_ok=True)
    idem_case = Case("init is idempotent", home=idem_home)
    idem_case.run_command(["init"], env={"FNO_CONFIG": str(tmp / "cfg-idem.toml")})
    idem_case.run_command(["init"], env={"FNO_CONFIG": str(tmp / "cfg-idem.toml")})
    cases.append(idem_case)
    case(
        "inventory without declared rows answers the note",
        base,
        ["inventory"],
    )
    case(
        "inventory declares rows bands and verdicts",
        base + INVENTORY_ROWS,
        ["inventory"],
    )
    case(
        "inventory json carries slots and policy",
        base + INVENTORY_ROWS,
        ["inventory", "--json"],
    )
    case("settings ls refuses a sub-day prune", base,
         ["settings", "ls", "--prune", "--age-days", "0"])

    out = []
    for c in cases:
        out.append({
            "label": c.label,
            "args": c.args,
            "config": c.config,
            "env": c.env,
            "exit": c.exit,
            "out": c.out,
            "err": c.err,
        })
    tmp_ctx.cleanup()
    return out


def settings_ls_case() -> dict:
    """One seeded-overlay readout: the parser folds a written overlay file
    back into the row table."""
    import time

    overlay_dir_ctx = tempfile.TemporaryDirectory()
    overlay_dir = Path(overlay_dir_ctx.name)
    pin_ctx = tempfile.TemporaryDirectory()
    pin = Path(pin_ctx.name)
    SCRUBS.append((str(pin), "<SETTINGS-TMP>"))
    state = pin / "state" / "route-settings"
    state.mkdir(parents=True)
    overlay = {
        "ANTHROPIC_BASE_URL": "https://api.z.ai/api/anthropic",
        "ANTHROPIC_AUTH_TOKEN": "tok-golden",
        "ANTHROPIC_MODEL": "glm-5.3",
        "ANTHROPIC_DEFAULT_HAIKU_MODEL": "glm-4.6",
    }
    path = state / "seeded.json"
    path.write_text(json.dumps(overlay), encoding="utf-8")
    age = time.time() - (20 * 86400)
    os.utime(path, (age, age))
    local = pin / "config.toml"
    local.write_text(ZAI_RECORD, encoding="utf-8")
    os.environ["FNO_CONFIG"] = str(local)
    os.environ["FNO_STATE_DIR"] = str(pin / "state")
    os.environ["FNO_GLOBAL_SETTINGS_PATH"] = str(pin / "absent-global.json")
    os.environ["FNO_AGENTS_HOME"] = str(pin / "agents-home")
    os.environ["FNO_NO_CANONICAL_CONFIG"] = "1"
    try:
        c = Case("settings ls folds a seeded overlay", home=pin).run_command(
            ["settings", "ls"]
        )
        row = {"label": c.label, "payload": {"mode": "settings"},
               "config": "", "env": {}, "overlay": overlay,
               "exit": c.exit, "out": c.out, "err": c.err}
    finally:
        overlay_dir_ctx.cleanup()
        pin_ctx.cleanup()
        unpin_env()
    return row


def dispatch_cases() -> list[dict]:
    """One payload per case: the parity test replays each payload against the
    door, so the golden carries the payload the OLD leg answered plus its
    answer."""
    from fno.config import load_settings
    from fno.dispatch_flags import (
        configured_dispatch_harness,
        configured_dispatch_route,
        infer_invoking_harness,
        reject_empty_model,
        resolve_dispatch_harness,
    )

    def answer_of(label: str, payload: dict, config: str, call,
                  env: dict | None = None) -> dict:
        pin_ctx = None
        if config:
            pin_ctx = tempfile.TemporaryDirectory()
            cfg = Path(pin_ctx.name) / "config.toml"
            cfg.write_text(config, encoding="utf-8")
            os.environ["FNO_CONFIG"] = str(cfg)
            os.environ["FNO_GLOBAL_SETTINGS_PATH"] = str(Path(pin_ctx.name) / "absent.json")
            os.environ["FNO_NO_CANONICAL_CONFIG"] = "1"
        saved = {k: os.environ.get(k) for k in (env or {})}
        os.environ.update(env or {})
        try:
            try:
                value = call()
                out = value if isinstance(value, str) else json.dumps(
                    value, sort_keys=True, default=repr)
                row = {"label": label, "payload": payload, "config": config,
                       "env": env or {}, "exit": 0, "out": out, "err": ""}
            except Exception as exc:  # noqa: BLE001 - the refusal IS the golden
                row = {"label": label, "payload": payload, "config": config,
                       "env": env or {},
                       "exit": getattr(exc, "exit_code", 1) or 1,
                       "out": "",
                       "err": f"{type(exc).__name__}: {exc}"}
        finally:
            if pin_ctx is not None:
                pin_ctx.cleanup()
                os.environ.pop("FNO_CONFIG", None)
                os.environ.pop("FNO_GLOBAL_SETTINGS_PATH", None)
                os.environ.pop("FNO_NO_CANONICAL_CONFIG", None)
            for k, v in saved.items():
                if v is None:
                    os.environ.pop(k, None)
                else:
                    os.environ[k] = v
        return row

    rows: list[dict] = []
    zai = ZAI_RECORD
    stage_cfg = zai + '[agents.profiles.target]\nprovider = "codex"\nroute = "zai/glm-5.3"\n'
    legacy_cfg = zai + '[dispatch]\nharness = "codex"\n'
    shadow_cfg = zai + '[dispatch]\nharness = "codex"\n[agents.profiles.target]\nprovider = "opencode"\n'

    def resolve_case(label, explicit, env, flag=None):
        payload = {"mode": "resolve", "explicit": explicit, "env": env}
        if flag:
            payload["flag"] = flag
        if explicit is not None:
            def call():
                value = resolve_dispatch_harness(explicit, flag=flag or "--harness")
                return [value[0], value[1]]
        else:
            def call():
                value = resolve_dispatch_harness(None, env=env, flag=flag or "--harness")
                return [value[0], value[1]]
        return answer_of(label, payload, config="", call=call, env=env)

    rows.append(resolve_case("resolve explicit wins", "codex", {}))
    rows.append(resolve_case("resolve explicit empty refuses", "   ", {}))
    rows.append(resolve_case("resolve infers from one marker", None,
                             {"CODEX_THREAD_ID": "th_golden"}))
    rows.append(resolve_case("resolve claudecode marker yields to the codex thread", None,
                             {"CODEX_THREAD_ID": "th_golden", "CLAUDECODE": "1"}))
    rows.append(resolve_case("resolve no marker answers builtin default", None, {}))
    rows.append(resolve_case("resolve flag naming rides through", "", {},
                             flag="--provider"))
    rows.append(answer_of("infer returns none on contradiction",
                          {"mode": "resolve", "explicit": None,
                           "env": {"CODEX_THREAD_ID": "th_golden",
                                   "CLAUDE_CODE_SESSION_ID": "ses_golden"}},
                          config="",
                          call=lambda: infer_invoking_harness({
                              "CODEX_THREAD_ID": "th_golden",
                              "CLAUDE_CODE_SESSION_ID": "ses_golden"})))
    rows.append(resolve_case("infer reads the codex thread", None,
                             {"CODEX_THREAD_ID": "th_golden"}))
    rows.append(answer_of("reject model none passes",
                          {"mode": "validate-model", "model": None},
                          config="",
                          call=lambda: reject_empty_model(None)))
    rows.append(answer_of("reject model token passes verbatim",
                          {"mode": "validate-model", "model": "glm-5.3[1m]"},
                          config="",
                          call=lambda: reject_empty_model("glm-5.3[1m]"))
                 | {"out": "glm-5.3[1m]"})
    rows.append(answer_of("reject model empty refuses",
                          {"mode": "validate-model", "model": "  "},
                          config="",
                          call=lambda: reject_empty_model("  ")))
    rows.append(answer_of("configured stage table answers harness and route",
                          {"mode": "configured", "command": "target"},
                          config=stage_cfg,
                          call=lambda: {"harness": (configured_dispatch_harness(load_settings()) or (None,))[0],
                                        "note": (configured_dispatch_harness(load_settings()) or (None, None))[1],
                                        "route": configured_dispatch_route(load_settings())}))
    rows.append(answer_of("configured legacy key still answers",
                          {"mode": "configured", "command": "target"},
                          config=legacy_cfg,
                          call=lambda: {"harness": (configured_dispatch_harness(load_settings()) or (None,))[0],
                                        "note": (configured_dispatch_harness(load_settings()) or (None, None))[1],
                                        "route": configured_dispatch_route(load_settings())}))
    rows.append(answer_of("configured unset answers none",
                          {"mode": "configured", "command": "target"},
                          config=zai,
                          call=lambda: {"harness": (configured_dispatch_harness(load_settings()) or (None,))[0],
                                        "note": (configured_dispatch_harness(load_settings()) or (None, None))[1],
                                        "route": configured_dispatch_route(load_settings())}))
    rows.append(answer_of("configured disagreement names the loser",
                          {"mode": "configured", "command": "target"},
                          config=shadow_cfg,
                          call=lambda: {"harness": (configured_dispatch_harness(load_settings()) or (None,))[0],
                                        "note": (configured_dispatch_harness(load_settings()) or (None, None))[1],
                                        "route": configured_dispatch_route(load_settings())}))
    return rows


SCRUBS: list[tuple[str, str]] = []


def scrub(case: dict) -> dict:
    """Replace this machine's tmp roots with a stable placeholder, so the
    golden byte-compares on any machine (the init line prints the target
    path; the tmp dir name changes every run)."""
    for root, placeholder in SCRUBS:
        case["out"] = case["out"].replace(root, placeholder)
        case["err"] = case["err"].replace(root, placeholder)
    return case


def main() -> int:
    commands = command_cases()
    settings_row = settings_ls_case()
    commands.append(settings_row)
    dispatch = dispatch_cases()
    commands = [scrub(c) for c in commands]
    dispatch = [scrub(c) for c in dispatch]

    GOLDEN_DIR.joinpath("config_route").mkdir(parents=True, exist_ok=True)
    GOLDEN_DIR.joinpath("dispatch_harness").mkdir(parents=True, exist_ok=True)
    commands_path = GOLDEN_DIR / "config_route" / "cases.json"
    commands_path.write_text(
        json.dumps({"subject": "config route", "cases": commands}, indent=2) + "\n",
        encoding="utf-8",
    )
    dispatch_path = GOLDEN_DIR / "dispatch_harness" / "cases.json"
    dispatch_path.write_text(
        json.dumps({"subject": "dispatch harness", "cases": dispatch}, indent=2) + "\n",
        encoding="utf-8",
    )
    print(f"captured {len(commands)} command cases -> {commands_path}")
    print(f"captured {len(dispatch)} dispatch cases -> {dispatch_path}")
    failed = [c["label"] for c in commands + dispatch if c["exit"] not in (0, 1, 2)]
    if failed:
        print(f"unexpected exit codes: {failed}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
