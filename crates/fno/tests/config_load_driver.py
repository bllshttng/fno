#!/usr/bin/env python3
"""Live oracle for the config loader port (the port protocol, step 2).

  --capture DIR   enumerate every pydantic validator decorator on the
                  settings model tree, probe each one through the real
                  loader, and freeze the outcomes into DIR:
                    validators.tsv  one row per decorator (the checklist)
                    cases.jsonl     one probe per line: the settings.yaml
                                    text, the exit code (0 loaded, 1
                                    refused), the stderr the load printed,
                                    and the validated value at the probed
                                    path (the whole model for loader cases)
                    retired.jsonl   the retired dispatch-key drop, frozen
                                    while its Python leg lived (that leg is
                                    deleted; the rows stay committed)

The loader is the live `fno.config.load_settings` with FNO_CONFIG pinned to
one settings.yaml per case, so every case is a single layer named
`<FIXTURE>/settings.yaml`. Capture refuses once the Python loader is gone:
a golden can only be frozen while the leg runs.

Validators are enumerated from pydantic itself
(`__pydantic_decorators__`), because the committed schema table carries no
validator mark. Each decorator is probed at every place its model mounts
under SettingsModel with a fixed set of loose inputs plus values derived
from the field's own JSON schema (enum members, bounds, the default).
Probes whose outcome repeats an earlier probe's are dropped, so each kept
case shows a distinct behavior.
"""
from __future__ import annotations

import contextlib
import io
import json
import os
import sys
import tempfile
import typing
from pathlib import Path

REPO = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(REPO / "cli" / "src"))

PROBES: list[object] = [
    True, False, 0, 1, -1, 2.5, 100000, "", "yes", "no", "off", "1", "abc",
    "a b", "*", "a/b", [], ["x"], [1], {}, {"x": 1}, None,
]
TYPE_PROBES: list[object] = ["abc", "yes", "3", 2.5, -1, True, [], {}, None]
CASES_PER_MOUNT = 6
MOUNT_KEY = "probe"


def _models(annotation: object) -> list[tuple[str, type]]:
    """(shape, model) for every BaseModel inside an annotation: shape is
    "model", "dict" (dict value) or "list" (list item)."""
    from pydantic import BaseModel

    found: list[tuple[str, type]] = []
    origin = typing.get_origin(annotation)
    args = typing.get_args(annotation)
    if isinstance(annotation, type) and issubclass(annotation, BaseModel):
        return [("model", annotation)]
    if origin is dict and len(args) == 2:
        return [("dict", m) for _, m in _models(args[1]) if _ == "model"]
    if origin is list and len(args) == 1:
        return [("list", m) for _, m in _models(args[0]) if _ == "model"]
    for arg in args:
        found.extend(_models(arg))
    return found


def mounts() -> dict[type, list[tuple[object, ...]]]:
    """Every model class reachable from SettingsModel, with the loc tuples
    where it mounts. A dict value mounts under MOUNT_KEY, a list item at 0."""
    from fno.config import SettingsModel

    seen: dict[type, list[tuple[object, ...]]] = {SettingsModel: [()]}
    queue: list[tuple[type, tuple[object, ...]]] = [(SettingsModel, ())]
    while queue:
        model, loc = queue.pop(0)
        for name, info in model.model_fields.items():
            for shape, sub in _models(info.annotation):
                here = loc + (name,) + ((MOUNT_KEY,) if shape == "dict" else (0,) if shape == "list" else ())
                if len(here) > 6:
                    continue
                first = sub not in seen
                seen.setdefault(sub, []).append(here)
                if first:
                    queue.append((sub, here))
    return seen


def decorators(model: type) -> list[tuple[str, str, tuple[str, ...]]]:
    """(kind, method, fields) for decorators the class body declares."""
    decs = model.__pydantic_decorators__
    rows: list[tuple[str, str, tuple[str, ...]]] = []
    for name, dec in decs.field_validators.items():
        if dec.cls_ref.split(":")[0] != f"{model.__module__}.{model.__qualname__}":
            continue
        rows.append(("field", name, tuple(dec.info.fields)))
    for name, dec in decs.model_validators.items():
        if dec.cls_ref.split(":")[0] != f"{model.__module__}.{model.__qualname__}":
            continue
        rows.append(("model", name, ()))
    return rows


def nest(loc: tuple[object, ...], value: object) -> object:
    for part in reversed(loc):
        value = [value] if isinstance(part, int) else {str(part): value}
    return value


