"""Build-freeze config (nested under 'config.build'); a file-budget module."""

from __future__ import annotations

from pydantic import BaseModel, ConfigDict, field_validator


class BuildBlock(BaseModel):
    """Build-freeze switch (nested under 'config.build')."""

    model_config = ConfigDict(extra="ignore")

    # While set, the rustc wrapper's doors refuse an agent compile and test
    # run with exit 86 and name CI as the gate; the user's own cargo and the
    # install build still run. The Rust doors read the key directly
    # (crates/fno-agents/src/test_run.rs `agent_freeze_set`); the Python model
    # carries it so `fno config set` can arm what the schema would otherwise
    # call unknown.
    agent_freeze: bool = False

    @field_validator("agent_freeze", mode="before")
    @classmethod
    def _reject_non_bool(cls, v: object) -> object:
        """Match the Rust reader: a TOML bool only. Pydantic would coerce the
        string "yes" to true and read the machine frozen while the doors read
        the same line unset, so a non-bool refuses at load instead."""
        if isinstance(v, bool):
            return v
        raise ValueError("build.agent_freeze is a TOML bool, not a string")
