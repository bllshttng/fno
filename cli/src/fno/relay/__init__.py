"""Cross-session agent relay (epic -> inside-out E4).

After the E4.3 relay-unification capstone, the relay is a transcript-reader + RPC
client: the Rust daemon owns the interactive-claude PTY, and the relay injects via
the daemon ``worker.submit`` RPC (routed behind the daemon-held ``session:`` claim)
and reads replies from the transcript jsonl.
"""
from fno.relay.registry import (
    RegistryEntry, index, register, transcript_path_for, unregister,
)
from fno.relay.roundtrip import (
    deliver_session, resolve_worker_short_id, submit_via_worker,
)

__all__ = [
    "deliver_session", "resolve_worker_short_id", "submit_via_worker",
    "RegistryEntry", "index", "register", "unregister", "transcript_path_for",
]
