"""Build-freeze config (nested under 'config.build'); a file-budget module."""

from __future__ import annotations

from pydantic import BaseModel, ConfigDict, field_validator


class BuildBlock(BaseModel):
    """Build-freeze switch (nested under 'config.build')."""

    model_config = ConfigDict(extra="ignore")

    # While set, the rustc wrapper's doors refuse an agent compile and test run
    # with exit 86 (crates/fno-agents/src/test_run.rs `agent_freeze_set`); the
    # model carries the key so `fno config set` can arm the freeze.
    agent_freeze: bool = False

    @field_validator("agent_freeze", mode="before")
    @classmethod
    def _reject_non_bool(cls, v: object) -> object:
        """TOML bool only, matching the Rust as_bool() read: coercing a string
        like "yes" to true would report frozen while the doors read it unset."""
        if isinstance(v, bool):
            return v
        raise ValueError("build.agent_freeze is a TOML bool, not a string")
