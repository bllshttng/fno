def clamp_repeat(requested, cap):
    """Clamp requested to [1, cap]."""
    if requested > cap:
        return cap
    if requested < 1:
        return 1
    return requested
