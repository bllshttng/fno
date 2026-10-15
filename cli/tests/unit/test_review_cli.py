"""``fno do review classify`` - the wiring the producers share.

The classifier, the record builder, and the truncation bounds are native
(``review-level findings record``); the goldens in
``crates/fno-agents/tests/golden/review_findings/`` carry their live
behavior. What stays Python-side is the wiring: the refusals classify
prints, the ``--review-round`` stamp, and the attest flow.
"""
from __future__ import annotations

import json

import pytest


def _finding(n: int, **overrides) -> dict:
    base = {
        "category": "correctness",
        "file": f"f{n}.py",
        "line": n,
        "summary": f"surviving detail {n}",
        "failure_scenario": f"scenario {n}",
    }
    base.update(overrides)
    return base


def _seed_record(monkeypatch: pytest.MonkeyPatch, record: dict) -> None:
    """Hand ``classify`` a canned record: the door read is the native
    classifier's contract, already pinned by the goldens, and these tests
    run without the binary."""
    from fno.review import cli as review_cli

    monkeypatch.setattr(
        review_cli, "build_emit_record", lambda findings_file: dict(record)
    )


class TestClassifyReviewRoundOption:
    """The ``--review-round`` flag: the declared scope reaches the record the
    emitter (and, through ``_ATTEST_RECORD_KEYS``, the attested row) reads."""

    def _invoke(self, tmp_path, monkeypatch, payload, *extra, record=None):
        from typer.testing import CliRunner

        from fno.review.cli import review_app

        findings = tmp_path / "findings.json"
        findings.write_text(json.dumps(payload), encoding="utf-8")
        _seed_record(monkeypatch, record or {"findings_blocking": 1, "findings_nonblocking": 0, "findings": []})
        return CliRunner().invoke(
            review_app,
            ["classify", "--findings-file", str(findings), "--emit-record", *extra],
        )

    def test_the_flag_decides_the_round_stamp(self, tmp_path, monkeypatch) -> None:
        """One contract, two arms: the flag stamps, its absence does not."""
        r = self._invoke(tmp_path, monkeypatch, [_finding(1)], "--review-round", "1")
        assert r.exit_code == 0, r.output
        assert json.loads(r.output)["review_round"] == 1
        r = self._invoke(tmp_path, monkeypatch, [_finding(1)])
        assert r.exit_code == 0, r.output
        assert "review_round" not in json.loads(r.output)

    def test_flag_overrides_the_payload_round(self, tmp_path, monkeypatch) -> None:
        r = self._invoke(
            tmp_path,
            monkeypatch,
            {"findings": [_finding(1)], "review_round": 3},
            "--review-round",
            "1",
            record={
                "findings_blocking": 1,
                "findings_nonblocking": 0,
                "findings": [],
                "review_round": 3,
            },
        )
        assert r.exit_code == 0, r.output
        assert json.loads(r.output)["review_round"] == 1

    def test_negative_round_refuses(self, tmp_path, monkeypatch) -> None:
        r = self._invoke(tmp_path, monkeypatch, [_finding(1)], "--review-round", "-1")
        assert r.exit_code == 2, r.output


