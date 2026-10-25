"""Schema walker + the committed config schema table.

The Pydantic ``SettingsModel`` is the single source of truth for what a config
key is, its type, and its default. This module walks that model (plus the
presentation ``registry``) and writes ONE derived artifact:
``crates/fno/config_schema.json``. The native ``fno config schema`` verb
renders every output (JSON Schema, Markdown reference, example TOML, wizard
plan) from that table, and the drift test regenerates it and compares, so the
table stays one source and never becomes a second one.

Regenerate with ``cd cli && uv run python -m fno.config.schema_gen --write``.

Determinism: leaves are emitted in ``model_fields`` declaration order (stable),
so regenerating twice produces byte-identical output (AC1-HP / AC5-FR).
"""
from __future__ import annotations

import json
import sys
import typing
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Iterator, Optional, TypeGuard

from pydantic import BaseModel

from fno.config import SettingsModel
from fno.config import registry as _registry


@dataclass(frozen=True)
class Leaf:
    """One terminal config key (a scalar, list, or map field)."""

    path: str
    type_str: str
    default: Any


def _is_model(t: object) -> TypeGuard[type[BaseModel]]:
    return isinstance(t, type) and issubclass(t, BaseModel)


def _model_arg(annotation: object) -> Optional[type[BaseModel]]:
    """Return the BaseModel class iff ``annotation`` is a *direct* model or
    Optional[model]. A ``list[Model]`` / ``dict[str, Model]`` is NOT descended:
    its container is a leaf (the value structure is dynamic).
    """
    if _is_model(annotation):
        return annotation
    origin = typing.get_origin(annotation)
    # Only unwrap Optional / Union, never list/dict/etc.
    if origin is typing.Union:
        args = [a for a in typing.get_args(annotation) if a is not type(None)]
        if len(args) == 1:
            arg = args[0]
            if _is_model(arg):
                return arg
    return None


def _type_str(annotation: object) -> str:
    """Render a field annotation as a short, human-readable type string."""
    origin = typing.get_origin(annotation)
    args = typing.get_args(annotation)
    if origin is typing.Union:
        non_none = [a for a in args if a is not type(None)]
        rendered = " | ".join(_type_str(a) for a in non_none)
        return f"{rendered} (optional)" if type(None) in args else rendered
    if origin in (list, typing.List):
        inner = _type_str(args[0]) if args else "Any"
        return f"list[{inner}]"
    if origin in (dict, typing.Dict):
        if len(args) == 2:
            return f"dict[{_type_str(args[0])}, {_type_str(args[1])}]"
        return "dict"
    if isinstance(annotation, type):
        return annotation.__name__
    return str(annotation)


def iter_leaves(
    model: type[BaseModel] = SettingsModel,
    *,
    _prefix: str = "",
    _instance: Optional[BaseModel] = None,
) -> Iterator[Leaf]:
    """Yield every terminal leaf of ``model`` in declaration order.

    Descends only into nested BaseModel fields (config blocks); collections of
    models (``list[Model]`` / ``dict[str, Model]``) are emitted as a single leaf
    for their container. Defaults are read off a default-constructed instance so
    the reported default is exactly what the model would produce.
    """
    if _instance is None:
        _instance = model()  # all blocks have defaults / default_factory
    for name, field in model.model_fields.items():
        path = f"{_prefix}.{name}" if _prefix else name
        inner = _model_arg(field.annotation)
        value = getattr(_instance, name, None)
        if inner is not None and isinstance(value, BaseModel):
            yield from iter_leaves(inner, _prefix=path, _instance=value)
        else:
            yield Leaf(path=path, type_str=_type_str(field.annotation), default=value)


def all_leaf_paths(model: type[BaseModel] = SettingsModel) -> list[str]:
    """All leaf dotted paths, in declaration order (used by the CI checks)."""
    return [leaf.path for leaf in iter_leaves(model)]


# ---------------------------------------------------------------------------
# The committed table
# ---------------------------------------------------------------------------

TABLE_PATH = Path(__file__).resolve().parents[4] / "crates" / "fno" / "config_schema.json"


def _jsonable(value: Any) -> Any:
    """Reduce a default to something `json.dumps` accepts.

    A default can be a Pydantic model now that a config value is a RECORD and
    not only a scalar (`agents.provider_limits` carries a `ProviderBudget`). Without
    this the generator raised on the model instance, so the docs gate failed
    with a TypeError instead of reporting drift.
    """
    if isinstance(value, BaseModel):
        return value.model_dump()
    if isinstance(value, dict):
        return {k: _jsonable(v) for k, v in value.items()}
    if isinstance(value, list):
        return [_jsonable(v) for v in value]
    return value


def table() -> str:
    """The committed table: Pydantic's JSON Schema plus one row per leaf.

    A leaf with no registry entry reads wizard ``never`` and empty text, which
    is what every renderer treated a missing entry as.
    """
    leaves: list[dict[str, Any]] = []
    for leaf in iter_leaves():
        meta = _registry.meta_for(leaf.path)
        leaves.append(
            {
                "path": leaf.path,
                "type": leaf.type_str,
                "default": _jsonable(leaf.default),
                "wizard": meta.wizard if meta else "never",
                "question": meta.question if meta else "",
                "default_source": meta.default_source if meta else "",
                "doc": meta.doc if meta else "",
            }
        )
    doc = {"json_schema": SettingsModel.model_json_schema(), "leaves": leaves}
    return json.dumps(doc, indent=1) + "\n"


def main(argv: list[str]) -> int:
    """``--write`` regenerates the table; ``--check`` exits 2 when it is stale."""
    rendered = table()
    if argv == ["--write"]:
        TABLE_PATH.write_text(rendered, encoding="utf-8")
        print(f"wrote {TABLE_PATH}")
        return 0
    if argv == ["--check"]:
        try:
            current = TABLE_PATH.read_text(encoding="utf-8")
        except (OSError, UnicodeDecodeError):
            current = None
        if current != rendered:
            print(
                f"error: {TABLE_PATH} is stale; run "
                "`cd cli && uv run python -m fno.config.schema_gen --write`",
                file=sys.stderr,
            )
            return 2
        print(f"{TABLE_PATH} is up to date")
        return 0
    print("usage: python -m fno.config.schema_gen --write | --check", file=sys.stderr)
    return 2


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
