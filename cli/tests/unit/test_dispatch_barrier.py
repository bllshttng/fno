"""The spawn-handover claim: the worker inherits it, and a wrong name cannot.

The guard that takes the handover claim is native; its verdicts are pinned by
the Rust suite. What stays here is the Python claims command that a booting
worker calls to inherit the claim.
"""
from typer.testing import CliRunner

# Import the claims CLI module BEFORE any test patches fno.claims.core. Its
# module-level `from .core import acquire_claim` binds whatever core carries at
# first import, so a patch that lands first (as xdist worker ordering can do)
# bakes the stub into the CLI module for the rest of the process, and a later
# handover test in the same worker calls the stub instead of the real acquire.
import fno.claims.cli  # noqa: F401

runner = CliRunner()


def _route_to(monkeypatch, root):
    # Route every claim key (node:/dispatch:) to an isolated tmp root so the real
    # acquire/claim_status machinery runs hermetically: FNO_CLAIMS_ROOT (the
    # documented global-root override) is the one door. It covers anything that
    # resolves through the env at call time, so a module-identity or import-order
    # pathology can never route a CLI acquire to the real shared root.
    monkeypatch.setenv("FNO_CLAIMS_ROOT", str(root))
    # Scrub this process's harness identity, so a fixture acquire stamps no
    # session id: the suite seeds dead-pid holders, and a stamp from the LIVE
    # pytest session lets the classifier's witness leg answer `live` for them.
    from fno.harness_identity import AMBIENT_IDENTITY_ENV

    for name in AMBIENT_IDENTITY_ENV:
        monkeypatch.delenv(name, raising=False)


def test_the_worker_inherits_the_claim_without_a_refusal(monkeypatch, tmp_path):
    """x-cd1e case 6. The handover must not abort the worker, and the key must
    never read free in between - the gap is the whole point of claiming early."""
    _route_to(monkeypatch, tmp_path)
    import os

    from fno.claims.cli import cli as claims_cli
    from fno.claims.core import acquire_claim, claim_status

    # The spawner's pid is not the worker's, so the acquire below can only win
    # through the handover, never as a same-pid reacquire.
    acquire_claim(
        "node:N", "spawn-handover:t-N", ttl_ms=180_000,
        pid=os.getppid(), root=tmp_path,
    )
    r = runner.invoke(
        claims_cli,
        [
            "acquire", "node:N",
            "--holder", "target-session:sid-1",
            "--ttl", "2h",
            "--pid", str(os.getpid()),
            "--handover-from", "spawn-handover:t-N",
        ],
    )
    assert r.exit_code == 0, r.output
    info = claim_status("node:N", root=tmp_path)
    assert info["holder"] == "target-session:sid-1"
    assert info["state"] == "live"


def test_a_wrong_handover_holder_cannot_take_a_live_claim(monkeypatch, tmp_path):
    """Naming the prior holder is the proof. Guessing wrong falls through to an
    ordinary acquire, which refuses a live foreign claim exactly as today."""
    _route_to(monkeypatch, tmp_path)
    import os

    from fno.claims.cli import cli as claims_cli
    from fno.claims.core import acquire_claim, claim_status

    acquire_claim(
        "node:N", "target-session:incumbent", ttl_ms=3_600_000,
        pid=os.getpid(), root=tmp_path,
    )
    # Pin the intruder to a pid DIFFERENT from the incumbent's. An omitted
    # --pid resolves the ambient harness session, which on an identity-less CI
    # runner degrades to this same process: the intruder then reacquires the
    # claim as its own pid (legal) and the refusal under test never fires.
    # getppid() is alive and distinct inside a pytest worker.
    r = runner.invoke(
        claims_cli,
        [
            "acquire", "node:N",
            "--holder", "target-session:intruder",
            "--ttl", "2h",
            "--pid", str(os.getppid()),
            "--handover-from", "spawn-handover:t-guessed",
        ],
    )
    assert r.exit_code == 1, f"{r.output}\n{_barrier_diag(tmp_path)}"
    status = claim_status("node:N", root=tmp_path)
    assert status.get("holder") == "target-session:incumbent", (
        f"{status}\n{_barrier_diag(tmp_path)}"
    )


def _barrier_diag(tmp_path) -> str:
    """What the failure needs to name: where the CLI's root actually pointed.

    x-c319: under the CI changed-packet the acquire has been observed to win
    where the seeded incumbent should refuse it, and the root the test seeded
    is the first suspect. Resolve the SAME doors the test set up, read the
    value NOW, and put the answer in the assertion message.
    """
    import os

    from fno.claims.io import claim_path
    from fno.claims.io import global_claims_dir

    def _seen(label: str, root) -> str:
        """Does a claim FILE exist where this door points, and what holds it?"""
        try:
            path = claim_path("node:N", root=root)
        except Exception as exc:  # noqa: BLE001 - a diagnostic never raises
            return f"{label}=<unresolvable: {type(exc).__name__}>"
        if not path.exists():
            return f"{label}={path}:ABSENT"
        try:
            from fno.claims.core import claim_status

            info = claim_status("node:N", root=root)
            return f"{label}={path}:holder={info.get('holder')}:state={info.get('state')}"
        except Exception as exc:  # noqa: BLE001
            return f"{label}={path}:PRESENT:<unreadable: {type(exc).__name__}>"

    # The doors, each answered SEPARATELY. The earlier version printed the
    # roots and stopped, so a failure could not say whether the seeded claim was
    # missing, in another directory, or present and read as free - three causes
    # with one appearance. A path plus its holder separates them in one run.
    return (
        f"[diag] global={global_claims_dir()} "
        f"env={os.environ.get('FNO_CLAIMS_ROOT')!r} tmp={tmp_path} "
        + " ".join(
            (
                _seen("seeded", tmp_path),
                _seen("default", None),
            )
        )
    )
