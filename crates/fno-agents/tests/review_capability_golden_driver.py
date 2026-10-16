#!/usr/bin/env python3
"""Capture driver for the `review_capability` goldens.

The Python leg (`fno.review_capability`) is the parity oracle until wave 2
deletes it. This driver runs that leg through the SAME door shape the Rust
`review-level capability` actions print, under env and fixture pins both
legs reproduce, and freezes one golden triple per case
(`<key>.exit` / `<key>.out` / `<key>.err` under `tests/golden/review_capability/`).

Two modes:

  --write-goldens            run every case, freeze the goldens (the capture
                             pass; run while the Python leg lives)
  --case LABEL               build the fixtures for one case, print the leg's
                             door bytes on stdout/stderr, exit with the door's
                             code (the parity test's capture-mode oracle)

Case outputs embed the per-run temp root, normalized to `<TMP>` (raw and
resolved forms) exactly as `review_level_parity.rs` normalizes its own
fixture paths. The case table below is the lockstep twin of
`review_capability_parity.rs`; add a case to BOTH or neither.
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import stat
import subprocess
import sys
import tempfile
from pathlib import Path

# Every prefix that can leak machine identity into `detect_session` or the
# config candidate walk. The parity test scrubs the same set before pinning
# the child's env.
SCRUB_PREFIXES = (
    "FNO_",
    "TARGET_",
    "CLAUDE_",
    "CODEX_",
    "GEMINI_",
    "OPENCODE_",
    "ANTHROPIC_",
    "OPENAI_",
    "ZAI_",
    "GLM_",
)

# Single-marker harness pins: one family present, so neither identity
# resolver has to arbitrate.
CLAUDE_MARK = {"CLAUDE_CODE_SESSION_ID": "s-claude-1"}
CODEX_MARK = {"CODEX_THREAD_ID": "s-codex-1"}
GEMINI_MARK = {"GEMINI_SESSION_ID": "s-gemini-1"}

# Crafted registry rows for the `requires` values the built-in table
# (sigma / code-review / declare) does not carry. Same five keys the payload
# descriptors serialize from `dataclasses.asdict`.
CRAFTED_REGISTRY = {
    "lane-review": {
        "kind": "local-attestation",
        "requires": "none",
        "invocation": "/fno:review",
        "asserts": "review-evidence",
        "invocations": {
            "claude": "/fno:review <level>",
            "codex": "$fno:review <level>",
        },
    },
    "human-approval": {
        "kind": "human",
        "requires": "operator",
        "invocation": "/ask-operator",
        "asserts": "review-evidence",
        "invocations": None,
    },
    "skill-reviewer": {
        "kind": "harness-skill",
        "requires": "skill",
        "invocation": "/my-review-skill run",
        "asserts": "invocation",
        "invocations": None,
    },
    "plugin-reviewer": {
        "kind": "harness-skill",
        "requires": "skill",
        "invocation": "fno:review-helper run",
        "asserts": "invocation",
        "invocations": None,
    },
    "panel": {
        "kind": "external-cli",
        "requires": "subagent-dispatch",
        "invocation": "/fno:review",
        "asserts": "review-evidence",
        "invocations": None,
    },
}


def slug(label: str) -> str:
    """Mirror `common::slug`: lowercase, non-alphanumeric runs to one `_`."""
    out: list[str] = []
    prev_us = False
    for ch in label:
        if ch.isascii() and ch.isalnum():
            out.append(ch.lower())
            prev_us = False
        elif not prev_us:
            out.append("_")
            prev_us = True
    return "".join(out).strip("_")


def scrubbed_environ() -> dict[str, str]:
    return {
        k: v
        for k, v in os.environ.items()
        if not k.startswith(SCRUB_PREFIXES)
    }


def apply_environ(next_env: dict[str, str]) -> None:
    for name in list(os.environ):
        if name not in next_env:
            del os.environ[name]
    os.environ.update(next_env)


GH_STUB = """\
#!/bin/sh
if [ "$1" = "repo" ]; then echo "org/repo"; exit 0; fi
if [ "$1" = "api" ]; then
  if [ -n "${GH_STUB_TOTAL}" ]; then echo "${GH_STUB_TOTAL}"; exit 0; fi
  exit 1
