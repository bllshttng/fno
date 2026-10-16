"""Attention settings (``attention.*``). The Rust daemon reads every key here."""

from typing import Any

from pydantic import BaseModel, ConfigDict, Field, model_validator


class AttentionBlock(BaseModel):
    """``sinks`` rows stay raw tables: the daemon checks each one and names a refusal."""

    model_config = ConfigDict(extra="ignore")

    enabled: bool = True
    sinks: list[dict[str, Any]] = Field(default_factory=list)

    @model_validator(mode="before")
    @classmethod
    def _ignore_retired_rows(cls, v: Any) -> Any:
        return {} if isinstance(v, list) else v
