"""Tests for dispatch_env and resolve_env_value.

Phase 03 of the provider rotation substrate (ab-256f6b6e).
Covers AC03.1-HP (dispatch arm), AC03.4-FR, AC03.6-EDGE,
AC03.7-FR, and the ProviderNotFoundError / ProviderUnavailableError contract.

Tests never invoke the real macOS `security` command: keychain calls are
intercepted via monkeypatch on subprocess.run.
"""
from __future__ import annotations

import subprocess
from pathlib import Path

import pytest
import yaml

from fno.adapters.providers.model import (
    ProviderNotFoundError,
    ProviderRecord,
    ProviderUnavailableError,
)
from fno.adapters.providers.staging import stage
from fno.adapters.providers.dispatch import dispatch_env, resolve_env_value


# ---------------------------------------------------------------------------
# Helpers / fixtures
# ---------------------------------------------------------------------------

def _write_settings(
    tmp_path: Path,
    records: list[dict],
    *,
    model_routing: dict | None = None,
) -> Path:
    """Write a settings.yaml with config.providers.records and return the repo_root."""
    settings = {
        "config": {
            "providers": {
                "records": records,
            }
        }
    }
    if model_routing is not None:
        settings["config"]["model_routing"] = model_routing
    fno_dir = tmp_path / ".fno"
    fno_dir.mkdir(parents=True, exist_ok=True)
    settings_path = fno_dir / "settings.yaml"
    settings_path.write_text(yaml.safe_dump(settings))
    return tmp_path


@pytest.fixture()
def creds_source(tmp_path: Path) -> Path:
    source = tmp_path / "canonical-creds"
    source.mkdir()
    (source / ".credentials.json").write_text('{"token": "dummy"}')
    return source


@pytest.fixture()
def staging_root(tmp_path: Path) -> Path:
    root = tmp_path / "providers"
    root.mkdir()
    return root


# ---------------------------------------------------------------------------
# AC03.1-HP: dispatch round-trip for oauth_dir / claude
# ---------------------------------------------------------------------------

def test_dispatch_env_claude_oauth(
    tmp_path: Path, creds_source: Path, staging_root: Path
) -> None:
    """dispatch_env for a claude oauth_dir record returns CLAUDE_CONFIG_DIR."""
    record = ProviderRecord(
        id="claude-max-primary",
        name="Claude Max Primary",
        harness="claude",
        auth="oauth_dir",
        credentials_source=creds_source,
    )
    repo_root = _write_settings(tmp_path, [record.model_dump(mode="json", exclude_none=True)])
    stage(record, root=staging_root)

    env = dispatch_env(record.id, repo_root=repo_root, root=staging_root)

    expected_link = staging_root / record.id / ".claude"
    assert env == {"CLAUDE_CONFIG_DIR": str(expected_link)}


def test_dispatch_env_gemini_oauth(
    tmp_path: Path, creds_source: Path, staging_root: Path
) -> None:
    """dispatch_env for a gemini oauth_dir record returns HOME pointing at staged home."""
    record = ProviderRecord(
        id="gemini-pro-a",
        name="Gemini Pro A",
        harness="gemini",
        auth="oauth_dir",
        credentials_source=creds_source,
    )
    repo_root = _write_settings(tmp_path, [record.model_dump(mode="json", exclude_none=True)])
    stage(record, root=staging_root)

    env = dispatch_env(record.id, repo_root=repo_root, root=staging_root)

    expected_home = staging_root / record.id / "home"
    assert env == {"HOME": str(expected_home)}


# ---------------------------------------------------------------------------
# AC03.6-EDGE: api_key env resolution via mocked keychain
# ---------------------------------------------------------------------------

