"""Tests for the lifted source-ahead write fence (x-3d21 R1).

The fence's registry behavior is covered where it is bound, in
`test_registry_schema_guard.py`, which passes unchanged across the lift. What
lives here is the mechanism itself: the source discriminator that moved with
it, and the measured reason it reaches no state file beyond the registry.
"""
from __future__ import annotations

from pathlib import Path

import pytest

from fno import state_fence


class _Boom(RuntimeError):
    pass


def _call(**over: object) -> None:
    kwargs: dict = {
        "target": Path("/shared/registry.json"),
        "shared": Path("/shared/registry.json"),
        "on_disk_version": 19,
        "code_version": 20,
        "source_root": Path("/checkout"),
        "error": _Boom,
        "what": "registry",
        "remedy": "point this checkout at its own registry",
    }
    kwargs.update(over)
    state_fence.refuse_source_ahead_write(**kwargs)  # type: ignore[arg-type]


# --------------------------------------------------------------------------
# The three conditions, one control each
# --------------------------------------------------------------------------


def test_a_source_ahead_write_to_the_shared_file_is_refused() -> None:
    """The positive control. The marker is the refusal naming the shared target."""
    with pytest.raises(_Boom) as exc:
        _call()

    message = str(exc.value)
    assert "/shared/registry.json" in message
    assert "/checkout" in message
    assert "fno doctor update" in message


def test_an_absent_version_is_not_a_raise() -> None:
    """AC7. Refusing here would leave a torn file unrepairable by the command
    meant to rewrite it."""
    _call(on_disk_version=None)
    _call(on_disk_version="20")  # type: ignore[arg-type]


def test_a_worktrees_git_file_counts_as_a_checkout(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """A linked worktree's ``.git`` is a FILE. ``is_dir()`` would miss every
    worktree, which is the only place this ever happens."""
    root = tmp_path / "checkout"
    module = root / "cli" / "src" / "fno" / "state_fence.py"
    module.parent.mkdir(parents=True)
    module.write_text("", encoding="utf-8")
    (root / ".git").write_text("gitdir: /elsewhere/.git/worktrees/x\n", encoding="utf-8")
    monkeypatch.setattr(state_fence, "__file__", str(module))
    state_fence.running_from_source.cache_clear()

    assert state_fence.running_from_source() == root

    state_fence.running_from_source.cache_clear()


def test_the_registry_still_reaches_the_discriminator_under_its_own_name(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """`registry._running_from_source` is the hook existing tests patch. The
    lift must not quietly break it into a second, unpatchable copy."""
    import fno.agents.registry as reg

    assert reg._running_from_source is state_fence.running_from_source
