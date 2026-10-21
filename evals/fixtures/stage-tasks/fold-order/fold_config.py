"""Config fold for spawned target workers.

Precedence: an explicit per-run value wins over user config, which wins
over defaults. The dispatcher carries a stamp of fields it sets on every
run; a stamp entry is only an override when THIS RUN actually chose it.
"""

DEFAULTS = {"auto_merge": False, "effort": "medium"}

# Stamped on every spawned worker run, whether or not the dispatch chose it.
UNCONDITIONAL_STAMP = {"auto_merge": False}


def fold(user_config, per_run_extra=None):
    merged = dict(DEFAULTS)
    merged.update(user_config or {})
    merged.update(UNCONDITIONAL_STAMP)
    merged.update(per_run_extra or {})
    return merged
