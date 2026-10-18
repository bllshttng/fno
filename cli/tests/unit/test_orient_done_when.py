"""The orienter's `done-when:` line must name every gate loop-check enforces.

The wedge this pins (x-0322): with `config.review.peers = ["codex"]` and no
`peer_identity`, loop-check synthesizes a composite local reviewer `peer` and
holds the loop until a head-pinned `review_attestation` for it exists. The
orienter read `github_apps` only, so it announced `reviewed by [none (PR + CI
only)]` -- the session planned its whole run believing there was no review
gate, shipped, promised, and only then discovered an attestation nothing in its
plan produced.

`reviewers` has the same shape and the same failure, so both are covered here.
"""
from __future__ import annotations

from pathlib import Path

import pytest

from fno.target.orient import _done_when_line


@pytest.fixture
def repo(tmp_path: Path, monkeypatch: pytest.MonkeyPatch):
    """A repo root whose only config layer is its own `.fno/config.toml`."""
    (tmp_path / ".fno").mkdir()
    empty_global = tmp_path / "global.toml"
    empty_global.write_text("")
    monkeypatch.setenv("FNO_GLOBAL_SETTINGS_PATH", str(empty_global))

    def _write(body: str) -> Path:
        (tmp_path / ".fno" / "config.toml").write_text(body)
        return tmp_path

    return _write


def test_no_gate_config_still_reads_pr_and_ci_only(repo):
    root = repo("")
    # A stock install carries PR + CI; the self-review floor for a code payload
    # appends after it (tested below), so assert the base as a prefix.
    assert _done_when_line({}, root).startswith(
        "PR + CI green + reviewed by [none (PR + CI only)]"
    )


def test_identity_free_peers_announce_the_local_peer_gate_and_its_producer(repo):
    # `opencode` arms the gate but `/review peer` cannot drive it, so it is not
    # offered as a choice - naming the one peer that runs is the whole point.
    root = repo('[review]\npeers = ["codex", "opencode"]\n')
    line = _done_when_line({}, root)
    assert "peer -> /fno:review peer codex --attest (cross-model only)" in line, line
    # The old string is the lie: it told the session there was nothing to run.
    assert "none (PR + CI only)" not in line, line


def test_peer_identity_keeps_peers_on_the_github_login_carrier(repo):
    # A shared identity makes every peer a posted-review login, not a local
    # attestation -- loop-check resolves no `peer` reviewer, so neither do we.
    # Asserting only the absence of the local gate would pass while the line
    # still read "none (PR + CI only)" for a repo whose login gate is live, so
    # the login itself must be named.
    root = repo('[review]\npeers = ["codex"]\npeer_identity = "fno-peer-bot"\n')
    line = _done_when_line({}, root)
    assert "peer ->" not in line, line
    assert "reviewed by [fno-peer-bot (post: /fno:review peer <pr#> codex --post; cross-model only)]" in line, line


def test_empty_peer_identity_is_absent_not_configured(repo):
    # loop-check's parser drops an empty peer_identity to None BEFORE the
    # `is_some()` test, so "" leaves the identity-free local gate live. Reading
    # "" as a configured login here would print "none (PR + CI only)" for an
    # armed gate - the wedge this whole file exists to close.
    root = repo('[review]\npeers = ["codex"]\npeer_identity = ""\n')
    line = _done_when_line({}, root)
    assert "peer -> /fno:review peer codex --attest (cross-model only)" in line, line
    assert "none (PR + CI only)" not in line, line


