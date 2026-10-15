"""`fno do target init` refuses a review gate this session cannot satisfy.

PR #618 discovered its unsatisfiable `reviewers: [sigma]` gate at the stop gate,
after the feature was built and pushed, and then misreported it for fifteen
turns. The check under test moves that discovery to the front of the run.

The capability resolution itself (which status a reviewer gets on which
harness, every refusal and reason string) now lives in the native
`review-level capability` door and is pinned by its Rust goldens
(`tests/golden/review_capability/`). What remains here is the CALLER WIRING:
this file pins exit codes, stderr echo, payload plumbing, and the
no-state-on-refusal contract, with the door stubbed at
`fno.rust_binary.verb_call`.
"""
from __future__ import annotations

from pathlib import Path

import pytest
import typer
from typer.testing import CliRunner

from fno.paths import target_state_path
from fno.target_cli import REVIEW_GATE_REFUSED, _refuse_unsatisfiable_reviewers

# Canned door answers. The real door's byte output is golden-pinned; these
# carry just enough shape to drive the caller's wiring.


def _reviewer_row(name: str, status: str, line: str, requires: str = "retired") -> dict:
    return {
        "name": name,
        "status": status,
        "reason": line,
        "line": line,
        "blocks_autonomy": status not in ("satisfiable", "unverifiable"),
        "kind": "local-attestation",
        "requires": requires,
        "asserts": "review-evidence",
        "invocation": "/fno:review",
    }


def _reviewers_answer(rows: list[dict], refusal: str | None) -> dict:
    return {
        "session": {
            "harness": "gemini",
            "substrate": "unknown",
            "attended": False,
            "provider": "unknown",
            "describe": "harness=gemini substrate=unknown provider=unknown",
        },
        "reviewers": rows,
        "local_peers": [],
        "reviewers_refusal": refusal,
        "local_peers_refusal": None,
    }


SIGMA_REFUSAL = (
    "fno do target init: config.review.reviewers cannot be satisfied on "
    "harness=gemini substrate=unknown provider=unknown.\n"
    "  unavailable: sigma - sigma is retired; run `/fno:review` (the default "
    "review lane) or change config.review.reviewers\n"
    "\n"
    "The gate is fail-closed: without a head-pinned review_attestation the "
    "session will block at the stop gate after the work is done.\n"
    "To proceed: change config.review.reviewers."
)

SIGMA_BLOCKED = _reviewers_answer(
    [
        _reviewer_row(
            "sigma",
            "unavailable",
            "unavailable: sigma - sigma is retired; run `/fno:review` (the default "
            "review lane) or change config.review.reviewers",
        )
    ],
    SIGMA_REFUSAL,
)

CODE_REVIEW_OK = _reviewers_answer(
    [
        _reviewer_row(
            "code-review",
            "satisfiable",
            "satisfiable: code-review - run `/fno:review`",
            requires="none",
        )
    ],
    None,
)

SUBAGENT_UNVERIFIABLE = _reviewers_answer(
    [
        _reviewer_row(
            "subagent-only",
            "unverifiable",
            "unverifiable: subagent-only - needs subagent-dispatch; no harness "
            "marker in this environment, so capability cannot be verified. "
            "Proceeding - if the gate does go unmet, run `/fno:review`",
            requires="subagent-dispatch",
        )
    ],
    None,
)

PEERS_BLOCKED = _reviewers_answer(
    [],
    None,
) | {
    "local_peers": [
        {
            "name": "codex",
            "status": "unavailable",
            "reason": "same model family as the author (codex)",
            "line": "unavailable: codex - same model family as the author (codex)",
            "eligible": False,
        }
    ],
    "local_peers_refusal": (
        "fno do target init: config.review.peers has no eligible cross-model "
        "local peer on harness=gemini substrate=unknown provider=unknown."
    ),
}