fi
exit 1
"""


def build_fixtures(case_dir: Path, spec: dict) -> Path:
    """Materialize one case's fixtures; returns the cwd the leg runs in."""
    case_dir.mkdir(parents=True, exist_ok=True)
    home = case_dir / "home"
    home.mkdir(exist_ok=True)
    repo = case_dir / "repo"
    repo.mkdir()
    subprocess.run(
        ["git", "init", "-q", "-b", "main"],
        cwd=repo,
        check=True,
        capture_output=True,
    )
    subprocess.run(
        ["git", "config", "user.email", "t@example.test"],
        cwd=repo,
        check=True,
        capture_output=True,
    )
    # `display_name` falls back to `git config user.name`, so the fixture
    # pins a name BOTH legs read back: the operator reason says "ask t".
    subprocess.run(
        ["git", "config", "user.name", "t"],
        cwd=repo,
        check=True,
        capture_output=True,
    )
    if spec.get("settings_yaml") is not None:
        block = repo / ".fno"
        block.mkdir()
        (block / "settings.yaml").write_text(spec["settings_yaml"])
    if spec.get("global_yaml") is not None:
        (case_dir / "global-settings.yaml").write_text(spec["global_yaml"])
    skill_mode = spec.get("skills", "none")
    if skill_mode in ("found", "absent"):
        root = home / ".claude" / "skills"
        root.mkdir(parents=True)
        if skill_mode == "found":
            skill = root / "my-review-skill"
            skill.mkdir()
            (skill / "SKILL.md").write_text("---\nname: x\n---\n")
    if spec.get("gh_stub"):
        bindir = case_dir / "bin"
        bindir.mkdir()
        stub = bindir / "gh"
        stub.write_text(GH_STUB)
        stub.chmod(stub.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)
    return repo


def case_environ(case_dir: Path, spec: dict) -> dict[str, str]:
    env = scrubbed_environ()
    env["HOME"] = str(case_dir / "home")
    env["GIT_CONFIG_GLOBAL"] = "/dev/null"
    env["GIT_CONFIG_SYSTEM"] = "/dev/null"
    # An ABSENT global settings file: the candidate walk must see the pin,
    # not the operator's real ~/.fno.
    env["FNO_GLOBAL_SETTINGS_PATH"] = str(case_dir / "global-settings.yaml")
    env.update(spec.get("markers", {}))
    if spec.get("gh_stub"):
        env["PATH"] = str(case_dir / "bin") + os.pathsep + env.get("PATH", "")
        total = {"false": "0", "true": "3", "none": ""}.get(spec["gh_stub"], "")
        if total:
            env["GH_STUB_TOTAL"] = total
        else:
            env.pop("GH_STUB_TOTAL", None)
    return env


# --- CASE TABLE ----------------------------------------------------------
# Lockstep twin of the `CASES` table in review_capability_parity.rs.


def rev(label, reviewers, markers=None, peers=None, peer_identity=None, skills="none"):
    return {
        "label": label,
        "action": "reviewers",
        "markers": dict(markers or {}),
        "payload": {
            "reviewers": reviewers,
            "peers": peers or [],
            "peer_identity": peer_identity,
        },
        "skills": skills,
    }


def cfg(label, settings_yaml, global_yaml=None):
    return {
        "label": label,
        "action": "reviewers",
        "markers": {},
        "payload": {"reviewers": [], "peers": [], "peer_identity": None},
        "settings_yaml": settings_yaml,
        "global_yaml": global_yaml,
    }


def app(label, apps, nudge=None, gh_stub=None):
    return {
        "label": label,
        "action": "github-apps",
        "markers": {},
        "payload": {"apps": apps, "nudge_logins": list(nudge or [])},
        "gh_stub": gh_stub,
    }


def inv(label, markers=None, **kwargs):
    return {"label": label, "action": "invocation", "markers": dict(markers or {}), "kwargs": kwargs}


def skp(label, name, harness, skills="none"):
    return {
        "label": label,
        "action": "skill-presence",
        "markers": {},
        "argv": ["--name", name, "--harness", harness],
        "skills": skills,
    }


