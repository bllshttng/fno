"""The dispatch command registry model: an outside skill as a descriptor, not a
bare string. Mirrors ``ReviewerDescriptor``; lookup drops shipped spellings."""
from __future__ import annotations

import re
from dataclasses import dataclass
from typing import Literal, Mapping, Optional, Sequence, Tuple

from pydantic import AliasChoices, BaseModel, ConfigDict, Field, field_validator


@dataclass(frozen=True)
class DispatchCommandDescriptor:
    # The unknown-harness default spelling, exactly as the worker must receive
    # it. Rendered verbatim; never fno-namespaced.
    invocation: str
    # Present: this command exists ONLY on these harnesses. Absent: `invocation`
    # is correct everywhere. Same field, same meaning, as ReviewerDescriptor.
    invocations: Optional[Mapping[str, str]] = None
    # `skill` is probed against the harness's skill roots at resolve time.
    requires: Literal["none", "skill"] = "none"
    # Whether the resolver appends the node id.
    takes_node_id: bool = True
    # What a completion proves, weakest last.
    asserts: Literal["pr", "doc", "invocation"] = "invocation"
    # The sessions-row phase stamped for a worker dispatched on this command.
    # Shipped commands infer theirs from the spawn_phase table; an outside command
    # is unknown to that table, so it declares its phase here. Absent, a
    # --node spawn is refused rather than launched unbound.
    session_phase: Optional[str] = None


#: The built-in dispatch commands (harness_map mirrors this tuple; cycle-free).
DEFAULT_DISPATCH_COMMANDS = ("/target", "/think", "/blueprint")

_COMMAND_BODY_RE = re.compile(r"^[a-z0-9][a-z0-9_-]*$")


def parse_command_token(tok: str) -> Optional[Tuple[str, bool]]:
    """Parse one command-seed token: `(command, namespaced)` or None.

    The shape rule every reader shares: a leading ``/`` or ``$`` sigil, no
    second ``/`` inside the token (an absolute path never matches), an
    optional ``fno:`` namespace, and a lowercase-word remainder. Both sigils
    parse - ``/fno:target``, ``$fno:target``, ``/target`` and ``$target``
    all yield ``("target", ...)``."""
    if len(tok) < 2 or tok[0] not in "/$":
        return None
    body = tok[1:]
    if "/" in body:
        return None
    namespaced = body.startswith("fno:")
    if namespaced:
        body = body[len("fno:"):]
    if not _COMMAND_BODY_RE.match(body):
        return None
    return body, namespaced


def is_command_seed(seed: Optional[str]) -> bool:
    """Whether ``seed``'s FIRST token is a command-shaped token.

    Index 0 is load-bearing: only a position-0 command may be rewritten or
    run unattended. A command inside prose must not pass."""
    if not seed:
        return False
    parts = seed.split()
    if not parts:
        return False
    return parse_command_token(parts[0]) is not None


def canonical_command_key(key: str) -> str:
    """Leading `/`, `/fno:x`, `$fno:x` and `$x` -> `/x`: the resolver's
    canonical spelling. Keys the parser rejects keep the legacy strip."""
    if parsed := parse_command_token(key.strip()):
        return "/" + parsed[0]
    k = key.strip().removeprefix("/")
    k = k.removeprefix("fno:")
    return "/" + k if k else k


def resolvable_commands(
    registry: Optional[Mapping[str, DispatchCommandDescriptor]] = None,
    allowed: Optional[Sequence[str]] = None,
) -> dict[str, DispatchCommandDescriptor]:
    """The registry minus shipped spellings: a key canonicalizing to a built-in
    or allowlisted command is dropped, so a shipped command cannot be redefined
    into a weaker descriptor."""
    allowed_canon = {
        canonical_command_key(v)
        for v in (tuple(allowed) if allowed is not None else DEFAULT_DISPATCH_COMMANDS)
        if isinstance(v, str) and v.strip()
    }
    return {
        canon: desc
        for key, desc in (registry or {}).items()
        if (canon := canonical_command_key(str(key))) not in allowed_canon
    }


