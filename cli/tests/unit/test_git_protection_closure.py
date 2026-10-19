"""git-protection hook: the closure-trailer gate over the fno-agents binary.

Split out of the retired test_pr_closure_producer.py. The hook's branch scan
now shells to `fno-agents pr-closure-branch-ids`; unit tests stub that call in
process (_hook_decision) and subprocess tests point FNO_AGENTS_BIN at a fake
binary answering from FAKE_TABLE, so every refusal assertion keeps its deny
semantics without a live graph.
"""
from __future__ import annotations

import importlib.util
import json
import os
import subprocess
import sys
import tempfile
from pathlib import Path

import pytest

REPO = Path(__file__).resolve().parents[3]
GATE = REPO / "scripts" / "ci" / "check-pr-node-closure.sh"
HOOK = REPO / "hooks" / "git-protection.py"

# Refs the fake binary reports node ids for; anything else answers []. The
# _gate entries mirror node_branch.rs node_ids() verbatim: the scanner yields
# grammar candidates (cache-dead is one), and the gate's family filter is what
# stops it being demanded.
FAKE_TABLE = {
    "feature/x-49ec": ["x-49ec"],
    "feature/x-1111": ["x-1111"],
    "feature/x-eeeb": ["x-eeeb"],
    "feature/xd863": ["xd863"],
    "chore/tidy-docs": [],
    "feature/x-49ec-cache-dead": ["x-49ec", "cache-dead"],
    "feature/x-49ec-tidy-docs": ["x-49ec"],
    "feature/x-dead-code": ["x-dead"],
}


def _load_hook():
    spec = importlib.util.spec_from_file_location("_git_protection_x49ec", HOOK)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def _gate(body: str, head_ref: str) -> int:
    """The real CI gate's exit code for this (body, ref) pair."""
    with tempfile.TemporaryDirectory() as tmp:
        fake = Path(tmp) / ".fake-fno-agents"
        fake.write_text(
            "#!/usr/bin/env python3\n"
            "import json, sys\n"
            f"TABLE = {json.dumps(FAKE_TABLE)}\n"
            "req = json.load(sys.stdin)\n"
            'print(json.dumps({"ids": TABLE.get(req.get("head_ref", ""), [])}))\n'
        )
        fake.chmod(0o755)
        return subprocess.run(
            ["bash", str(GATE)],
            env={
                "PATH": "/usr/bin:/bin",
                "FNO_AGENTS_BIN": str(fake),
                "PR_BODY": body,
                "PR_HEAD_REF": head_ref,
            },
            capture_output=True,
            text=True,
        ).returncode


def _hook_decision(command, branch, env=None, monkeypatch=None):
    hook = _load_hook()
    monkeypatch.setattr(hook, "get_current_branch", lambda: branch)
    # The hook's scan shells to the fno-agents binary; answer it in process.
    monkeypatch.setattr(
        hook, "_real_branch_node_ids", lambda ref: FAKE_TABLE.get(ref, [])
    )
    for key, value in (env or {}).items():
        monkeypatch.setenv(key, value)
    return hook._closure_trailer_refusal(command)


def test_a_batch_branch_never_parses_as_node_bearing():
    # <word>-<hex> IS the node-id grammar, so every batch-code-a1b2c3 branch
    # read as node-bearing. The gate then demanded an id the graph does not
    # carry, and one unknown id refuses the WHOLE binding.
    assert _gate("Batch body.", "feature/batch-code.a1b2c3") == 0


def test_what_the_producer_writes_always_satisfies_the_gate():
    # The property the two halves must share, run END TO END rather than
    # asserted on the producer alone. `cache-dead` fits the id grammar and is
    # ordinary English, so the producer refuses to claim it while the gate
    # once demanded it, and NO body satisfied both.
    head_ref = "feature/x-49ec-cache-dead"
    body = "Summary.\n\nFixes x-49ec\n"  # the producer's exact answer for this ref
    assert _gate(body, head_ref) == 0, (
        f"producer wrote a body the gate rejects for {head_ref!r}: {body!r}"
    )


def test_the_gate_still_fails_a_body_that_claims_nothing():
    # The negative control for the parity test. Loosening the gate to "at least
    # one" must not loosen it to "never fails", which is the shape that reads
    # as a passing gate while checking nothing.
    assert _gate("Summary with no trailer at all.", "feature/x-49ec") != 0


def _hook_main(command, cwd):
    env = dict(os.environ)
    # Route the hook's branch scan at the fixture's fake binary.
    env["FNO_AGENTS_BIN"] = str(Path(cwd) / ".fake-fno-agents")
    result = subprocess.run(
        [sys.executable, str(HOOK)],
        input=json.dumps({"tool_name": "Bash", "tool_input": {"command": command}}),
        capture_output=True, text=True, cwd=str(cwd), env=env,
    )
    # A crashed hook prints nothing, and silence here is the ALLOW shape. So a
    # NameError from a bad edit once made every case in a sweep "pass",
    # including the control. Exit code first, then the payload.
    assert result.returncode == 0, (
        f"hook exited {result.returncode}, so its empty stdout is a crash "
        f"rather than an allow: {result.stderr.strip()[:400]}"
    )
    return json.loads(result.stdout)["hookSpecificOutput"] if result.stdout.strip() else None