CASES = [
    # requires == "none": the scalar invocation, attended and unattended.
    rev("reviewers none scalar claude attended", ["code-review"], CLAUDE_MARK),
    rev(
        "reviewers none scalar codex unattended",
        ["code-review", "declare"],
        {**CODEX_MARK, "TARGET_UNATTENDED": "1"},
    ),
    # invocations map: hit, known-harness miss, unknown harness.
    rev("reviewers invocations claude hit", ["lane-review"], CLAUDE_MARK),
    rev("reviewers invocations gemini miss", ["lane-review"], GEMINI_MARK),
    rev("reviewers invocations unknown", ["lane-review"]),
    # operator: the attended/unattended fork.
    rev("reviewers operator attended", ["human-approval"], CLAUDE_MARK),
    rev(
        "reviewers operator unattended",
        ["human-approval"],
        {**CLAUDE_MARK, "TARGET_UNATTENDED": "1"},
    ),
    # skill: found, absent, plugin-qualified (all on claude).
    rev("reviewers skill found", ["skill-reviewer"], CLAUDE_MARK, skills="found"),
    rev("reviewers skill absent", ["skill-reviewer"], CLAUDE_MARK, skills="absent"),
    rev("reviewers skill plugin qualified", ["plugin-reviewer"], CLAUDE_MARK),
    # subagent-dispatch across the four harness answers.
    rev("reviewers subagent claude", ["panel"], CLAUDE_MARK),
    rev("reviewers subagent codex", ["panel"], CODEX_MARK),
    rev("reviewers subagent gemini", ["panel"], GEMINI_MARK),
    rev("reviewers subagent unknown", ["panel"]),
    # retired, unknown name, and a refusal mixing remedy classes.
    rev("reviewers retired sigma", ["sigma"], CLAUDE_MARK),
    rev("reviewers unknown name", ["phantom"], CLAUDE_MARK),
    rev(
        "reviewers mixed refusal",
        ["sigma", "panel", "phantom"],
        {**GEMINI_MARK, "TARGET_UNATTENDED": "1"},
    ),
    # local peers: cross-model, same family, unknown author, claude route,
    # and the identity-set shape that returns no verdicts at all.
    rev("peers cross model", [], CLAUDE_MARK, peers=["codex"]),
    rev("peers same family", [], CLAUDE_MARK, peers=["claude"]),
    rev("peers unknown author", [], peers=["codex"]),
    rev(
        "peers claude route",
        [],
        CLAUDE_MARK,
        peers=[{"provider": "claude", "model": "zai,glm-5.3"}],
    ),
    rev("peers identity set", [], CLAUDE_MARK, peers=["codex"], peer_identity="glm-peer"),
    # config.unattended.enabled: per-usable-value walk.
    cfg("config unattended true", "unattended:\n  enabled: true\n"),
    cfg("config unattended yes", 'unattended:\n  enabled: "yes"\n'),
    cfg("config unattended int", "unattended:\n  enabled: 1\n"),
    cfg(
        "config empty then global",
        'unattended:\n  enabled: ""\n',
        "unattended:\n  enabled: true\n",
    ),
    cfg("config block without enabled", "unattended:\n  mode: panel\n"),
    # github_apps: recognized, nudge alias, and the three probe arms.
    app("apps recognized", ["chatgpt-codex-connector"]),
    app("apps nudge alias", ["my-codex"], nudge=["my-codex-bot"]),
    app("apps typo probe false", ["chatgpt-codex-connector-typo"], gh_stub="false"),
    app("apps probe none", ["never-acted-bot"], gh_stub="none"),
    app("apps probe true", ["quiet-reviewer"], gh_stub="true"),
    app(
        "apps mixed refusal",
        ["chatgpt-codex-connector", "chatgpt-codex-connector-typo"],
        gh_stub="false",
    ),
    # invocation renders: bare, PR target, branch target, and the
    # validation refusals. No --root case: sizing would round-trip a binary.
    inv("invocation bare claude", CLAUDE_MARK, harness="claude"),
    inv(
        "invocation pr target",
        CLAUDE_MARK,
        harness="claude",
        pr_number=4183,
        head_sha="f4bbd762d65",
        base_branch="origin/main",
    ),
    inv(
        "invocation branch target codex",
        CODEX_MARK,
        harness="codex",
        branch="feature/x-b546",
        head_sha="f4bbd762d65",
        base_branch="main",
    ),
    inv(
        "invocation bad sha",
        CLAUDE_MARK,
        harness="claude",
        pr_number=4183,
        head_sha="nope",
        base_branch="main",
    ),
    inv(
        "invocation both targets",
        CLAUDE_MARK,
        pr_number=4183,
        branch="b",
        head_sha="f4bbd762d65",
        base_branch="main",
    ),
    inv("invocation missing pr fields", CLAUDE_MARK, pr_number=4183),
    inv("invocation ambient unknown", {}, harness=None),
    # skill-presence door: the probe's outcomes with pinned roots.
    skp("skill presence found", "my-review-skill", "claude", skills="found"),
    skp("skill presence absent", "no-such-skill", "claude", skills="absent"),
    skp("skill presence plugin", "fno:code-review", "claude"),
    skp("skill presence codex", "my-review-skill", "codex"),
    skp("skill presence no roots", "my-review-skill", "claude"),
]

