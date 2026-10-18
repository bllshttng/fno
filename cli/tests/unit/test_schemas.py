"""Tests for fno.schemas."""
import pytest
from pydantic import ValidationError

from fno.schemas import load_schema


def test_ac2_err_unknown_type_rejected():
    """AC2-ERR: load_schema('bogus') raises ValueError."""
    with pytest.raises(ValueError, match="unknown state type"):
        load_schema("bogus")


def test_fno_id_backfilled_from_legacy_session_id():
    """AC1-HP boundary: a pre-rename manifest (session_id only) back-fills fno_id."""
    Schema = load_schema("target")
    instance = Schema(session_id="20260421T093631Z-97817-920dac")
    assert instance.fno_id == "20260421T093631Z-97817-920dac"
    assert instance.session_id == "20260421T093631Z-97817-920dac"


def test_harness_backfilled_from_legacy_provider_and_session_fields():
    """A pre-rename manifest (provider + claude_session_id) resolves the harness_* block."""
    Schema = load_schema("target")
    inst = Schema.model_validate({
        "provider": "claude", "provider_mode": "standard",
        "claude_session_id": "03401fb3-aaaa-bbbb-cccc-ddddeeeeffff",
        "codex_thread_id": "null",
    })
    assert inst.harness == "claude"
    assert inst.harness_mode == "standard"
    assert inst.harness_session_id == "03401fb3-aaaa-bbbb-cccc-ddddeeeeffff"


def test_harness_wins_conflict_and_syncs_legacy():
    """Canonical harness is authoritative: a conflicting legacy provider is
    overwritten, and the matching per-provider session key is synced."""
    Schema = load_schema("target")
    inst = Schema.model_validate({
        "harness": "claude", "provider": "codex",  # conflict
        "harness_session_id": "sync-me",
    })
    assert inst.harness == "claude"
    assert inst.provider == "claude"  # legacy synced from canonical, not left "codex"


def test_session_id_accepts_canonical_codex_thread_uuid():
    Schema = load_schema("target")
    sid = "019f48e1-e641-7170-9ea9-921f07021967"
    assert Schema(session_id=sid).session_id == sid


@pytest.mark.parametrize(
    "sid",
    [
        "019f48e1-e641-7170-9ea9",
        "019F48E1-E641-7170-9EA9-921F07021967",
    ],
)
def test_session_id_rejects_noncanonical_codex_thread_strings(sid):
    Schema = load_schema("target")
    with pytest.raises(ValidationError):
        Schema(session_id=sid)


@pytest.mark.parametrize(
    "sid",
    [
        "20260630T192705Z-cl52366-8979b6",  # provider infix (self-mint, claude)
    ],
)
def test_session_id_accepts_provenance_infix(sid):
    """The 2-char provenance infix glued to the pid (segment 2) validates."""
    Schema = load_schema("target")
    assert Schema(session_id=sid).session_id == sid
