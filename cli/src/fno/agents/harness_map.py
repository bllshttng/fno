"""Harness-capability map + shared dispatch resolver (US1 / G3).

One versioned table from a *capability* to each harness's concrete value, so
dispatch is provider-neutral instead of claude-shaped by accident. Every
autonomous launcher (dispatch-node.sh, backlog advance, /think handoff, the
active_backlog daemon) resolves argv through :func:`resolve_dispatch` instead of
hand-rolling it - the duplicated-spawn bug class (fixed three of four
copies and missed the fourth) disappears when exactly one resolver owns the
(harness, substrate, command) decision (Locked Decision 10).

The resolver is PURE: config + context -> tuple. It never acquires a claim,
spawns, or touches the network. Claims and spawning stay in the launchers.

Per-environment override: ``config.dispatch`` (harness / substrate / command)
overlays the built-in defaults; the map itself is the versioned in-tree table.

Verified facts, each dated where it differs from the 2026-07-13 spike:
- permission_bypass tokens mirror the provider adapters (claude.py,
  codex.py, gemini.py) - the flag a headless/bg worker needs so it never wedges
  on an approval prompt (the concrete cause of the manual-approve pain).
- the thread seat is derived, never stored: ``features.spawn`` reads ``native``
  where fno's own launch arm is wired and journey-proven (claude, codex,
  opencode, pi, cursor-agent, grok), and every other harness falls back to
  ``headless`` (Locked Decision 3, HARNESSES.md). opencode's serve lane stays
  launch-only (``ask`` refuses, no steering over the HTTP API); its spawn row
  is what seats the lane.
- loop_participation REPLACED stop_hook (2026-08-28). The old field read
  "native" on every row and had no consumer. The paragraph that stood here
  recorded a 2026-07-13 verification of THREE harnesses, and six rows ended up
  carrying the value - the inheritance in writing, in the same file as the
  field. Each row is now measured against the artifact and the wiring that
  reaches it; gemini and opencode came out wrong. See the table's own comment
  for the per-harness evidence.
"""
from __future__ import annotations

import re
import tomllib
from copy import deepcopy
from functools import cache
from importlib.resources import files
from pathlib import Path
from typing import Optional

from fno.config._dispatch_commands import parse_command_token
from fno.config_io import _global_settings_path

# Command surface: HOW a footnote slash `/command` is natively invoked on a harness.
# One axis, the single source both dispatch surfaces normalize through
# (autonomous `/target thread` + `/agent spawn`):
#   "slash"       claude, agy, opencode -> "/[slash_prefix]command ..." native slash
#                 command; per-harness `slash_prefix` ("" for claude/agy, "fno:"
#                 for opencode's plugin-namespaced palette + `run --command`)
#   "codex-skill" codex                 -> "$fno:command ..." plugin skill expansion
#   "refused"     gemini                -> a loud error naming agy (deprecated)
_SLASH, _CODEX_SKILL, _REFUSED = "slash", "codex-skill", "refused"

# The canonical (claude-syntax) autonomous dispatch command. normalize_command
# maps it per-harness for the builtin `dispatch_command`, so the per-harness
# spelling lives in ONE place (command_surface), not five literal strings.
#
# The `--no-merge` flag is the merge POSTURE, resolved from
# config.auto_merge.grant rather than baked in. It was the free-text
# `no-merge` token until (config-deaf) and x-aaaa (free text stopped
# being a control input): the flag is the deterministic carrier that survives
# `fno do target start` resolving its argument to a bare node id, and unlike the
# token it cannot be manufactured by prose an LLM wrote into a brief.
_AUTONOMOUS_COMMAND = AUTONOMOUS_COMMAND = "/target --no-merge {id}"


@cache
def _carrier_vocab() -> tuple[tuple[str, ...], str, str]:
    """The carrier vocabulary from the ONE canonical merge_posture table
    : authored in the Rust tree, shipped here as generated package
    data. The Rust engine and these readers cannot drift."""
    import tomllib
    from importlib.resources import files

    table = tomllib.loads(
        files("fno.agents").joinpath("merge_posture.toml").read_text(encoding="utf-8")
    )
    return (
        tuple(table["target_family"]["spellings"]),
        str(table["carrier"]["flag"]),
        str(table["carrier"]["legacy_token"]),
    )


_TARGET_FAMILY = _carrier_vocab()[0]


