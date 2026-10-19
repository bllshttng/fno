from __future__ import annotations

import json
import os
import time

import pytest

from fno.pr import _quota
from fno.pr._proc import Result

# The autouse budget fixture below replaces these on the module; tests that
# exercise the real functions restore them from these import-time captures.
_REAL_ADMIT = _quota.admit
_REAL_RECORD_REFUSAL = _quota.record_refusal


@pytest.fixture(autouse=True)
def quiet_budget(monkeypatch):
    """Keep every execute_graphql test off the real fleet ledger.

    execute_graphql now admits against `fno-agents fleet-incident gh-budget`; a test that
    skipped the stub would charge the operator's machine-wide budget (or trip
    over a live fleet backoff). Tests that exercise the budget re-stub admit
    themselves.
    """
    refusals: list[str] = []
    monkeypatch.setattr(_quota, "admit", lambda argv: None)
    monkeypatch.setattr(_quota, "record_refusal", lambda stderr: refusals.append(stderr))
    return refusals


def _runner(remaining: int | None, calls: list[list[str]], command_result: Result | None = None):
    def run(cmd, **kwargs):
        calls.append(list(cmd))
        if cmd[-2:] == ["api", "rate_limit"]:
            if remaining is None:
                return Result(1, "", "instrument unavailable")
            return Result(
                0,
                json.dumps({"resources": {"graphql": {"remaining": remaining, "reset": 1787072400}}}),
                "",
            )
        return command_result or Result(0, '{"data":{"ok":true}}', "")

    return run


def test_discretionary_read_is_refused_at_floor_before_graphql(tmp_path):
    calls: list[list[str]] = []
    result = _quota.execute_graphql(
        "discretionary",
        ["pr", "view", "930", "--json", "headRefOid"],
        runner=_runner(_quota.GRAPHQL_RESERVE, calls),
        real_gh="/real/gh",
        lock_path=tmp_path / "quota.lock",
        probe_cache=tmp_path / "quota-probe.json",
    )
    assert result.returncode == _quota.REFUSED
    assert calls == [["/real/gh", "api", "rate_limit"]]
    assert "fno do pr info 930" in result.stderr
    assert "stop retrying GraphQL until" in result.stderr
    assert "still contains optional review-thread and coverage reads" in result.stderr


def test_unreadable_instrument_fails_closed_only_for_discretionary(tmp_path):
    discretionary_calls: list[list[str]] = []
    refused = _quota.execute_graphql(
        "discretionary",
        ["pr", "view", "930", "--json", "commits"],
        runner=_runner(None, discretionary_calls),
        real_gh="/real/gh",
        lock_path=tmp_path / "quota.lock",
        probe_cache=tmp_path / "quota-probe.json",
    )
    assert refused.returncode == _quota.REFUSED
    assert "instrument unavailable" in refused.stderr
    assert len(discretionary_calls) == 1

    coverage_calls: list[list[str]] = []
    allowed = _quota.execute_graphql(
        "coverage",
        ["pr", "view", "930", "--json", "commits"],
        runner=_runner(None, coverage_calls),
        real_gh="/real/gh",
        lock_path=tmp_path / "quota.lock",
        probe_cache=tmp_path / "quota-probe.json",
    )
    assert allowed.returncode == 0
    assert len(coverage_calls) == 2


def test_flag_first_argv_is_refused_without_touching_gh(tmp_path):
    calls: list[list[str]] = []
    result = _quota.execute_graphql(
        "discretionary",
        ["-f", "query={viewer{login}}", "--jq", ".data.viewer.login"],
        runner=_runner(5000, calls),
        real_gh="/real/gh",
        lock_path=tmp_path / "quota.lock",
        probe_cache=tmp_path / "quota-probe.json",
    )
    assert result.returncode == 2
    assert "command words first" in result.stderr
    assert "api graphql" in result.stderr
    assert calls == []


