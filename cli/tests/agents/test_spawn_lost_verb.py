"""A spawn payload whose `$fno:` prefix the calling shell ate is refused."""
from __future__ import annotations

import os
import shutil
import subprocess

import pytest

from fno.agents import harness_map
from fno.agents.harness_map import lost_verb_refusal

VERBS = frozenset({"target", "blueprint", "think", "review", "execute", "fix", "pr", "tdd", "law", "ship"})


@pytest.fixture(autouse=True)
def _roster(monkeypatch):
    monkeypatch.setattr(harness_map, "footnote_verbs", lambda: VERBS)


def _expand(shell: str, payload: str) -> str:
    """What `shell` hands fno for a double-quoted `payload`, with no `fno` var set."""
    argv = [shell, "-f", "-c"] if shell == "zsh" else [shell, "--norc", "-c"]
    script = f'printf "%s" "{payload}"'
    env = {"PATH": os.environ.get("PATH", "/usr/bin:/bin")}
    return subprocess.run(argv + [script], env=env, capture_output=True, text=True, check=True).stdout


# One row per mangled shape: bash leaves `:target`, zsh drops the first verb
# letter (`arget`), and a verb mid-sentence is scanned token by token.
@pytest.mark.parametrize("shell,payload", [
    ("bash", "$fno:target x-1"),
    ("zsh", "$fno:target x-1"),
    ("zsh", "do a $fno:blueprint x-1"),
])
def test_a_real_shell_mangling_is_refused(shell, payload):
    if shutil.which(shell) is None:
        pytest.skip(f"{shell} not installed")
    mangled = _expand(shell, payload)
    assert "$fno:" not in mangled, f"control: {shell} did not expand {payload!r}"
    reason = lost_verb_refusal(mangled)
    assert reason is not None, f"{shell} turned {payload!r} into {mangled!r}"
    assert "Single-quote the payload" in reason


def test_the_refusal_names_the_verb_and_the_quoting_fix():
    reason = lost_verb_refusal("arget x-1")
    assert "'arget'" in reason
    assert "'$fno:target ...'" in reason


@pytest.mark.parametrize(
    "payload",
    [
        "$fno:target x-1",
        "use dd to image the disk",
        "note: target the review queue",
    ],
)
def test_an_intact_payload_passes(payload):
    assert lost_verb_refusal(payload) is None


def test_the_target_family_is_caught_with_an_empty_roster(monkeypatch):
    monkeypatch.setattr(harness_map, "footnote_verbs", lambda: frozenset())
    assert lost_verb_refusal("arget x-1") is not None
    assert lost_verb_refusal(":blueprint x-1") is not None


def test_make_context_refuses_before_the_rust_route(monkeypatch):
    from typer.testing import CliRunner

    from fno.agents import rust_runtime
    from fno.agents.cli import agents_app

    monkeypatch.setenv("FNO_AGENTS_RUNTIME", "rust")
    routed = []
    monkeypatch.setattr(rust_runtime, "route_to_rust", lambda args, **k: routed.append(list(args)))
    res = CliRunner().invoke(
        agents_app,
        ["spawn", "--name", "eaten-m", "-H", "codex", "arget x-1", "--substrate", "thread"],
    )
    assert res.exit_code == 2, res.output
    assert "Single-quote the payload" in res.output
    assert routed == []