APPS_BLOCKED = {
    "verdicts": [
        {
            "login": "typo-bot-name",
            "status": "unavailable",
            "reason": "never reviewed or commented in this repository",
            "line": "unavailable: typo-bot-name - never reviewed or commented in "
            "this repository - likely a typo, or a GitHub App not installed here",
            "blocks_autonomy": True,
        }
    ],
    "refusal": (
        "fno do target init: config.review.github_apps names a bot that cannot "
        "review here.\n"
        "  unavailable: typo-bot-name - never reviewed or commented in this "
        "repository - likely a typo, or a GitHub App not installed here\n"
        "\n"
        "The gate is fail-closed: a required App that never reviews wedges the "
        "session until the budget ceiling.\n"
        "To proceed: fix the login in config.review.github_apps; or move it to "
        "config.review.optional_apps (honored-if-present, never waited on)."
    ),
}

APPS_OK = {
    "verdicts": [
        {
            "login": "chatgpt-codex-connector",
            "status": "satisfiable",
            "reason": "recognized review App; footnote knows how to reach it",
            "line": "satisfiable: chatgpt-codex-connector - recognized review App; "
            "footnote knows how to reach it",
            "blocks_autonomy": False,
        }
    ],
    "refusal": None,
}


SUBAGENT_REVIEWER_YAML = (
    "[subagent-only]\n"
    "    reviewer_registry:\n"
    "      subagent-only:\n"
    "        kind: local-attestation\n"
    "        requires: subagent-dispatch\n"
    "        invocation: /fno:review\n"
    "        asserts: review-evidence\n"
)


def _stub_verb_call(monkeypatch: pytest.MonkeyPatch, answers: dict):
    """Stub the door on its argv tail (`reviewers` vs `github-apps`). Returns
    the calls so payload plumbing is assertable."""
    calls: list = []

    def _fake(verb, payload, *args, **kwargs):
        action = verb[-1]
        calls.append((verb, payload))
        answer = answers[action]
        if isinstance(answer, Exception):
            raise answer
        return answer

    monkeypatch.setattr("fno.rust_binary.verb_call", _fake)
    return calls


def _config(tmp_path: Path, monkeypatch: pytest.MonkeyPatch, reviewers: str) -> None:
    cfg = tmp_path / "settings.yaml"
    cfg.write_text(f"schema_version: 1\nconfig:\n  review:\n    reviewers: {reviewers}\n")
    monkeypatch.setenv("FNO_CONFIG", str(cfg))


@pytest.fixture(autouse=True)
def _clear_cache():
    yield


# --- AC1: refuse at init -----------------------------------------------------


def test_init_exits_non_zero_before_touching_the_init_script(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
):
    _config(tmp_path, monkeypatch, "[sigma]")
    monkeypatch.setenv("GEMINI_SESSION_ID", "g1")
    monkeypatch.delenv("CLAUDE_CODE_SESSION_ID", raising=False)
    monkeypatch.setenv("TARGET_UNATTENDED", "1")
    _stub_verb_call(monkeypatch, {"reviewers": SIGMA_BLOCKED, "github-apps": APPS_OK})
    with pytest.raises(typer.Exit) as exc:
        _refuse_unsatisfiable_reviewers()
    assert exc.value.exit_code == 2


def test_satisfiable_reviewer_does_not_refuse(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
):
    _config(tmp_path, monkeypatch, "[code-review]")
    _stub_verb_call(monkeypatch, {"reviewers": CODE_REVIEW_OK, "github-apps": APPS_OK})
    _refuse_unsatisfiable_reviewers()  # no raise


def test_empty_reviewers_is_a_no_op(tmp_path: Path, monkeypatch: pytest.MonkeyPatch):
    _config(tmp_path, monkeypatch, "[]")
    _refuse_unsatisfiable_reviewers()  # no raise; the door is never called


def test_the_payload_carries_peers_and_descriptors(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
):
    """The caller's half of the door contract: reviewers, the descriptor table
    serialized from the registry, peers, and peer_identity all ride the
    payload; the door's answer keys drive the echo."""
    _config(tmp_path, monkeypatch, "[code-review]")
    calls = _stub_verb_call(monkeypatch, {"reviewers": CODE_REVIEW_OK, "github-apps": APPS_OK})
    _refuse_unsatisfiable_reviewers()
    (verb, payload) = calls[0]
    assert verb == ["review-level", "capability", "reviewers"]
    assert payload["reviewers"] == ["code-review"]
    assert payload["descriptors"]["code-review"]["invocation"] == "/fno:review"
    assert payload["peer_identity"] is None