def _mock_attest_world(monkeypatch, tmp_path):
    """Mock everything ``_attest_from_record`` reads outside the record itself.

    The cwd resolves branch ``review/pr-1`` (the reviewer-worktree shape); the
    row's branch field is what the caller resolved, the hold release is what
    the hook keyed. Returns (captured row data, ordered call markers).
    """
    import fno.events as events_mod
    from fno.review import cli as review_cli

    captured: dict = {}
    calls: list[str] = []
    real_build = events_mod._build

    def spy_build(kind, source, data):
        captured.update(data)
        return real_build(kind, source, data)

    git = {
        ("symbolic-ref", "--short", "refs/remotes/origin/HEAD"): "origin/main",
        ("merge-base", "HEAD", "origin/main"): "base0000",
        ("diff", "--name-only", "base0000..HEAD"): "a.py\n",
        ("diff", "--numstat", "base0000..HEAD"): "3\t1\ta.py\n",
    }
    monkeypatch.setattr(review_cli, "_git_out", lambda *args: git.get(tuple(args), ""))
    monkeypatch.setattr(
        "fno.review.invocation._settle_head_pin", lambda cwd: ("head1234", "review/pr-1")
    )
    monkeypatch.setattr(
        "fno.harness_identity.resolve_attester_identity", lambda: ("sess-a", "witness")
    )
    monkeypatch.setattr("fno.paths.resolve_repo_root", lambda *a, **kw: tmp_path)
    monkeypatch.setattr("fno.paths.project_log", lambda *a, **kw: tmp_path / "events.jsonl")
    monkeypatch.setattr("fno.events._build", spy_build)
    monkeypatch.setattr("fno.events.append_event", lambda *a, **kw: calls.append("append"))
    monkeypatch.setattr("fno.events.cli.mirror_to_global_log", lambda *a, **kw: None)
    monkeypatch.setattr(
        "fno.pr._review_hold.release_review_hold",
        lambda branch, **kw: calls.append(f"release:{branch}"),
    )
    return captured, calls


class TestAttestBranchOverride:
    """`FNO_ATTEST_BRANCH` overrides the attested ROW's branch field only.

    The shell producer resolves the PR branch with its upstream rewrite; the
    verb must record it without re-deriving a local name from cwd. The hold
    join and release stay keyed on the cwd-resolved name the hook used.
    """

    def test_override_replaces_the_row_branch_hold_keeps_cwd(self, tmp_path, monkeypatch) -> None:
        from fno.review import cli as review_cli

        captured, calls = _mock_attest_world(monkeypatch, tmp_path)
        verdict = review_cli._attest_from_record(
            {"findings_blocking": 0, "findings_nonblocking": 0, "findings": []},
            "code-review",
            "unknown",
            tmp_path / "findings.json",
            branch_override="feature/x-pr",
        )
        assert verdict == "pass"
        assert captured["branch"] == "feature/x-pr"
        assert calls == ["append", "release:review/pr-1"]

    def test_no_override_keeps_the_cwd_branch(self, tmp_path, monkeypatch) -> None:
        from fno.review import cli as review_cli

        captured, calls = _mock_attest_world(monkeypatch, tmp_path)
        verdict = review_cli._attest_from_record(
            {"findings_blocking": 0, "findings_nonblocking": 0, "findings": []},
            "code-review",
            "unknown",
            tmp_path / "findings.json",
        )
        assert verdict == "pass"
        assert captured["branch"] == "review/pr-1"
        assert calls == ["append", "release:review/pr-1"]


class TestClassifyBranchEnv:
    """`FNO_ATTEST_BRANCH` rides the attest leg only; the record never carries it.

    The flag-surface ratchet (scripts/ci/check_flag_registry.py) refuses a new
    typer.Option on the attest flow, so the producer-resolved PR branch crosses
    through the environment instead of a flag.
    """

    def _invoke(self, tmp_path, monkeypatch, payload, *extra):
        from typer.testing import CliRunner

        from fno.review.cli import review_app

        findings = tmp_path / "findings.json"
        findings.write_text(json.dumps(payload), encoding="utf-8")
        _seed_record(monkeypatch, {"findings_blocking": 1, "findings_nonblocking": 0, "findings": []})
        return CliRunner().invoke(
            review_app,
            ["classify", "--findings-file", str(findings), "--emit-record", *extra],
        )

    def test_env_alone_does_not_touch_the_record(self, tmp_path, monkeypatch) -> None:
        monkeypatch.setenv("FNO_ATTEST_BRANCH", "feature/x")
        r = self._invoke(tmp_path, monkeypatch, [_finding(1)])
        assert r.exit_code == 0, r.output
        record = json.loads(r.output)
        assert "branch" not in record
        assert record["findings_blocking"] == 1

    def test_classify_attest_sources_the_override_from_the_env(
        self, tmp_path, monkeypatch
    ) -> None:
        """The wiring end to end: classify --attest reads the env, the row
        records the producer-resolved branch, the hold release stays local."""
        from typer.testing import CliRunner

        from fno.review.cli import review_app

        captured, calls = _mock_attest_world(monkeypatch, tmp_path)
        _seed_record(monkeypatch, {"findings_blocking": 1, "findings_nonblocking": 0, "findings": []})
        monkeypatch.setenv("FNO_ATTEST_BRANCH", "feature/x-pr")
        findings = tmp_path / "findings.json"
        findings.write_text(json.dumps([_finding(1)]), encoding="utf-8")
        r = CliRunner().invoke(
            review_app,
            ["classify", "--findings-file", str(findings), "--attest", "code-review"],
            catch_exceptions=False,
        )
        assert r.exit_code == 0, r.output
        assert captured["branch"] == "feature/x-pr"
        assert calls == ["append", "release:review/pr-1"]


