"""Integration tests for `fno worktree ensure` (node x-73ca).

The verb is a mechanism-only primitive: given a repo MAIN checkout and a name,
it idempotently creates `<worktrees_base>/<repo>/<name>` (branched off
origin/main, not the dispatcher's local HEAD) and prints the path on stdout.
On any failure it exits non-zero and prints NOTHING on stdout, so a caller
doing `wt=$(fno worktree ensure ...)` falls back to its prior cwd and the
dispatch is never blocked.

HOME is pinned to a per-test temp dir so the default worktree base
(`~/.fno/worktrees`) lands in the sandbox. Real git repos are used because the
gitdir/common-dir distinction (main checkout vs linked worktree) is a
filesystem fact.
"""
from __future__ import annotations

import shutil
import subprocess
from pathlib import Path

import pytest
from typer.testing import CliRunner

from fno.cli import app

pytestmark = pytest.mark.skipif(shutil.which("git") is None, reason="git required")

runner = CliRunner()


def _git(*args: str, cwd: Path) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        ["git", "-c", "user.email=t@t", "-c", "user.name=t", *args],
        cwd=str(cwd), capture_output=True, text=True, check=True,
    )


@pytest.fixture
def main_repo(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Path:
    """A real git main checkout on `main` with one commit; HOME pinned to sandbox."""
    monkeypatch.setenv("HOME", str(tmp_path))
    main = tmp_path / "myrepo"
    main.mkdir()
    _git("init", "-q", "-b", "main", cwd=main)
    (main / "README.md").write_text("# repo\n")
    _git("add", "README.md", cwd=main)
    _git("commit", "-qm", "init", cwd=main)
    return main


def _default_wt(home: Path, repo: Path, name: str) -> Path:
    """Default worktree location: ~/.fno/worktrees/<repo>/<name> (no config)."""
    return home / ".fno" / "worktrees" / repo.name / name


def test_ensure_creates_worktree_and_prints_path(main_repo: Path, tmp_path: Path) -> None:
    """An omitted --harness never guesses a harness location: the default
    harness-native degrades to external and lands under the ~/.fno base."""
    res = runner.invoke(app, ["worktree", "ensure", "--repo", str(main_repo), "--name", "agent-a"])
    assert res.exit_code == 0, res.stderr
    wt = _default_wt(tmp_path, main_repo, "agent-a")
    assert res.stdout.strip() == str(wt)
    assert wt.is_dir()
    assert not (main_repo / ".claude" / "worktrees" / "agent-a").exists()
    # It is a real, registered worktree of the repo.
    toplevel = _git("rev-parse", "--show-toplevel", cwd=wt).stdout.strip()
    assert Path(toplevel).resolve() == wt.resolve()


def test_ensure_branches_from_origin_main_not_local_head(
    main_repo: Path, tmp_path: Path
) -> None:
    """AC1-HP / Locked Decision 5: the new branch is based on origin/main, NOT
    the dispatcher's (possibly stale, ahead) local HEAD."""
    origin = tmp_path / "origin.git"
    subprocess.run(["git", "init", "-q", "--bare", "-b", "main", str(origin)], check=True)
    _git("remote", "add", "origin", str(origin), cwd=main_repo)
    _git("push", "-q", "origin", "main", cwd=main_repo)
    origin_main_sha = _git("rev-parse", "HEAD", cwd=main_repo).stdout.strip()
    # Advance local main ahead of origin/main.
    (main_repo / "extra.txt").write_text("ahead\n")
    _git("add", "extra.txt", cwd=main_repo)
    _git("commit", "-qm", "ahead of origin", cwd=main_repo)
    local_head = _git("rev-parse", "HEAD", cwd=main_repo).stdout.strip()
    assert local_head != origin_main_sha

    res = runner.invoke(app, ["worktree", "ensure", "--repo", str(main_repo), "--name", "agent-b"])
    assert res.exit_code == 0, res.stderr
    wt = Path(res.stdout.strip())
    wt_head = _git("rev-parse", "HEAD", cwd=wt).stdout.strip()
    assert wt_head == origin_main_sha  # based on origin/main, not local HEAD


def _bare_origin(main_repo: Path, tmp_path: Path) -> Path:
    """Bare origin with `main` pushed from main_repo; returns the origin path."""
    origin = tmp_path / "origin.git"
    subprocess.run(["git", "init", "-q", "--bare", "-b", "main", str(origin)], check=True)
    _git("remote", "add", "origin", str(origin), cwd=main_repo)
    _git("push", "-q", "origin", "main", cwd=main_repo)
    return origin


def test_ensure_continues_origin_feature_branch_ahead_of_main(
    main_repo: Path, tmp_path: Path
) -> None:
    """AC2-HP (x-28ff): origin/feature/<name> ahead of main -> the new worktree
    tracks that branch at its tip, and the receipt names it as continued."""
    _bare_origin(main_repo, tmp_path)
    _git("checkout", "-qb", "feature/x-1234", cwd=main_repo)
    (main_repo / "a.txt").write_text("one\n")
    _git("add", "a.txt", cwd=main_repo)
    _git("commit", "-qm", "one", cwd=main_repo)
    (main_repo / "b.txt").write_text("two\n")
    _git("add", "b.txt", cwd=main_repo)
    _git("commit", "-qm", "two", cwd=main_repo)
    _git("push", "-q", "origin", "feature/x-1234", cwd=main_repo)
    tip = _git("rev-parse", "feature/x-1234", cwd=main_repo).stdout.strip()
    # Delete the LOCAL branch: the continued path fires when only origin has
    # the work (a fresh checkout, or a branch that never came back). A local
    # branch present in the canonical repo takes today's checkout path.
    _git("checkout", "-q", "main", cwd=main_repo)
    _git("branch", "-qD", "feature/x-1234", cwd=main_repo)

    res = runner.invoke(app, ["worktree", "ensure", "--repo", str(main_repo), "--name", "x-1234"])
    assert res.exit_code == 0, res.stderr
    wt = Path(res.stdout.strip())
    assert _git("rev-parse", "HEAD", cwd=wt).stdout.strip() == tip
    upstream = _git("rev-parse", "--abbrev-ref", "feature/x-1234@{upstream}", cwd=wt).stdout.strip()
    assert upstream == "origin/feature/x-1234"
    assert "base=continued:origin/feature/x-1234:+2" in res.stderr


def test_ensure_salvages_remote_salvage_ref_when_no_branch(
    main_repo: Path, tmp_path: Path
) -> None:
    """AC3-HP (x-28ff): only refs/fno/salvage/<name> on origin ahead of main ->
    the new branch starts at that commit and the receipt names it as salvaged."""
    _bare_origin(main_repo, tmp_path)
    _git("checkout", "-qb", "spill", cwd=main_repo)
    (main_repo / "c.txt").write_text("salvage me\n")
    _git("add", "c.txt", cwd=main_repo)
    _git("commit", "-qm", "unpushed work", cwd=main_repo)
    spill_tip = _git("rev-parse", "HEAD", cwd=main_repo).stdout.strip()
    _git("push", "-q", "origin", f"{spill_tip}:refs/fno/salvage/x-5555", cwd=main_repo)
    _git("checkout", "-q", "main", cwd=main_repo)

    res = runner.invoke(app, ["worktree", "ensure", "--repo", str(main_repo), "--name", "x-5555"])
    assert res.exit_code == 0, res.stderr
    wt = Path(res.stdout.strip())
    assert _git("rev-parse", "HEAD", cwd=wt).stdout.strip() == spill_tip
    assert "base=salvaged:refs/fno/salvage/x-5555:+1" in res.stderr


def test_ensure_reuse_honors_a_demanded_branch(main_repo: Path, tmp_path: Path) -> None:
    """A reused tree whose checkout differs from --branch switches to it.

    The composer's branch picker names an existing local branch; a tree the
    ensure created earlier (default branch) must not silently launch a
    branch the caller did not pick. A branch already checked out in another
    tree refuses instead (one checkout per branch).
    """
    _git("branch", "-q", "feature/picked", cwd=main_repo)
    first = runner.invoke(
        app,
        ["worktree", "ensure", "--repo", str(main_repo), "--name", "switcher"],
    )
    assert first.exit_code == 0, first.stderr
    wt = Path(first.stdout.strip())
    assert _git("branch", "--show-current", cwd=wt).stdout.strip() != "feature/picked"

    second = runner.invoke(
        app,
        [
            "worktree", "ensure", "--repo", str(main_repo),
            "--name", "switcher", "--branch", "feature/picked",
        ],
    )
    assert second.exit_code == 0, second.stderr
    assert second.stdout.strip() == str(wt)
    assert _git("branch", "--show-current", cwd=wt).stdout.strip() == "feature/picked"
    assert "created=false" in second.stderr, "reuse receipt keeps the caller's resume note"

    # Idempotent: same branch again reuses without a switch line.
    third = runner.invoke(
        app,
        [
            "worktree", "ensure", "--repo", str(main_repo),
            "--name", "switcher", "--branch", "feature/picked",
        ],
    )
    assert third.exit_code == 0, third.stderr
    assert third.stdout.strip() == str(wt)
    assert "switched" not in third.stderr

    # A branch checked out in ANOTHER worktree refuses the switch.
    _git("branch", "-q", "feature/elsewhere", cwd=main_repo)
    other = runner.invoke(
        app,
        [
            "worktree", "ensure", "--repo", str(main_repo),
            "--name", "elsewhere", "--branch", "feature/elsewhere",
        ],
    )
    assert other.exit_code == 0, other.stderr
    blocked = runner.invoke(
        app,
        [
            "worktree", "ensure", "--repo", str(main_repo),
            "--name", "switcher", "--branch", "feature/elsewhere",
        ],
    )
    assert blocked.exit_code != 0
    assert blocked.stdout.strip() == ""


def test_ensure_refuses_a_stray_dir_and_a_linked_repo(main_repo: Path, tmp_path: Path) -> None:
    """AC1-FR: a same-named NON-worktree dir is never clobbered, and a --repo
    that is itself a linked worktree must not nest. Both print nothing on
    stdout, so the caller falls back."""
    stray = _default_wt(tmp_path, main_repo, "stray")
    stray.mkdir(parents=True)
    sentinel = stray / "keep.txt"
    sentinel.write_text("do not delete\n")

    res = runner.invoke(app, ["worktree", "ensure", "--repo", str(main_repo), "--name", "stray"])
    assert res.exit_code != 0
    assert res.stdout.strip() == ""
    assert sentinel.read_text() == "do not delete\n"  # untouched

    linked = tmp_path / "linked"
    _git("worktree", "add", str(linked), "-b", "side", cwd=main_repo)
    res = runner.invoke(app, ["worktree", "ensure", "--repo", str(linked), "--name", "nested"])
    assert res.exit_code != 0
    assert res.stdout.strip() == ""


# --- policy gate (x-168b) ---------------------------------------------------


def _write_config(fno_dir: Path, body: str) -> None:
    fno_dir.mkdir(parents=True, exist_ok=True)
    (fno_dir / "config.toml").write_text(body)


def test_ensure_policy_never_returns_repo_root(main_repo: Path, tmp_path: Path) -> None:
    """AC1-HP: a per-project `never` policy launches in place: repo root on
    stdout, exit 0, and NO worktree created anywhere."""
    _write_config(
        main_repo / ".fno",
        f'[[work.workspaces.default.projects]]\npath = "{main_repo}"\nworktree = "never"\n',
    )
    res = runner.invoke(app, ["worktree", "ensure", "--repo", str(main_repo), "--name", "n"])
    assert res.exit_code == 0, res.stderr
    assert res.stdout.strip() == str(main_repo.resolve())
    assert not _default_wt(tmp_path, main_repo, "n").exists()


def test_ensure_policy_misconfig_refuses_fail_closed(main_repo: Path, tmp_path: Path) -> None:
    """A policy the verb cannot read refuses creation (empty stdout, non-zero):
    never auto-isolate on a misconfig, never report success in place.

    - an unparseable config.toml (AC1-ERR)
    - `work.workspaces` of the wrong type, which hides a `never` entry
    - an out-of-enum value (`conductor` is a base, not a mode), which names
      the valid values on stderr (AC2-ERR)
    - `never` plus an explicit --branch: an isolated branch cannot be
      created in place
    """
    cases = [
        ("broken", "this = = broken toml\n", []),
        ("malformed", '[work]\nworkspaces = "not-a-table"\n', []),
        (
            "enum",
            f'[[work.workspaces.default.projects]]\npath = "{main_repo}"\nworktree = "conductor"\n',
            [],
        ),
        (
            "branch",
            f'[[work.workspaces.default.projects]]\npath = "{main_repo}"\nworktree = "never"\n',
            ["--branch", "feature/batch-x"],
        ),
    ]
    for name, body, extra in cases:
        _write_config(main_repo / ".fno", body)
        res = runner.invoke(
            app, ["worktree", "ensure", "--repo", str(main_repo), "--name", name, *extra]
        )
        assert res.exit_code != 0, name
        assert res.stdout.strip() == "", name
        assert not _default_wt(tmp_path, main_repo, name).exists(), name
        if name == "enum":
            assert "never" in res.stderr and "harness-native" in res.stderr and "external" in res.stderr


def test_ensure_emits_stderr_receipt_naming_mode_and_path(
    main_repo: Path, tmp_path: Path
) -> None:
    """AC: every spawn emits exactly one stderr receipt naming the resolved mode
    and the worktree path - harness-native, external, and the reuse path alike
    (the path itself stays the SOLE stdout line so callers still read it clean).
    A claude payload with no policy config lands harness-native at
    <repo>/.claude/worktrees/<name>, not under the ~/.fno base."""
    hn = runner.invoke(
        app, ["worktree", "ensure", "--repo", str(main_repo), "--name", "r1", "--harness", "claude"]
    )
    assert hn.exit_code == 0, hn.stderr
    assert "policy=harness-native" in hn.stderr
    assert str(main_repo / ".claude" / "worktrees" / "r1") in hn.stderr
    assert hn.stdout.strip() == str(main_repo / ".claude" / "worktrees" / "r1")
    assert not _default_wt(tmp_path, main_repo, "r1").exists()

    ext = runner.invoke(app, ["worktree", "ensure", "--repo", str(main_repo), "--name", "r2"])
    assert ext.exit_code == 0, ext.stderr
    assert "policy=external" in ext.stderr
    assert str(_default_wt(tmp_path, main_repo, "r2")) in ext.stderr

    reuse = runner.invoke(
        app, ["worktree", "ensure", "--repo", str(main_repo), "--name", "r1", "--harness", "claude"]
    )
    assert reuse.exit_code == 0, reuse.stderr
    assert "reusing worktree at" in reuse.stderr
    assert reuse.stdout.strip() == str(main_repo / ".claude" / "worktrees" / "r1")


def test_ensure_reuses_the_branch_checkout_when_the_policy_path_moves(
    main_repo: Path, tmp_path: Path
) -> None:
    """A relocated policy path must reuse the branch's existing checkout.

    A branch has exactly one checkout. When the resolved path moves under an
    existing lane (a node whose harness stopped resolving to claude, or a
    `worktrees_base` edit), `git worktree add` refuses with "already used by
    worktree at ..." and the lane wedges on every tick, because its tree is
    unmerged and nothing reaps it.
    """
    first = runner.invoke(
        app, ["worktree", "ensure", "--repo", str(main_repo), "--name", "lane-a"]
    )
    assert first.exit_code == 0, first.stderr
    original = Path(first.stdout.strip())
    assert original == _default_wt(tmp_path, main_repo, "lane-a")

    # Same node, a harness whose policy resolves somewhere else.
    moved = runner.invoke(
        app,
        ["worktree", "ensure", "--repo", str(main_repo), "--name", "lane-a",
         "--harness", "claude"],
    )
    assert moved.exit_code == 0, moved.stderr
    assert Path(moved.stdout.strip()) == original
    assert "a branch has one checkout" in moved.stderr


def test_ensure_never_hands_back_a_missing_or_canonical_checkout(
    main_repo: Path, tmp_path: Path
) -> None:
    """Reuse-by-branch reads `git worktree list --porcelain`, which lists two
    rows that must never come back with exit 0.

    A directory deleted by hand stays LISTED (only a `prunable` line marks
    it): handing it back sends `spawn --cwd` and `target start`'s setup hook
    at a directory that is not there. The main checkout is a row too: with
    `feature/<name>` checked out in canonical, `target start` would read the
    repo root as `in_place` and launch on the canonical checkout, the
    contradiction the `never` policy arm refuses in the mirror case.
    """
    first = runner.invoke(
        app, ["worktree", "ensure", "--repo", str(main_repo), "--name", "lane-p"]
    )
    assert first.exit_code == 0, first.stderr
    original = Path(first.stdout.strip())
    shutil.rmtree(original)
    assert not original.exists()

    # The RELOCATED call is the one that consults the branch, so the prunable
    # row can only be handed back on this path.
    second = runner.invoke(
        app,
        ["worktree", "ensure", "--repo", str(main_repo), "--name", "lane-p",
         "--harness", "claude"],
    )
    assert not (
        second.exit_code == 0 and Path(second.stdout.strip() or ".") == original
    ), "handed back a path that does not exist"

    _git("checkout", "-q", "-b", "feature/lane-c", cwd=main_repo)
    out = runner.invoke(
        app, ["worktree", "ensure", "--repo", str(main_repo), "--name", "lane-c"]
    )
    assert not (
        out.exit_code == 0
        and Path(out.stdout.strip() or ".").resolve() == main_repo.resolve()
    ), "handed back the canonical checkout"
