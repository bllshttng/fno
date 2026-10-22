#!/usr/bin/env python3
"""Env-var registry: every environment variable either tree reads has a row
in docs/env-vars.md, and every row has a reader.

295 config keys carry a Meta row in cli/src/fno/config/registry.py; the ~218
environment variable names read across the Python and Rust trees had no
inventory at all (node, measured 2026-09-12). This check closes that:
the doc IS the registry, the scanner is the ratchet.

Failures (each names the file:line and the doc):
  1. a read with no table row,
  2. a table row no read site has,
  3. a row whose Read by column (py / rs / py+rs) disagrees with the sites.

Scans (regex feeds, per the plan, applied to whole file content so a call
split across lines is still seen):
  py  cli/src/fno/**/*.py   environ.get(, os.getenv(, getenv(, environ[,
                            environ.setdefault(, environ.pop( with a literal
                            UPPER_SNAKE name
  rs  crates/**/*.rs        env::var(, env::var_os( and env_dir_unless_null(
                            with a literal name,
                            any path part named target excluded

Known limit: a name read through any other variable (lowercase locals, loop
variables, concat!/format!-built names) is not seen. A name read through an
UPPER_SNAKE string constant resolves to its value: a definition in the same
file wins, else the side must hold exactly one definition; zero definitions
or two different values is a refusal. On the Rust side
only directories named tests are excluded, so a read inside a #[cfg(test)]
module in a src file still counts (it gets a row; delete the row if the read
moves out).

--update rewrites the table, preserving the Meaning text of surviving rows;
new rows get `unclear: <first site>` and dead rows drop.

Usage:
  python3 scripts/ci/check_env_registry.py              # check against the doc
  python3 scripts/ci/check_env_registry.py --update     # regenerate the table
  python3 scripts/ci/check_env_registry.py --selftest   # fixture selftest

Exit: 0 pass, 1 a refusal, 2 selftest failure.
"""

from __future__ import annotations

import argparse
import re
import sys
import tempfile
from collections import defaultdict
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent.parent
DOC_REL = Path("docs/env-vars.md")
PY_ROOT = Path("cli/src/fno")
RS_ROOT = Path("crates")

# Reads AND writes: a write defines the contract for a name as much as a read.
PY_READ_RE = re.compile(
    r"""(?x)
    environ\.(?:get|setdefault|pop)\(\s*['"]([A-Z][A-Z0-9_]+)['"]
    | os\.getenv\(\s*['"]([A-Z][A-Z0-9_]+)['"]
    | (?<![\w.])getenv\(\s*['"]([A-Z][A-Z0-9_]+)['"]
    | environ\[\s*['"]([A-Z][A-Z0-9_]+)['"]\s*\]
    """
)
RS_READ_RE = re.compile(r'\b(?:env::var(?:_os)?|env_dir_unless_null)\(\s*"([A-Z][A-Z0-9_]+)"')

# The same call shapes with a bare UPPER_SNAKE identifier instead of a literal.
RS_CONST_READ_RE = re.compile(
    r"\benv::var(?:_os)?\(\s*&?(?:[a-z_][a-z0-9_]*::)*([A-Z][A-Z0-9_]*)\s*\)"
)
PY_CONST_READ_RE = re.compile(
    r"""(?x)
    environ\.(?:get|setdefault|pop)\(\s*([A-Z][A-Z0-9_]*)\s*[,)\]]
    | os\.getenv\(\s*([A-Z][A-Z0-9_]*)\s*[,)\]]
    | (?<![\w.])getenv\(\s*([A-Z][A-Z0-9_]*)\s*[,)\]]
    | environ\[\s*([A-Z][A-Z0-9_]*)\s*\]
    """
)
# A constant whose value is the env var name itself.
RS_CONST_DEF_RE = re.compile(
    r"\bconst\s+([A-Z][A-Z0-9_]*)\s*:\s*&(:?'static\s+)?str\s*=\s*\"([_A-Z][_A-Z0-9]*)\""
)
PY_CONST_DEF_RE = re.compile(
    r"^([A-Z][A-Z0-9_]+)\s*(?::\s*[^=\n]+)?=\s*[\"']([_A-Z][_A-Z0-9]*)[\"']", re.M
)


