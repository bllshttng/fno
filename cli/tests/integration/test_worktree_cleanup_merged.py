"""Integration tests for `worktree cleanup --merged` (node x-2380).

Drives the real `scripts/lib/worktree-lifecycle.sh` + `scripts/setup/archive-worktree.sh`
against a throwaway git repo with a bare `origin`, so no real worktree is touched.
The two scripts are copied into the fixture side by side, so the sweep's
plugin-relative `../setup/archive-worktree.sh` path resolves to the real code.
"""
from __future__ import annotations

import os
import json
import shutil
import subprocess
import time
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parents[3]
LIFECYCLE_SRC = REPO_ROOT / "scripts" / "lib" / "worktree-lifecycle.sh"
LIFECYCLE_COMPAT_SRC = REPO_ROOT / "scripts" / "worktree-lifecycle.sh"
UNPUSHED_SRC = REPO_ROOT / "scripts" / "lib" / "worktree-unpushed.sh"
REAPABLE_SRC = REPO_ROOT / "scripts" / "lib" / "worktree-reapable.sh"
ARCHIVE_SRC = REPO_ROOT / "scripts" / "setup" / "archive-worktree.sh"
TARGET_GUARD_SRC = REPO_ROOT / "scripts" / "lib" / "target-guard.sh"
REMOVAL_EVENT_SRC = REPO_ROOT / "scripts" / "lib" / "worktree-removal-event.sh"


def _gate_binary() -> Path | None:
    """The gate binary under test: override, then this checkout's cargo build.

    The classifier is Rust since x-7b9c (the Python leg is deleted), so every
    test that wires the real lib needs a binary. cli-ci builds none; those
    tests skip there and run wherever one is built.
    """
    env_bin = os.environ.get("FNO_AGENTS_BIN")
    if env_bin and Path(env_bin).is_file():
        return Path(env_bin)
    for cand in (
        REPO_ROOT / "crates" / "fno-agents" / "target" / "release" / "fno-agents",
        REPO_ROOT / "target" / "release" / "fno-agents",
        REPO_ROOT / "crates" / "fno-agents" / "target" / "debug" / "fno-agents",
        REPO_ROOT / "target" / "debug" / "fno-agents",
    ):
        if cand.is_file():
            return cand
    return None


GATE_BIN = _gate_binary()
requires_gate = pytest.mark.skipif(
    GATE_BIN is None, reason="no fno-agents gate binary built (cargo build -p fno-agents)"
)


def _git(cwd: Path, *args: str, check: bool = True) -> subprocess.CompletedProcess:
    return subprocess.run(["git", "-C", str(cwd), *args], check=check, capture_output=True, text=True)


def _commit(wt: Path, name: str, body: str = "x") -> None:
    (wt / name).write_text(body)
    _git(wt, "add", name)
    _git(wt, "commit", "-m", f"add {name}")


@pytest.fixture
def repo(tmp_path: Path) -> Path:
    """A canonical checkout with an `origin` bare remote and a `main` branch.

    `.fno/` is gitignored (as in the real repo) so the symlink family never
    counts as a dirty tree.
    """
    origin = tmp_path / "origin.git"
    canon = tmp_path / "canon"
    subprocess.run(["git", "init", "--bare", "-b", "main", str(origin)], check=True, capture_output=True)
    subprocess.run(["git", "init", "-b", "main", str(canon)], check=True, capture_output=True)
    _git(canon, "config", "user.email", "t@t.com")
    _git(canon, "config", "user.name", "T")
    (canon / ".gitignore").write_text(".fno/\n")
    (canon / "README.md").write_text("# repo\n")
    _git(canon, "add", ".gitignore", "README.md")
    _git(canon, "commit", "-m", "init")
    _git(canon, "remote", "add", "origin", str(origin))
    _git(canon, "push", "-u", "origin", "main")
    _git(canon, "remote", "set-head", "origin", "main")
    # Vendor the scripts under test into the fixture so ARCHIVE path resolves.
    (canon / "scripts" / "lib").mkdir(parents=True)
    (canon / "scripts" / "setup").mkdir(parents=True)
    shutil.copy2(LIFECYCLE_SRC, canon / "scripts" / "lib" / "worktree-lifecycle.sh")
    shutil.copy2(LIFECYCLE_COMPAT_SRC, canon / "scripts" / "worktree-lifecycle.sh")
    shutil.copy2(UNPUSHED_SRC, canon / "scripts" / "lib" / "worktree-unpushed.sh")
    shutil.copy2(TARGET_GUARD_SRC, canon / "scripts" / "lib" / "target-guard.sh")
    shutil.copy2(REMOVAL_EVENT_SRC, canon / "scripts" / "lib" / "worktree-removal-event.sh")
    shutil.copy2(ARCHIVE_SRC, canon / "scripts" / "setup" / "archive-worktree.sh")
    return canon


def _sweep(
    canon: Path,
    *flags: str,
    env_extra: dict[str, str] | None = None,
    script_root: Path | None = None,
) -> subprocess.CompletedProcess:
    script = (script_root or canon) / "scripts" / "lib" / "worktree-lifecycle.sh"
    env = os.environ.copy()
    if env_extra:
        env.update(env_extra)
    return subprocess.run(
        ["bash", str(script), "cleanup", "--merged", *flags],
        cwd=str(canon), capture_output=True, text=True, env=env,
    )