# --- CASE TABLE END ------------------------------------------------------

# --- the Python leg, rendered through the door's shapes -------------------


def payload_descriptors(registry) -> dict:
    """The door payload's descriptor table: built-ins UNION the crafted rows,
    built-ins winning, each serialized as `dataclasses.asdict` with
    `invocations` a dict or None - the exact shape `target_cli` will send."""
    from dataclasses import asdict

    from fno.config import resolvable_reviewers

    return {
        name: asdict(d) for name, d in resolvable_reviewers(registry).items()
    }


def session_json(sess) -> dict:
    return {
        "harness": sess.harness,
        "substrate": sess.substrate,
        "attended": sess.attended,
        "provider": sess.provider,
        "describe": sess.describe(),
    }


def emit_reviewers(spec) -> tuple[str, str, int]:
    from fno.review_capability import (
        detect_session,
        local_peers_refusal_message,
        refusal_message,
        resolve_local_peers,
        resolve_reviewers,
    )

    payload = spec["payload"]
    registry = {
        k: dict(v) for k, v in CRAFTED_REGISTRY.items()
    } if payload["reviewers"] or payload["peers"] else None
    # The leg resolves against the registry; the door payload mirrors it.
    from fno.config import ReviewerDescriptor

    reg_objs = {
        name: ReviewerDescriptor(**row) for name, row in registry.items()
    } if registry else None
    sess = detect_session()
    verdicts = resolve_reviewers(payload["reviewers"], session=sess, registry=reg_objs)
    peer_verdicts = resolve_local_peers(payload["peers"], payload["peer_identity"], sess)
    reviewer_rows = [
        {
            "name": v.name,
            "status": v.status,
            "reason": v.reason,
            "line": v.line(),
            "blocks_autonomy": v.blocks_autonomy,
            "kind": v.descriptor.kind if v.descriptor is not None else None,
            "requires": v.descriptor.requires if v.descriptor is not None else None,
            "asserts": v.descriptor.asserts if v.descriptor is not None else None,
            "invocation": v.descriptor.invocation if v.descriptor is not None else None,
        }
        for v in verdicts
    ]
    peer_rows = [
        {
            "name": v.name,
            "status": v.status,
            "reason": v.reason,
            "line": v.line(),
            "eligible": v.eligible,
        }
        for v in peer_verdicts
    ]
    out = json.dumps(
        {
            "session": session_json(sess),
            "reviewers": reviewer_rows,
            "local_peers": peer_rows,
            "reviewers_refusal": refusal_message(verdicts, sess),
            "local_peers_refusal": local_peers_refusal_message(peer_verdicts, sess),
        },
        ensure_ascii=False,
    )
    return out, "", 0


def emit_github_apps(spec) -> tuple[str, str, int]:
    from fno.review_capability import (
        github_apps_refusal_message,
        resolve_github_apps,
    )

    payload = spec["payload"]
    # The probe runs LIVE: with the case's stub `gh` first on PATH, both legs
    # answer the same False / None / True without a network.
    verdicts = resolve_github_apps(
        payload["apps"], nudge_logins=payload["nudge_logins"]
    )
    rows = [
        {
            "login": v.login,
            "status": v.status,
            "reason": v.reason,
            "line": v.line(),
            "blocks_autonomy": v.blocks_autonomy,
        }
        for v in verdicts
    ]
    out = json.dumps(
        {
            "verdicts": rows,
            "refusal": github_apps_refusal_message(verdicts),
        },
        ensure_ascii=False,
    )
    return out, "", 0