def test_dispatch_env_api_key_keychain(
    tmp_path: Path, staging_root: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """dispatch_env for api_key record resolves KEYCHAIN refs via security CLI."""
    fake_key = "sk-ant-fake-key-abc123"
    captured_cmd: list[list[str]] = []

    def fake_subprocess_run(cmd, **kwargs):
        captured_cmd.append(list(cmd))
        result = subprocess.CompletedProcess(cmd, returncode=0)
        result.stdout = fake_key + "\n"
        return result

    import fno.adapters.providers.dispatch as dispatch_mod
    monkeypatch.setattr(dispatch_mod.subprocess, "run", fake_subprocess_run)

    record = ProviderRecord(
        id="anthropic-api-via-openclaw",
        name="Anthropic API via OpenClaw",
        harness="openclaw",
        auth="api_key",
        env={"ANTHROPIC_API_KEY": "${KEYCHAIN:anthropic-api-key-default}"},
    )
    repo_root = _write_settings(tmp_path, [record.model_dump(mode="json", exclude_none=True)])
    stage(record, root=staging_root)

    env = dispatch_env(record.id, repo_root=repo_root, root=staging_root)

    assert env == {"ANTHROPIC_API_KEY": fake_key}
    # Verify the security command was called with correct args.
    assert len(captured_cmd) == 1
    assert captured_cmd[0] == [
        "security", "find-generic-password", "-w", "-s", "anthropic-api-key-default"
    ]


def test_dispatch_env_route_uses_repository_model_settings(
    tmp_path: Path, staging_root: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """A route account resolves against the account repository's settings."""
    monkeypatch.setenv("ZAI_API_KEY", "wrong-repository-token")
    monkeypatch.setenv("PROJECT_ZAI_KEY", "project-token")
    record = ProviderRecord(
        id="zai",
        name="Z.AI",
        harness="claude",
        auth="api_key",
        route="zai/glm-5.3",
    )
    repo_root = _write_settings(
        tmp_path,
        [record.model_dump(mode="json", exclude_none=True)],
        model_routing={
            "providers": {
                "zai": {
                    "base_url": "https://project.example/anthropic",
                    "api_key_env": "PROJECT_ZAI_KEY",
                }
            }
        },
    )

    env = dispatch_env(record.id, repo_root=repo_root, root=staging_root)

    assert env["ANTHROPIC_BASE_URL"] == "https://project.example/anthropic"
    assert env["ANTHROPIC_AUTH_TOKEN"] == "project-token"


# ---------------------------------------------------------------------------
# ProviderNotFoundError
# ---------------------------------------------------------------------------

def test_dispatch_env_unknown_provider_raises_not_found(
    tmp_path: Path, staging_root: Path
) -> None:
    """dispatch_env raises ProviderNotFoundError for unknown provider_id."""
    repo_root = _write_settings(tmp_path, [])
    with pytest.raises(ProviderNotFoundError):
        dispatch_env("totally-unknown-id", repo_root=repo_root, root=staging_root)


# ---------------------------------------------------------------------------
# AC03.4-FR: Unstaged provider raises ProviderUnavailableError
# ---------------------------------------------------------------------------

def test_dispatch_env_unstaged_oauth_raises_unavailable(
    tmp_path: Path, creds_source: Path, staging_root: Path
) -> None:
    """dispatch_env raises ProviderUnavailableError when oauth_dir not staged."""
    record = ProviderRecord(
        id="claude-unstaged",
        name="Claude Unstaged",
        harness="claude",
        auth="oauth_dir",
        credentials_source=creds_source,
    )
    repo_root = _write_settings(tmp_path, [record.model_dump(mode="json", exclude_none=True)])
    # Deliberately do NOT call stage().
    with pytest.raises(ProviderUnavailableError, match="not staged"):
        dispatch_env(record.id, repo_root=repo_root, root=staging_root)


# ---------------------------------------------------------------------------
# AC03.7-FR: Unresolvable env ref raises ProviderUnavailableError
# ---------------------------------------------------------------------------

def test_dispatch_env_unresolvable_keychain_raises(
    tmp_path: Path, staging_root: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """AC03.7: dispatch_env raises ProviderUnavailableError when keychain lookup fails.

    The error must name BOTH the env var (ANTHROPIC_API_KEY) AND the
    unresolvable keychain reference (does-not-exist), per AC03.7 wording:
    'the error names the unresolvable reference'.
    """
    def fail_subprocess_run(cmd, **kwargs):
        raise subprocess.CalledProcessError(44, cmd)

    import fno.adapters.providers.dispatch as dispatch_mod
    monkeypatch.setattr(dispatch_mod.subprocess, "run", fail_subprocess_run)

    record = ProviderRecord(
        id="anthropic-api-via-openclaw",
        name="Anthropic API via OpenClaw",
        harness="openclaw",
        auth="api_key",
        env={"ANTHROPIC_API_KEY": "${KEYCHAIN:does-not-exist}"},
    )
    repo_root = _write_settings(tmp_path, [record.model_dump(mode="json", exclude_none=True)])
    stage(record, root=staging_root)

    with pytest.raises(ProviderUnavailableError) as exc_info:
        dispatch_env(record.id, repo_root=repo_root, root=staging_root)

    msg = str(exc_info.value)
    assert "ANTHROPIC_API_KEY" in msg, f"error should name the env var; got: {msg!r}"
    assert "does-not-exist" in msg, f"error should name the unresolvable keychain reference; got: {msg!r}"


# ---------------------------------------------------------------------------
# resolve_env_value unit tests
# ---------------------------------------------------------------------------

def test_resolve_env_value_plain_string() -> None:
    """Plain strings (no $ prefix) are returned as-is."""
    assert resolve_env_value("hello") == "hello"
    assert resolve_env_value("") == ""


def test_resolve_env_value_env_var(monkeypatch: pytest.MonkeyPatch) -> None:
    """${ENV:VAR} resolves from os.environ."""
    monkeypatch.setenv("MY_TEST_VAR_XYZ", "resolved-value")
    assert resolve_env_value("${ENV:MY_TEST_VAR_XYZ}") == "resolved-value"


def test_resolve_env_value_env_var_missing(monkeypatch: pytest.MonkeyPatch) -> None:
    """${ENV:MISSING_VAR} raises ProviderUnavailableError."""
    monkeypatch.delenv("MISSING_ENV_VAR_ABC", raising=False)
    with pytest.raises(ProviderUnavailableError):
        resolve_env_value("${ENV:MISSING_ENV_VAR_ABC}")


def test_resolve_env_value_file_success(tmp_path: Path) -> None:
    """${FILE:/path} reads the first line of the file."""
    key_file = tmp_path / "api.key"
    key_file.write_text("sk-file-key\nsecond line\n")
    assert resolve_env_value(f"${{FILE:{key_file}}}") == "sk-file-key"


def test_resolve_env_value_file_missing(tmp_path: Path) -> None:
    """${FILE:/missing} raises ProviderUnavailableError."""
    missing = tmp_path / "no-such-file.key"
    with pytest.raises(ProviderUnavailableError, match=str(missing)):
        resolve_env_value(f"${{FILE:{missing}}}")


def test_resolve_env_value_literal_escape() -> None:
    """${literal_value} (no recognized prefix) returns the inner contents verbatim."""
    assert resolve_env_value("${just-a-value}") == "just-a-value"


def test_dispatch_env_honors_config_dir(tmp_path: Path) -> None:
    """x-d012: a record with config_dir returns CLAUDE_CONFIG_DIR regardless of
    auth, so combo/failover never dispatches it on the ambient default account."""
    record = ProviderRecord(
        id="readyrule", name="ReadyRule", harness="claude", auth="managed",
        config_dir=Path("/x/.claude-alt"),
    )
    repo_root = _write_settings(tmp_path, [record.model_dump(mode="json", exclude_none=True)])
    env = dispatch_env(record.id, repo_root=repo_root)
    assert env == {"CLAUDE_CONFIG_DIR": "/x/.claude-alt"}


def test_dispatch_env_refuses_a_pin_whose_root_serves_another_account(
    tmp_path: Path, monkeypatch
) -> None:
    """AC2-ERR: the other launch env path emits the same refusal, from the same
    binding. Two launch paths that disagree about who is billed are two
    receipts, one of which is wrong."""
    from fno.adapters.providers import binding, managed

    store = tmp_path / "providers"
    store.mkdir()
    record = ProviderRecord(
        id="makers", name="Makers", harness="claude", auth="managed",
        config_dir=tmp_path / ".claude-alt",
    )
    repo_root = _write_settings(
        tmp_path, [record.model_dump(mode="json", exclude_none=True)]
    )
    managed.write_record_principal(
        "makers", {"account_uuid": "acc-makers", "organization_uuid": "org-1"}, store
    )
    monkeypatch.setattr(binding, "credential_blobs", lambda *_: ["blob"])
    monkeypatch.setattr(
        managed, "slot_principal",
        lambda _b: (
            {"account_uuid": "acc-readyrule", "organization_uuid": "org-1",
             "email": "readyrule@x"},
            None,
        ),
    )

    with pytest.raises(ProviderUnavailableError) as exc:
        dispatch_env(record.id, repo_root=repo_root, root=store)

    assert "account_identity_mismatch" in str(exc.value)
