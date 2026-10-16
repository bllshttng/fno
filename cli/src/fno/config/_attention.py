"""Attention settings (``attention.*``). The Rust daemon reads every key here."""

from pydantic import BaseModel, ConfigDict, Field


class AttentionSinkBlock(BaseModel):
    """One ``[[attention.sinks]]`` row. ``type`` is ntfy or webhook, which the daemon
    checks itself; ``token_env`` names the env var holding the bearer token."""

    model_config = ConfigDict(extra="ignore")

    name: str = ""
    type: str = "ntfy"
    url: str = ""
    topic: str = ""
    token_env: str = ""
    answer_base_url: str = ""
    body: str = "title-only"


class AttentionBlock(BaseModel):
    model_config = ConfigDict(extra="ignore")

    enabled: bool = True
    sinks: list[AttentionSinkBlock] = Field(default_factory=list)
