from __future__ import annotations

import os
import subprocess
import sys

import pytest

from fno.cli import _protect_process_path as protect_process_path
from fno.pr import gh_proxy
from fno.pr._proc import Result
from fno.pr.gh_proxy import Action, classify, delegate
from fno.setup.github_cli import InstallResult, ensure_proxy, worker_environment


def test_process_proxy_boundary_restores_path_on_close(monkeypatch, tmp_path):
    original = "/original/bin"
    proxy = str(tmp_path / "proxy")
    monkeypatch.setenv("PATH", original)
    monkeypatch.setattr(
        "fno.setup.github_cli.worker_environment",
        lambda base: {
            **dict(base),
            "PATH": f"{proxy}{os.pathsep}{original}",
            "FNO_GH_PROXY_DIR": proxy,
        },
    )
    closed = []

    class Context:
        def call_on_close(self, callback):
            closed.append(callback)

    protect_process_path(Context())
    assert os.environ["PATH"].split(os.pathsep)[0] == proxy
    assert os.environ["FNO_GH_PROXY_DIR"]
    assert len(closed) == 1
    closed[0]()
    assert os.environ["PATH"] == original
    assert "FNO_GH_PROXY_DIR" not in os.environ


def test_proxy_classifies_every_graphql_gh_surface():
    assert classify(["pr", "view", "930", "--json", "headRefOid"]) is Action.BROKER
    assert classify(["pr", "checks", "930"]) is Action.BROKER
    assert classify(["pr", "view", "930"]) is Action.BROKER
    assert classify(["pr", "view", "930", "--json=state"]) is Action.BROKER
    assert classify(["pr", "list", "--state", "open"]) is Action.BROKER
    assert classify(["pr", "status"]) is Action.BROKER
    assert classify(["api", "graphql", "-f", "query={viewer{login}}"]) is Action.BROKER
    assert classify(["api", "-X", "POST", "graphql", "-f", "query=x"]) is Action.BROKER
    assert classify(["api", "--hostname", "github.com", "graphql"]) is Action.BROKER
    assert classify(["api", "repos/o/r/pulls/930"]) is Action.DELEGATE
    # A repo global option ahead of the verb must not hide it from the broker.
    assert classify(["-R", "o/r", "pr", "view", "930"]) is Action.BROKER


def test_install_backs_up_an_unrelated_existing_wrapper(tmp_path):
    proxy = tmp_path / "gh"
    proxy.write_text("#!/bin/sh\necho existing\n")
    proxy.chmod(0o755)
    real = tmp_path / "real-gh"
    real.write_text("real")
    result = ensure_proxy(directory=tmp_path, real_gh=real)
    assert result.changed is True
    assert result.backup is not None
    assert result.backup.read_text() == "#!/bin/sh\necho existing\n"
    assert "fno-gh-proxy" in proxy.read_text()
    assert os.access(proxy, os.X_OK)


def _helper(monkeypatch, tmp_path, body="#!/bin/sh\nexit 0\n"):
    helper = tmp_path / "tools" / "fno-gh-proxy"
    helper.parent.mkdir()
    helper.write_text(body)
    helper.chmod(0o755)
    monkeypatch.setattr(
        "fno.setup.github_cli.shutil.which",
        lambda name: str(helper) if name == "fno-gh-proxy" else None,
    )
    return helper


def test_proxy_shim_finds_helper_with_only_its_directory_on_path(monkeypatch, tmp_path):
    _helper(monkeypatch, tmp_path, '#!/bin/sh\nexec "$FNO_REAL_GH" "$@"\n')
    real = tmp_path / "real-bin" / "gh"
    real.parent.mkdir()
    real.write_text("#!/bin/sh\nprintf 'gh version test\\n'\n")
    real.chmod(0o755)
    directory = tmp_path / "proxy"
    ensure_proxy(directory=directory, real_gh=real)

    result = subprocess.run(
        [str(directory / "gh"), "--version"],
        capture_output=True,
        text=True,
        env={
            "PATH": os.pathsep.join([str(directory), str(real.parent)]),
            "FNO_REAL_GH": str(real),
        },
        check=False,
    )

    assert result.returncode == 0, result.stderr
    assert "gh version test" in result.stdout