def _compat_age_sweep(canon: Path, *flags: str) -> subprocess.CompletedProcess:
    """The compatibility entrypoint execs the lib, so it drives the lib too."""
    script = canon / "scripts" / "worktree-lifecycle.sh"
    return subprocess.run(
        ["bash", str(script), "cleanup", "--older-than", "0d", *flags],
        cwd=str(canon), capture_output=True, text=True,
    )


def _archive(canon: Path, *args: str) -> subprocess.CompletedProcess:
    script = canon / "scripts" / "setup" / "archive-worktree.sh"
    return subprocess.run(
        ["bash", str(script), *args],
        cwd=str(canon), capture_output=True, text=True, stdin=subprocess.DEVNULL,
    )


def _cargo_sweep(canon: Path, *flags: str, env_extra: dict[str, str] | None = None) -> subprocess.CompletedProcess:
    script = canon / "scripts" / "lib" / "worktree-lifecycle.sh"
    env = os.environ.copy()
    if env_extra:
        env.update(env_extra)
    return subprocess.run(
        ["bash", str(script), "cleanup", "--cargo-targets", *flags],
        cwd=str(canon), capture_output=True, text=True, env=env,
    )


def _add_target(canon: Path, name: str, size: int, *, old: bool = False) -> Path:
    wt = canon / name
    _git(canon, "worktree", "add", str(wt), "-b", f"feature/{name}", "main")
    target = wt / "crates" / "fixture" / "target"
    target.mkdir(parents=True)
    (target / "artifact.bin").write_bytes(b"x" * size)
    if old:
        old_ts = 1_600_000_000
        os.utime(target / "artifact.bin", (old_ts, old_ts))
        os.utime(target, (old_ts, old_ts))
    return target


def test_cargo_target_apply_reaps_age_then_cap_and_is_idempotent(repo: Path):
    old = _add_target(repo, "cargo-old", 2 * 1024 * 1024, old=True)
    young = _add_target(repo, "cargo-young", 2 * 1024 * 1024)
    bounds = ("--cap-bytes", str(3 * 1024 * 1024), "--target-max-age", "7d")

    vetoed = _cargo_sweep(repo, *bounds, "--apply", "--dry-run")
    assert vetoed.returncode == 0, vetoed.stderr
    assert "mode=dry-run" in vetoed.stdout
    assert "would-reap" in vetoed.stdout
    assert old.exists() and young.exists(), "explicit dry-run must override apply"

    first = _cargo_sweep(repo, *bounds, "--apply")
    second = _cargo_sweep(repo, *bounds, "--apply")

    assert first.returncode == 0, first.stderr
    assert "mode=apply" in first.stdout
    assert "reason=age" in first.stdout
    assert "after_bytes=" in first.stdout
    assert "cap_bytes=3145728" in first.stdout
    assert not old.exists(), "over-age target must be reaped even when one deletion satisfies cap"
    assert young.exists(), "young target should remain once the cap is satisfied"
    assert second.returncode == 0, second.stderr
    assert "reaped=0" in second.stdout
    assert "reclaimed_bytes=0" in second.stdout


def _add_merged(canon: Path, name: str) -> Path:
    """Worktree whose branch is merged into origin/main (a reap candidate)."""
    wt = canon / name
    _git(canon, "worktree", "add", str(wt), "-b", f"feature/{name}", "main")
    _commit(wt, f"{name}.txt")
    _git(canon, "merge", "--no-ff", f"feature/{name}", "-m", f"merge {name}")
    _git(canon, "push", "origin", "main")
    return wt


def _add_merged_at(canon: Path, wt: Path, name: str) -> Path:
    wt.parent.mkdir(parents=True, exist_ok=True)
    _git(canon, "worktree", "add", str(wt), "-b", f"feature/{name}", "main")
    _commit(wt, f"{name}.txt")
    _git(canon, "merge", "--no-ff", f"feature/{name}", "-m", f"merge {name}")
    _git(canon, "push", "origin", "main")
    return wt


def _add_merged_with_deleted_upstream(canon: Path, name: str) -> Path:
    """The real post-merge shape: the branch tracked a remote branch, the merge
    landed in main, and the remote branch was deleted (GitHub does this on
    merge), so `fetch --prune` drops the ref while the branch config keeps
    naming it."""
    wt = canon / name
    _git(canon, "worktree", "add", str(wt), "-b", f"feature/{name}", "main")
    _commit(wt, f"{name}.txt")
    _git(wt, "push", "-u", "origin", f"feature/{name}")
    _git(canon, "merge", "--no-ff", f"feature/{name}", "-m", f"merge {name}")
    _git(canon, "push", "origin", "main")
    _git(canon, "push", "origin", "--delete", f"feature/{name}")
    _git(canon, "fetch", "--prune", "origin")
    return wt


def _add_detached(repo: Path, wt: Path, at: str = "main") -> Path:
    wt.parent.mkdir(parents=True, exist_ok=True)
    _git(repo, "worktree", "add", "--detach", str(wt), at)
    return wt


def _origin_bare(repo: Path) -> Path:
    return Path(_git(repo, "remote", "get-url", "origin").stdout.strip())


def _detached_at_deleted_remote_branch(repo: Path, name: str) -> Path:
    """A tree detached at a commit whose remote branch is GONE on the server:
    the local refs/remotes entry still points at the commit, so only a pruned
    refresh sees that no remote carries it anymore."""
    src = repo / f"{name}-src"
    _git(repo, "worktree", "add", str(src), "-b", f"feature/{name}", "main")
    _commit(src, f"{name}.txt")
    _git(src, "push", "origin", f"feature/{name}")
    sha = _git(src, "rev-parse", "HEAD").stdout.strip()
    _git(repo, "worktree", "remove", str(src))
    _git(_origin_bare(repo), "branch", "-D", f"feature/{name}")
    return _add_detached(repo, repo / f"wt-scratch-{name}", at=sha)


