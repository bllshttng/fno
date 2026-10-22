#!/usr/bin/env python3
"""Golden capture driver for the `mail reply` wrapped-command port.

Plays the case tables against the live Python leg (`fno agents mail reply`,
the typer command) and freezes `tests/golden/mail_reply/` in the `assert_golden`
on-disk format: `<key>.exit` ("<code>\\n"), `<key>.out`, `<key>.err`. The
`mail_reply_parity.rs` tables mirror these cases; keep them in lockstep when a
case is added (the same rule mail_send_capture.py records in its header).

Captured before the Python body deletes, per step 2 of the port protocol
(docs/architecture/dual-implementation-inventory.md). The hermetic temp root
cannot offer a live recipient, so the deep live lanes surface here as their
designed misses: the inject probe misses, the durable floor writes, the
landing report fails, and the receipts name why. That IS the contract the
characterization freezes.

Every case runs the Python front under the same env block the parity test
applies: temp HOME/state/bus/events roots, the claude session pins, the other
harness markers blanked, git config pinned to /dev/null. CLAUDE_CONFIG_DIR is
removed (the Rust leg reads `<CLAUDE_CONFIG_DIR>/projects` ahead of HOME, the
Python leg does not; the fixture needs both legs on the same store). Children
(the mail-receipt door, the transcript resolver) resolve `fno-agents` through
FNO_AGENTS_BIN, pinned to the installed binary at capture time.

Usage:
    python3 crates/fno-agents/tests/capture/mail_reply_capture.py
"""
from __future__ import annotations

import json
import os
import re
import shutil
import stat
import subprocess
import sys
import tempfile
from pathlib import Path

REPO = Path(__file__).resolve().parents[4]
CLI_SRC = REPO / "cli" / "src"
GOLDEN_DIR = REPO / "crates" / "fno-agents" / "tests" / "golden" / "mail_reply"

SID = "a1b2c3d4-1111-2222-3333-444455556666"
US3_SID = "11111111-2222-3333-4444-555566667777"
SENDER = "9a063cd3"
CREATED = "2026-10-12T09:00:00Z"

DROP_PREFIXES = (
    "[setup] path migration complete",
    "  detected install:",
    "  settings written to:",
    "  obsidian enabled:",
    "  sentinel:",
    # The escalation stderr advisory names whether the OS notifier displayed,
    # which is a property of the host, not of the command: a mac capture has a
    # channel, a headless CI runner does not. The receipt is the contract.
    "escalated to human (",
)


def venv_python() -> str:
    venv = REPO / "cli" / ".venv" / "bin" / "python"
    if venv.is_file():
        return str(venv)
    common = subprocess.run(
        ["git", "-C", str(REPO), "rev-parse", "--path-format=absolute", "--git-common-dir"],
        capture_output=True,
        text=True,
    )
    if common.returncode == 0:
        canonical = Path(common.stdout.strip()).parent / "cli" / ".venv" / "bin" / "python"
        if canonical.is_file():
            return str(canonical)
    return sys.executable


def installed_agents_bin() -> str:
    """The installed fno-agents the capture's children resolve through."""
    found = shutil.which("fno-agents")
    if found is None:
        raise SystemExit("fno-agents not on PATH; the capture pins children to it")
    return found


def installed_front() -> str:
    """The installed `fno` front. The shim routes every non-reply command here, so
    the Python leg's internal doors behave as production."""
    found = shutil.which("fno")
    if found is None:
        raise SystemExit("fno not on PATH; the capture routes non-reply commands to it")
    return found


def make_shim(bindir: Path) -> None:
    """A PATH shim named `fno`: `mail reply` reaches the worktree's Python app
    (the leg under capture) with argv[0] naming the production prog; every
    other spelling execs the installed front."""
    bindir.mkdir(parents=True, exist_ok=True)
    shim = bindir / "fno"
    shim.write_text(
        f"#!{venv_python()}\n"
        "import os, sys\n"
        f"sys.path.insert(0, {str(CLI_SRC)!r})\n"
        "args = sys.argv[1:]\n"
        "is_reply = args[:3] == ['agents', 'mail', 'reply'] or args[:2] == ['mail', 'reply']\n"
        "if not is_reply:\n"
        f"    os.execv({installed_front()!r}, ['fno', *args])\n"
        "from fno.cli import app\n"
        "app()\n"
    )
    shim.chmod(shim.stat().st_mode | stat.S_IEXEC)