def test_gh_global_flags_before_the_command_word_still_execute(tmp_path):
    """`gh -R o/r api graphql ...` is legal gh and the broker forwards it."""
    calls: list[list[str]] = []
    argv = ["-R", "o/r", "api", "graphql", "-f", "query={viewer{login}}"]
    result = _quota.execute_graphql(
        "discretionary",
        argv,
        runner=_runner(5000, calls, Result(0, "ok", "")),
        real_gh="/real/gh",
        lock_path=tmp_path / "quota.lock",
        probe_cache=tmp_path / "quota-probe.json",
    )
    assert result.returncode == 0
    assert calls[-1] == ["/real/gh", *argv], "the exec must keep the full argv"

    attached = ["--repo=o/r", "api", "graphql", "-f", "query={viewer{login}}"]
    calls.clear()
    result = _quota.execute_graphql(
        "discretionary",
        attached,
        runner=_runner(5000, calls, Result(0, "ok", "")),
        real_gh="/real/gh",
        lock_path=tmp_path / "quota.lock",
        probe_cache=tmp_path / "quota-probe.json",
    )
    assert result.returncode == 0
    assert calls[-1] == ["/real/gh", *attached]


def test_coverage_purpose_rejects_arbitrary_graphql(tmp_path):
    calls: list[list[str]] = []
    result = _quota.execute_graphql(
        "coverage",
        ["api", "graphql", "-f", "query={viewer{login}}"],
        runner=_runner(5000, calls),
        real_gh="/real/gh",
        lock_path=tmp_path / "quota.lock",
        probe_cache=tmp_path / "quota-probe.json",
    )
    assert result.returncode == 2
    assert "review-coverage reads only" in result.stderr
    assert calls == []


def test_preserved_wrapper_cannot_resolve_back_to_quota_proxy(tmp_path, monkeypatch):
    proxy_dir = tmp_path / "proxy"
    wrapper_dir = tmp_path / "wrapper"
    real_dir = tmp_path / "real"
    for directory in (proxy_dir, wrapper_dir, real_dir):
        directory.mkdir()
    proxy = proxy_dir / "gh"
    proxy.write_text("#!/bin/sh\necho proxy-recursion >&2\nexit 99\n")
    wrapper = wrapper_dir / "gh"
    wrapper.write_text(
        "#!/bin/sh\n"
        "self=$(dirname \"$0\")\n"
        "old=$IFS; IFS=:\n"
        "for d in $PATH; do [ \"$d\" = \"$self\" ] && continue; "
        "[ -x \"$d/gh\" ] && exec \"$d/gh\" \"$@\"; done\n"
        "IFS=$old\nexit 127\n"
    )
    real = real_dir / "gh"
    real.write_text(
        "#!/bin/sh\n"
        "if [ \"$*\" = \"api rate_limit\" ]; then "
        "echo '{\"resources\":{\"graphql\":{\"remaining\":5000,\"reset\":1787072400}}}'; "
        "else echo '{\"data\":{\"viewer\":{\"login\":\"ok\"}}}'; fi\n"
    )
    for executable in (proxy, wrapper, real):
        executable.chmod(0o755)
    monkeypatch.setattr(_quota, "github_cli_proxy_dir", lambda: proxy_dir)
    monkeypatch.delenv("FNO_REAL_GH", raising=False)
    monkeypatch.setenv("PATH", f"{proxy_dir}:{wrapper_dir}:{real_dir}:/usr/bin:/bin")
    result = _quota.execute_graphql(
        "discretionary",
        ["api", "graphql", "-f", "query={viewer{login}}"],
        lock_path=tmp_path / "quota.lock",
        probe_cache=tmp_path / "quota-probe.json",
    )
    assert result.returncode == 0
    assert '"login":"ok"' in result.stdout


def _shim(directory, name="gh"):
    """A proxy shim as `fno setup` writes it: a two-line script calling the broker."""
    directory.mkdir(parents=True, exist_ok=True)
    path = directory / name
    path.write_text('#!/bin/sh\nexec fno-gh-proxy "$@"\n')
    path.chmod(0o755)
    return path


