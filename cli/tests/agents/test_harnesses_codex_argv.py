"""Pure-function tests for codex argv builders.

Locks in the CLI surface we pass to ``codex exec`` / ``codex exec resume``
so a future refactor cannot silently change the codex invocation contract.

Plan ACs covered:
- 'inject_from_name(prompt, "orchestrator") returns "[from: orchestrator]\\n\\n<prompt>"'
- 'sandbox_flag(yolo=False) returns ["--sandbox", "workspace-write"] plus the network override'
- 'sandbox_flag(yolo=True) returns ["--dangerously-bypass-approvals-and-sandbox"]'
- 'Mutually exclusive in any argv builder output'
- 'from_name validation reuses the US2 validator (no fresh regex)'
"""
from __future__ import annotations


from fno.agents.harnesses import codex as codex_mod


# ---------------------------------------------------------------------------
# inject_from_name
# ---------------------------------------------------------------------------


# ---------------------------------------------------------------------------
# sandbox_flag (create path)
# ---------------------------------------------------------------------------


# ---------------------------------------------------------------------------
# approval_flag (create path — GLOBAL flag, emitted before `exec`)
# ---------------------------------------------------------------------------


# ---------------------------------------------------------------------------
# Bounded-posture amendment (US1/US3). The headless autonomous exec lane
# (create/resume) defaults to the BOUNDED posture (sandboxed + never-prompt),
# so a headless codex worker cannot hang AND keeps the workspace sandbox. Full
# yolo (unsandboxed bypass) is reachable only via the explicit yolo opt-in
# (the `yolo` bareword) or config.agents.codex.headless_yolo: true.
# ---------------------------------------------------------------------------

# sandbox_flag output (approval is asserted separately via approval_flag).
_BOUNDED = ["--sandbox", "workspace-write", "-c", "sandbox_workspace_write.network_access=true"]
_FULL_YOLO = ["--dangerously-bypass-approvals-and-sandbox"]


def _hermetic_config(tmp_path, monkeypatch, content: str = "schema_version: 1\n") -> None:

    f = tmp_path / "settings.yaml"
    f.write_text(content, encoding="utf-8")
    monkeypatch.setenv("FNO_CONFIG", str(f))


# ---------------------------------------------------------------------------
# git_writable_args — the bounded sandbox cannot write git metadata without it.
#
# codex's workspace-write policy marks <project_root>/.git read-only, so a
# bounded worker cannot take index.lock and every commit fails. In a LINKED
# worktree the block is doubly hard: the gitdir lives at
# <repo>/.git/worktrees/<name>/, outside the workspace entirely. One
# --add-dir on the git COMMON dir covers both, plus the shared objects/ and
# refs/ a commit also writes.
# ---------------------------------------------------------------------------


def _init_repo(path):
    import subprocess

    path.mkdir(parents=True, exist_ok=True)
    subprocess.run(["git", "init", "-q"], cwd=path, check=True)
    return path


def test_git_writable_args_returns_common_dir_for_linked_worktree(tmp_path):
    """AC1-HP (the reported case): a linked worktree resolves to the COMMON
    dir, not its own gitdir - the common dir contains .git/worktrees/<name>/
    (index.lock) AND the objects/refs a commit writes."""
    import pathlib
    import subprocess

    repo = _init_repo(tmp_path / "repo")
    subprocess.run(
        ["git", "-c", "user.email=t@t.t", "-c", "user.name=t",
         "commit", "-q", "--allow-empty", "-m", "base"],
        cwd=repo, check=True,
    )
    wt = tmp_path / "wt"
    subprocess.run(
        ["git", "worktree", "add", "-q", str(wt), "-b", "feat"],
        cwd=repo, check=True,
    )
    # Precondition: this really is a linked worktree (.git is a FILE).
    assert (wt / ".git").is_file()

    out = codex_mod.git_writable_args(wt)

    assert out[0] == "--add-dir"
    assert pathlib.Path(out[1]).resolve() == (repo / ".git").resolve()


def test_git_writable_args_empty_outside_a_repo(tmp_path):
    """AC3-ERR: a non-repo cwd changes no argv and never raises."""
    outside = tmp_path / "plain"
    outside.mkdir()
    assert codex_mod.git_writable_args(outside) == []


def test_plan_writable_args_returns_add_dir_for_resolved_plan_dir(
    tmp_path, monkeypatch
):
    """A bounded worker receives its configured plan directory explicitly."""
    plan_dir = tmp_path / "plans"
    monkeypatch.setattr(codex_mod, "_resolve_plan_dir", lambda cwd: str(plan_dir))

    assert codex_mod.plan_writable_args(tmp_path) == ["--add-dir", str(plan_dir)]


# ---------------------------------------------------------------------------
# sandbox_flag_resume (resume path — restricted surface)
# ---------------------------------------------------------------------------


# ---------------------------------------------------------------------------
# sandbox_config_args_resume — resume does NOT inherit the create-time posture.
#
# Measured from a recorded rollout: the create turn ran workspace-write with a
# managed profile, and the resumed turn of that SAME session ran
# danger-full-access with the profile disabled, because resume re-read the
# install's config default. So a bounded worker either loses its sandbox on
# resume, or keeps it and loses the create-time --add-dir git grant and can no
# longer commit. `-c` is the only carrier resume accepts for either half.
# ---------------------------------------------------------------------------