@pytest.fixture
def node_branch_repo(tmp_path):
    """A throwaway repo checked out on a node-bearing branch.

    The hook reads the CURRENT branch, so anchoring this to the test runner's
    own checkout would pass here and silently skip in CI, where a PR build sits
    on a detached HEAD and `git symbolic-ref` fails.
    """
    for argv in (
        ["git", "init", "-q", "-b", "feature/x-49ec"],
        ["git", "config", "user.email", "t@example.com"],
        ["git", "config", "user.name", "t"],
        ["git", "commit", "-q", "--allow-empty", "-m", "init"],
    ):
        subprocess.run(argv, cwd=tmp_path, check=True, capture_output=True)
    # A stand-in for fno-agents: one JSON request in, one {"ids": [...]} out.
    fake = tmp_path / ".fake-fno-agents"
    fake.write_text(
        "#!/usr/bin/env python3\n"
        "import json, sys\n"
        f"TABLE = {json.dumps(FAKE_TABLE)}\n"
        "req = json.load(sys.stdin)\n"
        'print(json.dumps({"ids": TABLE.get(req.get("head_ref", ""), [])}))\n'
    )
    fake.chmod(0o755)
    return tmp_path


# The hatch is judged by POSITION, so every case here goes through main() with
# a real segment list. Calling the predicate directly cannot test position.

def test_hook_honors_an_inline_hatch_prefixing_the_gh_call(node_branch_repo):
    # A PreToolUse hook is a separate process, so an inline assignment never
    # reaches its os.environ. Reading only the env made the documented hatch a
    # dead end, and the refusal message taught that spelling.
    payload = _hook_main(
        'FNO_PR_CLOSURE_OK=1 gh pr create --title t --body "$BODY"', node_branch_repo
    )
    assert payload is None or payload["permissionDecision"] != "deny"


def test_hook_ignores_a_hatch_that_is_not_an_assignment_prefix(node_branch_repo):
    # Position, never presence. A whole-command substring search read the
    # string out of any quoted argument and opened the gate - the same
    # marker-substring false allow this guard exists to close, reintroduced by
    # its own fix.
    payload = _hook_main(
        'git commit -m "note: FNO_PR_CLOSURE_OK=1 was needed" && gh pr create --body "$B"',
        node_branch_repo,
    )
    assert payload["permissionDecision"] == "deny"


@pytest.mark.parametrize(
    "command",
    [
        # A shell runner holds the whole create in one quoted token.
        'bash -c "gh pr create --title t --body no-trailer-here"',
        # A wrapper carrying its OWN option, which used to hide the verb.
        "timeout 60 gh pr create --title t --body no-trailer-here",
    ],
)
def test_hook_denies_a_wrapped_create_with_no_trailer(command, node_branch_repo):
    payload = _hook_main(command, node_branch_repo)
    assert payload["permissionDecision"] == "deny"


def test_hook_honors_a_real_hatch_through_a_wrapper(node_branch_repo):
    # The hatch sits after a wrapper that took its own option first. A thinner
    # prefix walk treated the wrapper's option as a hard stop and never
    # reached the assignment.
    payload = _hook_main(
        "timeout 60 FNO_PR_CLOSURE_OK=1 gh pr create --title t --body b",
        node_branch_repo,
    )
    assert payload is None or payload["permissionDecision"] != "deny"


@pytest.mark.parametrize(
    "command",
    [
        # A --body that merely QUOTES the words "--head <ref>" retargeted the
        # gate at a ref this PR does not ship.
        'gh pr create --body "see --head chore/tidy-docs for context"',
        # A --body naming a --body-file path in prose satisfied the file read.
        'gh pr create --body "pass --body-file .fno/pr-body.md next time"',
    ],
)
def test_hook_ignores_flags_quoted_inside_an_argument(command, node_branch_repo):
    # Same defect as the hatch, on the two neighbouring flags. Fixing one of
    # three instances is the decorative guard this whole node exists to close:
    # a reader sees a position check on the hatch and assumes its neighbours
    # got the same treatment. shlex collapses a quoted argument into ONE token,
    # so argv position is what separates a flag from a word.
    payload = _hook_main(command, node_branch_repo)
    assert payload["permissionDecision"] == "deny"


# --head and --body-file are POSITION-read from the segment's argv, so every
# case for them runs through main() with a real segment list.


def test_hook_still_denies_a_node_bearing_explicit_head(node_branch_repo):
    payload = _hook_main(
        'gh pr create --head feature/x-1111 --body "$BODY"', node_branch_repo
    )
    assert payload["permissionDecision"] == "deny"
    # The --head id is still what gets NAMED, and is still not prescribed.
    reason = payload["permissionDecisionReason"]
    assert "x-1111" in reason
    assert "Backlog-Closure: x-1111" not in reason


