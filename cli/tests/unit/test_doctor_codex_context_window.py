"""The codex effective-context-window check.

Codex clamps ``model_context_window`` to the model's server-supplied
``max_context_window`` and then keeps ``effective_context_window_percent`` of
it.  The TUI footer echoes the CONFIGURED number, so a config asking for 1M
reads as 1M while every turn runs at the clamped value.  These tests pin the
arithmetic, the one condition that speaks, and the claims the emitted line is
allowed to make.
"""
from __future__ import annotations

import json
import shutil
from pathlib import Path

from fno import doctor

# One real fetch holds both, which is why no file-wide "tier" label is true.
MODELS = [
    {
        "slug": "gpt-5.6-sol",
        "context_window": 272000,
        "max_context_window": 272000,
        "effective_context_window_percent": 95,
    },
    {
        "slug": "gpt-5.6-luna",
        "context_window": 272000,
        "max_context_window": 872000,
        "effective_context_window_percent": 95,
    },
]


def _write(
    home: Path,
    *,
    configured: int | None,
    cached_max: int = 272000,
    config_extra: str = "",
    percent: object = 95,
) -> None:
    lines = ['model = "gpt-5.6-sol"']
    if configured is not None:
        lines.append(f"model_context_window = {configured}")
    if config_extra:
        lines.append(config_extra)
    (home / "config.toml").write_text("\n".join(lines) + "\n")
    models = [dict(m) for m in MODELS]
    models[0]["max_context_window"] = cached_max
    models[0]["effective_context_window_percent"] = percent
    (home / "models_cache.json").write_text(
        json.dumps(
            {
                "fetched_at": "2026-08-18T19:13:37.907036Z",
                "client_version": "0.147.0",
                "models": models,
            }
        )
    )


def test_configured_over_cap_reports_the_clamped_window(tmp_path, monkeypatch) -> None:
    monkeypatch.setenv("CODEX_HOME", str(tmp_path))
    _write(tmp_path, configured=1000000, cached_max=272000)

    report = doctor._codex_context_window_report()

    assert report["overstated"] is True
    assert report["configured"] == 1000000
    assert report["max_context_window"] == 272000
    assert report["effective"] == 258400


def test_percent_shrink_alone_still_overstates(tmp_path, monkeypatch) -> None:
    """400000 under an 872000 cap still runs at 380000, and the footer says
    400000.  That is the same lie as the clamp, so it must not be silent."""
    monkeypatch.setenv("CODEX_HOME", str(tmp_path))
    _write(tmp_path, configured=400000, cached_max=872000)

    report = doctor._codex_context_window_report()

    assert report["effective"] == 380000
    assert report["overstated"] is True


def test_a_profile_redirects_which_model_is_described(tmp_path, monkeypatch) -> None:
    """A `profile` key overrides model selection, so reading the top-level
    `model` past one describes a model no thread runs."""
    monkeypatch.setenv("CODEX_HOME", str(tmp_path))
    _write(
        tmp_path,
        configured=1000000,
        config_extra='profile = "big"\n[profiles.big]\nmodel = "gpt-5.6-luna"',
    )

    report = doctor._codex_context_window_report()

    assert report["model"] == "gpt-5.6-luna"
    assert report["model_source"] == "profiles.big.model"
    assert report["max_context_window"] == 872000
    assert report["effective"] == 828400


def test_a_raised_cap_that_spares_the_clamp_is_still_flagged(tmp_path, monkeypatch) -> None:
    """gpt-5.4 sits at base 272000 and cap 1000000.  A configured 400000 escapes
    the clamp only because the last fetch won the raised cap, and reverts to
    258400 without it, so the cache caveat has to appear."""
    monkeypatch.setenv("CODEX_HOME", str(tmp_path))
    _write(tmp_path, configured=400000, cached_max=1000000)

    report = doctor._codex_context_window_report()

    assert report["effective"] == 380000
    assert report["leans_on_cached_cap"] is True
    line = _emit(report)[0]
    assert "above its 272000 base" in line
    assert "drops this to 258400" in line


def test_the_harness_surface_report_carries_and_gates_the_check(tmp_path, monkeypatch) -> None:
    """Pin the wiring, not just the pieces: the key name, the codex-on-PATH
    gate, and the fact the emitter reads the same key doctor writes."""
    monkeypatch.setenv("CODEX_HOME", str(tmp_path))
    _write(tmp_path, configured=1000000)

    # Opening the codex gate also opens the sibling plugin check, which shells
    # out to the real `codex` binary.  Stub it so this stays a unit test.
    import fno.setup.codex_plugin as codex_plugin

    monkeypatch.setattr(codex_plugin, "inspect_freshness", lambda: {"status": "fresh"})

    monkeypatch.setattr(doctor.shutil, "which", lambda name: None)
    assert "codex_context_window" not in doctor._harness_surface_report()

    monkeypatch.setattr(doctor.shutil, "which", lambda name: "/usr/local/bin/codex")
    surface = doctor._harness_surface_report()
    assert surface["codex_context_window"]["effective"] == 258400
    assert _emit_from_surface(surface) != []


def _emit_from_surface(surface: dict) -> list[str]:
    lines: list[str] = []
    doctor._emit_codex_context_window({"harness_surface": surface}, out=lines.append)
    return lines


def _emit(report: dict) -> list[str]:
    lines: list[str] = []
    doctor._emit_codex_context_window(
        {"harness_surface": {"codex_context_window": report}}, out=lines.append
    )
    return lines