def test_init_refuses_identity_free_peers_when_all_are_same_model(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
):
    cfg = tmp_path / "config.toml"
    cfg.write_text('[review]\npeers = ["codex"]\n')
    monkeypatch.setenv("FNO_CONFIG", str(cfg))
    monkeypatch.setenv("CODEX_THREAD_ID", "c1")
    _stub_verb_call(monkeypatch, {"reviewers": PEERS_BLOCKED, "github-apps": APPS_OK})
    with pytest.raises(typer.Exit) as exc:
        _refuse_unsatisfiable_reviewers()
    assert exc.value.exit_code == 2


def test_init_allows_identity_free_peers_with_cross_model_option(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
):
    cfg = tmp_path / "config.toml"
    cfg.write_text(
        '[review]\npeers = ["codex", '
        '{provider = "claude", model = "zai,glm-5.2"}]\n'
    )
    monkeypatch.setenv("FNO_CONFIG", str(cfg))
    monkeypatch.setenv("CODEX_THREAD_ID", "c1")
    _stub_verb_call(monkeypatch, {"reviewers": CODE_REVIEW_OK, "github-apps": APPS_OK})
    _refuse_unsatisfiable_reviewers()


def test_unverifiable_verdicts_are_echoed_as_notes_not_refusals(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
):
    """An unclassifiable session proceeds, but never silently."""
    _config(tmp_path, monkeypatch, SUBAGENT_REVIEWER_YAML)
    _stub_verb_call(
        monkeypatch,
        {"reviewers": SUBAGENT_UNVERIFIABLE, "github-apps": APPS_OK},
    )
    _refuse_unsatisfiable_reviewers()  # no raise
    err = capsys.readouterr().err
    assert "note target init:" in err
    assert "cannot be verified" in err


def test_unreadable_config_degrades_to_a_no_op(
    monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
):
    """A broken probe must never be the reason bootstrap cannot run - and must
    never be silent about having degraded. Asserting only "no raise" would pass
    against the swallow this replaced."""
    monkeypatch.setattr(
        "fno.config.load_settings", lambda: (_ for _ in ()).throw(RuntimeError("boom"))
    )
    _refuse_unsatisfiable_reviewers()  # no raise
    assert "review capability check skipped" in capsys.readouterr().err


def test_resolution_errors_are_not_swallowed(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
):
    """Only the config read degrades. Once a reviewer is known to be configured,
    guessing "available" is the wedge, so a failing door must propagate."""
    _config(tmp_path, monkeypatch, "[sigma]")
    _stub_verb_call(
        monkeypatch,
        {"reviewers": RuntimeError("probe died"), "github-apps": APPS_OK},
    )
    with pytest.raises(RuntimeError, match="probe died"):
        _refuse_unsatisfiable_reviewers()


# --- the call site itself, not just the helper --------------------------------
#
# Direct calls of `_refuse_unsatisfiable_reviewers` pin the helper; these drive
# the real command so the wiring and the printing are pinned. The door stays
# stubbed: the caller wiring is the contract here, the door's bytes are the
# goldens' contract.


def _invoke_init(tmp_path: Path, monkeypatch: pytest.MonkeyPatch, reviewers: str, env: dict):
    cfg = tmp_path / "settings.yaml"
    cfg.write_text(f"schema_version: 1\nconfig:\n  review:\n    reviewers: {reviewers}\n")
    monkeypatch.setenv("FNO_CONFIG", str(cfg))
    for var in ("CLAUDE_CODE_SESSION_ID", "CLAUDE_SESSION_ID", "CODEX_THREAD_ID",
                "CODEX_SESSION_ID", "GEMINI_SESSION_ID", "TARGET_UNATTENDED",
                "FNO_BG", "FNO_AGENT_SELF"):
        monkeypatch.delenv(var, raising=False)
    for k, v in env.items():
        monkeypatch.setenv(k, v)
    from fno.cli import app

    return CliRunner().invoke(app, ["do", "target", "init", "--input", "some-feature"])


