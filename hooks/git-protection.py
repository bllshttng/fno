#!/usr/bin/env python3
"""git-protection guard entry. The body lives in git_protection_impl.py.

Python never caches bytecode for a __main__ script, so the previous
single-file shape recompiled the whole guard on every Bash tool call. The
spawn path imports the impl module, whose bytecode compiles once into the
interpreter's cache and every later spawn loads.

The suite also loads this file through importlib and reaches guard
internals, and it monkeypatches functions here. That path execs the impl
body into this module's own namespace, so a patch rebind lands in the same
namespace the functions resolve and every load re-executes the body: the
semantics the single-file shape had.
"""
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

if __name__ == "__main__":
    import git_protection_impl as _impl

    _impl.main()
else:
    _body = os.path.join(
        os.path.dirname(os.path.abspath(__file__)), "git_protection_impl.py"
    )
    with open(_body, encoding="utf-8") as _fh:
        exec(compile(_fh.read(), _body, "exec"), globals())