@cache
def _shipped_commands() -> frozenset:
    """One cached read of the plugin surface; `footnote_commands` is the caller."""
    from fno.paths import resolve_plugin_script

    command_names: set[str] = set()
    try:
        skills = resolve_plugin_script("skills")
        command_names.update(p.name for p in skills.iterdir() if (p / "SKILL.md").is_file())
    except OSError:
        pass
    try:
        commands = resolve_plugin_script("commands")
        command_names.update(p.stem for p in commands.glob("*.md") if p.is_file())
    except OSError:
        pass
    return frozenset(command_names)


def footnote_commands() -> frozenset:
    """The shipped footnote command roster: every ``skills/<name>/SKILL.md`` and
    every ``commands/<name>.md`` the plugin ships.

    Read from the shipped plugin surface, never a retyped literal: a literal
    copy goes stale the first time a command ships, and the failure is silent -
    the new command simply stops normalizing on codex and nothing notices.

    Empty on any resolution or read failure, which on the codex surface leaves
    every bare ``/command`` literal - pass-through is the safe direction - while
    the plugin-qualified ``/fno:command`` spelling keeps working, since its
    namespace alone proves it is a footnote command.

    A good answer caches; an EMPTY one does not. Empty means the plugin
    surface did not RESOLVE, never that footnote ships no commands, and the
    resolver reads an env hint first, so the next call can succeed. Caching
    the failure froze the degraded answer for the whole process, silently: a
    codex worker gets `/target`, which codex reads as prose, not a command."""
    commands = _shipped_commands()
    if not commands:
        _shipped_commands.cache_clear()
    return commands


footnote_commands.cache_clear = _shipped_commands.cache_clear  # type: ignore[attr-defined]


def _refused_reason(harness: str) -> str:
    """The loud-refusal message for a deprecated harness with no dispatch lane -
    names the successor (agy) so the failure is actionable (AC2-ERR / AC3-UI)."""
    return (
        f"harness {harness!r} has no maintained footnote dispatch lane and is "
        f"deprecated; route this work to its successor 'agy' (or a "
        f"claude/codex/opencode harness) - no prose build brief is generated"
    )

# capability -> per-harness value, keyed by the READABLE_PROVIDERS set. Each
# harness carries a `command_surface`: the invocation form its native
# footnote skill takes, or `refused` where the harness is deprecated. A slash
# harness also carries `slash_prefix` (the plugin namespace). The thread seat
# is not stored: it derives from `features.spawn` reading `native` (fno's own
# driver + unattended journey test, never a bare resume primitive). `bg`
# remains a one-release input alias.
#
# Two pane capabilities, both EVIDENCE-GATED and read fail-closed (a missing key
# reads false), because each one used to be a blanket rule that was true of at
# most one harness:
#   autonomous_pane  Does a fire-and-forget pane worker on this harness run to
#                    completion without an operator? The old guard refused pane
#                    for EVERY harness on the claim that "a pane stalls waiting
#                    for a human". That claim is per-harness, not universal, and
#                    each true value here must be backed by an unattended journey
#                    test (see cli/tests/agents/test_spawn_pane.py) - never by
#                    another harness's result.
#   route_on_pane    Can a model route's complete environment reach a pane child
#                    on this harness? The pane launcher materializes a route via
#                    mux_spawn._mesh_env_wrapper, whose endpoint/auth/model
#                    assignment + inherited-credential scrub is claude-shaped;
#                    a harness whose route env semantics are not covered end to
#                    end stays false.
# Neither key changes `substrate_default`: permission to use pane is a separate
# decision from preferring it, and the defaults are unchanged.
_RESPONSE_ACTIONS = {"allow_once", "allow_always", "deny"}
_SESSION_LANES = {
    "interactive_create",
    "interactive_resume",
    "interactive_attach",
    "headless_create",
    "headless_resume",
}
_RESUME_KINDS = {"flag", "subcommand", "session_flag", "unsupported"}
_MODEL_SWITCH_KINDS = {"direct", "menu_walk", "unsupported"}
_MODEL_SWITCH_PLACEHOLDERS = {"model", "effort", "effort_label"}
_MODEL_SWITCH_EFFORTS = {"low", "medium", "high", "xhigh", "max"}
_KEY_TOKENS = {
    *(str(i) for i in range(1, 10)),
    "enter", "left", "right", "up", "down", "tab", "esc", "y", "a", "d",
    "unsupported",
}
_STOP_STRATEGIES = {"claude-short-id", "registry-noop"}
_REMOVE_STRATEGIES = {"claude-short-id", "codex-session-index", "registry-only"}
# Whether the fno target loop can CLOSE on a harness. Kept identical to the Rust
# validator's LOOP_PARTICIPATION so the two runtimes cannot disagree about which
# contracts are legal. The table's comment carries the per-harness measurement.
_LOOP_PARTICIPATION = {"native", "extension", "none"}


