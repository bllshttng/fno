"""Status-vocabulary tests that live beside the old logs command file.

The logs tests that shared this file became goldens in
crates/fno-agents/tests/golden/logs and Rust follow tests in
crates/fno-agents/src/logs.rs. What stays here is the status-vocabulary
contract: every word the list renderer produces must be filterable.
"""
def test_agent_status_filter_is_the_rendered_activity_vocabulary():
    from fno.agents.cli import AgentStatusFilter

    # x-c672 (AC7): served activity. The `live` token is gone.
    # x-e594: `refused` is the arm for a row whose last assistant turn is a
    # provider refusal, above writing/quiet/parked and below orphaned.
    assert {m.value for m in AgentStatusFilter} == {
        "writing", "quiet", "parked", "orphaned", "refused", "unknown",
    }


def test_every_word_the_renderer_produces_is_a_filterable_status():
    """The parity above is a hand-kept list, so exercise the renderer too: a
    word `rendered_activity` can return that `--status` cannot name is a filter
    nobody can reach, and the hardcoded set alone never catches one."""
    from fno.agents.cli import AgentStatusFilter
    from fno.agents.reachability import UNKNOWN, UNREACHABLE, rendered_activity

    words = {
        rendered_activity(
            truth_state=state,
            age_s=age,
            reachability=reach,
            provider_refusal=refusal,
        )
        for state in (None, "working", "done", "stalled", "your-move")
        for age in (None, 1, 10_000)
        for reach in ("reachable", UNKNOWN, UNREACHABLE)
        for refusal in (None, "provider_4xx_quota")
    }
    filterable = {m.value for m in AgentStatusFilter}
    assert words <= filterable, f"unfilterable: {sorted(words - filterable)}"
