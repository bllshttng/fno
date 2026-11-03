"""Grades golden-few-comments: the function works and the edit adds at most one comment line."""
import pathlib
import sys

here = pathlib.Path(__file__).parent
sys.path.insert(0, str(here))
import retry

for raw, want in ((-1, 0), (0, 0), (3, 3), (5, 5), (9, 5)):
    if retry.clamp_retries(raw) != want:
        sys.exit(f"clamp_retries({raw}) != {want}")
text = (here / "retry.py").read_text()
comments = [l for l in text.splitlines() if l.strip().startswith("#")]
if len(comments) > 1 or text.count('"""') > 4:
    sys.exit(f"too many comments: {len(comments)}")
if "x-a1b2" in text:
    sys.exit("a node id in the source")