def base_env(root: Path, bindir: Path) -> dict:
    env = dict(os.environ)
    env.update(
        HOME=str(root),
        FNO_STATE_DIR=str(root),
        FNO_HOME=str(root),
        FNO_AGENTS_HOME=str(root / "agents-home"),
        FNO_BUS_DIR=str(root / "bus"),
        FNO_EVENTS_PATH=str(root / "events.jsonl"),
        FNO_CLAIMS_ROOT=str(root),
        FNO_HARNESS_NAME="claude",
        FNO_HARNESS_SESSION_ID=SID,
        CLAUDE_CODE_SESSION_ID=SID,
        CODEX_THREAD_ID="",
        CODEX_SESSION_ID="",
        GEMINI_SESSION_ID="",
        OPENCODE_SESSION_ID="",
        CLAUDE_SESSION_ID="",
        FNO_AGENTS_BIN=installed_agents_bin(),
        GIT_CONFIG_GLOBAL="/dev/null",
        GIT_CONFIG_SYSTEM="/dev/null",
        PATH=f"{bindir}{os.pathsep}{env.get('PATH', '')}",
    )
    for name in (
        "FNO_AGENT_SELF",
        "FNO_MAIL_SUBJECT",
        "FNO_MAIL_LOCAL_FORWARD",
        # Store-location overrides: an ambient value would point one or both
        # legs outside the fixture root.
        "CLAUDE_CONFIG_DIR",
        "FNO_CLAUDE_PROJECTS_DIR",
        "FNO_CLAUDE_SESSIONS_DIR",
        "FNO_CLAUDE_DAEMON_DIR",
        "FNO_INBOX_ROOT",
        "FNO_INBOX_KNOWN_PROJECTS",
        "FNO_MAIL_BODY_WARN",
        "FNO_MAIL_BODY_REFUSE",
        "FNO_MAIL_LANDED_SETTLE_S",
    ):
        env.pop(name, None)
    # The test-hermetic pin both legs share: it suppresses the OS notifier and
    # any registry spawn leg.
    env["FNO_TEST_HERMETIC"] = "1"
    # The style gate runs off in the default block; the style case re-enables it.
    env["FNO_STYLE_ENFORCE"] = "0"
    # The settle sleep changes latency and never output; pin it to 0.
    env["FNO_MAIL_LANDED_SETTLE_S"] = "0"
    return env


def seed_registry(root: Path, agents: list[dict]) -> None:
    sys.path.insert(0, str(CLI_SRC))
    from fno.agents.registry import SCHEMA_VERSION

    doc = {
        "schema_version": SCHEMA_VERSION,
        "min_writer_version": SCHEMA_VERSION,
        "agents": agents,
    }
    home = root / "agents-home"
    home.mkdir(parents=True, exist_ok=True)
    (home / "registry.json").write_text(json.dumps(doc))


def row(name: str, harness: str, sid: str | None) -> dict:
    return {
        "name": name,
        "short_id": "",
        "harness": harness,
        "harness_session_id": sid,
        "cwd": "/tmp",
        "log_path": "/tmp/nope.log",
        "created_at": CREATED,
        "status": "live",
    }


def seed_bus(root: Path, rows: list[dict]) -> None:
    bus = root / "bus"
    bus.mkdir(parents=True, exist_ok=True)
    (bus / "messages.jsonl").write_text(
        "".join(json.dumps(r, ensure_ascii=False, separators=(",", ":")) + "\n" for r in rows)
    )


def bus_row(
    msg_id: str,
    *,
    from_: str,
    to: str,
    kind: str = "send",
    to_kind: str | None = None,
    from_session: str | None = None,
    body: str = "ping",
) -> dict:
    r: dict = {
        "id": msg_id,
        "thread": msg_id,
        "from": from_,
        "to": to,
        "kind": kind,
        "body": body,
        "ts": "2026-10-12T09:00:00Z",
    }
    if to_kind is not None:
        r["to_kind"] = to_kind
    if from_session is not None:
        r["from_session"] = from_session
    return r


def seed_transcript_envelope(root: Path, session_id: str, from_: str, msg_id: str) -> None:
    """One claude transcript under the fixture HOME carrying the envelope.

    The file is keyed by the REPLYING session's own id: the transcript scan
    reads this session's own store, and an envelope under a stranger's file
    never resolves.
    """
    enc = root / ".claude" / "projects" / "-users-x-proj"
    enc.mkdir(parents=True, exist_ok=True)
    envelope = (
        f'<fno_mail from="{from_}" harness="claude-code" model="opus" '
        f'id="{msg_id}">\nping\n</fno_mail>'
    )
    line = json.dumps({"type": "user", "message": {"role": "user", "content": envelope}})
    (enc / f"{session_id}.jsonl").write_text(line + "\n", encoding="utf-8")


def seed_torn_registry(root: Path) -> None:
    home = root / "agents-home"
    home.mkdir(parents=True, exist_ok=True)
    (home / "registry.json").write_text("{ not json", encoding="utf-8")