def test_dry_run_is_the_default_and_vetoes_apply(repo: Path):
    wt = _add_merged(repo, "reapme")
    before = _git(repo, "worktree", "list").stdout

    for flags in ((), ("--apply", "--dry-run")):
        r = _sweep(repo, *flags)
        assert r.returncode == 0, r.stderr
        assert "would-archive" in r.stdout
        assert "dry-run" in r.stdout
        assert wt.exists(), f"sweep {flags} removed the worktree"
    assert _git(repo, "worktree", "list").stdout == before


def test_apply_reaps_merged_in_prefix_and_preserves_branch(repo: Path, tmp_path: Path):
    # The in-scope tree has the shape every merged PR leaves: its upstream
    # was pruned. `@{u}` then prints literally, and an earlier caller read
    # that as "unpushed state not verifiable" and refused.
    in_scope = _add_merged_with_deleted_upstream(repo, "in-pruned")
    assert _git(in_scope, "rev-parse", "--verify", "--quiet", "@{u}", check=False).returncode != 0
    out_of_scope = _add_merged(repo, "out-merged")
    # A project other than footnote ships no scripts/: the sweep must run the
    # archive helper from its own install, never from the swept repo.
    plugin = tmp_path / "plugin"
    plugin.mkdir()
    shutil.move(str(repo / "scripts"), str(plugin / "scripts"))

    r = _sweep(repo, "--apply", "--prefix", "feature/in", script_root=plugin)
    diag = f"\n--- stdout ---\n{r.stdout}\n--- stderr ---\n{r.stderr}"

    assert r.returncode == 0, diag
    assert "1 archived" in r.stdout, diag
    assert "not verifiable" not in r.stderr, diag
    assert not in_scope.exists(), "worktree dir should be gone" + diag
    branches = _git(repo, "branch", "--list", "feature/in-pruned").stdout
    assert "feature/in-pruned" in branches, "branch must be preserved"
    assert out_of_scope.exists(), "out-of-prefix worktree must be untouched"


def test_merge_triggered_archive_refuses_pushed_but_unmerged_head(repo: Path):
    wt = repo / "wt-unmerged-archive"
    _git(repo, "worktree", "add", str(wt), "-b", "feature/unmerged-archive", "main")
    _commit(wt, "unmerged.txt")
    _git(wt, "push", "-u", "origin", "feature/unmerged-archive")

    result = _archive(repo, str(wt), "--merge-triggered")

    assert result.returncode == 2, result.stderr
    assert "origin/main" in result.stderr
    assert wt.exists(), "merge-triggered cleanup must keep an unmerged worktree"


# ── x-60de: every removal emits one attributable event row ─────────────────
def test_apply_removal_emits_event_row(repo: Path, tmp_path: Path, monkeypatch):
    """A removal without a row is unattributable: live trees lost on
    2026-08-25 and 2026-08-29 left no evidence because no removal path
    emitted anything. The row must name the path, the caller, the claim
    state read at decision time, and the reason - and reach the
    machine-global journal, the instrument that walked 2239 rows and found
    zero removals."""
    venv_bin = REPO_ROOT / "cli" / ".venv" / "bin"
    if not (venv_bin / "fno-py").exists():
        pytest.skip("cli venv absent (run fno doctor test once); emit falls back to deployed fno")
    monkeypatch.setenv("PATH", f"{venv_bin}{os.pathsep}{os.environ['PATH']}")
    from fno.events.store_client import resolve_native_bin

    monkeypatch.setenv("FNO_BIN", resolve_native_bin())
    # Sandbox the machine-global mirror through a point-config: the global
    # journal follows config state_dir, and FNO_CONFIG redirects config
    # loading for the emit subprocess.
    sandbox_state = tmp_path / "fno-state"
    sandbox_state.mkdir()
    sandbox_cfg = tmp_path / "fno-config.toml"
    sandbox_cfg.write_text(f'state_dir = "{sandbox_state}"\n')
    monkeypatch.setenv("FNO_CONFIG", str(sandbox_cfg))

    wt = _add_merged(repo, "reapme")

    r = _sweep(repo, "--apply")
    diag = f"\n--- stdout ---\n{r.stdout}\n--- stderr ---\n{r.stderr}"

    assert r.returncode == 0, diag
    assert not wt.exists(), diag

    def _rows(path: Path) -> list[dict]:
        from tests._event_rows import event_rows

        rows = event_rows(path)
        assert rows, f"no committed rows beside journal: {path}"
        return rows

    # The emit resolves the repo's space journal (the accessor the writers
    # use); the checkout copy is retired.
    from fno.paths import project_log

    project_rows = _rows(project_log("events.jsonl", project_root=repo))
    hits = [row for row in project_rows if row.get("type") == "worktree_removed"]
    assert hits, f"removal emitted no row; project log types: {[r.get('type') for r in project_rows]}"
    data = hits[-1]["data"]
    assert data["path"] == str(wt), diag
    assert data["caller"] == "cleanup --merged", diag
    assert data["claim"], "the row must carry the claim state read at decision time"
    assert data["reason"], "the row must carry why the tree was judged safe"
    assert data["branch"] == "feature/reapme", diag
    assert data["reclaimed_bytes"] > 0, diag

    global_rows = _rows(sandbox_state / "events.jsonl")
    assert any(row.get("type") == "worktree_removed" for row in global_rows), \
        "worktree_removed is a GLOBAL_MIRROR_TYPES row: it must reach the machine-global journal"