def _real(directory, name="gh"):
    directory.mkdir(parents=True, exist_ok=True)
    path = directory / name
    path.write_text("#!/bin/sh\necho real\n")
    path.chmod(0o755)
    return path


def test_resolve_real_gh_never_returns_a_bare_name(monkeypatch, tmp_path):
    """A bare "gh" is unusable by both callers, so it must never be returned.

    `delegate` execve's the result, and execve does no PATH lookup, so a bare
    name raises FileNotFoundError. `execute_graphql` runs the result while
    holding the flock, and a bare name re-enters the shim and blocks forever.
    """
    shim_dir = tmp_path / "unrecognized-shim"
    real_dir = tmp_path / "real"
    _shim(shim_dir)
    real = _real(real_dir)
    # The shim's own directory is TMPDIR-derived. A caller whose TMPDIR differs
    # (a background job, a launchd agent) computes a different one and so does
    # not recognize the shim sitting first on its inherited PATH.
    monkeypatch.setattr(_quota, "github_cli_proxy_dir", lambda: tmp_path / "elsewhere")
    monkeypatch.delenv("FNO_GH_PROXY_DIR", raising=False)
    monkeypatch.delenv("FNO_REAL_GH", raising=False)
    monkeypatch.setenv("PATH", f"{shim_dir}:{real_dir}")

    resolved = _quota.resolve_real_gh()

    assert resolved == str(real.resolve())
    assert os.path.isabs(resolved)


def test_resolve_real_gh_refuses_a_configured_shim(monkeypatch, tmp_path):
    """FNO_REAL_GH pointing at a shim would loop the broker into itself."""
    shim_dir = tmp_path / "shim"
    real_dir = tmp_path / "real"
    shim = _shim(shim_dir)
    real = _real(real_dir)
    monkeypatch.setattr(_quota, "github_cli_proxy_dir", lambda: tmp_path / "elsewhere")
    monkeypatch.delenv("FNO_GH_PROXY_DIR", raising=False)
    monkeypatch.setenv("FNO_REAL_GH", str(shim))
    monkeypatch.setenv("PATH", f"{real_dir}")

    assert _quota.resolve_real_gh() == str(real.resolve())


def test_is_proxy_shim_does_not_match_a_real_gh_that_mentions_the_name(tmp_path):
    """A comment merely naming the proxy must not be classified as the shim.

    A bare substring match would misclassify this as the shim, and
    ``resolve_real_gh``/``delegate_environment`` would then skip a working
    real ``gh``, turning it into "gh not found".
    """
    path = tmp_path / "gh"
    path.write_text("#!/bin/sh\n# not the fno-gh-proxy wrapper, just mentions it\necho real\n")
    path.chmod(0o755)

    assert _quota._is_proxy_shim(path) is False


def test_execute_graphql_refuses_when_the_proxy_cannot_identify_itself(monkeypatch):
    def broken():
        raise RuntimeError("config load failed")

    monkeypatch.setattr(_quota, "github_cli_proxy_dir", broken)
    monkeypatch.delenv("FNO_GH_PROXY_DIR", raising=False)
    monkeypatch.delenv("FNO_REAL_GH", raising=False)
    result = _quota.execute_graphql("discretionary", ["api", "graphql", "-f", "query=x"])
    assert result.returncode == 2
    assert "cannot identify its own install directory" in result.stderr


# ---- the reserve funds the NARROWER read, not only the wider one ----
#
# `_coverage_read` matched the field set EXACTLY, which inverted the cost bound
# it exists to enforce. The stop gate's round-budget read asks for `reviews`
# alone - a strict subset of the reviews shape - and it was refused on every
# call on every PR, while the sibling read asking for MORE fields passed. The
# reviews axis of the round count was inert from the day it shipped.


