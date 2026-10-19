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


def test_inject_from_name_bracket_prefix():
    out = codex_mod.inject_from_name("write the schema", "orchestrator-main")
    assert out == "[from: orchestrator-main]\n\nwrite the schema"


# ---------------------------------------------------------------------------
# sandbox_flag (create path)
# ---------------------------------------------------------------------------


def test_sandbox_flag_default_is_bounded():
    # Sandbox tokens only (workspace sandbox); approval is a separate global
    # flag emitted before `exec` - see approval_flag.
    assert codex_mod.sandbox_flag(yolo=False) == ["--sandbox", "workspace-write", "-c", "sandbox_workspace_write.network_access=true"]


def test_sandbox_flag_yolo_is_dangerous_bypass():
    assert codex_mod.sandbox_flag(yolo=True) == [
        "--dangerously-bypass-approvals-and-sandbox"
    ]


# ---------------------------------------------------------------------------
# approval_flag (create path — GLOBAL flag, emitted before `exec`)
# ---------------------------------------------------------------------------


def test_approval_flag_default_is_never_prompt():
    assert codex_mod.approval_flag(yolo=False) == ["--ask-for-approval", "never"]


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


def test_explicit_yolo_forces_bypass():
    """AC1-EDGE: the explicit yolo bareword forces the full bypass."""
    eff = codex_mod._effective_yolo(yolo=True, headless_yolo=False)
    assert codex_mod.sandbox_flag(eff) == _FULL_YOLO


def test_headless_default_resolves_from_config_to_bounded(tmp_path, monkeypatch):
    """headless_yolo=None reads config.agents.codex.headless_yolo. Default
    config (no agents block) -> BOUNDED."""
    _hermetic_config(tmp_path, monkeypatch)
    eff = codex_mod._effective_yolo(yolo=False, headless_yolo=None)
    assert codex_mod.sandbox_flag(eff) == _BOUNDED


def test_headless_config_full_yolo_resolves_to_bypass(tmp_path, monkeypatch):
    _hermetic_config(
        tmp_path,
        monkeypatch,
        "schema_version: 1\nconfig:\n  agents:\n    codex:\n      headless_yolo: true\n",
    )
    eff = codex_mod._effective_yolo(yolo=False, headless_yolo=None)
    assert codex_mod.sandbox_flag(eff) == _FULL_YOLO


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


def test_sandbox_flag_resume_default_is_empty():
    # `codex exec resume` takes no --sandbox flag, so the bounded case emits
    # nothing HERE and re-pins its posture through sandbox_config_args_resume.
    assert codex_mod.sandbox_flag_resume(yolo=False) == []


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


def test_sandbox_config_args_resume_pins_mode_and_both_writable_roots(
    tmp_path, monkeypatch
):
    import json
    import pathlib

    repo = _init_repo(tmp_path / "repo")
    plan_dir = tmp_path / "plans"
    monkeypatch.setattr(codex_mod, "_resolve_plan_dir", lambda cwd: str(plan_dir))
    out = codex_mod.sandbox_config_args_resume(repo)

    assert out[0] == "-c"
    assert out[1] == "sandbox_mode=workspace-write"
    assert out[2] == "-c"
    key, _, value = out[3].partition("=")
    assert key == "sandbox_workspace_write.writable_roots"
    assert [pathlib.Path(p).resolve() for p in json.loads(value)] == [
        (repo / ".git").resolve(),
        plan_dir.resolve(),
    ]


def test_sandbox_config_args_resume_empty_when_no_roots_resolve(
    tmp_path, monkeypatch
):
    """A posture we cannot resolve must never break the resume."""
    outside = tmp_path / "plain"
    outside.mkdir()
    monkeypatch.setattr(codex_mod, "_resolve_plan_dir", lambda cwd: "")
    assert codex_mod.sandbox_config_args_resume(outside) == []