def test_init_command_refuses_and_prints_the_reason(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
):
    """The refusal must reach the output from the real command, not merely sit
    in the canned answer."""
    _stub_verb_call(monkeypatch, {"reviewers": SIGMA_BLOCKED, "github-apps": APPS_OK})
    r = _invoke_init(
        tmp_path, monkeypatch, "[sigma]",
        {"GEMINI_SESSION_ID": "g1", "TARGET_UNATTENDED": "1"},
    )
    assert r.exit_code == 2
    assert "sigma" in r.output
    assert "change config.review.reviewers" in r.output


def test_init_command_refuses_before_the_bootstrap_script_runs(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
):
    """The check must sit ahead of `_resolve_init_script()`. Point the plugin
    root at an empty dir: if the check ran second, the missing-script refusal
    (exit 2, a different message) would win instead."""
    monkeypatch.setenv("CLAUDE_PLUGIN_ROOT", str(tmp_path / "empty-plugin-root"))
    _stub_verb_call(monkeypatch, {"reviewers": SIGMA_BLOCKED, "github-apps": APPS_OK})
    r = _invoke_init(
        tmp_path, monkeypatch, "[sigma]",
        {"GEMINI_SESSION_ID": "g1", "TARGET_UNATTENDED": "1"},
    )
    assert r.exit_code == 2
    assert "sigma" in r.output
    assert "needs the footnote plugin" not in r.output


def test_init_command_prints_the_unverifiable_note(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
):
    """An unclassifiable session proceeds, but never silently."""
    monkeypatch.setenv("CLAUDE_PLUGIN_ROOT", str(tmp_path / "empty-plugin-root"))
    _stub_verb_call(
        monkeypatch, {"reviewers": SUBAGENT_UNVERIFIABLE, "github-apps": APPS_OK}
    )
    r = _invoke_init(tmp_path, monkeypatch, SUBAGENT_REVIEWER_YAML, {})
    assert "note target init:" in r.output
    assert "cannot be verified" in r.output
    # It got past the reviewer check and died on the absent plugin root instead.
    assert "needs the footnote plugin" in r.output


def test_init_command_writes_no_state_when_it_refuses(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
):
    """"No work begins" is the half of AC1 the message assertions cannot see:
    a refusal that still stamped a manifest would leave a claimed node behind."""
    repo = tmp_path / "repo"
    (repo / ".fno").mkdir(parents=True)
    monkeypatch.chdir(repo)

    _stub_verb_call(monkeypatch, {"reviewers": SIGMA_BLOCKED, "github-apps": APPS_OK})
    r = _invoke_init(
        tmp_path, monkeypatch, "[sigma]",
        {"GEMINI_SESSION_ID": "g1", "TARGET_UNATTENDED": "1"},
    )

    assert r.exit_code == 2
    # Absence at BOTH resolutions: the refusal runs before the script, so if it
    # regressed, the script would write the space path and a legacy-only check
    # would still pass (conftest pins FNO_SPACES_DIR for this process).
    assert not (repo / ".fno" / "target-state.md").exists()
    assert not target_state_path(repo).exists()


# --- AC12: github_apps axis ---------------------------------------------------
#
# The App axis reaches the same two refusal helpers through the same door; the
# stub dispatches on the argv tail, and the wiring assertions (exit code, the
# echo, the ordering) stay here.


def _invoke_init_apps(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, github_apps: str, env: dict
):
    cfg = tmp_path / "settings.yaml"
    cfg.write_text(
        f"schema_version: 1\nconfig:\n  review:\n    github_apps: {github_apps}\n"
    )
    monkeypatch.setenv("FNO_CONFIG", str(cfg))
    for var in ("CLAUDE_CODE_SESSION_ID", "CLAUDE_SESSION_ID", "CODEX_THREAD_ID",
                "CODEX_SESSION_ID", "GEMINI_SESSION_ID", "TARGET_UNATTENDED",
                "FNO_BG", "FNO_AGENT_SELF"):
        monkeypatch.delenv(var, raising=False)
    for k, v in env.items():
        monkeypatch.setenv(k, v)
    from fno.cli import app

    return CliRunner().invoke(app, ["do", "target", "init", "--input", "some-feature"])


