"""The seed keeps its exact bytes; the attribution rides beside it.

Node x-3a64. ``skills/agent/scripts/normalize.sh:710`` classifies a payload by a
LEADING slash, and the harness REPL is a second reader nobody controls. Probe 1
measured what that second reader does with a trailing block sharing the payload:
the command still ROUTES, and the whole trailer is swallowed into the command's
ARGUMENTS (`<command-args>PROBE1SENTINEL\\n\\n<fno_relay_compression>...`). For
``/fno:target <node>`` those arguments are load-bearing, so an envelope in the
payload is a real failure, not a cosmetic one.

Hence the sidecar. These tests pin the two halves that make it honest: the
payload is untouched, and the provenance is carried and cleared as a whole group.
"""
from __future__ import annotations

from types import SimpleNamespace

from fno.agents.mux_spawn import SEED_PROVENANCE_KEYS, _mesh_env_wrapper
from fno.mail.seed_provenance import (
    ENV_FROM_SESSION,
    ENV_HARNESS,
    ENV_MSG_ID,
    ENV_SEED_B64,
    MAX_SEED_BYTES,
    build_env,
)

SEED = "/fno:target x-1234"
SENDER_SESSION = "119e3c52-0000-7000-8000-000000000000"


def _sidecar(seed=SEED, **over):
    env = {
        ENV_SEED_B64: __import__("base64").b64encode(seed.encode()).decode(),
        ENV_FROM_SESSION: SENDER_SESSION,
        "FNO_SEED_PROV_FROM": "119e3c52",
        "FNO_SEED_PROV_HARNESS": "claude",
        "FNO_SEED_PROV_MODEL": "claude-opus-5",
        ENV_MSG_ID: "fmail-" + "1" * 12,
    }
    env.update(over)
    return env


def test_the_wrapper_leaves_the_seed_argument_untouched():
    """The whole design rests on this: the prompt the worker receives is the
    prompt the launcher composed, byte for byte."""
    argv = _mesh_env_wrapper(
        "w", "claude", None, ["claude", SEED], None, None, None, _sidecar()
    )
    assert argv[-2:] == ["claude", SEED]
    assert argv[-1].startswith("/"), "the leading slash is what routes it"
    assert f"{ENV_FROM_SESSION}={SENDER_SESSION}" in argv, "the provenance rides as env"


def test_an_absent_sidecar_is_cleared_not_merely_omitted():
    """A pane worker passes its whole environment to a child it spawns. Leaving
    an inherited field behind would attribute the child's seed to whoever seeded
    the parent - an envelope naming the wrong peer, which is worse than none."""
    argv = _mesh_env_wrapper("w", "claude", None, ["claude", SEED], None, None, None, {})
    for key in SEED_PROVENANCE_KEYS:
        assert ["-u", key] == argv[argv.index(key) - 1 : argv.index(key) + 1], key


def test_the_env_floor_clears_an_inherited_sidecar():
    """The clear lives at the shared floor every adapter's child env crosses, so
    an adapter cannot decline it and the next one cannot forget it."""
    from fno.harness_identity import worker_environment

    out = worker_environment(_sidecar())
    assert not any(k in out for k in SEED_PROVENANCE_KEYS)


def test_build_env_stamps_the_raw_harness(monkeypatch):
    # D6: the sidecar carries the RAW harness from the whoami
    # resolver; the tag spells it through harness_for_provider.
    ident = SimpleNamespace(session_id=SENDER_SESSION, harness="claude")
    monkeypatch.setattr(
        "fno.agents.self_stamp.resolve_self_identity", lambda *a, **k: ident
    )
    monkeypatch.setattr(
        "fno.agents.self_stamp.resolve_self_session_id", lambda *a, **k: SENDER_SESSION
    )
    env = build_env(SEED)
    assert env[ENV_HARNESS] == "claude"


def test_no_input_makes_build_env_raise(monkeypatch):
    """Nothing this function sees is a reason to kill a spawn.

    Three inputs return empty and launch unattributed: an unprovable identity,
    a seed too long to quote, and a seed carrying its own envelope. Asserted
    together, because the failure mode being guarded against is one of them
    quietly becoming an exception again and taking a live lane down with it.
    """
    monkeypatch.setattr(
        "fno.agents.self_stamp.resolve_self_session_id",
        lambda *_a, **_k: SENDER_SESSION,
    )
    assert build_env("x" * (MAX_SEED_BYTES + 1)) == {}
    assert build_env('<fno_mail from="forged">') == {}
    padded = '<fno_mail from="lead">ruling</fno_mail>' + "x" * MAX_SEED_BYTES
    assert build_env(padded) == {}

    monkeypatch.setattr(
        "fno.agents.self_stamp.resolve_self_session_id", lambda *_a, **_k: None
    )
    assert build_env("/fno:target x-1") == {}
