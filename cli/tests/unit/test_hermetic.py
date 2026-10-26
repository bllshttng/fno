"""Unit tests for the ambient-state hermeticity primitive.

The load-bearing one is ``test_poisoned_parent_yields_the_clean_child_env``:
the whole dirty lane rests on ``neutralise(poison(E)) == neutralise(E)``, and
if that breaks the lane reports leaks that are really lane skew, people learn
to ignore it, and the instrument becomes worse than nothing. It is asserted
here, before anything depends on it.
"""
from __future__ import annotations

from pathlib import Path

import pytest

from fno.harness_identity import AMBIENT_IDENTITY_ENV
from fno.hermetic import (
    AMBIENT_LEAK_CANARY,
    neutralise,
    poison,
)


@pytest.fixture
def fixtures_dir():
    """The real poison fixture tree, used as the dirty lane uses it."""

    return Path(__file__).resolve().parents[1] / "fixtures" / "ambient-poison"


def test_an_unlisted_fno_var_is_dropped_without_being_named_here(tmp_path):
    """AC1-HP: deny-by-default, so a 125th FNO_ var neutralises for free.

    The name below appears in no allowlist and in no source file; if this test
    ever needs hermetic.py edited to pass, the inversion has regressed back to
    an allowlist and the whole design is gone.
    """
    out = neutralise({"FNO_SOMETHING_NOBODY_HAS_WRITTEN_YET": "x"}, tmp_path)
    assert "FNO_SOMETHING_NOBODY_HAS_WRITTEN_YET" not in out


def test_every_identity_marker_is_dropped(tmp_path):
    """Specimen 1's channel: identity the command itself resolves."""
    env = {name: "live-session" for name in AMBIENT_IDENTITY_ENV}
    out = neutralise(env, tmp_path)
    assert not [n for n in AMBIENT_IDENTITY_ENV if n in out]


def test_config_chain_is_bounded_by_a_ceiling(tmp_path):
    """Specimen 2's channel.

    Dropping the developer's FNO_CONFIG is not enough on its own: the candidate
    chain climbs to the canonical checkout through ``git worktree list``, and
    git ignores HOME. FNO_NO_CANONICAL_CONFIG=1 closes that climb for both
    loaders, and the ceiling still bounds the Rust readers.

    FNO_CONFIG is deliberately NOT re-pinned. Pinning it to one path overrides
    project-local discovery for the whole suite, which breaks the config-writing
    and worktree-policy tests while adding nothing: the sandboxed HOME already
    relocates ~/.fno/config.toml.
    """
    out = neutralise({"FNO_CONFIG": "/home/dev/.fno/config.toml"}, tmp_path)
    assert "FNO_CONFIG" not in out
    assert out["FNO_NO_CANONICAL_CONFIG"] == "1"
    assert str(tmp_path) in out["FNO_CONFIG_SEARCH_ROOT"]


def test_cargo_build_dir_is_pinned_into_the_sandbox(tmp_path):
    """AC15: the pin is env-independent, whatever the parent carries.

    The tracked .cargo/config.toml resolves an unpinned run into the REAL
    ~/.cargo/build, which grew ~30 GiB/day of test orphans; the pin must land
    under the sandbox home for both a poisoned and a clean parent.
    """
    poisoned = neutralise(
        {"HOME": "/home/dev", "CARGO_BUILD_BUILD_DIR": "/elsewhere/{workspace-path-hash}"},
        tmp_path,
    )
    clean = neutralise({"HOME": "/home/dev"}, tmp_path)
    for out in (poisoned, clean):
        assert out["CARGO_BUILD_BUILD_DIR"] == (
            f"{tmp_path / 'home' / '.fno' / 'cargo-build'}/{{workspace-path-hash}}"
        )


def test_runner_configured_vars_survive(tmp_path):
    """CI sets these deliberately; they are configuration, not ambient state."""
    out = neutralise({"FNO_RUST_FRONT": "/w/crates/fno/target/debug/fno"}, tmp_path)
    assert out["FNO_RUST_FRONT"] == "/w/crates/fno/target/debug/fno"


# ---------------------------------------------------------------------------
# Caches: sandboxing HOME must not relocate the toolchain.
# ---------------------------------------------------------------------------


