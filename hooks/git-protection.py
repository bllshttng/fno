#!/usr/bin/env python3
"""git-protection guard entry. The body lives in git_protection_impl.py.

Python never caches bytecode for a __main__ script, so the previous
single-file shape recompiled the whole guard on every Bash tool call. As an
imported module the body compiles once into __pycache__ and every later
spawn loads it instead.

Two callers reach this file: the fno-agents binary spawns it, and the test
suite loads it through importlib and reaches guard internals here. The
namespace relay keeps the second working, and the reload on that path keeps
its re-exec semantics: a fresh body exec is how a load sees config written
since the previous load.
"""
import importlib
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import git_protection_impl as _impl

if __name__ == "__main__":
    _impl.main()
else:
    globals().update(
        {
            name: value
            for name, value in vars(importlib.reload(_impl)).items()
            if not (name.startswith("__") and name.endswith("__"))
        }
    )
