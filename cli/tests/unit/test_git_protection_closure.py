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
import re
import subprocess
import sys
from pathlib import Path

import pytest

REPO = Path(__file__).resolve().parents[3]
GATE = REPO / "scripts" / "ci" / "check-pr-node-closure.sh"
HOOK = REPO / "hooks" / "git-protection.py"

# Refs the fake binary reports node ids for; anything else answers [].
FAKE_TABLE = {
    "feature/x-49ec": ["x-49ec"],
    "feature/x-1111": ["x-1111"],
    "feature/x-eeeb": ["x-eeeb"],
    "feature/xd863": ["xd863"],
    "chore/tidy-docs": [],
}


def _load_hook():
    spec = importlib.util.spec_from_file_location("_git_protection_x49ec", HOOK)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def _gate(body: str, head_ref: str) -> int:
    """The real CI gate's exit code for this (body, ref) pair."""
    return subprocess.run(
        ["bash", str(GATE)],
        env={"PATH": "/usr/bin:/bin", "PR_BODY": body, "PR_HEAD_REF": head_ref},
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


def test_hook_denies_a_bare_create_from_a_node_branch(monkeypatch):
    reason = _hook_decision(
        'gh pr create --title t --body "$BODY"', "feature/x-49ec", monkeypatch=monkeypatch
    )
    assert reason is not None
    # The refusal NAMES the candidate and never prescribes a trailer to paste.
    # A gate's remediation advice is a PRODUCER of claims, and this producer
    # reads no graph: the binary's scanner matches the id GRAMMAR, which ordinary
    # English also fits (`fix-dead-code` yields `fix-dead`). The old text told
    # the author to write that id, which greens CI and then voids the whole
    # binding at merge - the exact claim the producer refuses.
    assert "x-49ec" in reason
    assert "Backlog-Closure: x-49ec" not in reason, (
        "the refusal prescribes a literal trailer again, which is the "
        "unverified-mint defect this text was rewritten to close"
    )
    assert "fno do pr closure-trailer" in reason


def test_hook_allows_a_non_node_branch(monkeypatch):
    assert _hook_decision(
        'gh pr create --title t --body "$BODY"', "chore/tidy-docs", monkeypatch=monkeypatch
    ) is None


def test_hook_allows_when_the_composition_step_is_visible(monkeypatch):
    assert _hook_decision(
        'gh pr create --title t --body "${BODY}\n${CLOSURE_TRAILER}"',
        "feature/x-49ec",
        monkeypatch=monkeypatch,
    ) is None


def test_hook_allows_a_literal_trailer_in_the_command(monkeypatch):
    assert _hook_decision(
        'gh pr create --title t --body "s\n\nBacklog-Closure: x-49ec"',
        "feature/x-49ec",
        monkeypatch=monkeypatch,
    ) is None


def test_hook_recognizes_a_compact_literal_trailer(monkeypatch):
    assert _hook_decision(
        'gh pr create --title t --body "Fixes xd863"',
        "feature/xd863",
        monkeypatch=monkeypatch,
    ) is None


@pytest.mark.parametrize("domain", ["code", "docs", "front-end"])
def test_a_batch_branch_never_parses_as_node_bearing(domain):
    # <word>-<hex> IS the node-id grammar, so every batch-code-a1b2c3 branch
    # read as node-bearing. The gate then demanded an id the graph does not
    # carry, and one unknown id refuses the WHOLE binding.
    ref = f"feature/batch-{domain}.a1b2c3"
    assert _gate("Batch body.", ref) == 0


@pytest.mark.parametrize(
    "head_ref",
    [
        # The regression. `cache-dead` fits the id grammar and is ordinary
        # English, so the producer refuses to claim it while the gate demanded
        # it, and NO body satisfied both. Measured against the pre-fix gate:
        # this ref exits 1 there and 0 here. It is the docstring's own example.
        "feature/x-49ec-cache-dead",
        # These two already passed both gates and are pinned so they keep
        # passing. Saying which is which matters: three green rows over a
        # one-row fix reads as threefold proof.
        "feature/x-49ec-tidy-docs",
        "feature/x-49ec",
    ],
)
def test_what_the_producer_writes_always_satisfies_the_gate(head_ref):
    # The property the two halves must share, run END TO END rather than
    # asserted on the producer alone. The previous parity test pinned the
    # producer half and never invoked `_gate`, so a gate demanding an
    # unclaimable id passed it.
    body = "Summary.\n\nFixes x-49ec\n"  # the producer's exact answer for every ref here
    assert _gate(body, head_ref) == 0, (
        f"producer wrote a body the gate rejects for {head_ref!r}: {body!r}"
    )


def test_a_branch_naming_no_real_node_still_fails_the_gate():
    # The other half of the property, and NOT a bug. On `feature/x-dead-code`
    # the only candidate is `x-dead`, which the graph does not carry, so the
    # producer writes no trailer and the gate correctly demands a real claim.
    # The author names the real node or takes the documented hatch. (The gate
    # reads only the families the repo mints, so an ordinary English pair like
    # `fix-dead` is a branch word now, not a candidate.)
    # Without this case the parity test above reads as "the producer can always
    # satisfy the gate", which is false and would justify deleting the gate.
    body = "Summary."  # the producer writes nothing: the only candidate, x-dead, is not real
    assert "Backlog-Closure" not in body
    assert _gate(body, "feature/x-dead-code") != 0


def test_the_gate_still_fails_a_body_that_claims_nothing():
    # The negative control for the parity test. Loosening the gate to "at least
    # one" must not loosen it to "never fails", which is the shape that reads
    # as a passing gate while checking nothing.
    assert _gate("Summary with no trailer at all.", "feature/x-49ec") != 0


def test_hook_docstrings_do_not_claim_creation_is_ungated():
    # A PR must not ship a doc its own code disproves.
    #
    # Assert a POSITIVE marker, never an absence. The old body checked that one
    # exact phrase was gone, so any REWORD of the same stale claim passed and
    # the test read as protection. An absence has two explanations, the real
    # outcome and "the instrument never ran", and this one could not tell them
    # apart: it passes identically against an empty file.
    text = HOOK.read_text(encoding="utf-8")
    assert text, "hook source read as empty, so any absence check here is vacuous"
    # The docstring must state the gate that actually exists.
    assert "_closure_trailer_refusal" in text
    assert re.search(r"gh pr create.{0,400}closure", text, re.IGNORECASE | re.DOTALL), (
        "the hook no longer documents that it gates `gh pr create` on a "
        "closure trailer, so its docstring and its code disagree"
    )


def test_hook_escape_hatch_clears_the_gate(monkeypatch):
    assert _hook_decision(
        'gh pr create --title t --body "$BODY"',
        "feature/x-49ec",
        env={"FNO_PR_CLOSURE_OK": "1"},
        monkeypatch=monkeypatch,
    ) is None


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


@pytest.mark.parametrize(
    "command",
    [
        # In a commit message, in a compound command.
        'git commit -m "note: FNO_PR_CLOSURE_OK=1 was needed" && gh pr create --body "$B"',
        # In the PR body itself.
        'gh pr create --body "we set FNO_PR_CLOSURE_OK=1 here"',
        # Echoed before the call. This one already denied before the position
        # fix, but for the WRONG reason: the quote characters broke a
        # whitespace boundary. A test pinning a right answer for a wrong reason
        # rots the moment the quoting changes, so it is pinned here on position.
        'echo "FNO_PR_CLOSURE_OK=1" ; gh pr create --body "$B"',
    ],
)
def test_hook_ignores_a_hatch_that_is_not_an_assignment_prefix(command, node_branch_repo):
    # Position, never presence. A whole-command substring search read the
    # string out of any quoted argument and opened the gate - the same
    # marker-substring false allow this guard exists to close, reintroduced by
    # its own fix.
    payload = _hook_main(command, node_branch_repo)
    assert payload["permissionDecision"] == "deny"


# Wrapped creates, which had no test at all before this.
#
# Measured against the pre-unification hook rather than assumed: of the seven
# cases below, exactly ONE changed behavior when `_pr_create_signals` stopped
# re-implementing the prefix walk. `timeout 60 FNO_PR_CLOSURE_OK=1 gh pr
# create` denied before and allows now, because the thinner copy treated the
# wrapper's own option as a hard stop and never reached the assignment. The
# other six already behaved correctly and are pinned so they keep doing so.
#
# Saying that plainly matters more than the count. A comment claiming all
# seven caught the bug would read as sevenfold proof of a onefold fix.

@pytest.mark.parametrize(
    "command",
    [
        # A shell runner holds the whole create in one quoted token.
        'bash -c "gh pr create --title t --body no-trailer-here"',
        'sh -c "gh pr create --title t --body no-trailer-here"',
        # A wrapper carrying its OWN option, which used to hide the verb.
        "timeout 60 gh pr create --title t --body no-trailer-here",
        "env -i gh pr create --title t --body no-trailer-here",
    ],
)
def test_hook_denies_a_wrapped_create_with_no_trailer(command, node_branch_repo):
    payload = _hook_main(command, node_branch_repo)
    assert payload["permissionDecision"] == "deny"


@pytest.mark.parametrize(
    "command",
    [
        # The hatch travels INSIDE the runner's quoted command.
        'bash -c "FNO_PR_CLOSURE_OK=1 gh pr create --title t --body b"',
        # The hatch sits after a wrapper that took its own option first.
        "timeout 60 FNO_PR_CLOSURE_OK=1 gh pr create --title t --body b",
        # And before it, where a shell applies it to the wrapped command too.
        "FNO_PR_CLOSURE_OK=1 timeout 60 gh pr create --title t --body b",
    ],
)
def test_hook_honors_a_real_hatch_through_a_wrapper(command, node_branch_repo):
    # The assignment is a real prefix of the command that runs, so it is the
    # operator's stated intent whichever side of the wrapper it sits on.
    payload = _hook_main(command, node_branch_repo)
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

def test_hook_judges_the_explicit_head_ref_not_the_checkout(node_branch_repo):
    # The PR closes what its HEAD ref names, which is what the CI gate reads.
    payload = _hook_main(
        'gh pr create --head chore/tidy-docs --body "$BODY"', node_branch_repo
    )
    assert payload is None or payload["permissionDecision"] != "deny"


def test_hook_reads_the_short_head_flag(node_branch_repo):
    # `-H` is gh's short --head, the same one-spelling gap the -F fix closed.
    payload = _hook_main(
        'gh pr create -H chore/tidy-docs --body "$BODY"', node_branch_repo
    )
    assert payload is None or payload["permissionDecision"] != "deny"


def test_hook_still_denies_a_node_bearing_explicit_head(node_branch_repo):
    payload = _hook_main(
        'gh pr create --head feature/x-1111 --body "$BODY"', node_branch_repo
    )
    assert payload["permissionDecision"] == "deny"
    # The --head id is still what gets NAMED, and is still not prescribed.
    reason = payload["permissionDecisionReason"]
    assert "x-1111" in reason
    assert "Backlog-Closure: x-1111" not in reason


@pytest.mark.parametrize("flag", ["--body-file", "-F"])
def test_hook_reads_a_body_file_under_either_spelling(flag, node_branch_repo, tmp_path):
    body = tmp_path / "pr-body.md"
    body.write_text("Summary.\n\nFixes x-49ec\n")
    payload = _hook_main(f"gh pr create --title t {flag} {body}", node_branch_repo)
    assert payload is None or payload["permissionDecision"] != "deny"


def test_hook_never_denies_on_a_body_file_it_cannot_judge(node_branch_repo, tmp_path):
    # A stale file must NOT deny. This hook runs BEFORE the command, so a
    # fixed, never-cleaned path like .fno/pr-body.md holds the PREVIOUS PR's
    # body while the very same command is about to overwrite it. Judging that
    # content denied the compose-then-pass flow skills/ship/references/create.md
    # prescribes. The no-trailer case on a real file is now decided (and
    # denied) elsewhere; a STALE file that still carries a trailer remains a
    # known false ALLOW, the failure mode this hook's contract accepts while
    # ruling out a false DENY.
    body = tmp_path / "pr-body.md"
    body.write_text("Summary.\n\nBacklog-Closure: x-1111\n")
    payload = _hook_main(f"gh pr create --title t --body-file {body}", node_branch_repo)
    assert payload is None or payload["permissionDecision"] != "deny"


def test_hook_never_denies_on_a_body_file_that_does_not_exist_yet(node_branch_repo, tmp_path):
    # The same defect with the opposite symptom. A missing path is one of the
    # shapes the read cannot judge, so it still allows; the file that EXISTS
    # and carries no trailer is denied by its own case now.
    payload = _hook_main(
        f"gh pr create --title t --body-file {tmp_path / 'not-written-yet.md'}",
        node_branch_repo,
    )
    assert payload is None or payload["permissionDecision"] != "deny"


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


def test_hook_refusal_keeps_the_generator_and_still_prescribes_nothing(node_branch_repo, tmp_path):
    # The producer discipline survives: the refusal points at the
    # graph-reading generator and never hands over a pasteable trailer,
    # which is what the merge-time bind voids.
    body = tmp_path / "pr-body.md"
    body.write_text("Summary only.\n")
    payload = _hook_main(
        f"gh pr create --title t --body-file {body} --head feature/x-eeeb",
        node_branch_repo,
    )
    reason = payload["permissionDecisionReason"]
    assert "fno do pr closure-trailer" in reason
    assert "Backlog-Closure: x" not in reason


def test_hook_body_file_verdict_needs_a_node_bearing_branch(node_branch_repo, tmp_path):
    # The binary's scan returns empty for a branch naming no node, so the
    # body-file loop is never reached and the same untrailered file allows.
    body = tmp_path / "pr-body.md"
    body.write_text("Summary only.\n")
    payload = _hook_main(
        f"gh pr create --title t --body-file {body} --head chore/tidy-docs",
        node_branch_repo,
    )
    assert payload is None or payload["permissionDecision"] != "deny"


def test_hook_denies_the_untrailered_file_under_the_short_flag(node_branch_repo, tmp_path):
    # -F is gh's short --body-file; the short spelling must not become the
    # hole the long one lacks.
    body = tmp_path / "pr-body.md"
    body.write_text("Summary only.\n")
    payload = _hook_main(
        f"gh pr create --title t -F {body} --head feature/x-eeeb", node_branch_repo
    )
    assert payload["permissionDecision"] == "deny"


def test_hook_hatch_prefix_allows_an_untrailered_body_file(node_branch_repo, tmp_path):
    # The deliberate override stays: the assignment prefix is read by
    # POSITION on the create segment.
    body = tmp_path / "pr-body.md"
    body.write_text("Summary only.\n")
    payload = _hook_main(
        f"FNO_PR_CLOSURE_OK=1 gh pr create --title t --body-file {body} "
        f"--head feature/x-eeeb",
        node_branch_repo,
    )
    assert payload is None or payload["permissionDecision"] != "deny"


def test_hook_env_hatch_allows_an_untrailered_body_file(node_branch_repo, tmp_path, monkeypatch):
    # The other hatch spelling, read from the hook process environment.
    monkeypatch.setenv("FNO_PR_CLOSURE_OK", "1")
    body = tmp_path / "pr-body.md"
    body.write_text("Summary only.\n")
    payload = _hook_main(
        f"gh pr create --title t --body-file {body} --head feature/x-eeeb",
        node_branch_repo,
    )
    assert payload is None or payload["permissionDecision"] != "deny"


def test_hook_ignores_a_create_quoted_inside_a_body_argument(node_branch_repo):
    # A --body QUOTING the words of a body-file create is one token, never a
    # second create segment, so the guard must not read /tmp/whatever.md.
    # The verdict is still deny, because no composition marker runs in the
    # command - but the reason is the marker's, not a body-file read's.
    payload = _hook_main(
        'gh pr create --body "see gh pr create --body-file /tmp/whatever.md next time"',
        node_branch_repo,
    )
    assert payload["permissionDecision"] == "deny"
    reason = payload["permissionDecisionReason"]
    assert "/tmp/whatever.md" not in reason
    assert "no Backlog-Closure line" not in reason


def test_hook_heredoc_body_carries_no_body_file_verdict(node_branch_repo):
    # The command-substitution heredoc shape that once blocked a whole PR:
    # a heredoc body is not a --body-file value, so no path is judged and
    # the refusal carries no body-file sentence.
    payload = _hook_main(
        'gh pr create --title "t" --body "$(cat <<\'BODY\'\n'
        "| real `gh pr merge` | deny |\n"
        "prose mentioning gh pr merge\n"
        "BODY\n"
        ')"',
        node_branch_repo,
    )
    assert payload["permissionDecision"] == "deny"
    reason = payload["permissionDecisionReason"]
    assert "no Backlog-Closure line" not in reason


def test_hook_graphql_exec_matches_no_create_segment(node_branch_repo):
    # argv[0] is fno, not gh, and the routed tail is `api graphql`, judged by
    # a different arm. Pin it so a later refactor of the segment matcher
    # cannot start catching this spelling.
    payload = _hook_main(
        "fno do pr graphql-exec --purpose discretionary -- api graphql "
        "-f query=query { viewer { login } }",
        node_branch_repo,
    )
    assert payload is None


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


def test_hook_allows_an_unexpanded_variable_in_the_body_file_path(node_branch_repo):
    # Stated ceiling, not a bug: PreToolUse sees the raw token, the variable
    # resolves to no file on disk, and the read fails open.
    payload = _hook_main(
        "gh pr create --title t --body-file $CLAUDE_JOB_DIR/tmp/pr-body.md "
        "--head feature/x-eeeb",
        node_branch_repo,
    )
    assert payload is None or payload["permissionDecision"] != "deny"


def test_hook_allows_a_body_file_over_the_size_cap(node_branch_repo, tmp_path):
    # The 1 MiB cap bounds a wrong path, and an oversized file allows: the
    # gate reads only what a PR body needs.
    body = tmp_path / "pr-body.md"
    body.write_text("x" * ((1 << 20) + 1))
    payload = _hook_main(
        f"gh pr create --title t --body-file {body} --head feature/x-eeeb",
        node_branch_repo,
    )
    assert payload is None or payload["permissionDecision"] != "deny"


def test_hook_allows_a_body_file_that_is_a_directory(node_branch_repo, tmp_path):
    # A directory reads as an OSError on the file read, so it allows and the
    # hook must not raise (a crashed hook is a silent allow for everything).
    body = tmp_path / "a-directory"
    body.mkdir()
    payload = _hook_main(
        f"gh pr create --title t --body-file {body} --head feature/x-eeeb",
        node_branch_repo,
    )
    assert payload is None or payload["permissionDecision"] != "deny"


def test_hook_allows_a_stale_body_file_the_same_command_rewrites(node_branch_repo, tmp_path):
    # The docstring's second reason, as ONE command: the file on disk is the
    # stale PREVIOUS body, but the same command rewrites it before gh reads
    # it, so judging the bytes on disk would deny a body composed correctly
    # one step earlier. The rewrite segment names the path; the guard allows.
    body = tmp_path / "pr-body.md"
    body.write_text("stale trailer-less body\n")
    payload = _hook_main(
        f"printf '%s\\n' \"$BODY\" > {body} && "
        f"gh pr create --title t --body-file {body} --head feature/x-eeeb",
        node_branch_repo,
    )
    assert payload is None or payload["permissionDecision"] != "deny"


def test_hook_denies_the_same_stale_file_with_no_rewrite_segment(node_branch_repo, tmp_path):
    # The deny-side twin of the case above: same stale trailer-less file on
    # disk, no segment naming the path, so the bytes gh will read are the
    # bytes on disk and the guard judges them. Without this twin, the stale
    # case would pass against a function that never denies.
    body = tmp_path / "pr-body.md"
    body.write_text("stale trailer-less body\n")
    payload = _hook_main(
        f"gh pr create --title t --body-file {body} --head feature/x-eeeb",
        node_branch_repo,
    )
    assert payload["permissionDecision"] == "deny"


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


def test_hook_normpath_equates_the_redirect_and_flag_spellings(node_branch_repo, tmp_path):
    # ./x.md in the redirect and x.md in --body-file are one path; the raw
    # token comparison alone would let a ./-spelled rewrite deny its own
    # correctly composed body.
    body = tmp_path / "x.md"
    body.write_text("stale trailer-less body\n")
    payload = _hook_main(
        f"printf '%s\\n' \"$BODY\" > {tmp_path}/./x.md && "
        f"gh pr create --title t --body-file {tmp_path}/x.md --head feature/x-eeeb",
        node_branch_repo,
    )
    assert payload is None or payload["permissionDecision"] != "deny"


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

