"""`fno config doctor` names the band-routing gap (x-374b, task 1.2).

`config.routing.models` is config-first on purpose: declare no rows and
`resolve_grid` records `grid=no-inventory-declared` and picks nothing, so every
difficulty band lands on the ambient default. Nothing said so. Measured
2026-09-02: two joiners spawned on the most expensive lane for medium-band work,
bands computed correctly and never consulted.

Every test asserts a POSITIVE marker - the advisory line, or a row that makes it
go away - never a bare absence.
"""

from __future__ import annotations

import pytest
import typer

from fno.config_cli import _report_band_routing

requires_rust = pytest.mark.dev_build


def _capture(monkeypatch: pytest.MonkeyPatch) -> list[str]:
    out: list[str] = []
    monkeypatch.setattr(typer, "echo", lambda m="", **k: out.append(str(m)))
    return out


def _pin_inventory(monkeypatch: pytest.MonkeyPatch, *, declared: bool):
    """Pin `resolve_inventory` to a declared / undeclared answer."""
    from fno import route_resolve

    inv = route_resolve.resolve_inventory()
    monkeypatch.setattr(
        route_resolve,
        "resolve_inventory",
        lambda **kw: route_resolve.Inventory(rows=inv.rows, declared=declared),
    )


def _pin_roles(monkeypatch: pytest.MonkeyPatch, roles: dict):
    from types import SimpleNamespace

    monkeypatch.setattr(
        "fno.config.load_settings",
        lambda: SimpleNamespace(model_routing=SimpleNamespace(roles=roles)),
    )


@requires_rust
def test_doctor_names_verbs_with_empty_slots(monkeypatch):
    """AC4-EDGE: the line names BOTH halves - the declared inventory count and
    every dispatched verb whose slot has no lane."""
    _pin_inventory(monkeypatch, declared=False)
    _pin_roles(monkeypatch, {})
    out = _capture(monkeypatch)

    _report_band_routing()

    text = "\n".join(out)
    assert "band routing inactive:" in text
    assert "declares 0 row(s)" in text
    for verb in ("think", "blueprint", "target", "review", "role"):
        assert verb in text


def test_roles_set_alongside_an_empty_inventory_says_they_are_a_different_axis(
    monkeypatch,
):
    """The trap this line exists for: roles ARE configured, so the operator
    reasonably reads routing as on. Roles route by role, never by band."""
    _pin_inventory(monkeypatch, declared=False)
    _pin_roles(monkeypatch, {"tidy": "zai/glm-4.7"})
    out = _capture(monkeypatch)

    _report_band_routing()

    text = "\n".join(out)
    assert "band routing inactive:" in text
    assert "model_routing.roles" in text and "ROLE" in text


# ---------------------------------------------------------------------------
# Harness overlay readout (x-8975)
# ---------------------------------------------------------------------------


def _pin_overlay_settings(
    monkeypatch: pytest.MonkeyPatch,
    *,
    profiles: dict | None = None,
    defaults: dict | None = None,
):
    """Settings whose agents block carries raw harness overlays."""
    from types import SimpleNamespace

    agents = SimpleNamespace(
        defaults=SimpleNamespace(
            permission_mode=defaults.get("permission_mode", "") if defaults else "",
            effort=defaults.get("effort", "") if defaults else "",
            harness=defaults.get("harness", {}) if defaults else {},
        ),
        profiles={
            verb: SimpleNamespace(
                permission_mode=cfg.get("permission_mode", ""),
                effort=cfg.get("effort", ""),
                harness=cfg.get("harness", {}),
            )
            for verb, cfg in (profiles or {}).items()
        },
    )
    monkeypatch.setattr(
        "fno.config.load_settings", lambda: SimpleNamespace(agents=agents)
    )


def test_doctor_names_a_codex_scalar_that_cannot_serve_claude(monkeypatch):
    """AC3-HP: the live-shaped defect - a codex spelling on the target profile
    - gets one line naming the verb, the harness, the value, and the overlay
    path that fixes it."""
    from fno.config_cli import _report_harness_overlays

    _pin_overlay_settings(
        monkeypatch,
        profiles={"target": {"permission_mode": "yolo", "effort": "high"}},
    )
    out = _capture(monkeypatch)

    _report_harness_overlays()

    text = "\n".join(out)
    assert "agents.profiles.target.permission_mode" in text
    assert "'yolo'" in text and "claude" in text
    assert "[agents.profiles.target.harness.claude]" in text
