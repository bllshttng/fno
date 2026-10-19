"""Per-harness dispatch_command resolution (x-567d).

Each harness resolves to the right worker command: a native skill invocation
where one is verified (claude/agy ``/target``, opencode ``/fno:target``, codex
``$fno:target``). gemini is deprecated (successor: agy) and its dispatch lane is
a loud refusal - no prose brief (x-de43).
"""
from __future__ import annotations

import pytest

from fno.agents.harness_map import (
    DispatchResolveError,
    capabilities,
    normalize_command,
    resolve_dispatch,
)


@pytest.fixture(autouse=True)
def _installed_loop_extensions(monkeypatch):
    """Command-normalization tests are not about the loop gate's install
    state, which is machine environment: the gate itself is exercised in
    test_harness_loop_participation.py."""
    import fno.agents.harness_map as harness_map

    monkeypatch.setattr(harness_map, "_loop_gate_answer", lambda h, c: {"refusal": None})


@pytest.mark.parametrize(
    "harness,expected_prefix",
    [
        ("claude", "/target --no-merge "),
        ("codex", "$fno:target --no-merge "),
        ("opencode", "/fno:target --no-merge "),
    ],
)
def test_builtin_renders_each_command_surface(harness, expected_prefix):
    out = resolve_dispatch(harness=harness, node_id="x-abcd")
    assert out["command"] == f"{expected_prefix}x-abcd"


def test_gemini_dispatch_refused_naming_agy():
    # gemini is deprecated (successor: agy); its dispatch lane is a loud refusal.
    with pytest.raises(DispatchResolveError, match="agy"):
        resolve_dispatch(harness="gemini", node_id="x-abcd")


def test_config_command_overrides_the_per_harness_builtin():
    out = resolve_dispatch(
        harness="codex",
        node_id="x-abcd",
        dispatch_cfg={"command": "$fno:execute {id}"},
    )
    assert out["command"] == "$fno:execute x-abcd"


def test_explicit_command_wins_over_config_and_builtin():
    # The explicit slash template is canonical claude syntax, normalized on the
    # chosen harness (x-f0e2). Precedence is unchanged - explicit still beats
    # the config `$fno:execute`. `custom` names no shipped footnote verb, so
    # the codex normalizer leaves the token literal instead of capturing it
    # into a phantom `$fno:custom` skill.
    out = resolve_dispatch(
        harness="codex",
        node_id="x-abcd",
        command="/custom {id}",
        dispatch_cfg={"command": "$fno:execute {id}"},
    )
    assert out["command"] == "/custom x-abcd"


def test_qualified_dispatch_verb_canonicalizes_before_the_allowlist():
    # US7 review: the team contract sets `dispatch_verb=/fno:target` (every
    # dispatched verb is plugin-qualified). The bare-only allowlist must not
    # refuse it - it canonicalizes to `/target`, then renders per-harness.
    out = resolve_dispatch(harness="claude", node_id="x-abcd", verb="/fno:target")
    assert out["command"] == "/target x-abcd"


def test_template_without_id_is_rejected():
    with pytest.raises(DispatchResolveError):
        resolve_dispatch(harness="claude", node_id="x-abcd", command="no placeholder here")


# --- autonomous pane capabilities ----------------------------------------- #


def test_codex_autonomous_pane_is_capability_allowed_with_thread_default():
    explicit = resolve_dispatch(
        harness="codex",
        substrate="pane",
        node_id="x-abcd",
        trigger="autonomous",
    )

    assert capabilities("codex")["autonomous_pane"] is True
    assert explicit["substrate"] == "pane"
    assert resolve_dispatch(harness="codex", node_id="x-abcd")["substrate"] == "thread"


def test_unverified_harness_autonomous_pane_fails_closed():
    harness = "claude"
    assert capabilities(harness)["autonomous_pane"] is False

    with pytest.raises(
        DispatchResolveError,
        match=rf"harness {harness!r}.*autonomous_pane",
    ):
        resolve_dispatch(
            harness=harness,
            substrate="pane",
            node_id="x-abcd",
            trigger="autonomous",
        )


def test_missing_autonomous_pane_capability_fails_closed(monkeypatch):
    import fno.agents.harness_map as harness_map

    monkeypatch.delitem(harness_map._HARNESS_CAPS["opencode"], "autonomous_pane")

    with pytest.raises(
        DispatchResolveError,
        match=r"harness 'opencode'.*autonomous_pane",
    ):
        resolve_dispatch(
            harness="opencode",
            substrate="pane",
            node_id="x-abcd",
            trigger="autonomous",
        )


