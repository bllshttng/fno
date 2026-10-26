"""Transport for the ``spawn-overlay`` command: the route-slot shape. One JSON
payload in, one parsed JSON answer out. A missing, failing or malformed
answer raises :class:`SpawnOverlayUnavailable` - a named refusal, never a
silent spawn, exactly the posture route-slot set."""

from __future__ import annotations

from typing import Any

from fno.rust_binary import CommandUnavailable


class SpawnOverlayUnavailable(CommandUnavailable):
    """The fno-agents binary is missing, failed, or answered malformed JSON."""


def spawn_overlay_call(
    payload: dict[str, Any], timeout: float = 90
) -> dict[str, Any]:
    """One subprocess round-trip: JSON payload in, parsed JSON answer out.

    The default is 90s, not command_call's 30s: the compose's payload can name
    a node (a graph read) and a missing binary must not wedge a spawn either
    way.
    """
    # Call-time import: a module-top capture can permanently hold a test's
    # monkeypatched command_call stub, leaking it into every later reader.
    from fno.rust_binary import command_call

    return command_call("spawn-overlay", payload, SpawnOverlayUnavailable, timeout=timeout)


class AccountRefused(ValueError):
    """``accounts::launch`` refused an account; the message is its receipt."""


def account_overlay(account_id: str) -> dict[str, Any]:
    """The ``--account`` answer (``account_id``, ``env``, ``lane``, ``scrub``,
    ``login_probe``) from ``accounts::launch``, or :class:`AccountRefused`."""
    import os

    answer = spawn_overlay_call({"kind": "account-overlay", "account": account_id, "cwd": os.getcwd()})
    if answer.get("refusal") is not None:
        raise AccountRefused(answer["refusal"])
    return answer


def compose_credentials(account_env, route_env, base, *, account_id=None) -> dict[str, Any]:
    """``accounts::credentials`` compose: ``env``, ``decision``, ``scrub``, ``secret``."""
    return spawn_overlay_call({
        "kind": "credentials", "account_env": dict(account_env or {}),
        "route_env": dict(route_env or {}), "base": dict(base), "account_id": account_id,
    })
