"""The spawn --name rules: @ is the cross-host separator, never a name char."""
import pytest

from fno.agents.dispatch import DispatchAskError, validate_spawn_name


def test_at_in_spawn_name_refuses_naming_the_relay_form():
    with pytest.raises(DispatchAskError, match="cross-host"):
        validate_spawn_name("quinlan@mbp3")
    validate_spawn_name("quinlan")
