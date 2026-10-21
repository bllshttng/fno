import pytest

from clamp import clamp_repeat


def test_clamp_result_is_int():
    for requested in (-5, 0, 1, 7, 100):
        assert isinstance(clamp_repeat(requested, 5), int)