class TestAttestRoundStamp:
    """The --verify-fixes round stamp rides the lane's own emit.

    The shell emitter (emit-attestation.sh) stamps review_round when the hold
    flags carry --verify-fixes; the classify emit must carry the same stamp,
    or a verify round reads as a fresh one and rounds_used overruns the cap.
    The derivation lives in the Rust binary; these tests wire the plumbing.
    """

    def _invoke(self, tmp_path, monkeypatch, hold_flags, declared, **kw):
        from typer.testing import CliRunner

        from fno.review.cli import review_app

        captured, calls = _mock_attest_world(monkeypatch, tmp_path)
        _seed_record(monkeypatch, {"findings_blocking": 1, "findings_nonblocking": 0, "findings": []})
        seen = {}

        def fake_claim_status(key):
            return {"metadata": {"invocation_id": "ri-1", "flags": hold_flags}}

        monkeypatch.setattr(
            "fno.claims.core.claim_status", fake_claim_status, raising=True
        )

        def fake_call(verb, args, **kw2):
            seen.update({"verb": verb, "args": args})
            if declared is None:
                return "fno-agents binary not found", None
            # The Rust rule: a round only when the flags carry --verify-fixes.
            flags_json = args[args.index("--flags") + 1]
            if "--verify-fixes" not in json.loads(flags_json):
                return None, None
            return None, {"declared_round": declared}

        monkeypatch.setattr("fno.rust_binary.call_binary_json", fake_call)
        findings = tmp_path / "findings.json"
        findings.write_text(json.dumps([_finding(1)]), encoding="utf-8")
        argv = ["classify", "--findings-file", str(findings), "--attest", "code-review"]
        if kw.get("explicit_round") is not None:
            argv += ["--review-round", str(kw["explicit_round"])]
        r = CliRunner().invoke(review_app, argv, catch_exceptions=False)
        return r, captured, seen

    def test_hold_flags_decide_the_stamp(self, tmp_path, monkeypatch) -> None:
        for hold_flags, declared, expect in (
            (["--verify-fixes", "--comment"], 2, 2),
            (["--comment"], 2, None),
        ):
            r, captured, _seen = self._invoke(
                tmp_path, monkeypatch, hold_flags, declared=declared
            )
            assert r.exit_code == 0, r.output
            if expect is None:
                assert "review_round" not in captured
            else:
                assert captured["review_round"] == expect

    def test_the_stamp_arbitrates(self, tmp_path, monkeypatch) -> None:
        """Explicit --review-round wins; an unresolvable round stamps nothing
        and the emit still lands (fail-open, the emitter's posture)."""
        r, captured, _seen = self._invoke(
            tmp_path, monkeypatch, ["--verify-fixes"], declared=2, explicit_round=5
        )
        assert r.exit_code == 0, r.output
        assert captured["review_round"] == 5
        r, captured, _seen = self._invoke(
            tmp_path, monkeypatch, ["--verify-fixes"], declared=None
        )
        assert r.exit_code == 0, r.output
        assert "review_round" not in captured
        assert captured["verdict"] == "fail"