def _row_for(stdout: str, wt: Path) -> str:
    rows = [line for line in stdout.splitlines() if str(wt) in line]
    assert rows, f"no sweep row names {wt}:\n{stdout}"
    return rows[-1]


# ── every keep-reason holds in one sweep, none removed ──────────────────────
def test_keep_reasons(repo: Path, tmp_path: Path):
    # dirty: merged tip but an untracked file
    dirty = repo / "wt-dirty"
    _git(repo, "worktree", "add", str(dirty), "-b", "feature/dirty", "main")
    (dirty / "scratch.txt").write_text("uncommitted")

    # unmerged: pushed to its own remote branch, not in main
    unmerged = repo / "wt-unmerged"
    _git(repo, "worktree", "add", str(unmerged), "-b", "feature/unmerged", "main")
    _commit(unmerged, "u.txt")
    _git(unmerged, "push", "-u", "origin", "feature/unmerged")

    # unpushed: local commit, no upstream
    unpushed = repo / "wt-unpushed"
    _git(repo, "worktree", "add", str(unpushed), "-b", "feature/unpushed", "main")
    _commit(unpushed, "p.txt")

    # live: merged tip but a live owner_pid in the manifest
    live = _add_merged(repo, "wt-live")
    (live / ".fno").mkdir()
    (live / ".fno" / "target-state.md").write_text(
        f"owner_pid: {os.getpid()}\ngraph_node_id: x-live\n"
    )

    # detached with local commits: no branch holds them
    detached_local = _add_detached(repo, repo / "wt-scratch-unpushed")
    _commit(detached_local, "d.txt")

    # detached at a commit whose remote branch was deleted: judged against
    # the stale tracking ref the count reads 0, so only a pruned refresh keeps it
    detached_gone = _detached_at_deleted_remote_branch(repo, "gone")

    # merged, upstream pruned, then work after the merge: clearing the pruned
    # NAME must not clear the strictness it carried
    pruned_ahead = _add_merged_with_deleted_upstream(repo, "pruned-ahead")
    _commit(pruned_ahead, "after-merge.txt")

    # app-owned: a Codex Desktop tree is its chat's, never the sweep's
    codex_home = tmp_path / ".codex"
    app_owned = _add_merged_at(repo, codex_home / "worktrees" / "thread-b" / repo.name, "native")

    # permanent: scripts/ci/preflight.sh pins and hard-resets this tree
    preflight = _add_detached(repo, repo / ".claude" / "worktrees" / "preflight")

    r = _sweep(repo, "--apply", env_extra={"CODEX_HOME": str(codex_home)})
    diag = f"\n--- stdout ---\n{r.stdout}\n--- stderr ---\n{r.stderr}"

    assert r.returncode == 0, diag
    assert "kept (dirty)" in _row_for(r.stdout, dirty), diag
    assert "kept (unmerged)" in _row_for(r.stdout, unmerged), diag
    assert "kept (unpushed)" in _row_for(r.stdout, unpushed), diag
    assert "kept (live-session)" in _row_for(r.stdout, live), diag
    assert "kept (unpushed)" in _row_for(r.stdout, detached_local), diag
    assert "kept (unpushed)" in _row_for(r.stdout, detached_gone), diag
    assert "kept (app-owned)" in _row_for(r.stdout, app_owned), diag
    assert "kept (permanent)" in _row_for(r.stdout, preflight), diag
    assert "0 archived" in r.stdout, diag
    for wt in (
        dirty, unmerged, unpushed, live, detached_local, detached_gone,
        pruned_ahead, app_owned, preflight,
    ):
        assert wt.exists(), f"{wt} must not be removed" + diag


# ── AC1-ERR: fetch failure aborts loudly, nothing removed ───────────────────
def test_fetch_failure_aborts(repo: Path):
    wt = _add_merged(repo, "reapme")
    _git(repo, "remote", "set-url", "origin", str(repo / "does-not-exist.git"))

    r = _sweep(repo, "--apply")

    assert r.returncode != 0
    assert "aborting" in r.stderr
    assert wt.exists(), "no worktree may be removed after a fetch abort"


# ── AC2-EDGE: local-only .fno do state survives the reap ───────────────────────
def test_salvage_preserves_local_state(repo: Path):
    (repo / ".fno").mkdir()
    (repo / ".fno" / "config.toml").write_text("canonical\n")

    wt = _add_merged(repo, "reapme")
    fno = wt / ".fno"
    fno.mkdir()
    (fno / "target-state.md").write_text("graph_node_id: x-salv\nowner_pid: 999999\n")
    (fno / "events.jsonl").write_text("EVT\n")
    (fno / "scratchpad").mkdir()
    (fno / "scratchpad" / "note.md").write_text("SCRATCH\n")
    # a symlink into canonical — must NOT be salvaged
    (fno / "config.toml").symlink_to(repo / ".fno" / "config.toml")

    r = _sweep(repo, "--apply")
    diag = f"\n--- stdout ---\n{r.stdout}\n--- stderr ---\n{r.stderr}"
    assert r.returncode == 0, diag
    assert "1 archived" in r.stdout, diag
    assert not wt.exists(), diag

    canon_fno = repo / ".fno"
    # loose file -> salvage/<date>-<node>/events.jsonl
    evt = list(canon_fno.glob("salvage/*-x-salv/events.jsonl"))
    assert evt and evt[0].read_text() == "EVT\n", "events.jsonl not salvaged"
    # directory -> scratchpad/<date>-<node>/note.md
    note = list(canon_fno.glob("scratchpad/*-x-salv/note.md"))
    assert note and note[0].read_text() == "SCRATCH\n", "scratchpad/ not salvaged"
    # the symlinked config.toml must not have been copied into a date-node dir
    assert not list(canon_fno.glob("config.toml/*")), "symlink was wrongly salvaged"


