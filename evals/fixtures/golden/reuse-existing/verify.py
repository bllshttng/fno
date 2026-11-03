"""Grades golden-reuse-existing: report.py calls the slugify that exists."""
import ast
import pathlib
import sys

here = pathlib.Path(__file__).parent
sys.path.insert(0, str(here))
import report

if report.title_key("  Hello, World! ") != "hello-world":
    sys.exit("wrong key")
src = (here / "report.py").read_text()
tree = ast.parse(src)
names = {n.func.id for n in ast.walk(tree) if isinstance(n, ast.Call) and isinstance(n.func, ast.Name)}
attrs = {n.func.attr for n in ast.walk(tree) if isinstance(n, ast.Call) and isinstance(n.func, ast.Attribute)}
if "slugify" not in names | attrs:
    sys.exit("report.py does not call slugify")
if "import re" in src:
    sys.exit("report.py reimplements the slug")