def test_worker_environment_prepends_proxy_and_pins_delegate(tmp_path, monkeypatch):
    real = tmp_path / "real-gh"
    real.write_text("real")
    proxy = tmp_path / "proxy" / "gh"
    monkeypatch.setattr(
        "fno.setup.github_cli.ensure_proxy",
        lambda **_: InstallResult(proxy=proxy, delegate=real.resolve(), changed=False),
    )
    env = worker_environment(
        {"PATH": "/usr/bin", "KEEP": "yes", "FNO_REAL_GH": str(real)}
    )
    assert env["PATH"].split(os.pathsep)[0] == str(tmp_path / "proxy")
    assert env["FNO_GH_PROXY_DIR"] == str(tmp_path / "proxy")
    assert "FNO_REAL_GH" not in env
    assert env["KEEP"] == "yes"


def test_worker_environment_uses_config_free_fallback(monkeypatch, tmp_path):
    real = tmp_path / "real-gh"
    real.write_text("real")
    fallback = tmp_path / "fallback"
    calls = []

    def install(**kwargs):
        calls.append(kwargs.get("directory"))
        if kwargs.get("directory") is None:
            raise AttributeError("settings stub has no state_dir")
        return InstallResult(proxy=fallback / "gh", delegate=real, changed=True)

    monkeypatch.setattr("fno.setup.github_cli.ensure_proxy", install)
    monkeypatch.setattr("fno.setup.github_cli.fallback_proxy_dir", lambda: fallback)
    env = worker_environment({"PATH": "/usr/bin", "FNO_REAL_GH": str(real)})
    assert calls == [None, fallback]
    assert env["PATH"].split(os.pathsep)[0] == str(fallback)
    assert env["FNO_GH_PROXY_DIR"] == str(fallback)


def _proxy_dirs(monkeypatch, tmp_path):
    durable = tmp_path / "durable"
    fallback = tmp_path / "fallback"
    monkeypatch.setattr("fno.setup.github_cli.github_cli_proxy_dir", lambda: durable)
    monkeypatch.setattr("fno.setup.github_cli.fallback_proxy_dir", lambda: fallback)
    real = tmp_path / "real-bin" / "gh"
    real.parent.mkdir()
    real.write_text("#!/bin/sh\necho real\n")
    real.chmod(0o755)
    return durable, fallback, real


def _temp_first_path(fallback, real):
    ensure_proxy(directory=fallback, real_gh=real)
    return os.pathsep.join([str(fallback), str(real.parent), "/usr/bin"])


def test_worker_environment_moves_lineage_off_temp_shim(monkeypatch, tmp_path):
    durable, fallback, real = _proxy_dirs(monkeypatch, tmp_path)
    path = _temp_first_path(fallback, real)
    env = worker_environment({"PATH": path})
    entries = env["PATH"].split(os.pathsep)
    assert entries.index(str(durable)) < entries.index(str(fallback))
    assert env["FNO_GH_PROXY_DIR"] == str(durable.resolve())


def test_direct_pr_view_reaches_the_shared_floor_and_diagnostic(monkeypatch, capsys):
    monkeypatch.setattr(sys, "argv", ["gh", "pr", "view", "930", "--json", "reviews"])
    monkeypatch.setattr(gh_proxy._quota, "resolve_real_gh", lambda: "/real/gh")
    monkeypatch.setattr(
        gh_proxy._quota,
        "execute_graphql",
        lambda *args, **kwargs: Result(
            75,
            "",
            "GraphQL discretionary read refused: reset at 2026-08-19T00:00:00Z. "
            "Use `fno do pr info 930` for state/head/mergeability and `fno do pr status 930` "
            "for CI; stop retrying GraphQL until reset. `fno do pr status` still contains "
            "optional review-thread and coverage reads that are GraphQL; those reads "
            "preserve the reserved coverage budget.",
        ),
    )
    with pytest.raises(SystemExit, match="75"):
        gh_proxy.main()
    err = capsys.readouterr().err
    assert "Use `fno do pr info 930`" in err
    assert "stop retrying GraphQL until reset" in err
    assert "coverage reads that are GraphQL" in err


def test_resolve_real_gh_returns_a_path_even_with_no_proxy_dir_to_skip(
    tmp_path, monkeypatch
):
    """os.execve needs a path with a slash, never a bare command name.

    The real gh being the FIRST match on PATH (no proxy entry ahead of it to
    skip past) used to return the literal string "gh" instead of a resolved
    path, and delegate()'s os.execve("gh", ...) fails with FileNotFoundError:
    execve does not search PATH the way execvp does.
    """
    from fno.pr import _quota

    real = tmp_path / "gh"
    real.write_text("#!/bin/sh\necho real\n")
    real.chmod(0o755)
    monkeypatch.delenv("FNO_REAL_GH", raising=False)
    monkeypatch.delenv("FNO_GH_PROXY_DIR", raising=False)
    monkeypatch.setattr(_quota, "_proxy_dirs", lambda: set())
    monkeypatch.setenv("PATH", str(tmp_path))

    resolved = _quota.resolve_real_gh()

    assert resolved == str(real.resolve())
    assert os.sep in resolved