# ── AC2-FR: salvage failure blocks removal ──────────────────────────────────
def test_salvage_failure_keeps_worktree(repo: Path):
    canon_fno = repo / ".fno"
    canon_fno.mkdir()
    wt = _add_merged(repo, "reapme")
    fno = wt / ".fno"
    fno.mkdir()
    (fno / "events.jsonl").write_text("EVT\n")
    os.chmod(canon_fno, 0o500)  # unwritable canonical .fno
    try:
        r = _sweep(repo, "--apply")
        assert r.returncode == 0, r.stderr
        assert "salvage-failed" in r.stdout
        assert wt.exists(), "worktree must be kept when salvage fails"
    finally:
        os.chmod(canon_fno, 0o700)


def test_archive_force_refreshes_upstream_before_disclosure(repo: Path):
    wt = repo / "wt-force-stale-upstream"
    branch = "feature/force-stale-upstream"
    _git(repo, "worktree", "add", str(wt), "-b", branch, "main")
    _commit(wt, "unique.txt")
    unique_subject = "add unique.txt"
    short_sha = _git(wt, "rev-parse", "--short", "HEAD").stdout.strip()
    _git(wt, "push", "-u", "origin", branch)
    _git(_origin_bare(repo), "branch", "-f", branch, "main")

    r = _archive(repo, "--force", "--yes", "--delete-branch", str(wt))
    diag = f"stdout={r.stdout}\nstderr={r.stderr}"

    assert r.returncode == 0, diag
    assert "FORCE" in diag
    assert short_sha in diag
    assert unique_subject in diag
    assert "branch feature/force-stale-upstream deleted" in r.stderr
    assert not wt.exists(), diag
    assert not _git(repo, "branch", "--list", branch).stdout.strip()


def test_archive_delete_branch_refuses_unverifiable_remote_state(repo: Path):
    wt = repo / "wt-delete-unverifiable"
    branch = "feature/delete-unverifiable"
    _git(repo, "worktree", "add", str(wt), "-b", branch, "main")
    _commit(wt, "unique.txt")
    unique_sha = _git(wt, "rev-parse", "--short", "HEAD").stdout.strip()
    _git(repo, "remote", "remove", "origin")

    r = _archive(repo, "--yes", "--delete-branch", str(wt))
    diag = f"stdout={r.stdout}\nstderr={r.stderr}"

    assert r.returncode == 2, diag
    assert "not verifiable" in r.stderr, diag
    assert wt.exists(), diag
    assert _git(repo, "rev-parse", "--short", branch).stdout.strip() == unique_sha


def test_archive_refuses_unreadable_live_claim(
    repo: Path, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
):
    wt = _add_merged(repo, "unreadable-claim")
    fno_bin = tmp_path / "bin"
    fno_bin.mkdir()
    fake_fno = fno_bin / "fno"
    fake_fno.write_text("#!/bin/sh\nexit 1\n")
    fake_fno.chmod(0o755)
    monkeypatch.setenv("PATH", f"{fno_bin}:{os.environ['PATH']}")
    state = wt / ".fno"
    state.mkdir()
    (state / "target-state.md").write_text(
        "input: x-unreadable\ntarget_claim_key: node:x-unreadable\nowner_pid: 999999\n"
    )

    r = _archive(repo, "--force", "--yes", str(wt))

    assert r.returncode == 2
    assert "live target claim not verifiable" in r.stderr
    assert wt.exists()


def test_archive_force_rechecks_state_after_process_cleanup(repo: Path):
    wt = _add_merged(repo, "force-recheck")
    late_path = wt / "late.txt"
    holder_env = os.environ.copy()
    holder_env["LATE_PATH"] = str(late_path)
    holder = subprocess.Popen(
        [
            "bash",
            "-c",
            f"cd '{wt}'; trap 'printf late > \"$LATE_PATH\"; exit 0' TERM; while :; do sleep 1; done",
        ],
        cwd=repo,
        env=holder_env,
        start_new_session=True,
    )
    try:
        r = _archive(repo, "--force", "--yes", str(wt))
    finally:
        if holder.poll() is None:
            holder.terminate()
        holder.wait(timeout=5)

    diag = f"stdout={r.stdout}\nstderr={r.stderr}"
    assert r.returncode == 2, diag
    assert "forced state changed after disclosure" in r.stderr, diag
    assert (wt / "late.txt").exists(), diag
    assert wt.exists(), diag