def scan(root: Path) -> tuple[dict[str, dict[str, list[str]]], list[str]]:
    """Return ({name: {"py": [file:line, ...], "rs": [...]}}, refusal lines)."""
    sites: dict[str, dict[str, list[str]]] = defaultdict(lambda: defaultdict(list))
    refusals: list[str] = []

    def harvest(pattern: re.Pattern[str], text: str, rel: Path, side: str) -> None:
        # Whole-content finditer: a call split across lines is still a site.
        for m in pattern.finditer(text):
            lineno = text.count("\n", 0, m.start()) + 1
            name = next(g for g in m.groups() if g)
            sites[name][side].append(f"{rel}:{lineno}")

    # Pass 1: collect constant definitions per side (same-file overrides tree-wide).
    texts: dict[str, list[tuple[Path, str]]] = {"py": [], "rs": []}
    tree_defs: dict[str, dict[str, set[str]]] = {"py": defaultdict(set), "rs": defaultdict(set)}
    file_defs: dict[str, dict[Path, dict[str, set[str]]]] = {"py": defaultdict(dict), "rs": defaultdict(dict)}
    for side, paths, def_re in (
        ("py", sorted((root / PY_ROOT).rglob("*.py")), PY_CONST_DEF_RE),
        ("rs", sorted((root / RS_ROOT).rglob("*.rs")), RS_CONST_DEF_RE),
    ):
        for path in paths:
            rel = path.relative_to(root)
            if side == "rs" and any(part == "target" or part == "tests" for part in rel.parts):
                continue
            text = path.read_text(encoding="utf-8", errors="replace")
            texts[side].append((rel, text))
            for m in def_re.finditer(text):
                value = m.group(2) if side == "py" else m.group(3)
                tree_defs[side][m.group(1)].add(value)
                file_defs[side][rel].setdefault(m.group(1), set()).add(value)

    # Pass 2: literal reads, then const reads resolved to their value.
    for rel, text in texts["py"]:
        harvest(PY_READ_RE, text, rel, "py")
        for m in PY_CONST_READ_RE.finditer(text):
            lineno = text.count("\n", 0, m.start()) + 1
            const = next(g for g in m.groups() if g)
            _record_const(sites, refusals, file_defs["py"], tree_defs["py"], "py", rel, lineno, const)
    for rel, text in texts["rs"]:
        harvest(RS_READ_RE, text, rel, "rs")
        for m in RS_CONST_READ_RE.finditer(text):
            lineno = text.count("\n", 0, m.start()) + 1
            const = next(g for g in m.groups() if g)
            _record_const(sites, refusals, file_defs["rs"], tree_defs["rs"], "rs", rel, lineno, const)
    return sites, refusals


def _record_const(
    sites: dict[str, dict[str, list[str]]],
    refusals: list[str],
    file_defs: dict[Path, dict[str, set[str]]],
    tree_defs: dict[str, set[str]],
    side: str,
    rel: Path,
    lineno: int,
    const: str,
) -> None:
    values = file_defs.get(rel, {}).get(const) or tree_defs.get(const)
    if values and len(values) == 1:
        sites[next(iter(values))][side].append(f"{rel}:{lineno}")
    else:
        refusals.append(
            f"{rel}:{lineno}: env read through {const} resolves to "
            f"{len(values or ())} values; use a literal or one const"
        )


def read_doc(root: Path) -> dict[str, tuple[str, str]]:
    """Return {name: (read_by, meaning)} from the doc table."""
    doc = {}
    text = (root / DOC_REL).read_text(encoding="utf-8")
    for line in text.splitlines():
        m = re.match(r"^\|\s*`([_A-Z][_A-Z0-9]*)`\s*\|\s*([\w+]+)\s*\|(.*)\|\s*$", line)
        if m:
            doc[m.group(1)] = (m.group(2), m.group(3).strip())
    return doc


