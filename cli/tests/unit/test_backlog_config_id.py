"""Unit tests for config.backlog.id_prefix + id_hex_width.

The configurable node-ID scheme. Generation is strict (these validated config
fields drive minting); resolution stays liberal elsewhere. Schema defaults are
the LEGACY values (id_prefix absent -> ab- fallback at the accessor; hex width
8) so an existing config.backlog block with no id keys is byte-identical.
"""
from __future__ import annotations

import pytest
from pydantic import ValidationError

from fno.config import BacklogBlock, ConfigBlock


def test_defaults_are_the_legacy_scheme():
    # None means "not configured" -> the accessor falls back to legacy ab-.
    # An absent width resolves to the legacy 8, NOT the wizard's 4.
    assert ConfigBlock().backlog.id_prefix is None
    assert ConfigBlock().backlog.id_hex_width == 8


def test_prefix_normalizes_case_and_trailing_dash():
    assert BacklogBlock(id_prefix="XY").id_prefix == "xy-"
    assert BacklogBlock(id_prefix="fno-").id_prefix == "fno-"


@pytest.mark.parametrize("bad", ["cv-", "1ab", "toolongprefix"])
def test_prefix_rejects_reserved_and_malformed(bad):
    with pytest.raises(ValidationError):
        BacklogBlock(id_prefix=bad)


@pytest.mark.parametrize("bad", [3, 9])
def test_hex_width_rejects_out_of_range(bad):
    with pytest.raises(ValidationError):
        BacklogBlock(id_hex_width=bad)