def test_the_round_budget_reviews_read_can_consume_reserved_points(tmp_path):
    """The exact argv the stop gate sends: `pr view <n> --json reviews`.

    Pinned to the argv, not to a field set of the test's own invention. A test
    that asserted some narrower read is funded would pass while the one read
    that actually spins stays refused.
    """
    calls: list[list[str]] = []
    result = _quota.execute_graphql(
        "coverage",
        ["pr", "view", "1252", "--json", "reviews"],
        runner=_runner(_quota.GRAPHQL_RESERVE, calls),
        real_gh="/real/gh",
        lock_path=tmp_path / "quota.lock",
        probe_cache=tmp_path / "quota-probe.json",
    )
    assert result.returncode == 0, result.stderr
    assert calls == [
        ["/real/gh", "api", "rate_limit"],
        ["/real/gh", "pr", "view", "1252", "--json", "reviews"],
    ]


def test_a_subset_of_one_shape_is_funded_but_a_union_across_shapes_is_not(
    tmp_path,
):
    """Subset per shape, never against their union.

    A union test would fund combinations no caller makes, widening the reserve
    instead of correcting it. `commits` and `labels` are each funded alone and
    the pair is refused, which is the discriminator between the two designs.
    """
    assert _quota._coverage_read(["pr", "view", "1", "--json", "reviews"])
    assert _quota._coverage_read(["pr", "view", "1", "--json", "reviews,headRefOid"])
    assert _quota._coverage_read(["pr", "view", "1", "--json", "commits"])
    assert _quota._coverage_read(["pr", "view", "1", "--json", "labels"])
    assert not _quota._coverage_read(["pr", "view", "1", "--json", "commits,labels"])
    assert not _quota._coverage_read(["pr", "view", "1", "--json", "reviews,labels"])
    assert not _quota._coverage_read(["pr", "view", "1", "--json", "files"])


def _console_script_shim(directory, name="gh"):
    """A shim as pip or uv writes it: a console script, with no exec line.

    Observed 2026-09-07 at `~/.local/bin/gh`. `_is_proxy_shim` only knew the
    two-line sh wrapper, so this read as a real binary, `resolve_real_gh`
    handed the proxy its own shim, and every gh read in the session died on
    the re-entry refusal.
    """
    directory.mkdir(parents=True, exist_ok=True)
    path = directory / name
    path.write_text(
        "#!/usr/bin/python3\n"
        "# -*- coding: utf-8 -*-\n"
        "import sys\n"
        "from fno.pr.gh_proxy import main\n"
        "if __name__ == '__main__':\n"
        "    sys.exit(main())\n"
    )
    path.chmod(0o755)
    return path


def test_a_console_script_shim_is_recognized_and_skipped(monkeypatch, tmp_path):
    shim_dir = tmp_path / "console-shim"
    real_dir = tmp_path / "real"
    shim = _console_script_shim(shim_dir)
    real = _real(real_dir)
    monkeypatch.setattr(_quota, "github_cli_proxy_dir", lambda: tmp_path / "elsewhere")
    monkeypatch.delenv("FNO_GH_PROXY_DIR", raising=False)
    monkeypatch.delenv("FNO_REAL_GH", raising=False)
    monkeypatch.setenv("PATH", f"{shim_dir}:{real_dir}")

    assert _quota._is_proxy_shim(shim), "the console-script shape must read as a shim"
    assert _quota.resolve_real_gh() == str(real.resolve())


# ---- the fleet request budget (admit before real gh, record on refusal) ----


BUDGET_LINE = (
    "gh budget: fleet GitHub rate limit held locally (budget: 450/450 points "
    "in 60s | backoff 0s left); this command did not reach GitHub. Retry "
    "after 1s. Ledger: fno-agents fleet-incident gh-budget status"
)