def render(sites: dict[str, dict[str, list[str]]], meanings: dict[str, str]) -> str:
    lines = [
        "# Environment variable registry",
        "",
        "Every env var name either tree reads, one row each. The registry IS this table. `scripts/ci/check_env_registry.py` fails a read with no row, a row with no reader, and a wrong Read by. `--update` regenerates the table and preserves Meaning text.",
        "",
        "A meaning not derivable from the read site stays `unclear: <file:line>`, never invented. A name read through an UPPER_SNAKE string constant resolves to its value; any other variable stays unseen (scanner limit).",
        "",
        "| Name | Read by | Meaning |",
        "|------|---------|---------|",
    ]
    for name in sorted(sites):
        read_by = "+".join(sorted(sites[name]))
        first = sites[name][next(iter(sorted(sites[name])))][0]
        lines.append(f"| `{name}` | {read_by} | {meanings.get(name, f'unclear: {first}')} |")
    return "\n".join(lines) + "\n"


def run(root: Path, update: bool = False) -> int:
    doc_path = root / DOC_REL
    sites, refusals = scan(root)
    if refusals:
        print(f"env-registry: FAIL ({len(refusals)})\n" + "\n".join(refusals), file=sys.stderr)
        return 1
    if update:
        old = read_doc(root) if doc_path.exists() else {}
        meanings = {name: meaning for name, (_, meaning) in old.items()}
        doc_path.parent.mkdir(parents=True, exist_ok=True)
        doc_path.write_text(render(sites, meanings), encoding="utf-8")
        print(f"env-registry: wrote {len(sites)} rows to {DOC_REL}")
        return 0
    doc = read_doc(root)
    fails = []
    for name, by in sorted(sites.items()):
        want = "+".join(sorted(by))
        if name not in doc:
            fails.append(
                f"{name} read at {by[sorted(by)[0]][0]} has no row in {DOC_REL}"
            )
        elif doc[name][0] != want:
            fails.append(
                f"{name}: doc says Read by {doc[name][0]}, sites say {want} "
                f"(first: {by[sorted(by)[0]][0]}); fix {DOC_REL}"
            )
    for name in sorted(set(doc) - set(sites)):
        fails.append(f"{name}: row in {DOC_REL} has no read site; delete the row")
    if fails:
        print(f"env-registry: FAIL ({len(fails)})\n" + "\n".join(fails), file=sys.stderr)
        return 1
    print(f"env-registry: ok ({len(sites)} names, all rows agree)")
    return 0