def _contract_error(harness: str, field: str, detail: str) -> "DispatchResolveError":
    return DispatchResolveError(
        f"harness capability contract: harness {harness!r} field {field!r}: {detail}"
    )


def _validate_row(harness: str, caps: dict) -> None:
    """Validate ONE capability row against the contract.

    The per-harness loop body of :func:`parse_capability_contract`,
    extracted so the config-override merge can gate a candidate row
    through the SAME validation the bundled table ships under.
    Raises :class:`DispatchResolveError` naming harness + field on the
    first bad field."""
    required = {
        "permission_bypass", "resume", "autonomous_pane", "route_on_pane",
        "loop_participation", "command_surface", "permission_response", "resume_strategy",
        "model_switch_strategy",
        "ready_marker", "ready_rule_ids", "send_keys_enter_delay_ms", "submit_keys",
        "stop_strategy", "remove_strategy", "manifest_rules", "session_binding",
    }
    if not isinstance(caps, dict) or not required <= caps.keys():
        missing = sorted(required - set(caps or {}))
        raise _contract_error(harness, "contract", f"missing fields: {', '.join(missing)}")
    responses = caps["permission_response"]
    if not isinstance(responses, dict) or set(responses) != _RESPONSE_ACTIONS:
        raise _contract_error(harness, "permission_response", "needs all three actions")
    for action, response in responses.items():
        if not isinstance(response, dict) or not isinstance(response.get("supported"), bool):
            raise _contract_error(harness, f"permission_response.{action}", "bad support flag")
        keys = response.get("keys")
        rules = response.get("rule_ids")
        if not isinstance(keys, list) or not all(key in _KEY_TOKENS for key in keys):
            raise _contract_error(harness, "permission_response", f"bad keys for {action}")
        if not isinstance(rules, list) or not all(isinstance(rule, str) and rule for rule in rules):
            raise _contract_error(harness, "permission_response", f"bad rule ids for {action}")
        if response["supported"] and (not keys or not rules):
            raise _contract_error(harness, "permission_response", f"empty supported {action}")
    marker = caps["ready_marker"]
    ready_rules = caps["ready_rule_ids"]
    manifest_rules = caps["manifest_rules"]
    if not isinstance(marker, str) or not isinstance(ready_rules, list) or not isinstance(
        manifest_rules, list
    ):
        raise _contract_error(harness, "ready_marker", "must name a rule or unsupported")
    parsed_rules = {
        rule.get("id"): rule.get("state")
        for rule in manifest_rules
        if isinstance(rule, dict)
        and isinstance(rule.get("id"), str)
        and rule.get("state") in {"idle", "blocked"}
    }
    if len(parsed_rules) != len(manifest_rules):
        raise _contract_error(harness, "manifest_rules", "contains a malformed rule")
    if marker != "unsupported" and marker not in ready_rules:
        raise _contract_error(harness, "ready_marker", f"unknown rule {marker!r}")
    if marker != "unsupported" and parsed_rules.get(marker) != "idle":
        raise _contract_error(harness, "ready_marker", f"unknown positive rule {marker!r}")
    for action, response in responses.items():
        for rule_id in response["rule_ids"]:
            if parsed_rules.get(rule_id) != "blocked":
                raise _contract_error(
                    harness,
                    "permission_response",
                    f"{action} names unknown blocked rule {rule_id!r}",
                )
    delay = caps["send_keys_enter_delay_ms"]
    submit = caps["submit_keys"]
    if not isinstance(delay, int) or isinstance(delay, bool) or delay < 0:
        raise _contract_error(harness, "send_keys_enter_delay_ms", "must be non-negative")
    if not isinstance(submit, list) or not submit or not all(key in _KEY_TOKENS for key in submit):
        raise _contract_error(harness, "submit_keys", "has an invalid key token")
    # A lane that never submits must not carry a delay: the number would
    # describe a wait nothing performs. The CONVERSE does not hold and was
    # asserted here until codex disproved it. A supported contract may
    # legitimately need no wait - measured against codex 0.148.0, a
    # carriage return sent immediately after the text submits correctly,
    # while claude needs 800ms. The old rule read a coincidence across the
    # then-current harnesses as an invariant.
    if submit == ["unsupported"] and delay != 0:
        raise _contract_error(
            harness,
            "send_keys_enter_delay_ms",
            "an unsupported submit contract cannot carry a nonzero delay",
        )
    strategy = caps["resume_strategy"]
    forms = strategy.get("forms") if isinstance(strategy, dict) else None
    if not isinstance(forms, dict) or set(forms) != _SESSION_LANES:
        raise _contract_error(harness, "resume_strategy", "needs every session lane")
    for lane, form in forms.items():
        kind = form.get("kind") if isinstance(form, dict) else None
        tokens = form.get("tokens") if isinstance(form, dict) else None
        if kind not in _RESUME_KINDS or not isinstance(tokens, list) or not all(
            isinstance(token, str) and token for token in tokens
        ):
            raise _contract_error(harness, "resume_strategy", f"malformed {lane}")
        if kind == "unsupported" and tokens:
            raise _contract_error(harness, "resume_strategy", f"unsupported {lane} has tokens")
        if (
            lane == "interactive_attach"
            and kind != "unsupported"
            and "{short_id}" not in tokens
            and "{session_id}" not in tokens
        ):
            # An attach form must name the id its harness's own attach
            # command takes: claude's short jobId, or a full session id
            # where a short one would collide (a codex UUIDv7 head-8 is a
            # ~65.5s bucket).
            raise _contract_error(harness, "resume_strategy", f"{lane} drops its attach id")
        if lane.endswith("resume") and kind != "unsupported" and "{session_id}" not in tokens:
            raise _contract_error(harness, "resume_strategy", f"{lane} drops session id")
    model_switch = caps["model_switch_strategy"]
    expected_switch_fields = {
        "kind", "tokens", "effort_labels", "status_command", "status_pattern",
        # Optional: the status-bar fallback a truncated /status frame needs.
        "status_bar_pattern",
    }
    if not isinstance(model_switch, dict) or set(model_switch) - expected_switch_fields:
        raise _contract_error(harness, "model_switch_strategy", "malformed strategy")
    switch_kind = model_switch["kind"]
    switch_tokens = model_switch["tokens"]
    effort_labels = model_switch["effort_labels"]
    status_command = model_switch["status_command"]
    status_pattern = model_switch["status_pattern"]
    status_bar_pattern = model_switch.get("status_bar_pattern")
    if status_bar_pattern is not None and not isinstance(status_bar_pattern, str):
        raise _contract_error(harness, "model_switch_strategy", "malformed status bar pattern")
    if switch_kind not in _MODEL_SWITCH_KINDS:
        raise _contract_error(harness, "model_switch_strategy", "unknown kind")
    if not isinstance(switch_tokens, list) or not all(
        isinstance(token, str) and token for token in switch_tokens
    ):
        raise _contract_error(harness, "model_switch_strategy", "malformed tokens")
    if not isinstance(effort_labels, dict) or not all(
        effort in _MODEL_SWITCH_EFFORTS
        and isinstance(label, str)
        and label
        for effort, label in effort_labels.items()
    ):
        raise _contract_error(harness, "model_switch_strategy", "malformed effort labels")
    placeholders: list[str] = []
    for token in switch_tokens:
        token_placeholders = re.findall(r"\{([^{}]+)\}", token)
        remainder = re.sub(r"\{[^{}]+\}", "", token)
        if "{" in remainder or "}" in remainder:
            raise _contract_error(harness, "model_switch_strategy", "malformed placeholder")
        placeholders.extend(token_placeholders)
    unknown = set(placeholders) - _MODEL_SWITCH_PLACEHOLDERS
    if unknown:
        raise _contract_error(
            harness, "model_switch_strategy", f"unknown placeholder {sorted(unknown)[0]!r}"
        )
    if switch_kind == "unsupported":
        if switch_tokens or effort_labels or status_command or status_pattern:
            raise _contract_error(
                harness, "model_switch_strategy", "unsupported strategy has executable data"
            )
        if status_bar_pattern is not None:
            raise _contract_error(
                harness, "model_switch_strategy", "unsupported strategy has executable data"
            )
    else:
        if not isinstance(status_command, str) or not status_command.startswith("/"):
            raise _contract_error(harness, "model_switch_strategy", "missing status command")
        if not isinstance(status_pattern, str) or not status_pattern:
            raise _contract_error(harness, "model_switch_strategy", "missing status pattern")
        try:
            compiled_status = re.compile(status_pattern)
        except re.error as exc:
            raise _contract_error(
                harness, "model_switch_strategy", f"invalid status pattern: {exc}"
            ) from exc
        if not {"model", "effort"} <= compiled_status.groupindex.keys():
            raise _contract_error(
                harness, "model_switch_strategy", "status pattern needs model and effort groups"
            )
        if switch_kind == "direct":
            if placeholders.count("model") != 1 or placeholders.count("effort") != 1:
                raise _contract_error(
                    harness, "model_switch_strategy", "direct needs model and effort placeholders"
                )
            if "effort_label" in placeholders or effort_labels:
                raise _contract_error(
                    harness, "model_switch_strategy", "direct cannot carry menu labels"
                )
        elif (
            placeholders.count("model") != 1
            or placeholders.count("effort_label") != 1
            or "effort" in placeholders
            or placeholders.index("model") > placeholders.index("effort_label")
            or set(effort_labels) != _MODEL_SWITCH_EFFORTS
        ):
            raise _contract_error(
                harness,
                "model_switch_strategy",
                "menu_walk needs ordered model and effort targets",
            )
    if caps["loop_participation"] not in _LOOP_PARTICIPATION:
        raise _contract_error(harness, "loop_participation", "unknown member")
    # Only an `extension` row may name an artifact: a `native` row closes its
    # loop through a shell hook and a `none` row closes it through nothing.
    # The converse is legal and load-bearing - an `extension` row with an
    # EMPTY path is a harness whose extension fno has not written yet, and
    # the loop gate in capability_leaves.rs refuses a looping dispatch at it.
    if caps["loop_participation"] != "extension" and caps.get("loop_extension"):
        raise _contract_error(
            harness, "loop_extension",
            "only an extension harness may name a loop artifact",
        )
    if caps["stop_strategy"] not in _STOP_STRATEGIES:
        raise _contract_error(harness, "stop_strategy", "unknown strategy")
    if caps["remove_strategy"] not in _REMOVE_STRATEGIES:
        raise _contract_error(harness, "remove_strategy", "unknown strategy")
    binding = caps["session_binding"]
    if not isinstance(binding, dict) or set(binding) != {"strategy", "required", "timeout_ms"}:
        raise _contract_error(harness, "session_binding", "malformed strategy")
    if binding["strategy"] not in {
        "preassigned-or-session-start", "rollout-fd-or-daemon",
        # caller-assigned-cwd-scoped: the caller mints the id AND the
        # harness scopes its lookup by cwd, so the identity is the PAIR and
        # the id alone addresses nothing. Distinct from "preassigned",
        # where the id is the whole handle.
        "preassigned", "caller-assigned-cwd-scoped", "callee-minted-read-back",
        "store-lookup", "unsupported",
    }:
        raise _contract_error(harness, "session_binding", "unknown strategy")
    if not isinstance(binding["required"], bool) or not isinstance(binding["timeout_ms"], int):
        raise _contract_error(harness, "session_binding", "bad required/timeout values")
    if binding["timeout_ms"] < 0 or (binding["required"] and binding["timeout_ms"] == 0):
        raise _contract_error(harness, "session_binding", "required binding needs a timeout")