def test_a_budget_refusal_stops_the_command_before_real_gh(tmp_path, monkeypatch):
    calls: list[list[str]] = []
    monkeypatch.setattr(_quota, "admit", lambda argv: BUDGET_LINE)
    result = _quota.execute_graphql(
        "discretionary",
        ["pr", "view", "930", "--json", "headRefOid"],
        runner=_runner(5000, calls),
        real_gh="/real/gh",
        lock_path=tmp_path / "quota.lock",
        probe_cache=tmp_path / "quota-probe.json",
    )
    assert result.returncode == _quota.REFUSED
    assert result.stdout == ""
    assert BUDGET_LINE in result.stderr
    # The probe ran (it is exempt, 0 points); the real command never did.
    assert calls == [["/real/gh", "api", "rate_limit"]]


def test_a_rate_limited_command_records_the_refusal_once(tmp_path, quiet_budget):
    result = _quota.execute_graphql(
        "discretionary",
        ["pr", "view", "930", "--json", "headRefOid"],
        runner=_runner(
            5000,
            [],
            Result(1, "", "API rate limit exceeded for user ID 1 (HTTP 403)"),
        ),
        real_gh="/real/gh",
        lock_path=tmp_path / "quota.lock",
        probe_cache=tmp_path / "quota-probe.json",
    )
    assert result.returncode == 1
    assert len(quiet_budget) == 1
    assert "HTTP 403" in quiet_budget[0]


def test_record_refusal_sends_only_githubs_markers(monkeypatch):
    monkeypatch.setattr(_quota, "record_refusal", _REAL_RECORD_REFUSAL)
    sent: list[dict] = []
    monkeypatch.setattr(_quota, "_gh_budget", lambda payload: sent.append(payload))
    _quota.record_refusal("gh: API rate limit exceeded (HTTP 403)")
    _quota.record_refusal("gh: secondary limit hit (HTTP 429)")
    _quota.record_refusal("gh budget: fleet GitHub rate limit held locally")
    _quota.record_refusal("HTTP 500: server error")
    assert [s["op"] for s in sent] == ["refused", "refused"]


def test_record_refusal_lets_the_measured_bucket_overrule_core_wording(monkeypatch):
    # The caller gates on the live probe before reaching here, so a refusal
    # this far never had a drained primary bucket: the generic "rate limit"
    # wording then contradicts the measurement, and the measurement wins.
    monkeypatch.setattr(_quota, "record_refusal", _REAL_RECORD_REFUSAL)
    monkeypatch.setattr(_quota, "LAST_BUDGET", {"core_remaining": 4300})
    sent: list[dict] = []
    monkeypatch.setattr(_quota, "_gh_budget", lambda payload: sent.append(payload))
    _quota.record_refusal("gh: API rate limit exceeded (HTTP 403)")
    assert [s["kind"] for s in sent] == ["secondary"]
    monkeypatch.setattr(_quota, "LAST_BUDGET", None)
    _quota.record_refusal("gh: API rate limit exceeded (HTTP 403)")
    assert [s["kind"] for s in sent] == ["secondary", "core"]


def test_record_refusal_survives_an_unavailable_verb(monkeypatch, capsys):
    from fno.rust_binary import VerbUnavailable

    monkeypatch.setattr(_quota, "record_refusal", _REAL_RECORD_REFUSAL)

    def unavailable(payload):
        raise VerbUnavailable("fno-agents binary not found")

    monkeypatch.setattr(_quota, "_gh_budget", unavailable)
    _quota.record_refusal("API rate limit exceeded (HTTP 403)")
    assert "refusal not recorded" in capsys.readouterr().err


def test_admit_returns_the_refusal_line_and_admits_on_none(monkeypatch):
    monkeypatch.setattr(_quota, "admit", _REAL_ADMIT)
    monkeypatch.setenv("FNO_AGENT_SELF", "quota-test-worker")
    answers = iter(
        [
            {"verdict": "refused", "refusal": BUDGET_LINE},
            {"verdict": "admitted"},
        ]
    )
    monkeypatch.setattr(_quota, "_gh_budget", lambda payload: next(answers))
    assert _quota.admit(["pr", "view", "1"]) == BUDGET_LINE
    assert _quota.admit(["pr", "view", "1"]) is None