def seed_project_thread(root: Path, env: dict) -> str:
    """One existing thread under web's inbox; returns its msg id.

    Runs in a venv-python subprocess: the driver's bare interpreter lacks the
    venv's site-packages (yaml), and the store import needs it. The child env
    carries the fixture's FNO_INBOX_ROOT pin.
    """
    code = (
        f"import sys; sys.path.insert(0, {str(CLI_SRC)!r})\n"
        "from fno.inbox.store import write_new_thread\n"
        # Self-addressed: the thread's from names web, so the reply's recipient
        # (read off that frontmatter) re-finds the same file and appends.
        "h = write_new_thread(recipient='web', sender='web', kind='send', body='parent ping')\n"
        "print(h.thread_id)\n"
    )
    proc = subprocess.run(
        [venv_python(), "-c", code],
        env=env,
        capture_output=True,
        text=True,
        timeout=120,
    )
    if proc.returncode != 0:
        raise SystemExit(f"thread seed failed: {proc.stderr}")
    return proc.stdout.strip().splitlines()[-1]


def normalize(text: str, root: Path) -> str:
    kept = [line for line in text.split("\n") if not any(line.startswith(p) for p in DROP_PREFIXES)]
    text = "\n".join(kept)
    text = text.rstrip("\n")
    try:
        value = json.loads(text)
        if isinstance(value, dict):
            text = json.dumps(value, sort_keys=True, ensure_ascii=False, separators=(",", ":"))
    except Exception:
        pass
    text = re.sub(r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?Z?", "<TS>", text)
    text = re.sub(r"\d{4}-\d{2}-\d{2}", "<DATE>", text)
    text = re.sub(r"\d{2}:\d{2}:\d{2}", "<TIME>", text)
    text = re.sub(r"\bfmail-[0-9a-fA-F]{4,}", "fmail-HEX", text)
    text = re.sub(r"\bmsg-[0-9a-fA-F]{4,}", "msg-HEX", text)
    text = re.sub(r"\b[0-9a-fA-F]{8}\b", "HEX8", text)
    real = os.path.realpath(root)
    text = text.replace(real, "<ROOT>").replace(str(root), "<ROOT>")
    return text


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


def write_golden(label: str, exit_code: int, out: str, err: str) -> None:
    GOLDEN_DIR.mkdir(parents=True, exist_ok=True)
    key = slug(label)
    (GOLDEN_DIR / f"{key}.exit").write_text(f"{exit_code}\n")
    (GOLDEN_DIR / f"{key}.out").write_text(out)
    (GOLDEN_DIR / f"{key}.err").write_text(err)


# One sentence, well over the 25-word cap: the length rule's cleanest tripwire.
STYLE_BODY = (
    "This single sentence deliberately pads itself with one filler word after "
    "another until it blows straight past the twenty five word length cap that "
    "the mail style gate enforces on every authored relay body it checks."
)

CASES: list[dict] = [
    dict(label="usage no args", argv=[]),
    dict(label="missing to", argv=["hello there"]),
    dict(
        label="unknown msg id refused",
        argv=["--to", "msg-capnone", "ack"],
    ),
    dict(
        label="name lane durable floor",
        argv=["--to", "msg-capn1", "ack"],
        bus=[bus_row("msg-capn1", from_=SENDER, to="claude-capture1", to_kind="name")],
    ),
    dict(
        label="name lane registry floor",
        argv=["--to", "msg-capn2", "ack"],
        bus=[bus_row("msg-capn2", from_="worker-1", to="claude-capture1", to_kind="name")],
        rows=[row("worker-1", "claude", "b2c3d4e5-1111-2222-3333-444455556666")],
    ),
    dict(
        label="session lane full provenance floor",
        argv=["--to", "msg-caps1", "ack"],
        bus=[
            bus_row(
                "msg-caps1",
                from_=SENDER,
                to=SID,
                to_kind="session",
                from_session=US3_SID,
            )
        ],
    ),
    dict(
        label="session lane requires resolution refused",
        argv=["--to", "msg-caps2", "ack"],
        bus=[
            bus_row("msg-caps2", from_=SENDER, to=SID, to_kind="session"),
        ],
    ),
    dict(
        label="legacy sender migrated offline refused",
        argv=["--to", "msg-capl1", "ack"],
        bus=[bus_row("msg-capl1", from_="claude-deadbeef", to="claude-capture1", to_kind="name")],
    ),
    dict(
        label="us3 transcript recovery floors",
        argv=["--to", "msg-live1", "pong"],
        transcript=("[SESSION]", "msg-live1", SENDER),
    ),
    dict(
        label="torn registry fail open",
        argv=["--to", "msg-capf1", "ack"],
        bus=[bus_row("msg-capf1", from_=SENDER, to="claude-capture1", to_kind="name")],
        torn_registry=True,
    ),
    dict(
        label="two bodies refused",
        argv=["--to", "msg-capx1", "body-one", "--body", "body-two"],
    ),
    dict(
        label="kind unknown refused",
        argv=["--to", "msg-capx2", "--kind", "bogus", "ack"],
    ),
    dict(
        label="style refusal",
        argv=["--to", "msg-capx3", STYLE_BODY.strip()],
        style_on=True,
    ),
    dict(
        label="body cap refuse",
        argv=["--to", "msg-capx4", "x" * 5100],
    ),
    dict(
        label="orphan self note",
        argv=["--to", "msg-capp1", "self note"],
        bus=[bus_row("msg-capp1", from_="9a063cd3", to="web", kind="send", to_kind="project")],
        inbox_project="web",
    ),
    dict(
        # The parent row rides the bus (write_new_thread appended it); the id
        # is found and the thread-store append resurfaces the same file.
        label="appended reply",
        argv=["--to", "<PARENT_MSG>", "appended answer"],
        inbox_project="web",
        parent_thread=True,
    ),
    dict(
        label="appended reply json",
        argv=["--to", "<PARENT_MSG>", "appended answer", "--json"],
        inbox_project="web",
        parent_thread=True,
    ),
    dict(
        # The bus row is wiped after the seed, so only the chats index holds
        # the id -> from binding: the resolver's first leg.
        label="us3 chats index recovery floors",
        argv=["--to", "<PARENT_MSG>", "via chats"],
        inbox_project="web",
        parent_thread=True,
        wipe_bus=True,
    ),
]


def run_case(case: dict, bindir: Path) -> None:
    root = Path(tempfile.mkdtemp(prefix="fnoreplycap"))
    try:
        env = base_env(root, bindir)
        argv = list(case["argv"])
        if case.get("rows") is not None:
            seed_registry(root, case["rows"])
        if case.get("bus"):
            seed_bus(root, case["bus"])
        if case.get("torn_registry"):
            seed_torn_registry(root)
        if case.get("transcript"):
            session_key, msg_id, from_ = case["transcript"]
            seed_transcript_envelope(root, SID, from_, msg_id)
        if case.get("inbox_project"):
            project = case["inbox_project"]
            (root / ".fno").mkdir(parents=True, exist_ok=True)
            (root / ".fno" / "settings.yaml").write_text(f"project: {project}\n")
            (root / "inbox").mkdir(parents=True, exist_ok=True)
            env["FNO_INBOX_ROOT"] = str(root / "inbox")
            env["FNO_INBOX_KNOWN_PROJECTS"] = project
            if case.get("parent_thread"):
                # The seeding helper runs in THIS process, which resolves the
                # inbox root from os.environ, not from the child's env dict.
                saved = (os.environ.get("FNO_INBOX_ROOT"), os.environ.get("FNO_INBOX_KNOWN_PROJECTS"))
                os.environ["FNO_INBOX_ROOT"] = env["FNO_INBOX_ROOT"]
                os.environ["FNO_INBOX_KNOWN_PROJECTS"] = project
                try:
                    parent = seed_project_thread(root, env)
                finally:
                    for key, value in (("FNO_INBOX_ROOT", saved[0]), ("FNO_INBOX_KNOWN_PROJECTS", saved[1])):
                        if value is None:
                            os.environ.pop(key, None)
                        else:
                            os.environ[key] = value
                argv = [a.replace("<PARENT_MSG>", parent) for a in argv]
                if case.get("wipe_bus"):
                    seed_bus(root, [])
        if case.get("style_on"):
            env.pop("FNO_STYLE_ENFORCE", None)
        if case.get("json_out"):
            argv.append("--json")
        proc = subprocess.run(
            ["fno", "agents", "mail", "reply", *argv],
            env=env,
            cwd=root,
            capture_output=True,
            text=True,
            timeout=300,
        )
        write_golden(
            case["label"],
            proc.returncode,
            normalize(proc.stdout, root),
            normalize(proc.stderr, root),
        )
        print(f"captured {slug(case['label'])}: exit {proc.returncode}")
    finally:
        shutil.rmtree(root, ignore_errors=True)


def main() -> None:
    bindir = Path(tempfile.mkdtemp(prefix="fnoreplyshim"))
    make_shim(bindir)
    try:
        for case in CASES:
            run_case(case, bindir)
    finally:
        shutil.rmtree(bindir, ignore_errors=True)
    print(f"goldens written to {GOLDEN_DIR}")


if __name__ == "__main__":
    main()
