"""Attention settings (``attention.*``). The Rust daemon reads every key here."""

from pydantic import BaseModel, ConfigDict, Field


class AttentionSinkBlock(BaseModel):
    """One ``[[attention.sinks]]`` row: where an open question is pushed.

    ``type`` is ``ntfy`` or ``webhook``; the daemon refuses any other value with a
    tick detail rather than failing config load. ``token_env`` names the env var
    holding the bearer token. ``body`` is ``"title-only"`` by default, because
    anyone who knows a public topic reads it; ``"full"`` adds the recommendation.
    """

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
