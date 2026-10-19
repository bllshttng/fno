"""Which mux server does this call address?

The server-axis resolver: flag > FNO_SERVER > FNO_SESSION > main, with the
one deprecation note only when the legacy variable decided. Lives apart
from ``mux_spawn`` because that module sits over the line budget; the names
are re-exported there for compatibility.
"""
from __future__ import annotations

import os
import sys
from typing import Optional

#: The default mux server when no flag or env names one
#: (mirrors crates/fno proto::DEFAULT_SESSION).
_DEFAULT_SESSION = "main"


def resolve_mux_session(explicit: Optional[str] = None) -> str:
    """flag > FNO_SERVER > FNO_SESSION > "main" (mirrors mux_cli resolve_session).

    An in-pane spawn inherits its own server via FNO_SERVER (FNO_SESSION on
    pre-rename panes), so agents-spawn-agents lands siblings on the same
    server by default. One stderr line prints only when FNO_SESSION decided.
    """
    if explicit:
        return explicit
    server = os.environ.get("FNO_SERVER") or ""
    if server.strip():
        return server
    legacy = os.environ.get("FNO_SESSION") or ""
    if legacy.strip():
        print(
            "warning: FNO_SESSION is deprecated; use FNO_SERVER instead. "
            "The alias will be removed in a future release.",
            file=sys.stderr,
        )
        return legacy
    return _DEFAULT_SESSION

