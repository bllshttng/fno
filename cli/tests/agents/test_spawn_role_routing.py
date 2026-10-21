"""`fno agents spawn --promote` routing: load-bearing on the autonomous substrates.

Grant-at-spawn (``--promote level=N,scope=X``) is implemented only in the Python
``cmd_spawn`` path; the Rust client does not parse ``--promote``. But pane is the
DEFAULT substrate and pane spawns already divert to Python via
``_is_pane_substrate_spawn`` - so a BARE ``spawn --promote`` already works. The
defect lives only on the autonomous substrates (``--substrate bg`` and the
headless spellings ``--substrate headless``/``--once``/``-o``), where
``_is_pane_substrate_spawn`` returns False and the spawn would otherwise reach
the Rust client and die with ``unknown flag: --promote``. Those are exactly the
substrates where succession matters most (an stepping_down lead grants on a worker).

So these tests do NOT exercise a bare spawn (that passes before AND after the
fix and proves nothing). They exercise the autonomous substrates and assert the
role detector is what diverts them - the pane exclusion does not.
"""
from __future__ import annotations



def test_role_bearing_spawn_detected_across_forms() -> None:
    from fno.agents.rust_runtime import _is_promotion_bearing_spawn

    assert _is_promotion_bearing_spawn(
        "spawn", ["spawn", "--name", "w", "--promote", "level=1,scope=x-d7e4"]
    )
    assert _is_promotion_bearing_spawn(
        "spawn", ["spawn", "w", "--promote=level=1,scope=x-d7e4"]
    )
    assert not _is_promotion_bearing_spawn("spawn", ["spawn", "w", "-k", "x"])
    # A non-role flag does not trip it; only spawn carries it.
    assert not _is_promotion_bearing_spawn("spawn", ["spawn", "w", "--role", "build"])
    assert not _is_promotion_bearing_spawn("role", ["role", "w", "--promote", "level=1,scope=x"])


def test_resume_and_route_detectors_catch_attached_short_forms() -> None:
    """The -k fix's sibling detectors (-r resume, -P route) must catch the same
    Click attached-short-option form, or a --substrate bg spawn spelled -r<id>
    or -P<vendor> routes to the Rust binary and fails. One _has_flag matcher
    covers all three; a per-detector copy matched -X and -X=V but missed -XV."""
    from fno.agents.rust_runtime import (
        _is_resume_bearing_spawn,
        _is_route_bearing_spawn,
    )

    assert _is_resume_bearing_spawn("spawn", ["spawn", "w", "-rabc"])
    assert _is_resume_bearing_spawn("spawn", ["spawn", "w", "--resume", "abc"])
    assert _is_route_bearing_spawn("spawn", ["spawn", "w", "-Pzai"])
    assert _is_route_bearing_spawn("spawn", ["spawn", "w", "--route=zai/glm"])
    # --provider is the decomposed route spelling; the Rust client would launch
    # it on the primary model.
    assert _is_route_bearing_spawn("spawn", ["spawn", "w", "--provider", "zai"])
    # Non-matching flags and other commands do not trip them.
    assert not _is_resume_bearing_spawn("spawn", ["spawn", "w", "--name", "n"])
    assert not _is_route_bearing_spawn("role", ["role", "w", "-Pzai"])


def test_long_only_detectors_detect_and_respect_argv_boundary() -> None:
    """The detectors with no short form (role, dispatch-account) route
    through the same _has_flag matcher as the short-bearing ones. Two properties
    must hold: they detect their flag in both spellings, and - the bug this pins
    - a flag AFTER the --argv break belongs to the spawned payload, not fno, so
    it must not keep an otherwise-Rustable spawn in Python. _is_dispatch_account
    _bearing_spawn once iterated raw ``args`` past that boundary (every sibling
    used _args_before_argv); migrating it to _has_flag closes that one-of-N gap."""
    from fno.agents.rust_runtime import (
        _is_dispatch_account_bearing_spawn,
        _is_role_bearing_spawn,
    )

    assert _is_role_bearing_spawn("spawn", ["spawn", "w", "--role", "build"])
    assert _is_dispatch_account_bearing_spawn(
        "spawn", ["spawn", "w", "--dispatch-account", "ci"]
    )
    assert _is_dispatch_account_bearing_spawn(
        "spawn", ["spawn", "w", "--dispatch-account=ci"]
    )
    # The load-bearing assertion: a flag after --argv is the payload's, not fno's.
    # dispatch-account is the one that used to read past the boundary.
    for det, flag in (
        (_is_role_bearing_spawn, "--role"),
        (_is_dispatch_account_bearing_spawn, "--dispatch-account"),
    ):
        assert not det("spawn", ["spawn", "w", "--argv", flag, "x"])
    # Other commands never route.
    assert not _is_dispatch_account_bearing_spawn(
        "role", ["role", "w", "--dispatch-account", "ci"]
    )


def test_pane_detector_stops_at_the_passthrough_fence() -> None:
    """x-1caa: a fenced provider token is not fno's flag. Before the fence
    broke this scan, a passthrough ``-p``/``--once``/``--substrate`` flipped
    the pane detector to False and rerouted a pane-default spawn onto the
    Rust binary lane - past the billing guard, the headless-form refusal, and
    the pane-only passthrough refusal (the guard-on-one-path trap)."""
    from fno.agents.rust_runtime import _is_pane_substrate_spawn

    assert _is_pane_substrate_spawn("spawn", ["spawn", "hi", "--", "-p"])
    assert _is_pane_substrate_spawn("spawn", ["spawn", "hi", "--", "--once", "x"])
    assert _is_pane_substrate_spawn("spawn", ["spawn", "hi", "--", "--substrate", "thread"])
    # Pre-fence spellings still count, and the --argv payload boundary holds.
    assert not _is_pane_substrate_spawn("spawn", ["spawn", "hi", "-o"])
    assert not _is_pane_substrate_spawn(
        "spawn", ["spawn", "hi", "--substrate", "headless"]
    )
    assert _is_pane_substrate_spawn("spawn", ["spawn", "hi", "--argv", "-p"])
