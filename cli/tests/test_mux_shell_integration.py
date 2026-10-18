"""config.mux.shell_integration coercion (OSC 133 auto-inject knob)."""
from __future__ import annotations

from fno.config import ConfigBlock, MuxBlock


def test_shell_integration_coerces_to_off_or_mux_panes():
    """One row per branch of the coercer, plus the field default.

    YAML 1.1 parses unquoted `off` as boolean False, so `shell_integration: off`
    reaches the validator as `False`; a naive `isinstance(v, str)` check would
    silently re-enable it.
    """
    def coerce(value):
        return MuxBlock.model_validate({"shell_integration": value}).shell_integration

    assert coerce(False) == "off"
    assert coerce(" off ") == "off"
    assert coerce("garbage") == "mux-panes"
    assert MuxBlock().shell_integration == "mux-panes"


def test_nonmapping_mux_block_degrades_to_defaults():
    """`mux: off` (a scalar) must not raise out of the whole settings load."""
    cb = ConfigBlock.model_validate({"mux": 42})
    assert cb.mux.shell_integration == "mux-panes"
    cb2 = ConfigBlock.model_validate({"mux": "off"})
    assert cb2.mux.shell_integration == "mux-panes"