def test_archive_refuses_codex_app_owned_worktree_even_with_force(
    repo: Path, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
):
    codex_home = tmp_path / ".codex"
    wt = codex_home / "worktrees" / "thread-a" / repo.name
    wt.parent.mkdir(parents=True)
    _git(repo, "worktree", "add", "--detach", str(wt), "main")
    monkeypatch.setenv("CODEX_HOME", str(codex_home))

    r = _archive(repo, "--force", str(wt))

    assert r.returncode == 6
    assert "app-owned Codex worktree" in r.stderr
    assert "archive its associated chat" in r.stderr
    assert wt.exists()


def test_age_sweep_apply_removes_only_the_clean_detached_tree(
    repo: Path, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
):
    """The age sweep force-removes under --apply, so each guard below is the
    only thing between it and lost work. A detached tree has no preserved
    branch to fall back on; DIRTY is never touched by any automatic path,
    branched or detached. The in-flight eval tree rides the uncommitted-work
    guard via its untracked marker file (cli/src/fno/evals/runner.py)."""
    codex_home = tmp_path / ".codex"
    app_owned = codex_home / "worktrees" / "thread-c" / repo.name
    app_owned.parent.mkdir(parents=True)
    _git(repo, "worktree", "add", "--detach", str(app_owned), "main")
    monkeypatch.setenv("CODEX_HOME", str(codex_home))

    unpushed = _add_detached(repo, repo / "wt-age-unpushed")
    _commit(unpushed, "age.txt")

    dirty = repo / "wt-age-branched-dirty"
    _git(repo, "worktree", "add", str(dirty), "-b", "feature/wt-age-branched-dirty", "main")
    _commit(dirty, "branched.txt")
    (dirty / "dirty.txt").write_text("uncommitted\n")

    clean = _add_detached(repo, repo / "wt-age-clean")

    r = _compat_age_sweep(repo, "--apply")

    assert r.returncode == 0, r.stderr
    assert f"SKIP: {app_owned} (app-owned Codex worktree)" in r.stdout
    assert f"SKIP: {unpushed} (detached HEAD holds unpushed commits)" in r.stdout
    assert f"SKIP: {dirty} (holds uncommitted work: " in r.stdout
    assert "REMOVED" in r.stdout
    assert not clean.exists(), r.stdout
    assert app_owned.exists() and unpushed.exists()
    assert (dirty / "dirty.txt").exists()


def test_age_sweep_is_dry_run_by_default(repo: Path):
    """Both removal modes share one default: a bare sweep reports, --apply
    executes. The age mode used to remove on a bare call - a lead ran it
    expecting a preview and 13 trees went."""
    wt = _add_detached(repo, repo / "wt-age-default")

    r = _compat_age_sweep(repo)

    assert r.returncode == 0, r.stderr
    assert "WOULD REMOVE" in r.stdout
    assert "dry-run" in r.stdout
    assert wt.exists(), "a bare age sweep must not remove anything"


# ── detached scratch trees: classified by content, not branch name ──────────
def test_apply_reaps_detached_scratch_tree(repo: Path):
    """A scratch tree is detached BY CONSTRUCTION; a head that some remote
    already carries is recreatable, so the sweep must reap it instead of
    keeping it on the branch-name proxy."""
    wt = _add_detached(repo, repo / "wt-scratch-reapme")

    r = _sweep(repo, "--apply")
    diag = f"\n--- stdout ---\n{r.stdout}\n--- stderr ---\n{r.stderr}"

    assert r.returncode == 0, diag
    assert "1 archived" in r.stdout, diag
    assert not wt.exists(), "clean detached tree with nothing unpushed should be archived" + diag


def test_archive_refuses_a_detached_head_no_remote_is_proven_to_carry(repo: Path):
    """The removal-time gate must agree with the sweep, or a tree the sweep
    keeps can still be archived by hand. No network, no verdict: refs that
    cannot be refreshed may be stale, so they never license a removal."""
    local = _add_detached(repo, repo / "wt-scratch-local")
    _commit(local, "l.txt")
    r = _archive(repo, str(local))
    assert r.returncode == 2, f"stdout={r.stdout}\nstderr={r.stderr}"
    assert "detached HEAD not on any remote" in r.stderr
    assert local.exists()

    gone = _detached_at_deleted_remote_branch(repo, "gone2")
    r = _archive(repo, str(gone))
    assert r.returncode == 2, f"stdout={r.stdout}\nstderr={r.stderr}"
    assert gone.exists(), f"stderr={r.stderr}"

    unverifiable = _add_detached(repo, repo / "wt-scratch-unverifiable")
    _git(repo, "remote", "set-url", "origin", str(repo / "unreachable.git"))
    r = _archive(repo, str(unverifiable))
    assert r.returncode == 2, f"stdout={r.stdout}\nstderr={r.stderr}"
    assert "not verifiable" in r.stderr, f"stderr={r.stderr}"
    assert unverifiable.exists(), f"stderr={r.stderr}"


def test_dead_secondary_remote_degrades_instead_of_aborting(repo: Path):
    """fetch --all fails if ANY remote is dead, and one stale account remote
    must not brick the reclaim command. Origin stays the merged-check baseline
    (branched judging continues); the detached tree is kept because its refs
    cannot be verified against the dead remote - with a receipt that says so,
    not a phantom unpushed count."""
    merged = _add_merged(repo, "reapme")
    det = _add_detached(repo, repo / "wt-scratch-fork")
    _git(repo, "remote", "add", "fork", str(repo / "dead-fork.git"))

    r = _sweep(repo, "--apply")
    diag = f"\n--- stdout ---\n{r.stdout}\n--- stderr ---\n{r.stderr}"

    assert r.returncode == 0, diag
    assert "1 archived" in r.stdout, diag
    assert not merged.exists(), diag
    assert "remote refs unverifiable" in r.stdout, diag
    assert det.exists(), "refs unverifiable against a dead remote must keep detached trees" + diag