def test_stock_install_announces_the_self_review_floor(repo, monkeypatch):
    # A code payload on a lane-less stock install carries the self-review
    # obligation: the done-when line names it and a verb this harness serves.
    def _stub_invocation(verb, payload, *args, **kwargs):
        return {"invocation": "/fno:review <level> --comment", "harness": "claude"}

    monkeypatch.setattr("fno.rust_binary.verb_call", _stub_invocation)
    root = repo("")
    line = _done_when_line({}, root)
    assert "self-review required for code" in line
    assert "report the literal refusal and stop" in line
    assert "--to-self --raw" not in line
    # An UNBACKED opt-out in the file is revoked at read time (the merge-gating
    # lifetime rule), so the floor clause stays: a line that dropped it would
    # let a stale `false` silence the gate announcement.
    monkeypatch.setenv("FNO_CLAIMS_ROOT", str(root / "claims-root"))
    root = repo("[review]\nself_review_required = false\n")
    line = _done_when_line({}, root)
    assert "self-review required for code" in line
    # A LIVE claim backs the opt-out: the floor clause is genuinely absent.
    from fno.claims import acquire_claim

    acquire_claim(
        "config-optout:review.self_review_required",
        "session-a",
    )
    line = _done_when_line({}, root)
    assert "self-review required for code" not in line
    assert line.startswith("PR + CI green + reviewed by [none (PR + CI only)]")
    # A configured local lane already names the reviewer, so no floor clause.
    root = repo('[review]\nreviewers = ["code-review"]\n')
    line = _done_when_line({}, root)
    assert "self-review required for code" not in line


def test_unreadable_review_config_reads_unknown_not_no_gate(repo):
    # A reviewers typo raises out of the Python validator but still declares a
    # gate loop-check will hold. "none (PR + CI only)" would be the same lie.
    # Only the REVIEW half is unknown: PR + CI green holds whatever the config
    # says, so dropping it would understate the gate in the other direction.
    root = repo('[review]\nreviewers = ["sigmaa"]\n')
    assert _done_when_line({}, root) == (
        "PR + CI green + reviewed by [unknown (config.review unreadable) "
        "| resolve: fno config doctor]"
    )


def test_undrivable_only_peers_name_the_missing_runner_not_a_producer(repo):
    # loop-check arms the composite `peer` gate on any identity-free entry, but
    # `/review peer` matches its provider by name: `opencode` is discarded and
    # the provider silently falls back to codex. Printing it as a producer is
    # the wedge in its loudest form - a command that runs and reviews on a model
    # nobody configured, or is refused outright on a codex-authored session.
    root = repo('[review]\npeers = ["opencode"]\n')
    line = _done_when_line({}, root)
    assert "no /fno:review peer runner for [opencode]" in line, line
    assert "/fno:review peer opencode --attest" not in line, line
    assert "none (PR + CI only)" not in line, line


def test_registry_reviewer_producer_appends_the_emit_step(repo):
    # A built-in carries its own emit; a project-registered reviewer does not,
    # so following its invocation verbatim would review and leave the gate unmet.
    root = repo(
        "[review]\n"
        'reviewers = ["house-panel"]\n'
        '[review.reviewer_registry.house-panel]\n'
        'kind = "local-attestation"\n'
        'requires = "none"\n'
        'invocation = "/house-panel"\n'
        'asserts = "invocation"\n'
    )
    line = _done_when_line({}, root)
    assert (
        "house-panel -> /house-panel, then "
        "bash skills/review/scripts/emit-attestation.sh house-panel"
    ) in line, line


def test_no_external_suppresses_every_login_gate(repo):
    # loop-check skips ALL GitHub-login reads under no_external, so announcing
    # an App bot or an identity-backed peer post sends the session to satisfy a
    # review nothing waits on. Local attestations survive: reviewers_ok is
    # computed independently of that skip.
    root = repo(
        "[review]\n"
        'github_apps = ["chatgpt-codex-connector"]\n'
        'optional_apps = ["some-optional-bot"]\n'
        'peers = ["codex"]\n'
    )
    line = _done_when_line({"no_external": "true"}, root)
    assert "chatgpt-codex-connector" not in line, line
    assert "some-optional-bot" not in line, line
    assert "none (--no-external skips every login gate)" in line, line
    assert "peer -> /fno:review peer codex --attest (cross-model only)" in line, line