def test_hook_allows_a_body_file_the_same_command_is_about_to_write(node_branch_repo, tmp_path):
    # A PreToolUse hook runs BEFORE the command, so the documented spelling
    # names a path that does not exist yet. Denying it is the false DENY the
    # docstring rules out.
    target = tmp_path / "not-yet.md"
    payload = _hook_main(
        f"printf '%s' \"$B\" > {target}\ngh pr create --title t --body-file {target}",
        node_branch_repo,
    )
    assert payload is None or payload["permissionDecision"] != "deny"


# The body-file read judges a file that EXISTS, so the deny case and its
# false-positive edges all replay the x-eeeb specimen through main(): the
# guard is the segment list, and a verdict pinned on the predicate alone is
# the decorative shape this node exists to close.

def test_hook_denies_an_existing_untrailered_body_file(node_branch_repo, tmp_path):
    # The x-eeeb specimen, real flag order: four worker PRs went red through
    # this one door on 2026-09-20, because the read answered True for every
    # path. The file existed and was readable; the guard could have judged it.
    body = tmp_path / "pr-body.md"
    body.write_text("Summary only.\n")
    payload = _hook_main(
        f"gh pr create --title t --body-file {body} --base main --head feature/x-eeeb",
        node_branch_repo,
    )
    assert payload["permissionDecision"] == "deny"


def test_hook_refusal_names_the_judged_path_not_a_missing_verb(node_branch_repo, tmp_path):
    # The refusal names the file the guard opened. It must NOT name
    # `fno do pr create`, which does not exist; a refusal naming a missing
    # verb sends the reader into a second error.
    body = tmp_path / "pr-body.md"
    body.write_text("Summary only.\n")
    payload = _hook_main(
        f"gh pr create --title t --body-file {body} --head feature/x-eeeb",
        node_branch_repo,
    )
    reason = payload["permissionDecisionReason"]
    assert str(body) in reason
    assert "fno do pr create" not in reason


def test_hook_denies_the_untrailered_file_under_the_short_flag(node_branch_repo, tmp_path):
    # -F is gh's short --body-file; the short spelling must not become the
    # hole the long one lacks.
    body = tmp_path / "pr-body.md"
    body.write_text("Summary only.\n")
    payload = _hook_main(
        f"gh pr create --title t -F {body} --head feature/x-eeeb", node_branch_repo
    )
    assert payload["permissionDecision"] == "deny"


def test_hook_allows_the_sanctioned_create_sequence(node_branch_repo):
    # The two-command create flow runs: compose the
    # body ending in a GENERATED trailer (spelled literally here; the parity
    # goldens pin the generator's grammar), write it to the
    # fixed path, then create as a separate command. Allowed on the merits:
    # the file exists and carries the trailer.
    (node_branch_repo / ".fno").mkdir()
    (node_branch_repo / ".fno" / "pr-body.md").write_text(
        "Summary.\n\nFixes x-49ec\n"
    )
    payload = _hook_main(
        "gh pr create --title t --body-file .fno/pr-body.md", node_branch_repo
    )
    assert payload is None or payload["permissionDecision"] != "deny"


def test_hook_second_create_segment_is_not_a_rewrite(node_branch_repo, tmp_path):
    # Every create segment is excluded from the token set; if only the
    # CURRENT one were, its own --body-file value would match and every body
    # file would allow itself. Two creates naming one untrailered file deny.
    body = tmp_path / "pr-body.md"
    body.write_text("stale trailer-less body\n")
    payload = _hook_main(
        f"gh pr create --title t --body-file {body} && "
        f"gh pr create --title u --body-file {body} --head feature/x-eeeb",
        node_branch_repo,
    )
    assert payload["permissionDecision"] == "deny"


def test_hook_denies_the_full_pretooluse_payload(node_branch_repo):
    # End to end through main(), not just the predicate: a helper that returns a
    # reason nothing calls is the decorative-guard shape this node exists to fix.
    payload = _hook_main('gh pr create --title t --body "$BODY"', node_branch_repo)
    assert payload["permissionDecision"] == "deny"
    reason = payload["permissionDecisionReason"]
    assert "x-49ec" in reason
    assert "fno do pr closure-trailer" in reason
    assert "Backlog-Closure: x-49ec" not in reason


def test_hook_main_allows_a_composed_body(node_branch_repo):
    payload = _hook_main(
        'gh pr create --title t --body "${BODY}\n${CLOSURE_TRAILER}"', node_branch_repo
    )
    assert payload is None or payload["permissionDecision"] != "deny"


def test_hook_main_gates_gh_behind_a_repo_flag(node_branch_repo):
    # `gh --repo o/r pr create` is the same operation; the global-option strip
    # is what keeps this from being a one-spelling guard.
    payload = _hook_main('gh --repo o/r pr create --body "$BODY"', node_branch_repo)
    assert payload["permissionDecision"] == "deny"
