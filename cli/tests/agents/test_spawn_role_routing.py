"""`fno agents spawn` routing detectors: the Python-only flags keep a spawn on
the Python door. ``--promote`` is not one of them: the Rust client's promote
door (``spawn_promote``) owns a team spawn, so these detectors pin the flags
that still divert.
"""
from __future__ import annotations


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
    # Non-matching flags and other verbs do not trip them.
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
    # Other verbs never route.
    assert not _is_dispatch_account_bearing_spawn(
        "role", ["role", "w", "--dispatch-account", "ci"]
    )

