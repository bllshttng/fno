#!/usr/bin/env python3
"""Run 0 footnote agent: a Harbor adapter that drives the `footnote` binary.

The binary is the linux/amd64 musl build `setup-imac.sh` drops at
$WS/bin/linux-amd64/footnote, from the fnh sha pinned in manifest.json
(amendment_14). `setup-imac.sh` copies this file into the run workspace,
where `launch.py` imports it through PYTHONPATH.

One trial: write a v2 launch spec (LAUNCH_SPEC_V in
crates/fno-agents/src/footnote_transcript.rs; the binary refuses any other
version), upload it into the container, pipe it to `footnote` on stdin, then
fold the transcript's `usage` records into Harbor's token totals. Zero usage
with a model call stays unmeasured, never zero (Amendment 2). The agent log
for the 1302 rule is /logs/agent/footnote.txt; a transcript copy lands
beside it.
"""
import json
import os
import shlex
import uuid
from pathlib import Path

from harbor.agents.installed.base import BaseInstalledAgent
from harbor.environments.base import BaseEnvironment
from harbor.models.agent.context import AgentContext

SPEC_V = 2
MODEL = "glm-5.3-flash"
CONTEXT_WINDOW = 200_000
ZAI_ANTHROPIC = "https://api.z.ai/api/anthropic"
PERMISSION_MODE = "bypassPermissions"
SESSIONS_DIR = "/logs/agent/footnote/sessions"
LOG_FILE = "/logs/agent/footnote.txt"
TRANSCRIPT_COPY = "/logs/agent/footnote-transcript.jsonl"


def default_linux_bin() -> Path:
    ws = Path(os.environ.get("HARNESS_FIT_WS", Path.home() / "evals-workspace" / "harness-fit")).expanduser()
    return ws / "bin" / "linux-amd64" / "footnote"


class Footnote(BaseInstalledAgent):
    @staticmethod
    def name() -> str:
        return "footnote"

    def version(self) -> str | None:
        return os.environ.get("FOOTNOTE_FNH_SHA") or "unknown"

    async def install(self, environment: BaseEnvironment) -> None:
        src = Path(os.environ.get("FOOTNOTE_LINUX_BIN") or default_linux_bin())
        if not src.is_file():
            raise FileNotFoundError(f"no linux/amd64 footnote at {src}; run setup-imac.sh")
        await environment.upload_file(src, "/usr/local/bin/footnote")
        await self.exec_as_root(environment, command="chmod 755 /usr/local/bin/footnote")

    async def run(self, instruction: str, environment: BaseEnvironment, context: AgentContext) -> None:
        key = self._get_env("ZAI_API_KEY")
        if not key:
            raise ValueError("ZAI_API_KEY is not set; launch.py puts it in the harbor env")
        fno_id = f"tb2-{uuid.uuid4().hex[:20]}"
        # /tmp, not /logs/agent: the spec carries the API key, and Harbor syncs
        # /logs/agent to the host trial dir. The transcript copy stays behind.
        spec_path = f"/tmp/footnote-launch-spec-{fno_id}.json"
        pwd = await self.exec_as_agent(environment, command="pwd")
        spec = {
            "v": SPEC_V,
            "mode": "create",
            "fno_id": fno_id,
            "session_dir": SESSIONS_DIR,
            "cwd": (pwd.stdout or "").strip(),
            "model": self.model_name or MODEL,
            "message": instruction,
            "timeout_secs": None,
            "node": None,
            "plan_path": None,
            "cost_cap_usd": None,
            "wall_cap_minutes": None,
            "plugin_root": None,
            "parent_session_id": None,
            "permission_mode": PERMISSION_MODE,
            # The price table only backs a dollar cap; this run sets none.
            "price_cache": f"{SESSIONS_DIR}/price-cache.json",
            "context_window": CONTEXT_WINDOW,
            "endpoint": {
                "base_url": ZAI_ANTHROPIC,
                "key": key,
                "bearer": True,
                "wire": "anthropic",
                "provider_id": "zai",
                "route": "env",
            },
        }
        await self.exec_as_agent(environment, command=f"mkdir -p {shlex.quote(SESSIONS_DIR)}")
        await self._upload_config_text(
            environment, content=json.dumps(spec), remote_path=spec_path, filename="launch-spec.json",
        )
        await self.exec_as_agent(
            environment,
            command=f"footnote < {shlex.quote(spec_path)} > {shlex.quote(LOG_FILE)} 2>&1",
        )
        transcript = f"{SESSIONS_DIR}/{fno_id}.jsonl"
        out = await self.exec_as_agent(environment, command=f"cat {shlex.quote(transcript)}")
        await self.exec_as_agent(
            environment,
            command=f"cat {shlex.quote(transcript)} > {shlex.quote(TRANSCRIPT_COPY)}",
        )
        self._fold_usage(out.stdout or "", context)

    def _fold_usage(self, text: str, context: AgentContext) -> None:
        n_in = n_cache = n_write = n_out = 0
        cost = 0.0
        for line in text.splitlines():
            line = line.strip()
            if not line:
                continue
            try:
                row = json.loads(line)
            except ValueError:
                continue
            if row.get("type") != "usage":
                continue
            d = row.get("data") or {}
            n_in += d.get("input_tokens") or 0
            n_cache += d.get("cache_read_tokens") or 0
            n_write += d.get("cache_write_tokens") or 0
            n_out += d.get("output_tokens") or 0
            cost += d.get("cost_usd") or 0.0
        if n_in == 0 and n_out == 0:
            return
        # Harbor counts cache inside input; fnh prices cache writes at input,
        # so only the reads ride the cache total.
        context.n_input_tokens = n_in + n_cache + n_write
        context.n_cache_tokens = n_cache
        context.n_output_tokens = n_out
        context.cost_usd = cost if cost > 0 else None

    def populate_context_post_run(self, context: AgentContext) -> None:
        if context.n_input_tokens is not None or context.n_output_tokens is not None:
            return
        copy = self.logs_dir / "footnote-transcript.jsonl"
        if copy.exists():
            self._fold_usage(copy.read_text(errors="ignore"), context)
