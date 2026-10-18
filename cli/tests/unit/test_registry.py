"""Tests for worker registry (workers.jsonl) and spawn/register-worker subcommands."""

from __future__ import annotations

import json
import multiprocessing
import uuid
from pathlib import Path

from typer.testing import CliRunner

from fno.cli import app

runner = CliRunner()


# Module-top-level helper for multiprocessing.Pool. Closures and methods
# can't be pickled by Pool's worker spawn, so the callable lives here.
# Reference pattern: cli/tests/unit/test_event_log.py::_worker_emit.
def _register_worker_in_pool(args: tuple) -> None:
    """Worker function for cross-process race test.

    Args are passed as a tuple so Pool.map can fan out a single iterable.
    """
    from fno.runtime.registry import register_worker

    workers_file_str, worker_id, task, campaign = args
    register_worker(
        worker_id=worker_id,
        task=task,
        campaign=campaign,
        workers_file=Path(workers_file_str),
    )


# ---------------------------------------------------------------------------
# registry module tests
# ---------------------------------------------------------------------------


def test_ac2_hp_register_worker_atomic_concurrent(tmp_path):
    """Concurrent register_worker calls across PROCESSES produce N complete JSONL lines.

    Previously this used threading.Thread, which is serialized by the GIL
    for bytecode and never actually exercised the filelock's cross-process
    contention path. Port to multiprocessing.Pool so the workers run in
    real subprocesses and the FileLock in register_worker is the only
    thing preventing byte-level interleave on workers.jsonl. (#22)
    """
    workers_file = tmp_path / "workers.jsonl"
    n_workers = 4
    args_list = [
        (str(workers_file), f"worker-{i}", f"task-{i}", "campaign-1") for i in range(n_workers)
    ]

    # spawn, not fork: the pytest worker process carries background threads
    # (xdist, keeper clients), and forking a multithreaded parent can hand the
    # child a locked lock - the 300s pytest-timeout kill this test hit under
    # load. Spawn re-executes the interpreter, so nothing inherited is held.
    ctx = multiprocessing.get_context("spawn")
    with ctx.Pool(processes=n_workers) as pool:
        pool.map(_register_worker_in_pool, args_list)

    lines = workers_file.read_text().strip().splitlines()
    assert len(lines) == n_workers, (
        f"Expected {n_workers} lines, got {len(lines)} - root cause could be "
        f"byte-level interleave (filelock failed) OR a worker that skipped "
        f"acquisition and silently returned without writing. Both are "
        f"failure modes the filelock is supposed to prevent."
    )
    ids = {json.loads(line)["worker_id"] for line in lines}
    assert ids == {f"worker-{i}" for i in range(n_workers)}
    # Each line must be valid JSON (no interleaved bytes).
    for line in lines:
        json.loads(line)


# ---------------------------------------------------------------------------
# register-worker subcommand (CLI)
# ---------------------------------------------------------------------------


def test_register_worker_cli(tmp_path):
    """fno agents workspace register-worker --id X appends to workers.jsonl."""
    workers_file = tmp_path / "workers.jsonl"
    worker_id = str(uuid.uuid4())

    result = runner.invoke(
        app,
        [
            "agents",
            "workspace",
            "register-worker",
            "--id",
            worker_id,
            "--task",
            "some task",
            "--workers-file",
            str(workers_file),
            "--json",
        ],
    )

    assert result.exit_code == 0, f"Output: {result.output}"
    data = json.loads(result.output)
    assert data["status"] == "registered"
    assert data["worker_id"] == worker_id

    lines = workers_file.read_text().strip().splitlines()
    assert len(lines) == 1
    assert json.loads(lines[0])["worker_id"] == worker_id