def test_init_command_refuses_a_typo_github_app(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
):
    """AC12 end to end: a typo'd github_apps login never seen in the repo exits
    non-zero from the real command, before any bootstrap script runs."""
    _stub_verb_call(monkeypatch, {"reviewers": CODE_REVIEW_OK, "github-apps": APPS_BLOCKED})
    r = _invoke_init_apps(tmp_path, monkeypatch, "[typo-bot-name]", {})
    assert r.exit_code == 2
    assert "typo-bot-name" in r.output
    assert "config.review.optional_apps" in r.output


def test_init_command_allows_a_recognized_github_app(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
):
    """A recognized App must pass the github_apps check (and then fail later on
    the absent plugin root, proving it got past this gate, not that it refused)."""
    monkeypatch.setenv("CLAUDE_PLUGIN_ROOT", str(tmp_path / "empty-plugin-root"))
    _stub_verb_call(monkeypatch, {"reviewers": CODE_REVIEW_OK, "github-apps": APPS_OK})
    r = _invoke_init_apps(tmp_path, monkeypatch, "[chatgpt-codex-connector]", {})
    assert "config.review.github_apps names a bot" not in r.output


def _invoke_gate(tmp_path: Path, monkeypatch: pytest.MonkeyPatch, review_block: str,
                 env: dict | None = None):
    cfg = tmp_path / "settings.yaml"
    cfg.write_text(f"schema_version: 1\nconfig:\n  review:\n{review_block}")
    monkeypatch.setenv("FNO_CONFIG", str(cfg))
    for var in ("CLAUDE_CODE_SESSION_ID", "CLAUDE_SESSION_ID", "CODEX_THREAD_ID",
                "CODEX_SESSION_ID", "GEMINI_SESSION_ID", "TARGET_UNATTENDED",
                "FNO_BG", "FNO_AGENT_SELF"):
        monkeypatch.delenv(var, raising=False)
    for k, v in (env or {}).items():
        monkeypatch.setenv(k, v)
    from fno.cli import app

    return CliRunner().invoke(app, ["do", "target", "check-review-gate"])


def test_check_review_gate_is_silent_and_zero_on_an_empty_config(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
):
    """AC6-HP. Both refusals return early on an empty list, so a repo that
    configures no review at all pays nothing and hears nothing."""
    r = _invoke_gate(tmp_path, monkeypatch, "    reviewers: []\n    github_apps: []\n")
    assert r.exit_code == 0
    assert r.output.strip() == ""


def test_check_review_gate_refuses_an_unsatisfiable_reviewer(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
):
    """The verb re-stamps the wrapper's exit 2 as its own refusal code, which is
    the only code the shell caller treats as a refusal."""
    _stub_verb_call(monkeypatch, {"reviewers": SIGMA_BLOCKED, "github-apps": APPS_OK})
    r = _invoke_gate(
        tmp_path, monkeypatch, "    reviewers: [sigma]\n",
        {"GEMINI_SESSION_ID": "g1", "TARGET_UNATTENDED": "1"},
    )
    assert r.exit_code == REVIEW_GATE_REFUSED
    assert "sigma" in r.output
    assert "change config.review.reviewers" in r.output


def test_check_review_gate_refuses_a_typo_github_app(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
):
    """The App axis reaches the verb too - it is the axis PR #638 added and the
    one the direct path never checked."""
    _stub_verb_call(monkeypatch, {"reviewers": CODE_REVIEW_OK, "github-apps": APPS_BLOCKED})
    r = _invoke_gate(tmp_path, monkeypatch, "    github_apps: [typo-bot-name]\n")
    assert r.exit_code == REVIEW_GATE_REFUSED
    assert "typo-bot-name" in r.output


def test_check_review_gate_checks_apps_even_when_reviewers_pass(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
):
    """Ordering pin: a satisfiable reviewer must not short-circuit the App axis.
    Returning after the first clean verdict would restore half the gap."""
    _stub_verb_call(monkeypatch, {"reviewers": CODE_REVIEW_OK, "github-apps": APPS_BLOCKED})
    r = _invoke_gate(
        tmp_path, monkeypatch,
        "    reviewers: [code-review]\n    github_apps: [typo-bot-name]\n",
        {"CLAUDE_CODE_SESSION_ID": "s1"},
    )
    assert r.exit_code == REVIEW_GATE_REFUSED
    assert "typo-bot-name" in r.output