def selftest() -> int:
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        py = root / PY_ROOT
        rs = root / RS_ROOT / "fno" / "src"
        py.mkdir(parents=True)
        rs.mkdir(parents=True)
        (py / "a.py").write_text(
            'import os\nx = os.environ.get("FOO_BAR")\ny = os.environ[\n    "BAZ_QUX"\n]\n',
            encoding="utf-8",
        )
        (rs / "b.rs").write_text(
            'fn f() {\n    if let Ok(v) = std::env::var(\n        "FOO_BAR",\n    ) {}\n}\n',
            encoding="utf-8",
        )
        # A read inside a tests dir is out of scope by contract.
        tests = root / RS_ROOT / "fno" / "tests"
        tests.mkdir()
        (tests / "t.rs").write_text('fn t() { env::var("TEST_ONLY"); }\n', encoding="utf-8")

        run(root, update=True)
        doc = read_doc(root)
        if set(doc) != {"FOO_BAR", "BAZ_QUX"}:
            print(f"selftest: rows wrong: {sorted(doc)}", file=sys.stderr)
            return 2
        if doc["FOO_BAR"][0] != "py+rs":
            print("selftest: FOO_BAR must read py+rs", file=sys.stderr)
            return 2
        if run(root) != 0:
            print("selftest: consistent tree did not pass", file=sys.stderr)
            return 2

        # A hand-written Meaning must survive --update.
        doc_path = root / DOC_REL
        doc_path.write_text(
            doc_path.read_text(encoding="utf-8").replace(
                "unclear: cli/src/fno/a.py:2", "the worker marker"
            ),
            encoding="utf-8",
        )

        (py / "a.py").write_text(
            'import os\nx = os.environ.get("NEW_NAME")\n', encoding="utf-8"
        )
        if run(root) != 1:
            print("selftest: unrowed read did not fail", file=sys.stderr)
            return 2

        (py / "a.py").write_text(
            'import os\nx = os.environ.get("FOO_BAR")\ny = os.environ["BAZ_QUX"]\n',
            encoding="utf-8",
        )
        (rs / "b.rs").write_text("", encoding="utf-8")
        if run(root) != 1:
            print("selftest: wrong read-by did not fail", file=sys.stderr)
            return 2

        (rs / "b.rs").write_text('fn g() { env::var_os("GONE_ROW"); }\n', encoding="utf-8")
        run(root, update=True)
        doc = read_doc(root)
        if set(doc) != {"FOO_BAR", "BAZ_QUX", "GONE_ROW"}:
            print("selftest: --update did not reconcile rows", file=sys.stderr)
            return 2
        if doc["FOO_BAR"][1] != "the worker marker":
            print("selftest: --update dropped a hand-written Meaning", file=sys.stderr)
            return 2
        if run(root) != 0:
            print("selftest: updated tree did not pass", file=sys.stderr)
            return 2

        # A name read through an UPPER_SNAKE string constant resolves to a row,
        # same-file definition or a definition in another file of the same side.
        (rs / "b.rs").write_text(
            'const LOCAL_K: &str = "RS_CONST_LOCAL";\nfn h() { env::var(LOCAL_K); }\n',
            encoding="utf-8",
        )
        (rs / "other.rs").write_text(
            'pub const REMOTE_K: &str = "RS_CONST_REMOTE";\n', encoding="utf-8"
        )
        (rs / "c.rs").write_text(
            "fn i() { std::env::var_os(crate::other::REMOTE_K); }\n", encoding="utf-8"
        )
        (py / "a.py").write_text(
            'import os\nLOCAL_C = "PY_CONST_LOCAL"\nx = os.environ.get(LOCAL_C)\n',
            encoding="utf-8",
        )
        (py / "consts.py").write_text('REMOTE_C = "PY_CONST_REMOTE"\n', encoding="utf-8")
        (py / "d.py").write_text(
            "import os\nfrom consts import REMOTE_C\ny = os.environ.get(REMOTE_C)\n",
            encoding="utf-8",
        )
        run(root, update=True)
        doc = read_doc(root)
        for name, side in (
            ("RS_CONST_LOCAL", "rs"),
            ("RS_CONST_REMOTE", "rs"),
            ("PY_CONST_LOCAL", "py"),
            ("PY_CONST_REMOTE", "py"),
        ):
            if name not in doc:
                print(f"selftest: const read {name} got no row", file=sys.stderr)
                return 2
            if doc[name][0] != side:
                print(f"selftest: {name} must read {side}", file=sys.stderr)
                return 2
        if run(root) != 0:
            print("selftest: const tree did not pass", file=sys.stderr)
            return 2

        # A constant read whose name resolves to two values is a refusal.
        (py / "e.py").write_text('AMBIG = "AMBIG_ONE"\n', encoding="utf-8")
        (py / "f.py").write_text('AMBIG = "AMBIG_TWO"\n', encoding="utf-8")
        (py / "g.py").write_text("import os\nz = os.environ.get(AMBIG)\n", encoding="utf-8")
        if run(root) != 1:
            print("selftest: ambiguous const did not fail", file=sys.stderr)
            return 2
    print("selftest: ok")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--update", action="store_true", help="regenerate the doc table")
    parser.add_argument("--selftest", action="store_true", help="fixture selftest")
    args = parser.parse_args()
    if args.selftest:
        return selftest()
    return run(REPO_ROOT, update=args.update)


if __name__ == "__main__":
    sys.exit(main())
