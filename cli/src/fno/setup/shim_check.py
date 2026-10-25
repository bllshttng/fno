"""fno shims must never dangle into a cleaned mktemp dir, or go missing.
Scans the tool bin for fno* symlinks that dangle or resolve under a temp
root, and for required shims that are absent, and points them at the
durable copy.
"""

from __future__ import annotations

import os
import sys
import tempfile
from pathlib import Path

UV_TOOL_FNO_BIN = Path.home() / ".local" / "share" / "uv" / "tools" / "fno" / "bin"
# `fno doctor update` lands the Rust binaries here, fno-gh-proxy among them;
# the wheel does not ship that one.
CARGO_BIN = Path(os.environ.get("CARGO_HOME") or Path.home() / ".cargo") / "bin"
# Every gh read and every fno command dies when one of these is missing.
REQUIRED_SHIMS = ("fno", "fno-gh-proxy")


def _temp_root() -> str:
    return str(Path(tempfile.gettempdir()).resolve())


def scan(bin_dir: Path | None = None) -> dict:
    """Report every fno* symlink defect (dangling, temp-resolving, absent).

    Scope is the fno prefix, so a broken third-party link is not our finding.
    An absent required shim counts only when a durable copy exists to link:
    a pure-Python install ships no fno binary, and that is not a link defect.
    """
    directory = Path(bin_dir) if bin_dir else Path.home() / ".local" / "bin"
    temp_root = _temp_root()
    # The running venv's bin is the durable copy the repair arm relinks to
    # (see _defect), so a shim resolving there is healthy even when the tool
    # venv sits under the system temp root - the release smoke's UV_TOOL_DIR
    # does exactly that. Resolved: the temp root may carry symlinked ancestors
    # (macOS /var -> /private/var) the lexical comparison would miss.
    venv_bin = Path(sys.executable).parent.resolve()
    defects: list[dict] = []
    for entry in sorted(Path(directory).glob("fno*")):
        if not entry.is_symlink():
            continue
        target = Path(os.readlink(entry))
        if not target.is_absolute():
            target = entry.parent / target
        resolved = target.resolve()
        if not resolved.exists():
            defects.append(_defect(entry, resolved, "dangling"))
        elif resolved.is_relative_to(venv_bin):
            continue
        elif resolved.is_relative_to(Path(temp_root)):
            defects.append(_defect(entry, resolved, "temp-resolving"))
    if Path(directory).is_dir():
        for name in REQUIRED_SHIMS:
            entry = Path(directory) / name
            if entry.exists() or entry.is_symlink():
                continue
            defect = _defect(entry, entry, "absent")
            durable = Path(defect["repair"]).resolve() if defect["repair"] else None
            # A durable copy under the temp root would relink into the very
            # defect the re-scan condemns, failing an install that passed.
            if durable and (durable.is_relative_to(venv_bin) or not durable.is_relative_to(Path(temp_root))):
                defects.append(defect)
    return {"bin_dir": str(directory), "defects": defects, "healthy": not defects}


def _defect(link: Path, resolved: Path, problem: str) -> dict:
    # Prefer the RUNNING venv's bin (the channels invoke through the tool venv
    # python), so a uv store outside the default home still repairs.
    candidates = [Path(sys.executable).parent, UV_TOOL_FNO_BIN, CARGO_BIN]
    durable = next(
        (d / link.name for d in candidates if (d / link.name).is_file() and os.access(d / link.name, os.X_OK)),
        UV_TOOL_FNO_BIN / link.name,
    )
    repairable = durable.is_file() and os.access(durable, os.X_OK)
    return {
        "name": link.name,
        "link": str(link),
        "target": str(resolved),
        "problem": problem,
        "repair": str(durable) if repairable else None,
    }


def repair(defects: list[dict]) -> list[str]:
    """Repoint repairable defects to the durable copy; return what still fails."""
    remaining: list[str] = []
    for defect in defects:
        durable = defect.get("repair")
        if not durable:
            remaining.append(f"{defect['name']}: no durable copy at {durable}")
            continue
        link = Path(defect["link"])
        tmp = link.with_name(f".{link.name}.relink.{os.getpid()}")
        try:
            tmp.symlink_to(durable)
            os.replace(tmp, link)  # atomic: a reader never sees the link absent
        except OSError as exc:
            remaining.append(f"{defect['name']}: relink failed: {exc}")
        finally:
            Path(tmp).unlink(missing_ok=True)
    return remaining


def main(argv: list[str] | None = None) -> int:
    args = list(sys.argv[1:] if argv is None else argv)
    do_repair = "--repair" in args
    bin_dir = None
    if "--bin-dir" in args:
        bin_dir = Path(args[args.index("--bin-dir") + 1])
    report = scan(bin_dir)
    for defect in report["defects"]:
        print(f"shim defect: {defect['name']} -> {defect['target']} ({defect['problem']})")
    if do_repair and report["defects"]:
        for line in repair(report["defects"]):
            print(f"shim unrepairable: {line}")
        report = scan(bin_dir)
    if not report["healthy"]:
        print(f"shim scan: {len(report['defects'])} defect(s) in {report['bin_dir']}; re-run with --repair, or relink to {UV_TOOL_FNO_BIN}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