def test_init_marks_the_gate_as_already_run_for_the_script(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
):
    """AC3-CON, wrapper half: `init` hands the script FNO_TARGET_INIT_GATED=1,
    so the script skips a gate pass that already happened. Without it the App
    probe pays two 30s-timeout `gh` calls per unrecognized login twice."""
    captured: dict = {}

    class _Proc:
        returncode = 0

    def _fake_run(cmd, **kwargs):
        if "env" in kwargs:
            captured["env"] = kwargs["env"]
        return _Proc()

    monkeypatch.setattr("fno.target_cli.subprocess.run", _fake_run)
    monkeypatch.setattr("fno.target_cli._print_orientation_report", lambda *a, **k: None)
    monkeypatch.setattr("fno.target_cli._maybe_dispatch_work_start", lambda *a, **k: None)
    monkeypatch.setattr("fno.target_cli._maybe_reconcile_lane_slot", lambda *a, **k: None)
    monkeypatch.setattr("fno.target_cli._maybe_check_resume_receipt", lambda *a, **k: None)

    r = _invoke_init(tmp_path, monkeypatch, "[]", {"CLAUDE_CODE_SESSION_ID": "s1"})

    assert r.exit_code == 0, r.output
    assert captured["env"].get("FNO_TARGET_INIT_GATED") == "1"


def test_the_script_reads_the_marker_this_module_writes():
    """AC3-CON, cross-language half. The wrapper test above and the shell test in
    tests/hooks/test_init_review_gate_direct.sh each assert their own side's
    literal, so renaming ONE side leaves both green and silently restores the
    double probe. This is the door's era version of the same cross-check."""
    import fno.target_cli as tc

    repo_root = Path(tc.__file__).resolve().parents[3]
    script = repo_root / "hooks" / "helpers" / "init-target-state.sh"
    if not script.is_file():  # bare `pip install fno` ships no hooks/
        pytest.skip("plugin hooks/ not present in this install")
    text = script.read_text(encoding="utf-8")

    # Anchored to the EXECUTABLE forms, not the bare names. Both names also
    # occur in this block's comments and in its note string, so a whole-file
    # grep for them stays green after the invocation is deleted outright -
    # which is exactly what a mutation run proved.
    assert '"${FNO_TARGET_INIT_GATED:-}" != "1"' in text, (
        "init-target-state.sh must still READ the marker, not merely mention it"
    )
    assert "fno do target check-review-gate && _RG_RC=0" in text, (
        "init-target-state.sh must still INVOKE the gate, not merely name it"
    )
    assert f"-eq {REVIEW_GATE_REFUSED} ]]" in text, (
        f"init-target-state.sh must test for -eq {REVIEW_GATE_REFUSED}; "
        "the shell literal and REVIEW_GATE_REFUSED have drifted apart"
    )


def test_refusal_code_is_one_click_can_never_produce(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
):
    """The regression a real run found. Click exits 2 on UsageError, so an
    installed `fno` predating this verb answers `No such command
    'check-review-gate'` with exit 2. If the refusal spoke 2 as well, the shell
    caller could not tell them apart and every direct bootstrap would hard-refuse
    the moment the CLI fell behind source - a broken gate bricking the mandatory
    path, which is exactly what the fail-open rule forbids."""
    from fno.cli import app

    assert REVIEW_GATE_REFUSED not in (0, 1, 2)

    monkeypatch.setenv("FNO_CONFIG", str(tmp_path / "absent.yaml"))
    stale = CliRunner().invoke(app, ["do", "target", "no-such-verb-x4a60"])
    assert stale.exit_code == 2
    assert stale.exit_code != REVIEW_GATE_REFUSED


def test_init_still_speaks_exit_2_on_a_refusal(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
):
    """AC2-HP. The verb's private exit code must not leak into `fno do target
    init`, whose refusal contract is byte-for-byte unchanged."""
    _stub_verb_call(monkeypatch, {"reviewers": SIGMA_BLOCKED, "github-apps": APPS_OK})
    r = _invoke_init(
        tmp_path, monkeypatch, "[sigma]",
        {"GEMINI_SESSION_ID": "g1", "TARGET_UNATTENDED": "1"},
    )
    assert r.exit_code == 2