def parse_capability_contract(text: str) -> tuple[int, dict[str, dict]]:
    """Parse the packaged per-harness contract and reject partial defaults."""
    try:
        root = tomllib.loads(text)
    except tomllib.TOMLDecodeError as exc:
        raise DispatchResolveError(f"harness capability contract is invalid TOML: {exc}") from exc
    version = root.get("map_version")
    harnesses = root.get("harness")
    if not isinstance(version, int) or version < 1:
        raise DispatchResolveError("harness capability contract field 'map_version' is invalid")
    if not isinstance(harnesses, dict) or not harnesses:
        raise DispatchResolveError(
            "harness capability contract harness set is empty or not a table"
        )
    # The rows-stay-a-subset-of-the-roster pin lives on the Rust side (the
    # provider round-trip test reads this table's source): a roster read
    # here would fire on the module-level parse below, which is an import.
    for harness, caps in harnesses.items():
        _validate_row(harness, caps)
    return version, harnesses


def normalize_command(command: str, harness: str) -> str:
    """Translate a claude-syntax footnote slash command to ``harness``'s native
    invocation - the single normalizer both dispatch surfaces route through.

    ``/target --no-merge {id}`` becomes, per the harness ``command_surface``:
      - ``slash`` (claude, agy, opencode) -> ``/[slash_prefix]target --no-merge {id}``
        (prefix ``""`` for claude/agy -> verbatim; ``"fno:"`` for opencode's
        plugin-namespaced palette + ``opencode run --command`` -> ``/fno:target``)
      - ``codex-skill`` (codex)           -> ``$fno:target --no-merge {id}`` (swap the
        leading ``/command`` for ``$fno:command``; codex exec expands the plugin skill)
      - ``refused`` (gemini)              -> a loud :class:`DispatchResolveError`
        naming agy; the harness is deprecated and has no dispatch lane.

    ``command`` is expected to lead with ``/`` (a footnote slash command); a
    non-slash string is returned unchanged for the slash/codex surfaces (nothing
    to rewrite). So is a slash token with an INTERNAL slash (``/usr/bin/script
    {id}``): an absolute path is nobody's footnote command, and the guard lives
    HERE rather than at one call site so every caller inherits it - it used to
    live in ``resolve_dispatch`` alone, and the direct callers bypassed it and
    captured an absolute path into a phantom ``$fno:usr/bin/script`` skill.

    On the codex surface a bare ``/command`` is rewritten only when it names a
    shipped footnote command (:func:`footnote_commands`) that is not also a declared
    native command of the harness (``native_verbs`` in the capability table) -
    native codex commands (``/review``, ``/model``, ...) pass through untouched.
    The plugin-qualified ``/fno:command`` spelling is unambiguous by namespace and
    always rewrites. Pure string transform; no config or IO."""
    caps = capabilities(harness)  # loud on an unknown harness, before anything
    surface = caps["command_surface"]
    if surface == _REFUSED:
        # The deprecation tripwire stays ahead of the parse gate: a refused
        # harness has no dispatch lane, so every command is refused loudly,
        # whatever its first token shapes as.
        raise DispatchResolveError(_refused_reason(harness))
    cmd = command.strip()
    first_word = cmd.split(maxsplit=1)[0] if cmd else ""
    seed = parse_command_token(first_word) if first_word else None
    if seed is None:
        return cmd
    command_name, namespaced = seed
    tail = cmd[len(first_word):]
    slash_sigil = first_word.startswith("/")
    if surface == _CODEX_SKILL:
        if not slash_sigil:
            return cmd
        # The namespaced spelling is unambiguous by namespace: it always
        # rewrites, native or not (``/fno:review`` names the fno lane even
        # though bare ``/review`` is codex's native command).
        if namespaced:
            return "$fno:" + command_name + tail
        # A bare ``/command`` is rewritten only when it names a shipped footnote
        # command that is not also a declared native command of this harness: an
        # unknown or native command stays literal instead of being captured into
        # a phantom ``$fno:`` skill. ``review`` is the collision case - bare
        # ``/review`` on codex is the NATIVE command, and the fno lane is reached
        # namespaced, as ``/fno:review``.
        native = {v for v in caps.get("native_verbs") or () if isinstance(v, str)}
        if "/" + command_name in native:
            return cmd
        # The dispatch command is footnote's own: a STATIC fact. The roster read
        # below needs a resolvable plugin root, and an unresolvable one returns
        # an empty roster indistinguishable from "not ours", which rendered
        # `/target` for a codex worker as an ordinary pass-through. Not caching
        # that empty answer stops it freezing; this bypass makes it impossible.
        if first_word in _TARGET_FAMILY:
            return "$fno:" + command_name + tail
        if command_name not in footnote_commands():
            return cmd
        return "$fno:" + command_name + tail
    if surface == _SLASH:
        # Plugin-namespace prefix swap only (never re-tokenize): claude/agy inject
        # the skill natively (""), opencode's fno plugin exposes it as `/fno:command`.
        # The single rule renders every command - no per-command allowlist (AC4-EDGE).
        prefix = caps.get("slash_prefix", "")
        # A native command of the harness stays literal: `/undo` on opencode is
        # opencode's own palette command, and namespacing it would mint a phantom
        # `/fno:undo` plugin skill. Same `native_verbs` roster the codex-skill
        # branch reads; the claude/agy rows are inert here only because their
        # prefix is empty.
        native = {v for v in caps.get("native_verbs") or () if isinstance(v, str)}
        if not namespaced and not slash_sigil:
            return cmd
        if not namespaced and "/" + command_name in native:
            return cmd
        # Idempotent over the builtin rung: the resolve seam re-normalizes the
        # already-namespaced `/fno:command`, so re-applying would double it.
        if namespaced and prefix and first_word.startswith("/" + prefix):
            return cmd
        # The Rust renderer's order: the row's prefix first, then agy, `fno:`.
        if namespaced and prefix:
            return "/" + prefix + command_name + tail
        if namespaced and harness == "agy":
            return "/" + command_name + tail
        if namespaced:
            return "/fno:" + command_name + tail
        return "/" + prefix + command_name + tail
    return cmd