def test_malformed_trigger_fails_closed_on_capability_enabled_pane():
    with pytest.raises(DispatchResolveError, match="unknown dispatch trigger"):
        resolve_dispatch(
            harness="codex",
            substrate="pane",
            node_id="x-abcd",
            trigger="autonamous",
        )


def test_opencode_bg_resolves_on_the_spawn_claim_not_the_pane_bit():
    """opencode's thread seat comes from its spawn claim (native, measured
    2026-09-03), never from a pane capability: its autonomous_pane bit is
    false while the bg alias still resolves. A capability on one substrate
    must not leak into another."""
    assert capabilities("opencode")["autonomous_pane"] is False
    out = resolve_dispatch(
        harness="opencode",
        substrate="thread",
        node_id="x-abcd",
        trigger="autonomous",
    )
    assert out["substrate"] == "thread"
    assert out["thread"] is True
    with pytest.raises(DispatchResolveError, match="retired"):
        resolve_dispatch(
            harness="opencode",
            substrate="bg",
            node_id="x-abcd",
            trigger="autonomous",
        )


# --- the normalizer (x-a5e4) ------------------------------------------------ #


def test_normalize_command_is_verb_agnostic_for_codex():
    # ANY footnote /verb -> $fno:verb on codex, not just /target.
    assert normalize_command("/fno:ship pr create", "codex") == "$fno:ship pr create"


# ---------------------------------------------------------------------------
# x-8e59: the builtin rung reads config.dispatch.auto_merge
#
# x-4391 shipped the key to 2 of 3 dispatch paths. The builtin here was the
# deaf one, so an operator who set the key still got `/target --no-merge <id>`,
# a manifest frozen at auto_merge_approved: false, and a refused `fno do pr merge`.
# The two callers that honored it did so by passing an explicit command around
# the builtin rather than fixing it.
# ---------------------------------------------------------------------------


def test_builtin_drops_no_merge_when_auto_merge_configured():
    out = resolve_dispatch(
        harness="codex", node_id="x-abcd", dispatch_cfg={"auto_merge": True}
    )
    assert out["command"] == "$fno:target x-abcd"


def test_non_boolean_auto_merge_never_grants_merge():
    """Granting merge authority is the irreversible direction, so the read is a
    strict identity check. A truthy non-bool is not a grant."""
    out = resolve_dispatch(
        harness="claude", node_id="x-abcd", dispatch_cfg={"auto_merge": "true"}
    )
    assert out["command"] == "/target --no-merge x-abcd"


def test_unreadable_config_fails_safe_to_no_merge(monkeypatch):
    import fno.config as _config

    def _boom(*a, **k):
        raise RuntimeError("config is a smoking crater")

    monkeypatch.setattr(_config, "load_settings", _boom)
    out = resolve_dispatch(harness="claude", node_id="x-abcd")
    assert out["command"] == "/target --no-merge x-abcd"


def test_auto_merge_does_not_touch_an_explicit_command():
    """The posture applies to the builtin only: an explicit command already says
    what to run, and rewriting a caller's own template would be the surprise."""
    out = resolve_dispatch(
        harness="claude",
        node_id="x-abcd",
        command="/target --no-merge --reconcile /tmp/m.md {id}",
        dispatch_cfg={"auto_merge": True},
    )
    assert out["command"] == "/target --no-merge --reconcile /tmp/m.md x-abcd"


def test_decision_receipt_names_the_merge_posture():
    """The receipt is how an operator confirms the key was read at all - the
    bug this closes was invisible precisely because nothing said so."""
    allow = resolve_dispatch(
        harness="claude", node_id="x-abcd", dispatch_cfg={"auto_merge": True}
    )
    deny = resolve_dispatch(harness="claude", node_id="x-abcd", dispatch_cfg={})
    assert "command=builtin(merge)" in allow["decision"]
    assert "command=builtin(no-merge)" in deny["decision"]


def test_partial_settings_object_does_not_drop_auto_merge():
    """A settings stub carrying only `.auto_merge.grant` must still yield it.

    Field access used to be one try block over `d.harness`/`d.substrate`/
    `d.command`, so a settings object missing any one of them raised and threw
    the WHOLE dict away - one absent key silently disabling every other. That is
    the shape of the bug being fixed, so it gets its own test. x-4be1: the
    grant lives OUTSIDE the dispatch block, so a stub with an auto_merge block
    and no dispatch overlay still resolves the grant."""
    import types

    from fno.agents.harness_map import _load_dispatch_cfg

    stub = types.SimpleNamespace(
        auto_merge=types.SimpleNamespace(grant="dispatch")
    )
    assert _load_dispatch_cfg(stub)["auto_merge"] is True


# ---------------------------------------------------------------------------
# x-ebd2: the lifecycle rung. Law d-834b6ff1: difficulty decides at intake
# (no plan, no status to read); the plan's rung decides at re-dispatch. The
# stored dispatch_verb is audit input, never truth.
# ---------------------------------------------------------------------------


