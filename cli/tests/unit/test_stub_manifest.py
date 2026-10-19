"""Tests for the stub-manifest module (G3, x-24b7).

Covers the schema (validate/write/load) and the path convention. The
merge-hold lookup moved into the authorized-merge owner
(crates/fno-agents/src/merge_gates.rs); the reconcile verdict stays here.
"""
from __future__ import annotations

import pytest

from fno import stub_manifest as sm


def test_validate_rejects_stub_missing_locators():
    with pytest.raises(sm.StubManifestError):
        sm.validate({"node": "x-1", "stubs": [{"stub_id": "a", "file": ""}]})


# ---- merge-hold lookup ----

def test_verdict_manifest_missing(tmp_path):
    # AC5-FR: no manifest -> refuse (do not finalize a half-real PR).
    v = sm.reconcile_verdict("x-1", tmp_path)
    assert v["outcome"] == sm.MANIFEST_MISSING


def test_verdict_malformed_manifest_is_missing(tmp_path):
    # AC5-FR: a partial/malformed manifest refuses, never crashes.
    p = sm.manifest_path("x-1", tmp_path)
    p.parent.mkdir(parents=True, exist_ok=True)
    p.write_text("{bad", encoding="utf-8")
    assert sm.reconcile_verdict("x-1", tmp_path)["outcome"] == sm.MANIFEST_MISSING


def test_verdict_no_contract_test_is_drift(tmp_path):
    # Locked Decision 5: a missing executable gate REFUSES (never guess).
    sm.write("x-1", [{"stub_id": "a", "file": "f", "kind": "fn"}], tmp_path)
    assert sm.reconcile_verdict("x-1", tmp_path)["outcome"] == sm.DRIFT


def test_verdict_passing_suite_authorizes(tmp_path):
    sm.write("x-1", [{"stub_id": "a", "file": "f", "kind": "fn"}], tmp_path,
             contract_test="true")
    assert sm.reconcile_verdict("x-1", tmp_path)["outcome"] == sm.AUTHORIZE


def test_verdict_failing_suite_is_drift(tmp_path):
    # AC4-ERR: the landed schema fails the contract test -> refuse auto-de-stub.
    sm.write("x-1", [{"stub_id": "a", "file": "f", "kind": "fn"}], tmp_path,
             contract_test="false")
    assert sm.reconcile_verdict("x-1", tmp_path)["outcome"] == sm.DRIFT


# ---- G4: mark_reconciled (de-stub finalize) ----

def test_mark_reconciled_flips_flag_and_preserves_fields(tmp_path):
    sm.write("x-1", [{"stub_id": "a", "file": "f", "kind": "fn"}], tmp_path,
             contract_version=3, contract_ref="d.md#ic", contract_test="pytest -q")
    sm.mark_reconciled("x-1", tmp_path)
    loaded = sm.load(sm.manifest_path("x-1", tmp_path))
    assert loaded["reconciled"] is True
    assert loaded["contract_version"] == 3
    assert loaded["contract_test"] == "pytest -q"
    assert loaded["stubs"][0]["stub_id"] == "a"