class DispatchResolveError(ValueError):
    """A dispatch cannot be resolved (unknown harness, bad substrate, empty
    command). Carries a message naming the offending value AND the map location
    so the failure is loud and actionable (AC1-ERR)."""


_PACKAGED_CONTRACT_TEXT = (
    files("fno.agents").joinpath("harness_capabilities.toml").read_text(encoding="utf-8")
)
MAP_VERSION, _BUNDLED_CAPS = parse_capability_contract(_PACKAGED_CONTRACT_TEXT)

#: Fail-open report of every override block a reader declined, naming the
#: config file and the reason (AC1-ERR). A warning never un-configures a
#: working harness: the bundled row stays and the mistake is on the record.
OVERRIDE_WARNINGS: list[str] = []

# The shallow lane keys an override may still use, mapped into the
# bundled row's nested paths so ONE override shape feeds both readers.
_LANE_ALIAS_PATHS = {
    "attach": ("resume_strategy", "forms", "interactive_attach"),
    "resume": ("resume_strategy", "forms", "interactive_resume"),
}


def _override_config_candidates() -> list[Path]:
    """The same candidate chain the Rust reader uses
    (agents_view.rs ``config_toml_candidates``): ``$PWD/.fno/config.toml``
    first, then the ``config_io`` global settings path's sibling
    ``config.toml`` (``FNO_GLOBAL_SETTINGS_PATH`` when set, else the state
    dir; an empty env var reads as unset there too)."""
    candidates = [Path.cwd() / ".fno" / "config.toml"]
    candidates.append(_global_settings_path().with_name("config.toml"))
    return candidates


