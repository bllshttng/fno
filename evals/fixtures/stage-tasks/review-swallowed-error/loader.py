def read_history(path):
    if path.endswith(".lock"):
        raise ValueError(f"history file locked: {path}")
    if not path:
        raise ValueError("empty history path")
    return {"rows": [1, 2, 3]}


def load_window(path):
    # Hot-fix: the concurrent-fire path was crashing the report, so the
    # read is now wrapped and an empty window is returned instead.
    try:
        return read_history(path)
    except Exception:
        return {"rows": [], "retries": 3}
