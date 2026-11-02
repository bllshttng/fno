"""Telemetry settings (``telemetry.*``). The Rust daemon reads every key here."""

from pydantic import BaseModel, ConfigDict, Field


class TelemetryOtlpBlock(BaseModel):
    """OTLP log export. ``headers_env`` names the env var holding the auth headers."""

    model_config = ConfigDict(extra="ignore")

    enabled: bool = False
    endpoint: str = ""
    headers_env: str = "FNO_OTLP_HEADERS"
    log_content: bool = False


class TelemetryEvalEventsBlock(BaseModel):
    """Eval runs as PostHog events. ``token_env`` names the env var holding the project token."""

    model_config = ConfigDict(extra="ignore")

    enabled: bool = False
    host: str = "https://us.i.posthog.com"
    token_env: str = "FNO_POSTHOG_PROJECT_TOKEN"


class TelemetryBlock(BaseModel):
    model_config = ConfigDict(extra="ignore")

    claude_otel: bool = True
    otlp: TelemetryOtlpBlock = Field(default_factory=TelemetryOtlpBlock)
    eval_events: TelemetryEvalEventsBlock = Field(default_factory=TelemetryEvalEventsBlock)