def test_non_origin_single_remote_detached_still_archives(repo: Path):
    """The refresh verifies whichever remotes exist. A repo whose only remote
    is named anything but origin must still archive a detached head that the
    remote carries; hardcoding origin made every such repo read as
    permanently unverifiable."""
    _git(repo, "remote", "rename", "origin", "upstream")
    det = _add_detached(repo, repo / "wt-scratch-upstream")

    r = _archive(repo, str(det))

    assert r.returncode == 0, f"stdout={r.stdout}\nstderr={r.stderr}"
    assert not det.exists(), f"stderr={r.stderr}"


# ── x-11d8: setup's own symlinks must not stop the sweep ────────────────────
#
# `wt_reapable` discounts them, but `git worktree remove` still refuses on
# untracked content. Without --force the sweep spends salvage and the process
# sweep and THEN fails, so this drives the whole path end to end.


def _wire_real_reapable(canon: Path) -> None:
    """Give the fixture the real classifier, not the lifecycle stub.

    The classifier is the fno-agents gate binary since x-7b9c (the Python leg
    the old venv arm ran is deleted). The vendored lib execs it; the env
    override points the fixture copy at THIS checkout's build, because the
    copy anchors its own search under the fixture root, which has none.
    """
    if GATE_BIN is None:
        pytest.skip("no fno-agents gate binary; build one (cargo build -p fno-agents)")
    # Named, because without it wt_reapable falls through to whatever
    # `fno-agents` is on PATH or installed, which may predate the command and
    # answers nothing. The test would then fail as "kept (dirty)" and read as
    # a code defect.
    os.environ["FNO_AGENTS_BIN"] = str(GATE_BIN)
    shutil.copy2(REAPABLE_SRC, canon / "scripts" / "lib" / "worktree-reapable.sh")


def _graph_home(tmp_path: Path, rows: list[dict]) -> dict[str, str]:
    """A throwaway graph store the gate's node reader accepts.

    The working graph is absent (an empty graph), so the rows land in the
    advisory archive the same reader folds in - exactly where done nodes
    live once their files retire.
    """
    home = tmp_path / "graph-home" / "agents"
    home.mkdir(parents=True, exist_ok=True)
    # The fold imports through the typed model: a row it cannot represent
    # never lands in the store, so each seed carries the fields it requires.
    full = [
        {
            "id": row["id"],
            "slug": row["id"],
            "title": f"node {row['id']}",
            "type": "feature",
            "status": row["status"],
            "priority": "p2",
        }
        for row in rows
    ]
    (home.parent / "graph-archive.json").write_text(json.dumps({"entries": full}))
    return {"FNO_AGENTS_HOME": str(home)}


def _add_done_node_wt(canon: Path, name: str, node_id: str) -> Path:
    """A linked tree whose manifest names `node_id`, aged past the 30m grace,
    holding one untracked file the sweep must salvage."""
    wt = canon / name
    _git(canon, "worktree", "add", str(wt), "-b", f"feature/{name}", "main")
    (wt / ".fno").mkdir()
    (wt / ".fno" / "target-state.md").write_text(f"graph_node_id: {node_id}\n")
    (wt / "scratch.txt").write_text("salvage me\n")
    old = time.time() - 49 * 3600
    os.utime(wt / ".git", (old, old))
    return wt


def test_apply_reaps_a_tree_whose_only_dirt_is_setup_symlinks(repo: Path):
    _wire_real_reapable(repo)
    wt = _add_merged(repo, "setuplinks")
    (repo / ".agents").mkdir(exist_ok=True)
    (repo / ".claude").mkdir(exist_ok=True)
    (repo / ".claude" / "skills").mkdir(exist_ok=True)
    (wt / "cli" / ".claude").mkdir(parents=True)
    (wt / "cli" / ".agents").symlink_to(repo / ".agents")
    (wt / "cli" / ".claude" / "skills").symlink_to(repo / ".claude" / "skills")
    assert _git(wt, "status", "--porcelain").stdout.strip(), "the tree must read dirty"

    r = _sweep(repo, "--apply")
    diag = f"\n--- stdout ---\n{r.stdout}\n--- stderr ---\n{r.stderr}"

    assert r.returncode == 0, diag
    assert "1 archived" in r.stdout, diag
    assert not wt.exists(), "worktree dir should be gone" + diag
    assert (repo / ".claude" / "skills").is_dir(), "the link target must survive"


# ── The done-node bucket (x-7b9c): salvage first, then remove, branch kept ──