def test_intake_lifecycle_answer_resolves_target_and_rides_the_note():
    # The table lives in backlog_ready.rs; the resolver is its client and the
    # note rides the decision trail verbatim.
    out = resolve_dispatch(
        harness="codex",
        node_id="x-abcd",
        lifecycle=("/target", "verb=lifecycle(intake difficulty=low -> /target)"),
    )
    assert out["command"] == "$fno:target --no-merge x-abcd"
    assert out["verb"] == "/target"
    assert any("intake difficulty=low" in d for d in out["decision"])


def test_redispatch_design_rung_declared_target_wins_naming_lifecycle_answer():
    # A declared verb is never silently reconciled away. The decision trail
    # still names what the lifecycle would have answered.
    out = resolve_dispatch(
        harness="claude",
        node_id="x-abcd",
        verb="/fno:target",
        lifecycle=(
            "/target",
            "verb=declared(/target; lifecycle answers /blueprint: plan design)",
        ),
    )
    assert out["command"] == "/target --no-merge x-abcd"
    assert out["verb"] == "/target"
    assert any(
        "verb=declared(/target; lifecycle answers /blueprint: plan design)" in d
        for d in out["decision"]
    )


def _stub_door(monkeypatch, impl):
    """Stub the keeper client: ``impl(method, params)`` answers or raises."""
    import fno.graph.store as store

    class _FakeClient:
        @staticmethod
        def request(method, params):
            assert method == "effective_verb"
            return impl(method, params)

    monkeypatch.setattr(store, "_client_for", lambda path: _FakeClient())


def _refusal_text(rung):
    return (
        f"dispatch verb cannot be derived for node x-abcd: plan rung "
        f"{rung!r} with difficulty '' answers no lifecycle rung"
    )


def test_unanswerable_plan_rung_refuses(monkeypatch):
    from fno.agents.node_dispatch import node_effective_verb

    rung = "done"

    def door(method, params):
        raise RuntimeError(_refusal_text(rung))

    _stub_door(monkeypatch, door)
    with pytest.raises(DispatchResolveError, match=rung) as exc_info:
        node_effective_verb({"id": "x-abcd"})
    assert_refusal_names_subject_and_cites_no_node(exc_info.value)


def assert_refusal_names_subject_and_cites_no_node(exc: DispatchResolveError) -> None:
    # A lead reads the refusal cold: the failing node must be the first
    # identifier in the sentence, and a trailing citation invites a misread
    # (the node that shipped this assertion went to fix the cited node).
    message = str(exc)
    assert "for node x-abcd" in message
    assert message.index("for node x-abcd") < message.index("plan rung")
    assert "(x-" not in message


def test_out_of_family_declared_verb_keeps_declared_precedence():
    # /think is outside the lifecycle table: the door abstains and the node's
    # own declaration rides the allowlist-checked verb rung as declared.
    out = resolve_dispatch(
        harness="claude",
        node_id="x-abcd",
        verb="/think",
        lifecycle=(None, "verb=lifecycle(out-of-family /think; declared precedence holds)"),
    )
    assert out["command"] == "/think x-abcd"
    assert out["verb"] is None
    assert any("out-of-family /think" in d for d in out["decision"])


def test_derived_blueprint_ignores_the_operator_target_template():
    # config.dispatch.command is a target-phase contract; a derived blueprint
    # is a different phase and renders its own verb.
    out = resolve_dispatch(
        harness="claude",
        node_id="x-abcd",
        lifecycle=("/blueprint", "verb=lifecycle(intake difficulty=high -> /blueprint)"),
        dispatch_cfg={"command": "/target --special {id}"},
    )
    assert out["command"] == "/blueprint x-abcd"


def test_stage_table_resolves_the_derived_verb_profile():
    # Locked Decision 4: the resolved verb is the profile key, so a medium
    # planless node reaches agents.profiles.blueprint.provider even though its
    # stored verb (or the default) says target.
    import types

    stub = types.SimpleNamespace(
        agents=types.SimpleNamespace(
            profiles={
                "blueprint": types.SimpleNamespace(provider="codex"),
            }
        ),
        dispatch=None,
    )
    out = resolve_dispatch(
        node_id="x-abcd",
        lifecycle=("/blueprint", "verb=lifecycle(intake difficulty=medium -> /blueprint)"),
        settings=stub,
    )
    assert out["harness"] == "codex"
    assert out["command"] == "$fno:blueprint x-abcd"


def test_normalize_command_gemini_refused():
    with pytest.raises(DispatchResolveError, match="agy"):
        normalize_command("/target --no-merge {id}", "gemini")