def test_emitted_line_names_the_numbers_and_the_cache_provenance(tmp_path, monkeypatch) -> None:
    monkeypatch.setenv("CODEX_HOME", str(tmp_path))
    _write(tmp_path, configured=1000000, cached_max=272000)

    lines = _emit(doctor._codex_context_window_report())

    assert len(lines) == 1
    line = lines[0]
    assert "1000000" in line
    assert "272000" in line
    assert "258400" in line
    assert "gpt-5.6-sol" in line
    assert "2026-08-18T19:13:37.907036Z" in line
    # The cap turns on surface AND originator, so the line must not pin it to
    # the originator alone: codex exec and codex app-server are served
    # differently under one originator string.
    assert "surface and originator both" in line
    assert "records neither" in line
    assert "its base" in line
    # No file-wide tier claim: one cache holds sol at base and luna above it.
    assert "tier" not in line


def test_emitter_is_silent_when_the_configured_value_is_honest(tmp_path, monkeypatch) -> None:
    """percent=100 and no clamp: the footer tells the truth, so say nothing."""
    monkeypatch.setenv("CODEX_HOME", str(tmp_path))
    _write(tmp_path, configured=200000, cached_max=872000, percent=100)

    assert doctor._codex_context_window_report()["overstated"] is False
    assert _emit(doctor._codex_context_window_report()) == []


def test_a_missing_cap_reports_a_reason_rather_than_inventing_one(
    tmp_path, monkeypatch
) -> None:
    """Substituting the base produced a number the cache never carried, then
    the line had to disclaim the very cap it quoted.  No cap, no answer."""
    monkeypatch.setenv("CODEX_HOME", str(tmp_path))
    _write(tmp_path, configured=1000000, cached_max=None)

    report = doctor._codex_context_window_report()

    assert report == {"reason": "cache-schema-drift: no max_context_window for gpt-5.6-sol"}
    assert _emit(report) == []


def test_a_non_finite_or_out_of_range_percent_is_dropped(tmp_path, monkeypatch) -> None:
    """NaN and inf raise inside the arithmetic, and the caller swallows that.
    A negative or over-100 percent prints an impossible window as fact."""
    monkeypatch.setenv("CODEX_HOME", str(tmp_path))
    for bad in (float("nan"), float("inf"), -50, 150):
        _write(tmp_path, configured=1000000, percent=bad)
        report = doctor._codex_context_window_report()
        assert report.get("percent") is None, bad
        assert report["effective"] == 272000, bad


def test_an_impossible_window_is_rejected_by_the_same_gate(tmp_path, monkeypatch) -> None:
    """The range bound belongs INSIDE the gate, not beside one field.  A cap
    of -5 or 0 printed 'an effective -5' as fact, and an 80-digit cap landed
    verbatim in both the human line and the JSON payload."""
    monkeypatch.setenv("CODEX_HOME", str(tmp_path))
    for bad in (-5, 0, 10**80):
        _write(tmp_path, configured=1000000, cached_max=bad)
        report = doctor._codex_context_window_report()
        assert report == {
            "reason": "cache-schema-drift: no max_context_window for gpt-5.6-sol"
        }, bad


def _live_daemon_socket(monkeypatch):
    """A real listening unix socket where the daemon's control socket lives.

    The probe is a connect now, so a bare touched file reads dead (the file
    survives its process).  Returns (listener, home) for the caller to close
    and remove; a short CODEX_HOME keeps the bind path under the AF_UNIX
    104-char limit.
    """
    import socket as _socket
    import tempfile

    short = tempfile.mkdtemp(prefix="fno-x571-", dir="/tmp")
    (Path(short) / "app-server-control").mkdir(parents=True)
    monkeypatch.setenv("CODEX_HOME", short)
    listener = _socket.socket(_socket.AF_UNIX, _socket.SOCK_STREAM)
    listener.bind(str(Path(short) / "app-server-control" / "app-server-control.sock"))
    listener.listen(1)
    return listener, short


def test_a_live_app_server_daemon_is_named_as_the_thing_holding_the_cap_down(
    tmp_path, monkeypatch
) -> None:
    """Every app-server fetch lands the base cap whatever clientInfo name it
    presents, so a live daemon keeps pulling a raised cap back down.  That is
    the actionable half, and the operator only sees it if the line says it."""
    monkeypatch.setenv("CODEX_HOME", str(tmp_path))
    _write(tmp_path, configured=1000000, cached_max=272000)
    (tmp_path / "app-server-control").mkdir()

    assert "app-server daemon is live" not in _emit(doctor._codex_context_window_report())[0]

    listener, short = _live_daemon_socket(monkeypatch)
    try:
        _write(Path(short), configured=1000000, cached_max=272000)
        assert "app-server daemon is live" in _emit(doctor._codex_context_window_report())[0]
    finally:
        listener.close()
        shutil.rmtree(short, ignore_errors=True)


def test_a_malformed_cache_shape_returns_a_reason_not_a_raise(tmp_path, monkeypatch) -> None:
    """Same contract as the profiles branch: the caller swallows a raise, so a
    raise here drops the key from --json and hides that anything went wrong."""
    monkeypatch.setenv("CODEX_HOME", str(tmp_path))
    (tmp_path / "config.toml").write_text(
        'model = "gpt-5.6-sol"\nmodel_context_window = 1000000\n'
    )

    for shape in ('{"models": "nope"}', '{"models": ["nope"]}', '"nope"', "[]"):
        (tmp_path / "models_cache.json").write_text(shape)
        report = doctor._codex_context_window_report()
        assert "reason" in report, shape
