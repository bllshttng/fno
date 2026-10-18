"""fno.cost package marker: the live modules are _register and _session_cost.

The former package API (update, upsert_cost_session, check_budget, the
per-provider caps) had no caller and is deleted; the Rust legs
(crates/fno-agents/src/backlog/cutover.rs, loopcheck/budget.rs) are the only
implementation.
"""