def _lane_alias_normalized(override: dict) -> dict:
    """Translate the shallow lane keys into the nested bundled paths, so
    ``[harness.<name>.attach]`` lands on ``resume_strategy.forms.
    interactive_attach`` exactly as it does in the Rust reader."""
    out = {key: value for key, value in override.items() if key not in _LANE_ALIAS_PATHS}
    for alias, path in _LANE_ALIAS_PATHS.items():
        if alias not in override:
            continue
        node = out
        for key in path[:-1]:
            node = node.setdefault(key, {})
        node[path[-1]] = deepcopy(override[alias])
    return out


def _deep_merge_row(base: dict, override: dict) -> dict:
    """Recursive per-field merge, config winning: the per-field provider
    merge in crates/fno-agents/src/provider_route.rs as the precedent, one
    level deeper so a dotted table header
    (``[harness.x.resume_strategy.forms.interactive_attach]``) can correct one
    lane without rewriting the other four. A non-dict value replaces whole."""
    merged = deepcopy(base)
    for key, value in override.items():
        if isinstance(value, dict) and isinstance(merged.get(key), dict):
            merged[key] = _deep_merge_row(merged[key], value)
        else:
            merged[key] = deepcopy(value)
    return merged


def _apply_capability_overrides() -> None:
    """Overlay ``[harness.<name>]`` blocks from the config chain onto
    ``_HARNESS_CAPS``, in place, first candidate wins per harness name
    (project-local before global - the loader's record precedence). Every
    candidate row passes through :func:`_validate_row` BEFORE it lands, so an
    override obeys the same contract the bundled table ships under; a rejected
    override keeps the bundled row and names itself in
    :data:`OVERRIDE_WARNINGS` (AC1-ERR, fail-open). A row for a name the
    roster does not carry is refused by name (AC1-ERR, never advertise an
    unmeasured dispatch lane)."""
    _HARNESS_CAPS.clear()
    _HARNESS_CAPS.update(deepcopy(_BUNDLED_CAPS))
    OVERRIDE_WARNINGS.clear()
    overridden: set[str] = set()
    for path in _override_config_candidates():
        try:
            body = path.read_text(encoding="utf-8")
        except OSError:
            continue
        try:
            doc = tomllib.loads(body)
        except tomllib.TOMLDecodeError as exc:
            OVERRIDE_WARNINGS.append(f"{path}: invalid TOML: {exc}")
            continue
        table = doc.get("harness")
        if not isinstance(table, dict):
            continue
        from fno.harness_names import KNOWN_HARNESSES

        for name, override in table.items():
            if name in overridden or not isinstance(override, dict):
                continue
            if name not in KNOWN_HARNESSES:
                OVERRIDE_WARNINGS.append(
                    f"{path}: harness {name!r} override rejected: absent from KNOWN_HARNESSES"
                )
                continue
            candidate = _deep_merge_row(
                _HARNESS_CAPS.get(name, {}), _lane_alias_normalized(override)
            )
            try:
                _validate_row(name, candidate)
            except DispatchResolveError as exc:
                OVERRIDE_WARNINGS.append(f"{path}: harness {name!r} override rejected: {exc}")
                continue
            _HARNESS_CAPS[name] = candidate
            overridden.add(name)