def test_admit_fails_open_when_the_verb_is_unavailable(monkeypatch, capsys):
    monkeypatch.setenv("FNO_AGENT_SELF", "quota-test-worker")
    from fno.rust_binary import VerbUnavailable

    monkeypatch.setattr(_quota, "admit", _REAL_ADMIT)

    def unavailable(payload):
        raise VerbUnavailable("no binary")

    monkeypatch.setattr(_quota, "_gh_budget", unavailable)
    assert _quota.admit(["pr", "view", "1"]) is None
    assert "ledger unavailable, admitting" in capsys.readouterr().err


def _budget_raising(monkeypatch, exc):
    from fno.rust_binary import VerbUnavailable

    monkeypatch.setattr(_quota, "admit", _REAL_ADMIT)

    def unavailable(payload):
        raised = VerbUnavailable(exc.pop("message"))
        returncode = exc.get("returncode")
        if returncode is not None:
            raised.returncode = returncode
        raise raised

    monkeypatch.setattr(_quota, "_gh_budget", unavailable)


def test_admit_refuses_when_the_reader_was_signalled(monkeypatch):
    monkeypatch.setenv("FNO_AGENT_SELF", "quota-test-worker")
    _budget_raising(
        monkeypatch,
        {"message": "fno-agents fleet-incident exited -9: ", "returncode": -9},
    )
    refusal = _quota.admit(["pr", "view", "1"])
    assert refusal is not None
    assert "-9" in refusal
    assert "signalled" in refusal


def test_admit_fails_open_for_a_user_when_the_reader_was_signalled(monkeypatch):
    # The distress refusal holds agent fan-out only. A user at the terminal
    # is never held, even by a dead budget.
    monkeypatch.delenv("FNO_AGENT_SELF", raising=False)
    monkeypatch.delenv("CLAUDECODE", raising=False)
    monkeypatch.delenv("CLAUDE_CODE_SESSION_ATTENDED", raising=False)
    _budget_raising(
        monkeypatch,
        {"message": "fno-agents fleet-incident exited -9: ", "returncode": -9},
    )
    assert _quota.admit(["pr", "view", "1"]) is None


def test_a_drained_probe_answer_is_never_cached(tmp_path):
    calls = []
    cache = tmp_path / "quota-probe.json"
    _quota.execute_graphql(
        "discretionary",
        ["api", "graphql", "-f", "query={viewer{login}}"],
        runner=_runner(0, calls),
        real_gh="/real/gh",
        lock_path=tmp_path / "quota.lock",
        probe_cache=cache,
    )
    assert not cache.exists(), "a drained bucket must keep probing live"


def test_a_stale_cache_row_probes_live_again(tmp_path):
    calls = []
    cache = tmp_path / "quota-probe.json"
    cache.write_text(json.dumps({"ts": time.time() - 3600, "remaining": 5000, "reset": 1787072400}))
    _quota.execute_graphql(
        "discretionary",
        ["api", "graphql", "-f", "query={viewer{login}}"],
        runner=_runner(5000, calls),
        real_gh="/real/gh",
        lock_path=tmp_path / "quota.lock",
        probe_cache=cache,
    )
    assert any(c[-2:] == ["api", "rate_limit"] for c in calls), calls


def test_refusal_kind_classifies_github_stderr_in_dependency_order():
    # The order matters: the secondary and abuse messages both contain the
    # plain "rate limit" phrase, so the specific phrases must win first.
    assert _quota._refusal_kind("gh: You have exceeded a secondary rate limit [HTTP 403]") == "secondary"
    assert _quota._refusal_kind("gh: abuse detection mechanism triggered [HTTP 403]") == "abuse"
    assert _quota._refusal_kind("gh: API rate limit exceeded [HTTP 403]") == "core"
    assert _quota._refusal_kind("gh: something else went wrong [HTTP 403]") == "unknown"