def test_main_refuses_to_run_when_the_marker_carries_our_own_pid(monkeypatch, capsys):
    # os.execve leaves no child and PRESERVES the pid, so a marker equal to our
    # own pid is the only evidence of a repeat.
    monkeypatch.setattr(sys, "argv", ["gh", "api", "rate_limit"])
    monkeypatch.setenv("FNO_GH_PROXY_DEPTH", str(os.getpid()))
    with pytest.raises(SystemExit) as exc:
        gh_proxy.main()
    # `.code`, never `match=`: `match` is a regex SEARCH over the exception
    # text, so `match="2"` also passes for SystemExit(127) and pins nothing.
    assert exc.value.code == 2
    assert "re-entered itself" in capsys.readouterr().err


def test_delegate_stamps_the_reentry_marker_into_the_exec_environment(monkeypatch):
    seen = {}

    def execve(path, argv, env):
        seen["env"] = env
        raise RuntimeError("exec sentinel")

    monkeypatch.setattr("fno.pr.gh_proxy.os.execve", execve)
    monkeypatch.setattr(
        "fno.pr.gh_proxy._quota.delegate_environment", lambda: {"PATH": "/real/bin"}
    )
    monkeypatch.setattr("fno.pr.gh_proxy._quota.admit", lambda args: None)
    with pytest.raises(RuntimeError, match="exec sentinel"):
        delegate("/real/gh", ["auth", "status"])
    # The successor execve creates keeps this pid, which is what makes the
    # marker mean "I am myself again" rather than "someone upstream ran gh".
    assert seen["env"]["FNO_GH_PROXY_DEPTH"] == str(os.getpid())


BUDGET_LINE = (
    "gh budget: fleet GitHub rate limit held locally (budget: 2/2 points "
    "in 60s | backoff 0s left); this command did not reach GitHub. Retry "
    "after 1s. Ledger: fno-agents fleet-incident gh-budget status"
)


def test_delegate_refused_by_the_budget_never_invokes_the_real_gh(monkeypatch, capsys):
    called = []
    monkeypatch.setattr("fno.pr.gh_proxy.os.execve", lambda *a, **k: called.append(a))
    monkeypatch.setattr("fno.pr.gh_proxy._quota.admit", lambda args: BUDGET_LINE)
    with pytest.raises(SystemExit) as exc:
        delegate("/real/gh", ["api", "repos/o/r"])
    # `.code`, never `match=`: `match` regex-searches the text and also pins 75.
    assert exc.value.code == gh_proxy._quota.REFUSED
    assert BUDGET_LINE in capsys.readouterr().err
    assert called == [], "the real gh must never be exec'd under a budget refusal"


def test_main_turns_a_proxy_identity_failure_into_a_clean_refusal(monkeypatch, capsys):
    monkeypatch.setattr(sys, "argv", ["gh", "auth", "status"])
    monkeypatch.delenv("FNO_GH_PROXY_DEPTH", raising=False)

    def broken():
        raise gh_proxy._quota.ProxyIdentityError("config load failed")

    monkeypatch.setattr(gh_proxy._quota, "resolve_real_gh", broken)
    with pytest.raises(SystemExit) as exc:
        gh_proxy.main()
    assert exc.value.code == 2
    assert "cannot identify its own install directory" in capsys.readouterr().err


def test_worker_environment_drops_an_inherited_reentry_marker(monkeypatch, tmp_path):
    # A delegated gh runs git, a git hook runs fno, and fno builds a worker env
    # from os.environ. The marker rides along and the worker's first, only gh
    # call would refuse. Same reason FNO_REAL_GH is dropped one line above.
    real = tmp_path / "real"
    real.mkdir()
    gh = real / "gh"
    gh.write_text("#!/bin/sh\necho real\n")
    gh.chmod(0o755)
    env = worker_environment(
        {"PATH": str(real), "FNO_GH_PROXY_DEPTH": "1", "FNO_GH_PROXY_DIR": str(tmp_path / "proxy")}
    )
    assert "FNO_GH_PROXY_DEPTH" not in env
