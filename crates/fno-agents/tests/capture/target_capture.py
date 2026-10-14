#!/usr/bin/env python3
"""Golden capture driver for the `fno do target init` / `start` contract.

Plays the scenario tables against the live Python leg and freezes
`tests/golden/target_init/` and `tests/golden/target_start/` in the
`assert_golden` on-disk format: `<key>.exit` ("<code>\\n"), `<key>.out`,
`<key>.err`, plus `<key>.fx` for the side-effect listings (manifest fields,
claim state, worktree presence) that streams cannot carry.

The leg under capture is reached through a PATH shim that emulates the front
door with the worktree's own source: `do target`/`agents`/`worktree` dispatch
to `<venv python> -m fno.cli` with the worktree's `cli/src` on PYTHONPATH, and
`backlog` dispatches to the native `fno-agents` binary (the backlog namespace
is native-only). Every case runs in a temp git repo with a temp HOME, because
the helper acquires claims with `FNO_CLAIMS_ROOT="$HOME"`.

Values a second machine cannot reproduce are normalized to named tokens:
the case root to `<TMP>`, a minted node id to `<NODE>`, UUID session ids to
`<SESSION>`, pids to `<PID>`, and date/timestamp stamps to `<DATE>`/`<TS>`.

The case tables below are the contract the `*_parity.rs` files mirror; keep
them in lockstep when a case is added (the same rule review_level_parity
records in its header).

Usage:
    python3 crates/fno-agents/tests/capture/target_capture.py init
    python3 crates/fno-agents/tests/capture/target_capture.py start
"""
from __future__ import annotations

import json
import os
import re
import shutil
import socket
import subprocess
import sys
import tempfile
import uuid
from pathlib import Path

REPO = Path(__file__).resolve().parents[4]
CLI_SRC = REPO / "cli" / "src"
GOLDEN_INIT = REPO / "crates" / "fno-agents" / "tests" / "golden" / "target_init"
GOLDEN_START = REPO / "crates" / "fno-agents" / "tests" / "golden" / "target_start"

SCRUB_PREFIXES = (
    "FNO_",
    "CLAUDE_",
    "CODEX_",
    "ANTHROPIC_",
    "OPENAI_",
    "ZAI_",
    "GLM_",
    "TARGET_",
)
# A pid that no machine allocates: far above every platform's pid_max, so the
# claim liveness read always answers dead and the orienter always spells the
# same state.
DEAD_PID = "2147483647"
# The claim-owner override every captured run carries: the helper anchors the
# node claim to this session id, so holder attribution is deterministic.
PINNED_SESSION_ID = "00000000-0000-0000-0000-000000000000"


def venv_python() -> str:
    venv = REPO / "cli" / ".venv" / "bin" / "python"
    if venv.is_file():
        return str(venv)
    # A worktree has no venv of its own; the canonical checkout's does.
    common = subprocess.run(
        ["git", "rev-parse", "--path-format=absolute", "--git-common-dir"],
        cwd=REPO,
        capture_output=True,
        text=True,
    )
    if common.returncode == 0:
        canonical = Path(common.stdout.strip()).parent / "cli" / ".venv" / "bin" / "python"
        if canonical.is_file():
            return str(canonical)
    return sys.executable


def native_agents_bin() -> str:
    found = shutil.which("fno-agents")
    if found:
        return found
    raise SystemExit("capture needs an fno-agents binary on PATH (the deployed one)")


def make_shim(bindir: Path) -> None:
    """The front-door shim: worktree Python for the CLI trees, native binary
    for the backlog namespace."""
    vpy = venv_python()
    agents = native_agents_bin()
    for name in ("fno", "fno-py"):
        body = (
            "#!/bin/bash\n"
            "if [ \"$1\" = \"backlog\" ]; then\n"
            f"  exec '{agents}' \"$@\"\n"
            "fi\n"
            f"exec env PYTHONPATH='{CLI_SRC}' '{vpy}' -m fno.cli \"$@\"\n"
        )
        path = bindir / name
        path.write_text(body)
        path.chmod(0o755)


