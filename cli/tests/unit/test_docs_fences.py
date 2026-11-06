"""Every documented config fence parses as the language it declares.

`docs/path-config.md` named TOML files and handed the reader YAML to put in
them. `tomllib.loads` on that YAML raises, `_load_raw` swallows the raise, and
`fno config doctor` certified the file. An operator who copied the documented
schema into the documented path got a file that no-ops and a doctor that said
OK.

This walks every fence in `docs/`, so the trap cannot come back through a page
nobody thought to check.
"""
from __future__ import annotations

import tomllib
from pathlib import Path

import yaml

_DOCS = Path(__file__).resolve().parents[3] / "docs"


def _load_yaml(body: str) -> object:
    """Drain every document in ``body``.

    A yaml fence in these docs is often a markdown FRONTMATTER example, which
    is a multi-document stream (`---` opens a second document). ``safe_load``
    refuses that; ``safe_load_all`` is the parser those pages actually declare.
    """
    return list(yaml.safe_load_all(body))


_PARSERS = {"toml": tomllib.loads, "yaml": _load_yaml, "yml": _load_yaml}


def _fences(text: str) -> list[tuple[int, str, str]]:
    """(1-indexed opening line, language, body) for each fence in ``text``."""
    out: list[tuple[int, str, str]] = []
    lines = text.splitlines()
    i = 0
    while i < len(lines):
        line = lines[i]
        if not line.startswith("```"):
            i += 1
            continue
        info = line[3:].strip()
        start = i + 1
        j = start
        while j < len(lines) and not lines[j].startswith("```"):
            j += 1
        if info in _PARSERS:
            out.append((i + 1, info, "\n".join(lines[start:j])))
        i = j + 1
    return out


def _config_fences() -> list[tuple[Path, int, str, str]]:
    found: list[tuple[Path, int, str, str]] = []
    for path in sorted(_DOCS.rglob("*.md")):
        for lineno, lang, body in _fences(path.read_text(encoding="utf-8")):
            found.append((path, lineno, lang, body))
    return found


def test_every_declared_fence_parses_as_its_language() -> None:
    """A toml fence holds TOML and a yaml fence holds YAML, everywhere in docs.

    The count assertion is the positive control on the WALKER: a walker that
    found no files, or no fences, would otherwise pass this test by finding
    nothing to reject.
    """
    fences = _config_fences()
    assert len(fences) > 0, f"walked {_DOCS} and found no toml/yaml fences at all"

    failures: list[str] = []
    for path, lineno, lang, body in fences:
        try:
            _PARSERS[lang](body)
        except Exception as exc:  # noqa: BLE001 - the whole point is the message
            rel = path.relative_to(_DOCS.parent)
            failures.append(f"{rel}:{lineno} declares {lang} but does not parse: {exc}")
    assert not failures, "\n".join(failures)