@requires_gate
def test_done_node_apply_salvages_untracked_pins_detached_and_keeps_branch(
    repo: Path, tmp_path: Path
):
    """AC6-HP + AC7-HP: tree gone, branch at the same sha, untracked file
    salvaged, a detached commit no branch names pinned before removal, and
    the removal row names the done-node reason."""
    if not (REPO_ROOT / "cli" / ".venv" / "bin" / "fno-py").exists():
        pytest.skip("cli venv absent; the emit falls back to deployed fno")
    _wire_real_reapable(repo)
    env = _graph_home(
        tmp_path,
        [{"id": "x-d0ne1", "status": "done"}, {"id": "x-de7a1", "status": "done"}],
    )
    # The removal row mirrors to the machine-global journal, which follows
    # config state_dir: sandbox it the same way the event-row test does.
    sandbox_state = tmp_path / "fno-state"
    sandbox_state.mkdir()
    sandbox_cfg = tmp_path / "fno-config.toml"
    sandbox_cfg.write_text(f'state_dir = "{sandbox_state}"\n')
    env["FNO_CONFIG"] = str(sandbox_cfg)
    from fno.events.store_client import resolve_native_bin

    env["FNO_BIN"] = resolve_native_bin()
    wt = _add_done_node_wt(repo, "wt-done", "x-d0ne1")
    _commit(wt, "u.txt")  # unmerged commit: today's sweep keeps this tree
    sha = _git(wt, "rev-parse", "HEAD").stdout.strip()
    det = _add_done_node_wt(repo, "wt-det", "x-de7a1")
    _git(det, "checkout", "-q", "--detach")
    _commit(det, "d.txt")  # one commit on no branch and no remote
    det_sha = _git(det, "rev-parse", "HEAD").stdout.strip()

    r = _sweep(repo, "--apply", env_extra=env)
    diag = f"\n--- stdout ---\n{r.stdout}\n--- stderr ---\n{r.stderr}"

    assert r.returncode == 0, diag
    assert "archived" in r.stdout, diag
    assert not wt.exists(), "the done-node tree was not removed" + diag
    # The branch (with its unpushed commit) survives at the same sha.
    assert _git(repo, "rev-parse", "--verify", "feature/wt-done").stdout.strip() == sha
    salvaged = list((repo / ".fno" / "salvage").glob("*-x-d0ne1/untracked/scratch.txt"))
    assert salvaged, "the untracked file was not salvaged" + diag
    assert salvaged[0].read_text() == "salvage me\n"
    assert not det.exists(), "the detached done-node tree was not removed" + diag
    pinned = _git(repo, "rev-parse", "--verify", "salvage/wt-det").stdout.strip()
    assert pinned == det_sha, "the pinned branch does not hold the detached commit" + diag
    journal = sandbox_state / "events.jsonl"
    from tests._event_rows import event_rows

    rows = event_rows(journal)
    assert rows, f"no removal row was emitted; stderr had: {r.stderr[-400:]}"
    assert any(
        "done-node; tree removed, branch kept" in json.dumps(row) for row in rows
    ), "the removal row does not name the done-node reason"


@requires_gate
def test_done_node_salvage_failure_keeps_tree(repo: Path, tmp_path: Path):
    """AC6-ERR: an unwritable salvage destination keeps the worktree."""
    _wire_real_reapable(repo)
    env = _graph_home(tmp_path, [{"id": "x-sa1v1", "status": "done"}])
    wt = _add_done_node_wt(repo, "wt-salv", "x-sa1v1")
    (repo / ".fno").mkdir(exist_ok=True)
    os.chmod(repo / ".fno", 0o500)
    try:
        r = _sweep(repo, "--apply", env_extra=env)
        diag = f"\n--- stdout ---\n{r.stdout}\n--- stderr ---\n{r.stderr}"
        assert r.returncode == 0, diag
        assert "kept (salvage-failed)" in r.stdout, diag
        assert wt.exists(), "a salvage failure removed the tree" + diag
        assert (wt / "scratch.txt").exists(), "salvage lost the untracked file"
    finally:
        os.chmod(repo / ".fno", 0o755)


# ── worktree.prune_done=aggressive: the tracked diff goes to a ref ──────────
# The balanced verdict (keep a modified-tracked tree) is the classifier's,
# pinned by its Rust rows; only aggressive adds a bash salvage step.


@requires_gate
def test_aggressive_salvages_the_tracked_diff_to_a_ref_then_prunes(
    repo: Path, tmp_path: Path
):
    _wire_real_reapable(repo)
    env = _graph_home(tmp_path, [{"id": "x-4ggr1", "status": "done"}])
    cfg = tmp_path / "prune-aggressive.toml"
    cfg.write_text('[worktree]\nprune_done = "aggressive"\n')
    env["FNO_CONFIG"] = str(cfg)
    wt = _add_done_node_wt(repo, "wt-4gg", "x-4ggr1")
    _commit(wt, "tracked.txt", "original\n")
    (wt / "tracked.txt").write_text("edited\n")
    branch_sha = _git(wt, "rev-parse", "HEAD").stdout.strip()

    r = _sweep(repo, "--apply", env_extra=env)
    diag = f"\n--- stdout ---\n{r.stdout}\n--- stderr ---\n{r.stderr}"

    assert r.returncode == 0, diag
    assert "archived" in r.stdout, diag
    assert not wt.exists(), "aggressive did not prune the salvaged tree" + diag
    assert (
        _git(repo, "rev-parse", "--verify", "feature/wt-4gg").stdout.strip()
        == branch_sha
    ), "the branch did not survive" + diag
    ref = "refs/fno/salvage/x-4ggr1"
    assert _git(repo, "rev-parse", "--verify", ref, check=False).returncode == 0, (
        "no salvage ref after removal" + diag
    )
    salvaged = _git(repo, "show", f"{ref}:tracked.txt").stdout
    assert salvaged == "edited\n", f"the ref does not hold the diff: {salvaged}" + diag
    # The untracked salvage still runs under aggressive.
    untracked = list((repo / ".fno" / "salvage").glob("*-x-4ggr1/untracked/scratch.txt"))
    assert untracked, "the untracked file was not salvaged" + diag