_HARNESS_CAPS: dict[str, dict] = {}
_apply_capability_overrides()


def known_harnesses() -> list[str]:
    """Sorted names of the harnesses that carry a capability row: the
    loud-error candidate list and the dispatch-capable roster. The COMPLETE
    roster is ``fno.harness_names.KNOWN_HARNESSES`` (the Rust list, served
    through that door); hermes and openclaw sit on it with no row here."""
    return sorted(_HARNESS_CAPS)


def capabilities(harness: str) -> dict:
    """Capability dict for ``harness``. Raises :class:`DispatchResolveError`
    naming the map module when unknown - never silently defaults to claude."""
    caps = _HARNESS_CAPS.get(harness)
    if caps is None:
        raise DispatchResolveError(
            f"unknown harness {harness!r}; the harness-capability map "
            f"(fno.agents.harness_map) knows: {', '.join(known_harnesses())}"
        )
    return caps


def is_declared(harness: str) -> bool:
    """True when ``harness`` carries a measured capability row. The declared/
    undeclared split is a TABLE fact, never a name list, so a new row flips a
    harness to declared with no code edit."""
    return harness in _HARNESS_CAPS


def render_session_argv(
    harness: str,
    lane: str,
    session_id: Optional[str] = None,
    *,
    short_id: Optional[str] = None,
) -> list[str]:
    """Render one form with the identity type its contract declares."""
    form = capabilities(harness)["resume_strategy"]["forms"].get(lane)
    if form is None:
        raise DispatchResolveError(f"harness {harness!r} resume_strategy has no lane {lane!r}")
    if form["kind"] == "unsupported":
        raise DispatchResolveError(
            f"harness {harness!r} lane {lane!r} is unsupported by resume_strategy"
        )
    tokens = list(form["tokens"])
    if "{short_id}" in tokens:
        if session_id:
            raise DispatchResolveError(
                f"harness {harness!r} lane {lane!r} needs a short_id, not a session_id"
            )
        if not short_id:
            raise DispatchResolveError(
                f"harness {harness!r} lane {lane!r} needs a non-empty short_id"
            )
        return [short_id if token == "{short_id}" else token for token in tokens]
    if short_id:
        raise DispatchResolveError(
            f"harness {harness!r} lane {lane!r} accepts a session_id, not a short_id"
        )
    if "{session_id}" not in tokens:
        return tokens
    if session_id:
        return [session_id if token == "{session_id}" else token for token in tokens]
    if lane.endswith("create"):
        index = tokens.index("{session_id}")
        start = index - 1 if index > 0 and tokens[index - 1].startswith("-") else index
        return tokens[:start] + tokens[index + 1 :]
    raise DispatchResolveError(f"harness {harness!r} lane {lane!r} needs a non-empty session id")


# The env budget a brief must fit; 8 KB, measured in UTF-8 bytes (Locked
# Decision 9 / epic Boundaries). Oversized -> explicit error, never truncation.
_BRIEF_MAX_BYTES = 8192