def base_env(root: Path, bindir: Path, session_pid: str = DEAD_PID) -> dict:
    env = {}
    for key, value in os.environ.items():
        if any(key.startswith(p) for p in SCRUB_PREFIXES):
            continue
        env[key] = value
    env["HOME"] = str(root / "home")
    (root / "home").mkdir(parents=True, exist_ok=True)
    env["PATH"] = f"{bindir}:{env.get('PATH', '')}"
    env["GIT_CONFIG_GLOBAL"] = "/dev/null"
    env["GIT_CONFIG_SYSTEM"] = "/dev/null"
    env["FNO_SESSION_HARNESS"] = "claude"
    env["FNO_SESSION_PID"] = session_pid
    env["TERM"] = "dumb"
    return env


def make_repo(root: Path) -> Path:
    repo = root / "repo"
    repo.mkdir(parents=True)
    def git(*args: str) -> None:
        subprocess.run(
            ["git", "-c", "commit.gpgsign=false", *args],
            cwd=repo,
            env={**os.environ, "GIT_CONFIG_GLOBAL": "/dev/null", "GIT_CONFIG_SYSTEM": "/dev/null"},
            check=True,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
    git("init", "-q", "-b", "main", ".")
    git("config", "user.email", "t@example.test")
    git("config", "user.name", "t")
    (repo / "base.txt").write_text("seed\n")
    git("add", "-A")
    git("commit", "-q", "-m", "base")
    git("branch", "origin/main")
    git("checkout", "-q", "-b", "feature/work")
    return repo


def make_stub_plugin(root: Path) -> Path:
    """A stub plugin whose helper prints the TARGET_*/FNO_TARGET_* env and
    exits 0, so a golden holds the exact environment init hands the helper."""
    stub = root / "stub-plugin"
    helper = stub / "hooks" / "helpers"
    helper.mkdir(parents=True)
    (helper / "init-target-state.sh").write_text(
        "#!/bin/bash\n"
        "env | grep -E '^(TARGET_|FNO_TARGET_)' | sort\n"
        "exit 0\n"
    )
    (helper / "init-target-state.sh").chmod(0o755)
    return stub


def seed_node(env: dict, cwd: Path, title: str) -> str:
    out = subprocess.run(
        ["fno", "backlog", "idea", title, "--difficulty", "medium"],
        cwd=cwd,
        env=env,
        check=True,
        capture_output=True,
        text=True,
    )
    return json.loads(out.stdout)["id"]


def prewarm(env: dict, cwd: Path) -> None:
    """Absorb the one-time path-migration banner a fresh HOME's first Python
    CLI dispatch prints, so the captured streams hold only scenario output."""
    subprocess.run(
        ["fno", "do", "target", "init"],
        cwd=cwd,
        env=env,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        timeout=120,
    )


UUID_RE = re.compile(r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}")
ISO_TS_RE = re.compile(r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(\.\d+)?(Z|[+-]\d{2}:?\d{2})?")
TS_RE = re.compile(r"\d{8}T\d{10,}")
DATE_RE = re.compile(r"\d{4}-\d{2}-\d{2}")
SHA_RE = re.compile(r"\b[0-9a-f]{40}\b")


def path_tokens(root: Path) -> list:
    """Every spelling the case root (and its dashed slug) can appear as.

    macOS answers /var/... and /private/var/... for the same directory, and
    the space-slice manifest path embeds the repo path with slashes replaced
    by dashes, so the token set carries both realpaths and both dashed forms.
    """
    real = Path(os.path.realpath(root))
    repo = root / "repo"
    real_repo = real / "repo"
    return [
        (str(real_repo), "<TMP_REPO>"),
        (str(root / "repo"), "<TMP_REPO>"),
        (str(real), "<TMP>"),
        (str(root), "<TMP>"),
        (str(real_repo).replace("/", "-"), "<TMPD_REPO>"),
        (str(root / "repo").replace("/", "-"), "<TMPD_REPO>"),
        (str(real).replace("/", "-"), "<TMPD>"),
        (str(root).replace("/", "-"), "<TMPD>"),
    ]


def normalize(text: str, root: Path, node: str) -> str:
    for spelling, token in path_tokens(root):
        text = text.replace(spelling, token)
    if node:
        parts = node.split("+")
        # A two-node case keeps the ids distinguishable: the first seeded node
        # is <NODE>, the second <NODE_OTHER>, so a leg that swaps them in its
        # output cannot hide behind one shared token.
        for i, n in enumerate(parts):
            token = "<NODE>" if i == 0 else "<NODE_OTHER>"
            text = text.replace(n, token)
    text = ISO_TS_RE.sub("<TS>", text)
    text = TS_RE.sub("<TS>", text)
    text = DATE_RE.sub("<DATE>", text)
    text = SHA_RE.sub("<SHA>", text)
    text = re.sub(r"owner_pid: \d+", "owner_pid: <PID>", text)
    text = re.sub(r"FNO_SESSION_PID=\d+", "FNO_SESSION_PID=<PID>", text)
    text = re.sub(r'"pid": \d+', '"pid": <PID>', text)
    text = re.sub(r'"pid":\d+', '"pid":<PID>', text)
    text = re.sub(r"pid=\d+", "pid=<PID>", text)
    host = socket.gethostname()
    text = text.replace(host, "<HOST>")
    if "." not in host:
        text = text.replace(f"{host}.local", "<HOST>")
    text = re.sub(
        r'"(acquired_at|expires_at|renewed_at|created_at|updated_at)": \d+',
        r'"\1": <TS_MS>',
        text,
    )
    return text


def manifest_paths(root: Path) -> list:
    home = root / "home"
    found = []
    for base in (home / ".fno" / "spaces", home / ".fno"):
        if not base.is_dir():
            continue
        found.extend(base.rglob("target-state.md"))
    for base in ((root / "repo").glob(".claude/worktrees/*"), (home / ".fno" / "worktrees").glob("*/*")):
        for wt in base:
            candidate = wt / ".fno" / "target-state.md"
            if candidate.is_file():
                found.append(candidate)
    return sorted(set(found))


def side_effects(root: Path, env: dict, cwd: Path, node: str) -> str:
    """The side-effect listing: every manifest's normalized fields plus the
    live claim state for the node (the helper's own status read)."""
    lines = ["manifests:"]
    for path in manifest_paths(root):
        rel = str(path).replace(str(root), "<TMP>")
        lines.append(f"- {rel}")
        text = path.read_text(encoding="utf-8", errors="replace")
        for line in text.splitlines():
            stripped = line.strip()
            if stripped and not stripped.startswith("#"):
                lines.append(f"  {normalize(stripped, root, node)}")
    lines.append("claims:")
    status = subprocess.run(
        ["fno", "agents", "claim", "status", f"node:{node or 'none'}", "--no-roster"],
        cwd=cwd,
        env=env,
        capture_output=True,
        text=True,
        timeout=60,
    )
    body = (status.stdout + status.stderr).strip()
    if body:
        lines.append(f"  exit={status.returncode}")
        for line in body.splitlines():
            lines.append(f"  {normalize(line.strip(), root, node)}")
    else:
        lines.append(f"  exit={status.returncode} (no output)")
    worktrees = set()
    for pattern in (
        (root / "repo").glob(".claude/worktrees/*"),
        (root / "home" / ".fno" / "worktrees").glob("*/*"),
        (root).glob("linked"),
        (root).glob("ghost-wt"),
    ):
        worktrees.update(str(p) for p in pattern)
    lines.append("worktrees:")
    for wt in sorted(worktrees):
        lines.append(f"- {normalize(wt, root, node)}")
    return "\n".join(lines) + "\n"


def write_golden(dir_: Path, label: str, exit_code: int, out: str, err: str, fx: str | None) -> None:
    dir_.mkdir(parents=True, exist_ok=True)
    key = slug(label)
    (dir_ / f"{key}.exit").write_text(f"{exit_code}\n")
    (dir_ / f"{key}.out").write_text(out)
    (dir_ / f"{key}.err").write_text(err)
    if fx is not None:
        (dir_ / f"{key}.fx").write_text(fx)


def slug(label: str) -> str:
    out = []
    prev_us = False
    for ch in label:
        if ch.isascii() and ch.isalnum():
            out.append(ch.lower())
            prev_us = False
        elif not prev_us:
            out.append("_")
            prev_us = True
    return "".join(out).strip("_")


def run_fno(root: Path, argv: list, cwd: Path, env: dict) -> subprocess.CompletedProcess:
    return subprocess.run(
        ["fno", *argv],
        cwd=cwd,
        env=env,
        capture_output=True,
        text=True,
        timeout=600,
    )


def run_case(root: Path, argv: list, cwd: Path, env: dict, node: str, with_fx: bool) -> tuple:
    proc = run_fno(root, argv, cwd, env)
    out = normalize(proc.stdout, root, node)
    err = normalize(proc.stderr, root, node)
    fx = normalize(side_effects(root, env, cwd, node), root, node) if with_fx else None
    return proc.returncode, out, err, fx


class Case:
    def __init__(self, label, argv=None, fixture="repo", plugin="stub", seed=None,
                 env_extra=None, cwd_sub=None, with_fx=False, pre=True):
        self.label = label
        self.argv = argv or []
        self.fixture = fixture          # repo | plain | wt_of_repo
        self.plugin = plugin            # stub | real | none
        self.seed = seed                # optional second node title
        self.env_extra = env_extra or {}
        self.cwd_sub = cwd_sub
        self.with_fx = with_fx
        self.pre = pre


INIT_CASES = [
    Case("no-input-or-plan", argv=["do", "target", "init"]),
    Case(
        "deliverables-zero",
        argv=["do", "target", "init", "--input", "fix the login redirect bug", "--deliverables", "0"],
    ),
    Case(
        "size-bogus",
        argv=["do", "target", "init", "--input", "fix the login redirect bug", "--size", "Q"],
    ),
    Case(
        "helper-missing",
        argv=["do", "target", "init", "--input", "fix the login redirect bug"],
        plugin="none",
    ),
    Case(
        "plan-path-unreadable",
        argv=["do", "target", "init", "--plan-path", "<ROOT>/missing/plan.md"],
    ),
    Case(
        "beastmode-inherited-scrubbed",
        argv=["do", "target", "init", "--input", "fix the login redirect bug"],
        env_extra={"TARGET_BEASTMODE": "1"},
    ),
    Case(
        "no-merge-flag",
        argv=["do", "target", "init", "--input", "fix the login redirect bug", "--no-merge"],
    ),
    Case(
        "node-input-claims-once",
        argv=["do", "target", "init", "--input", "<NODE>"],
        seed="node input claims the node once",
        plugin="real",
        with_fx=True,
    ),
    Case(
        "free-text-input-no-claim",
        argv=["do", "target", "init", "--input", "fix the login redirect bug"],
        plugin="real",
        with_fx=True,
    ),
    Case(
        "plan-less-code-node-derives-deliverables",
        argv=["do", "target", "init", "--input", "<NODE>"],
        seed="plan less code node derives deliverables",
        plugin="real",
        with_fx=True,
    ),
]

START_CASES = [
    Case(
        "outside-git",
        argv=["do", "target", "start", "fix the login redirect bug"],
        fixture="plain",
    ),
    Case(
        "cold-start-claims-and-prints-receipt",
        argv=["do", "target", "start", "<NODE>"],
        seed="cold start claims and prints receipt",
        plugin="real",
        with_fx=True,
    ),
    Case(
        "second-run-already-claimed",
        argv=["do", "target", "start", "<NODE>"],
        seed="second run already claimed",
        plugin="real",
        with_fx=True,
        pre=False,
    ),
    Case(
        "foreign-live-holder-parks",
        argv=["do", "target", "start", "<NODE>"],
        seed="foreign live holder parks",
        plugin="real",
        with_fx=True,
        pre=False,
    ),
    Case(
        "inside-linked-worktree-isolated",
        argv=["do", "target", "start", "<NODE>"],
        seed="inside linked worktree isolated",
        plugin="real",
        cwd_sub="linked",
    ),
    Case(
        "init-failure-tree-unclaimed",
        argv=["do", "target", "start", "<NODE>"],
        seed="init failure tree unclaimed",
        plugin="none",
        with_fx=True,
    ),
    Case(
        "manifest-names-another-node",
        argv=["do", "target", "start", "<NODE>"],
        seed="manifest names another node",
        plugin="real",
        with_fx=True,
        pre=False,
    ),
    Case(
        "ghost-node-refuses",
        argv=["do", "target", "start", "ab-ghost000"],
        plugin="real",
        with_fx=True,
    ),
]


def play(case: Case, root: Path) -> None:
    """Build the case fixture, run the scenario, freeze the golden."""
    bindir = root / "bin"
    bindir.mkdir(parents=True, exist_ok=True)
    make_shim(bindir)

    if case.fixture == "repo":
        cwd = make_repo(root)
    elif case.fixture == "plain":
        cwd = root / "plain"
        cwd.mkdir(parents=True)
    else:
        raise ValueError(case.fixture)

    node = ""
    other = ""
    if case.seed:
        node = seed_node(base_env(root, bindir), cwd, case.seed)
        # Disjoint words: a shared term flips idea into its dedup fold-offer,
        # which prints a notice instead of the JSON receipt.
        other = seed_node(base_env(root, bindir), cwd, "zulu quebec foxtrot secondary filler")

    plugin_root = ""
    if case.plugin == "stub":
        plugin_root = str(make_stub_plugin(root))
    elif case.plugin == "real":
        plugin_root = str(REPO)
    elif case.plugin == "none":
        # CLAUDE_PLUGIN_ROOT is authoritative when set (no fallthrough), so a
        # root without the helper is what surfaces the missing-plugin refusal.
        empty = root / "empty-plugin"
        empty.mkdir(parents=True, exist_ok=True)
        plugin_root = str(empty)

    env_extra = dict(case.env_extra)
    if plugin_root:
        env_extra["CLAUDE_PLUGIN_ROOT"] = plugin_root
    if case.pre:
        prewarm(base_env(root, bindir), cwd)

    # The run's session pin needs a LIVE pid (a dead pair fails harness
    # provenance closed and refuses the claim), so the case holds a sleeper
    # process alive through every captured subprocess. TARGET_SESSION_ID is
    # the helper's documented claim-owner override (megawalk walkers pre-
    # assign it); pinned, the holder attribution is deterministic.
    sleepers = [subprocess.Popen(["sleep", "180"])]
    session_pid = str(sleepers[0].pid)
    env_extra.setdefault("TARGET_SESSION_ID", PINNED_SESSION_ID)

    try:
        if case.label == "second-run-already-claimed":
            first_env = base_env(root, bindir, session_pid)
            first_env.update(env_extra)
            run_fno(root, ["do", "target", "start", node], cwd, first_env)
        elif case.label == "foreign-live-holder-parks":
            # A live foreign holder, the real shape: session A cold-starts the
            # node (worktree, manifest, live claim), then session B starts the
            # same node - the bind path parks on the foreign live claim.
            first_env = base_env(root, bindir, session_pid)
            first_env.update(env_extra)
            run_fno(root, ["do", "target", "start", node], cwd, first_env)
            sleepers.append(subprocess.Popen(["sleep", "180"]))
            session_pid = str(sleepers[1].pid)
            env_extra["TARGET_SESSION_ID"] = "11111111-1111-1111-1111-111111111111"
        elif case.label == "inside-linked-worktree-isolated":
            subprocess.run(
                ["git", "worktree", "add", "-q", str(root / "linked"), "-b", "linked"],
                cwd=cwd, check=True, capture_output=True, env=base_env(root, bindir),
            )
            cwd = root / "linked"
        elif case.label == "ghost-node-refuses":
            # A linked worktree holding a hand-written manifest whose node is
            # not in the graph: the bind path's ghost refusal.
            wt = root / "ghost-wt"
            subprocess.run(
                ["git", "worktree", "add", "-q", str(wt), "-b", "ghost"],
                cwd=cwd, check=True, capture_output=True, env=base_env(root, bindir),
            )
            manifest_dir = wt / ".fno"
            manifest_dir.mkdir(parents=True)
            (manifest_dir / "target-state.md").write_text(
                "---\n"
                "session_id: 00000000-0000-0000-0000-000000000000\n"
                "graph_node_id: ab-ghost000\n"
                "---\n"
            )
            cwd = wt
        elif case.label == "manifest-names-another-node":
            # Cold-start node A, then start node B from inside A's worktree:
            # the bind path refuses a manifest naming another node.
            first_env = base_env(root, bindir, session_pid)
            first_env.update(env_extra)
            run_fno(root, ["do", "target", "start", node], cwd, first_env)
            trees = sorted((root / "repo").glob(".claude/worktrees/*"))
            if not trees:
                trees = sorted((root / "home" / ".fno" / "worktrees").glob("*/*"))
            cwd = trees[0].resolve()

        argv = [a.replace("<ROOT>", str(root)).replace("<NODE>", node or "ab-00000000") for a in case.argv]
        if case.label == "manifest-names-another-node":
            # Start the OTHER node from inside node A's worktree: the bind
            # path refuses a manifest naming another node.
            argv = [a.replace(node, other) for a in argv]
            node_key = node + "+" + other
        else:
            node_key = node

        env = base_env(root, bindir, session_pid)
        env.update(env_extra)
        code, out, err, fx = run_case(root, argv, cwd, env, node_key, case.with_fx)
    finally:
        for s in sleepers:
            s.kill()
            s.wait()

    subject_dir = GOLDEN_INIT if INIT_SUBJECT else GOLDEN_START
    write_golden(subject_dir, case.label, code, out, err, fx)
INIT_SUBJECT = False


def setup_of(case):
    """The parity-side setup semantics keyed by label; the parity harness's
    row builder mirrors this mapping."""
    mapping = {
        "second-run-already-claimed": "second-run",
        "foreign-live-holder-parks": "foreign-holder",
        "inside-linked-worktree-isolated": "linked-worktree",
        "ghost-node-refuses": "ghost-worktree",
        "manifest-names-another-node": "manifest-another",
    }
    return mapping.get(case.label, "normal")


def dump_tables():
    """The case tables as JSON: the generated copy the parity harness reads,
    so the scenario tables live in exactly one file (this driver). Regenerate
    with:

        python3 crates/fno-agents/tests/capture/target_capture.py --dump-tables \
          > crates/fno-agents/tests/capture/tables.json
    """
    def as_row(c):
        return {
            "label": c.label,
            "argv": list(c.argv),
            "fixture": c.fixture,
            "plugin": c.plugin,
            "seed": c.seed,
            "env_extra": sorted(c.env_extra.items()),
            "pre": c.pre,
            "with_fx": c.with_fx,
            "setup": setup_of(c),
        }

    return json.dumps(
        {
            "init": [as_row(c) for c in INIT_CASES],
            "start": [as_row(c) for c in START_CASES],
        },
        indent=2,
    )


def main():
    global INIT_SUBJECT
    if len(sys.argv) > 1 and sys.argv[1] == "--dump-tables":
        print(dump_tables())
        return
    subject = sys.argv[1] if len(sys.argv) > 1 else "init"
    INIT_SUBJECT = subject == "init"
    cases = INIT_CASES if INIT_SUBJECT else START_CASES
    golden_dir = GOLDEN_INIT if INIT_SUBJECT else GOLDEN_START
    golden_dir.mkdir(parents=True, exist_ok=True)
    top = Path(tempfile.mkdtemp(prefix="target-capture-"))
    print(f"capture root: {top}")
    for case in cases:
        case_root = top / slug(case.label)
        case_root.mkdir(parents=True)
        try:
            play(case, case_root)
            print(f"[ok] {case.label}")
        except Exception as exc:  # noqa: BLE001 - one bad case must not stop the sweep
            print(f"[FAIL] {case.label}: {exc}")
    print("done")


if __name__ == "__main__":
    main()