def at(dump: object, loc: tuple[object, ...]) -> object:
    for part in loc:
        if isinstance(part, int):
            dump = dump[part] if isinstance(dump, list) and len(dump) > part else None
        else:
            dump = dump.get(str(part)) if isinstance(dump, dict) else None
    return dump


def schema_probes(model: type, field: str) -> list[object]:
    """Values the field's own JSON schema suggests: enum members, bounds,
    one past each bound, and the default."""
    try:
        schema = model.model_json_schema()["properties"][field]
    except Exception:  # noqa: BLE001 - a probe source, not a contract
        return []
    found: list[object] = []

    def visit(node: object) -> None:
        if not isinstance(node, dict):
            return
        for member in node.get("enum", []):
            found.append(member)
        if "const" in node:
            found.append(node["const"])
        for bound, step in (("minimum", -1), ("maximum", 1), ("exclusiveMinimum", 0), ("exclusiveMaximum", 0)):
            if bound in node:
                found.extend([node[bound], node[bound] + step])
        for sub in node.get("anyOf", []):
            visit(sub)

    visit(schema)
    if "default" in schema:
        found.append(schema["default"])
    return found


def load(root: Path, text: str) -> tuple[int, str, object]:
    """One load through the live loader: (exit, stderr, model dump)."""
    import fno.config as config
    from fno.config import _loader, load_settings

    # Every case is a fresh process to the reader: the legacy-alias notes
    # fire once per process, and Python's warning registry dedupes by site.
    config._DEPRECATED_WARNED.clear()
    settings = root / "settings.yaml"
    settings.write_text(text, encoding="utf-8")
    os.environ["FNO_CONFIG"] = str(settings)
    _loader._load_settings_at.cache_clear()
    err = io.StringIO()
    with contextlib.redirect_stderr(err):
        try:
            model = load_settings()
        except _loader.SettingsRefused as exc:
            return 1, fold(err.getvalue(), root), fold(exc.message, root)
    return 0, fold(err.getvalue(), root), model.model_dump(mode="json")


def fold(text: str, root: Path) -> str:
    resolved = str(root.resolve())
    for token in (f"/private{resolved}", resolved, str(root)):
        text = text.replace(token, "<FIXTURE>")
    return text


def dump_yaml(value: object) -> str:
    import yaml

    return yaml.safe_dump(value, sort_keys=False, default_flow_style=False, allow_unicode=True)