def emit_invocation(spec) -> tuple[str, str, int]:
    from fno.review_capability import detect_session, render_self_review_invocation

    kw = spec["kwargs"]
    try:
        rendered = render_self_review_invocation(
            kw.get("harness"),
            None,
            pr_number=kw.get("pr_number"),
            branch=kw.get("branch"),
            head_sha=kw.get("head_sha"),
            base_branch=kw.get("base_branch"),
        )
    except ValueError as exc:
        # The door re-raises the Python message on stderr with exit 2.
        return "", str(exc) + "\n", 2
    harness = kw.get("harness") or detect_session().harness
    out = json.dumps(
        {"invocation": rendered, "harness": harness}, ensure_ascii=False
    )
    return out, "", 0


def emit_skill_presence(spec) -> tuple[str, str, int]:
    from fno.review_capability import resolve_skill_presence

    argv = spec["argv"]
    name = argv[argv.index("--name") + 1]
    harness = argv[argv.index("--harness") + 1]
    status, reason = resolve_skill_presence(name, harness)
    out = json.dumps({"status": status, "reason": reason}, ensure_ascii=False)
    return out, "", 0


def run_case(spec) -> tuple[str, str, int]:
    """Run one case's Python leg with its pins applied. Returns the door's
    (stdout, stderr, exit), paths normalized to `<TMP>`."""
    root = Path(tempfile.mkdtemp(prefix="review-cap-capture-"))
    saved_env = dict(os.environ)
    saved_cwd = os.getcwd()
    try:
        case_dir = root / slug(spec["label"])
        cwd = build_fixtures(case_dir, spec)
        apply_environ(case_environ(case_dir, spec))
        os.chdir(cwd)
        if spec["action"] == "reviewers":
            out, err, code = emit_reviewers(spec)
        elif spec["action"] == "github-apps":
            out, err, code = emit_github_apps(spec)
        elif spec["action"] == "invocation":
            out, err, code = emit_invocation(spec)
        else:
            out, err, code = emit_skill_presence(spec)
        # The door prints stdout with `println!`, so its stream always ends
        # in one newline; stderr strings carry their own.
        if out:
            out = out + "\n"
    finally:
        os.chdir(saved_cwd)
        os.environ.clear()
        os.environ.update(saved_env)
    raw = str(root)
    resolved = str(root.resolve())
    def norm(text: str) -> str:
        return text.replace(resolved, "<TMP>").replace(raw, "<TMP>")
    shutil.rmtree(root, ignore_errors=True)
    return norm(out), norm(err), code


def write_goldens(subject_dir: Path) -> int:
    failures = []
    for spec in CASES:
        out, err, code = run_case(spec)
        key = slug(spec["label"])
        (subject_dir / f"{key}.exit").write_text(f"{code}\n")
        (subject_dir / f"{key}.out").write_text(out)
        (subject_dir / f"{key}.err").write_text(err)
        print(f"{key} exit={code}", flush=True)
        if not out and code == 0:
            failures.append(key)
    if failures:
        print(f"EMPTY STDOUT (exit 0) for: {', '.join(failures)}", file=sys.stderr)
        return 1
    return 0


def run_one(label: str) -> int:
    import contextlib
    import io

    for spec in CASES:
        if slug(spec["label"]) == slug(label):
            # The oracle caller compares stderr byte-for-byte, so the
            # driver's own loader noise ("config file ... missing") is
            # swallowed and only the leg's door stderr flows through.
            buffer = io.StringIO()
            with contextlib.redirect_stderr(buffer):
                out, err, code = run_case(spec)
            sys.stdout.write(out)
            sys.stderr.write(err)
            return code
    print(f"no such case: {label}", file=sys.stderr)
    return 2


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--write-goldens", action="store_true")
    parser.add_argument("--case", dest="case_label")
    args = parser.parse_args()
    if args.write_goldens:
        subject = Path(__file__).resolve().parent / "golden" / "review_capability"
        subject.mkdir(parents=True, exist_ok=True)
        return write_goldens(subject)
    if args.case_label:
        return run_one(args.case_label)
    parser.print_usage(sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main())