class DispatchBlock(BaseModel):
    """Autonomous-dispatch profile (nested under 'config.dispatch').

    Per-environment overlay of the in-tree harness-capability map
    (`fno.agents.harness_map`): the shared `resolve_node_spawn` resolver reads
    these as the config rung between an explicit flag and the built-in default.
    All three are empty ("") = unset, so a fresh install resolves exactly as the
    built-ins (harness=claude, per-harness substrate, `/target --no-merge {id}`).

    No value validation here: the resolver checks harness and substrate at
    resolve time, fail-loud.

    `command` is a template with a single `{id}` placeholder. A node's
    `dispatch_command`/`dispatch_brief` (US3) layer on top: the resolver validates
    the command against `allowed_commands` and carries the brief via env only, so a
    graph field crossing into a worker prompt is a guarded trust boundary.
    """

    model_config = ConfigDict(extra="ignore")

    # DEPRECATED: the stage table (`agents.profiles.<command>.provider`) owns
    # the harness axis; this key is its fallback rung for one release.
    harness: str = ""
    substrate: str = ""
    command: str = ""
    # Registered commands from outside fno (unioned with allowed_commands at resolve;
    # a shipped or allowlisted spelling cannot be redefined here).
    # Both keys read their pre-rename spelling too, so an existing config.toml loads.
    command_registry: dict[str, DispatchCommandDescriptor] = Field(
        default_factory=dict, validation_alias=AliasChoices("command_registry", "verb_registry")  # verb-wording: allow
    )
    # US3 command allowlist: a node dispatch_command must match or the resolver refuses.
    allowed_commands: list[str] = Field(
        default_factory=lambda: list(DEFAULT_DISPATCH_COMMANDS),
        validation_alias=AliasChoices("allowed_commands", "allowed_verbs"),  # verb-wording: allow
    )
    # DEPRECATED: reads as `auto_merge.grant` for one release; no consumer reads it.
    auto_merge: bool = False
    # defer (default, byte-identical to today) or fail over to the next
    # healthy provider in the active combo; an unknown value degrades to "defer".
    on_exhaustion: str = "defer"
    # Proactive LOW cutover, opt-in, default 0 = off. Deliberately inverted from
    # defer_horizon_minutes: a distant reset means leave now, not wait.
    cutover_low_after_minutes: int = 0
    # The blueprint floor for a plan-less node. "high" (default) blueprints only
    # high difficulty, size L, or a `premise-question` tag; the rest go to
    # /target. "medium" blueprints medium and up, "low" all, "never" none.
    # Read by fno-agents effective_command and the lead check-in.
    blueprint_floor: str = "high"

    @field_validator("auto_merge", mode="before")
    @classmethod
    def _coerce_auto_merge(cls, v: object) -> object:
        """Only a real TOML boolean grants merge: a string like "yes" resolves
        False, so a config error never grants merge rights."""
        return v if isinstance(v, bool) else False

    @field_validator("on_exhaustion", mode="before")
    @classmethod
    def _coerce_on_exhaustion(cls, v: object) -> object:
        """Only the two literals are honored; anything else degrades to "defer".
        Same degrade-toward-safety stance as _coerce_auto_merge: a typo can never
        enable failover."""
        return v if v in ("defer", "failover") else "defer"

    @field_validator("cutover_low_after_minutes", mode="before")
    @classmethod
    def _coerce_cutover_low(cls, v: object) -> object:
        """Only a nonnegative TOML integer arms proactive LOW cutover; a bool,
        a string, or a negative degrades to 0 (off). Same stance as the two
        above: a config typo can never arm automatic rerouting."""
        return v if isinstance(v, int) and not isinstance(v, bool) and v >= 0 else 0

    @field_validator("blueprint_floor", mode="before")
    @classmethod
    def _coerce_blueprint_floor(cls, v: object) -> object:
        """Only the four literals are honored; anything else degrades to "high",
        the lean default. Case and surrounding space are normalized like the
        Rust reader lowercases them, so both surfaces answer one value. Same
        stance as the validators above: a typo can never widen blueprint
        ceremony past the lean-dispatch ruling."""
        if isinstance(v, str) and v.strip().lower() in ("high", "medium", "low", "never"):
            return v.strip().lower()
        return "high"
