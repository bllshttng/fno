"""Task-context binding gates (x-59b0).

Pins the Python doors around the native verdicts: the init gate refuses a
declared required binding that fails native revalidation (before any claim),
the resume receipt carries a binding compatibly (legacy = explicitly unbound,
legacy digests unchanged), and corruption refuses by name. The verdict logic
itself is the Rust module's; the journey test drives the binary end to end.
"""

import hashlib
import json
from pathlib import Path

import pytest

from fno.rust_binary import command_call

requires_rust = pytest.mark.dev_build


def _source_bytes(body: str) -> dict:
    return {
        "path": "PLAN.md",
        "content_revision": "b48ba4b8c",
        "content_digest": hashlib.sha256(body.encode()).hexdigest(),
        "byte_size": len(body.encode()),
    }


def _binding_request(tmp_path: Path, body: str = "plan bytes\n") -> dict:
    src = tmp_path / "PLAN.md"
    src.write_text(body, encoding="utf-8")
    return {
        "version": 1,
        "node": "x-59b0",
        "attempt": "20260912T055500Z-test99-abc123",
        "harness": "claude",
        "session": "sess-worker",
        "worktree": str(tmp_path),
        "plan_path": "PLAN.md",
        "plan_digest": hashlib.sha256(body.encode()).hexdigest(),
        "required_constraints": ["Do not widen scope beyond the plan"],
        "required_sources": [_source_bytes(body)],
        "source_bytes": len(body.encode()),
        "payload_bytes": 512,
        "stage": "prepared",
    }


def _write_bound_binding(tmp_path: Path, binding_file: Path) -> dict:
    request = _binding_request(tmp_path)
    answer = command_call("task-context-prepare", {"binding": request})
    assert answer["ok"] is True, answer
    bound = dict(request)
    bound["binding_digest"] = answer["binding_digest"]
    binding_file.write_text(json.dumps(bound), encoding="utf-8")
    return bound


def _gate(node: str, root: str, env: dict | None = None) -> dict:
    request = {"node": node, "root": root}
    if env is not None:
        request["env"] = env
    return command_call("task-context-gate", request)


@requires_rust
def test_declared_but_unreadable_binding_refuses_by_name(tmp_path):
    missing = tmp_path / "gone.json"
    verdict = _gate("x-59b0", str(tmp_path), env={"FNO_TASK_CONTEXT_FILE": str(missing)})
    assert verdict["ok"] is False
    assert verdict["reason"] == "context_binding_unreadable"


@requires_rust
def test_gate_refuses_when_required_source_changed(tmp_path):
    binding_file = tmp_path / "task-context-x-59b0.json"
    _write_bound_binding(tmp_path, binding_file)
    (tmp_path / "PLAN.md").write_text("CHANGED bytes\n", encoding="utf-8")
    verdict = _gate("x-59b0", str(tmp_path), env={"FNO_TASK_CONTEXT_FILE": str(binding_file)})
    assert verdict["ok"] is False
    assert verdict["reason"] == "context_stale_source"

# The receipt transport compatibility tests died with the resume leg: the
# receipt model, its integrity tag, and the door's refusals are the native
# module's contracts now, pinned by the goldens and the characterization test
# in crates/fno-agents/tests/resume_receipt_parity.rs.