def capture(golden: Path) -> None:
    from fno.config import SettingsModel  # noqa: F401 - refuses once the leg is gone

    golden.mkdir(parents=True, exist_ok=True)
    root = Path(tempfile.mkdtemp())
    home = root / "home"
    (home / ".fno").mkdir(parents=True)
    os.environ.clear()
    os.environ.update(
        {
            "PATH": "/usr/bin:/bin",
            "HOME": str(home),
            "FNO_GLOBAL_SETTINGS_PATH": str(home / ".fno" / "settings.yaml"),
            "FNO_NO_CANONICAL_CONFIG": "1",
            "FNO_STATE_DIR": str(root / "state"),
        }
    )
    os.chdir(root)
    sys.argv = ["fno"]
    import warnings

    # The host owns the warning's file:line prefix (it names the venv); the
    # contract is the category and the text.
    warnings.simplefilter("always")
    warnings.showwarning = lambda message, category, *_a, **_k: print(
        f"{category.__name__}: {message}", file=sys.stderr
    )

    tsv: list[str] = []
    cases: list[dict[str, object]] = []
    exit_code, stderr, dump = load(root, "{}\n")
    cases.append({"name": "defaults", "validator": "", "loc": [], "yaml": "{}\n", "exit": exit_code, "stderr": stderr, "value": dump})
    # The shipped example config: every key at its documented value. A false
    # refusal here would break every native config read.
    import tomllib

    example = dump_yaml(tomllib.loads((REPO / "docs" / "config.example.toml").read_text(encoding="utf-8")))
    exit_code, stderr, dump = load(root, example)
    cases.append({"name": "example", "validator": "", "loc": [], "yaml": example, "exit": exit_code, "stderr": stderr, "value": dump})

    for model, locs in mounts().items():
        for kind, method, fields in decorators(model):
            row = f"{model.__module__}\t{model.__qualname__}\t{method}"
            tsv.append(row)
            # The first mount carries the probes; the others repeat its code.
            targets = [(locs[0] + (f,), f) for f in fields] if kind == "field" else [(locs[0], None)]
            for loc, field in targets:
                seen: set[str] = set()
                kept = 0
                probes = (schema_probes(model, field) if field else []) + PROBES
                for probe in probes:
                    if kept >= CASES_PER_MOUNT:
                        break
                    if not loc and not isinstance(probe, dict):
                        continue
                    text = dump_yaml(nest(loc, probe))
                    exit_code, stderr, out = load(root, text)
                    value = out if exit_code else at(out, loc)
                    key = json.dumps([exit_code, stderr, value, value == probe], sort_keys=True, default=str)
                    if key in seen:
                        continue
                    seen.add(key)
                    kept += 1
                    cases.append(
                        {
                            "name": f"{model.__qualname__}.{method}.{len(cases)}",
                            "validator": row,
                            "loc": list(loc),
                            "yaml": text,
                            "exit": exit_code,
                            "stderr": stderr,
                            "value": value,
                        }
                    )
    # One bad value per JSON-Schema leaf type, for the pydantic error kinds
    # no validator owns (Unknown 4 in the plan).
    table = json.loads((REPO / "crates" / "fno" / "config_schema.json").read_text(encoding="utf-8"))
    by_type: dict[str, list[str]] = {}
    for leaf in table["leaves"]:
        by_type.setdefault(leaf["type"], []).append(leaf["path"])
    for leaf_type, paths in by_type.items():
        for path in paths[:2]:
            loc = tuple(path.split("."))
            seen = set()
            for probe in TYPE_PROBES:
                text = dump_yaml(nest(loc, probe))
                exit_code, stderr, out = load(root, text)
                value = out if exit_code else at(out, loc)
                key = json.dumps([exit_code, stderr, value, value == probe], sort_keys=True, default=str)
                if key in seen:
                    continue
                seen.add(key)
                cases.append({"name": f"type.{path}.{len(cases)}", "validator": "", "loc": list(loc), "yaml": text, "exit": exit_code, "stderr": stderr, "value": value})
    # A zero lane cap closes the lane; the probe budget above can drop it.
    for name, probe in (("scalar", {"zai": 0}), ("table", {"zai": {"lanes": 0}, "anthropic": {"lanes": 0}})):
        text = dump_yaml(nest(("agents", "provider_limits"), probe))
        exit_code, stderr, out = load(root, text)
        cases.append({"name": f"closed_lane.{name}", "validator": "", "loc": ["agents", "provider_limits"], "yaml": text, "exit": exit_code, "stderr": stderr, "value": at(out, ("agents", "provider_limits"))})
    (golden / "validators.tsv").write_text("".join(f"{r}\n" for r in tsv), encoding="utf-8")
    with (golden / "cases.jsonl").open("w", encoding="utf-8") as fh:
        for case in cases:
            fh.write(json.dumps(case, sort_keys=True) + "\n")
    print(f"{len(tsv)} validators, {len(cases)} cases")


def emit_fields(target: Path) -> None:
    """The model table the native model reads: per model, its extra mode and
    its fields in definition order, each with its dumped default (a factory
    is called once) or required. The schema table sorts keys and drops
    factories, and both decide refusal order and the defaults dump."""
    from pydantic import TypeAdapter
    from pydantic_core import PydanticUndefined

    table: dict[str, object] = {}
    for model in mounts():
        fields = []
        for name, info in model.model_fields.items():
            row: dict[str, object] = {"name": name}
            if info.alias and info.alias != name:
                row["alias"] = info.alias
            if info.default_factory is not None:
                value = info.default_factory()
            elif info.default is PydanticUndefined:
                row["required"] = True
                fields.append(row)
                continue
            else:
                value = info.default
            row["default"] = TypeAdapter(info.annotation).dump_python(value, mode="json")
            fields.append(row)
        table[model.__name__] = {"extra": model.model_config.get("extra") or "ignore", "fields": fields}
    from fno.config.registry import FIELD_META

    doc = {"models": table, "field_meta": list(FIELD_META)}
    target.write_text(json.dumps(doc, indent=1) + "\n", encoding="utf-8")


def main() -> None:
    if len(sys.argv) == 3 and sys.argv[1] == "--capture":
        capture(Path(sys.argv[2]).resolve())
        return
    if len(sys.argv) == 3 and sys.argv[1] == "--emit-fields":
        emit_fields(Path(sys.argv[2]).resolve())
        return
    sys.exit("usage: config_load_driver.py --capture GOLDEN_DIR | --emit-fields PATH")


if __name__ == "__main__":
    main()
