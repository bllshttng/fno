import subprocess
from pathlib import Path

import pytest

from tests.unit._front_dev import front_dev_binary


REPO_ROOT = Path(__file__).resolve().parents[3]


def test_unmeasured_refusal_names_only_remedies_the_commands_support():
    evidence = REPO_ROOT / "crates/fno-agents/src/evidence.rs"
    source = evidence.read_text(encoding="utf-8")
    start = source.index('the ruling asserts a code fact')
    end = source.index("Pair a zero with a control", start)
    refusal = source[start:end]

    assert "fno inbox decide" in refusal
    assert "--question-id" in refusal
    assert "fno inbox law set" in refusal
    assert "fno inbox outstanding clear" in refusal
    assert "with no --answer" in refusal

    front = front_dev_binary()
    if front is None:
        raise pytest.skip(
            "compiled fno front binary not present (build with `cargo build "
            "--manifest-path crates/fno/Cargo.toml --bin fno)`"
        )
    helped = subprocess.run(
        [str(front), "inbox", "decide", "--help"],
        capture_output=True,
        text=True,
    )
    assert helped.returncode == 0, helped.stderr
    for option in ("--read", "--question-id"):
        assert option in helped.stdout, f"fno inbox decide must expose {option}"

    # The clear surface answers through the native front door now; the
    # Python mount is gone with the port, so the probe rides the same
    # binary the user does.
    clear = subprocess.run(
        [str(front), "inbox", "outstanding", "clear", "--help"],
        capture_output=True,
        text=True,
    )
    assert clear.returncode == 0, clear.stderr
    assert "--read" not in clear.stdout, "fno inbox outstanding clear does not take --read"
