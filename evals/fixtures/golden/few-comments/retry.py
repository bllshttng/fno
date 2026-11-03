"""Retry settings."""

MAX_RETRIES = 5


def next_delay(attempt):
    return min(60, 2**attempt)