def test_unset_cache_vars_are_pinned_to_the_real_home(tmp_path):
    """AC1-ERR: an UNSET cache var is the dangerous one.

    Left alone it resolves under the sandboxed HOME, and the next cargo step
    rebuilds the world. It has to be pinned explicitly, not merely passed
    through.
    """
    out = neutralise({"HOME": "/home/dev"}, tmp_path)
    assert out["CARGO_HOME"].endswith("/.cargo")
    assert not out["CARGO_HOME"].startswith(str(tmp_path))
    assert out["RUSTUP_HOME"].endswith("/.rustup")
    assert out["XDG_CACHE_HOME"].endswith("/.cache")


# ---------------------------------------------------------------------------
# Git identity: an ambient channel HOME does not close.
# ---------------------------------------------------------------------------


def test_a_commit_gets_a_synthetic_identity_not_the_developers(tmp_path):
    """Both directions, in one probe.

    Reading: a test fixture's commits currently carry the real developer's
    name and email, which `git log` in that fixture can then assert on.
    Depending: exactly one shell harness commits without setting a local
    identity, so it passes on any machine with a ~/.gitconfig and has nothing
    to fall back on when there is none.
    """
    import subprocess

    env = neutralise({}, tmp_path)
    repo = tmp_path / "probe"
    repo.mkdir()

    def git(*args):
        return subprocess.run(
            ["git", *args], cwd=repo, env=env, capture_output=True, text=True
        )

    git("init", "-q")
    (repo / "f").write_text("x")
    git("add", "f")
    assert git("commit", "-q", "-m", "probe").returncode == 0

    author = git("log", "-1", "--format=%an <%ae>").stdout.strip()
    assert author == "fno doctor test <fno-test@localhost>"
    # init.defaultBranch is ambient too: a developer who sets it renames the
    # branch a test just created.
    assert git("rev-parse", "--abbrev-ref", "HEAD").stdout.strip() == "main"


# ---------------------------------------------------------------------------
# The invariant the dirty lane rests on.
# ---------------------------------------------------------------------------


def _representative_env() -> dict:
    """A parent env with the shapes a real runner carries."""
    return {
        "PATH": "/usr/bin:/bin",
        "LANG": "en_US.UTF-8",
        "HOME": "/home/dev",
        "CARGO_HOME": "/opt/cargo",
        "FNO_RUST_FRONT": "/w/fno",
    }


def test_poisoned_parent_yields_the_clean_child_env(tmp_path, fixtures_dir):
    """AC1-PROP: neutralise(poison(E)) == neutralise(E), canary aside.

    This is the dirty lane's entire premise. A green dirty lane means the
    inventory is complete; that claim is only worth anything if the two lanes
    are otherwise identical by construction.
    """
    base = _representative_env()
    clean = neutralise(base, tmp_path)
    dirty = neutralise(poison(base, fixtures_dir), tmp_path)

    clean.pop(AMBIENT_LEAK_CANARY, None)
    dirty.pop(AMBIENT_LEAK_CANARY, None)
    assert clean == dirty


def test_the_canary_is_the_only_difference(tmp_path, fixtures_dir):
    """Positive control, asserted as a property rather than trusted.

    The canary must survive neutralise - that is what lets the dirty lane go
    red - and it must be the ONLY thing that does, or the lane's redness stops
    being attributable.
    """
    base = _representative_env()
    clean = neutralise(base, tmp_path)
    dirty = neutralise(poison(base, fixtures_dir), tmp_path)

    differing = {
        k for k in set(clean) | set(dirty) if clean.get(k) != dirty.get(k)
    }
    assert differing == {AMBIENT_LEAK_CANARY}


# ---------------------------------------------------------------------------
# Guardrails on the API itself.
# ---------------------------------------------------------------------------


def test_neutralise_refuses_without_a_sandbox():
    """Fail closed: a caller that forgets the sandbox must not get a
    half-neutralised env that looks hermetic."""
    with pytest.raises(ValueError, match="sandbox"):
        neutralise({"HOME": "/home/dev"})


def test_neutralise_stamps_a_receipt(tmp_path):
    """A positive marker, so a caller can assert hermeticity was APPLIED
    rather than inferring it from the absence of a leak."""
    assert neutralise({}, tmp_path)["FNO_TEST_HERMETIC"] == "1"
