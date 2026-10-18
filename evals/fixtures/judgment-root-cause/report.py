"""Count ledger rows by status for the daily report."""


def summary(rows):
    counts = {}
    for row in rows:
        key = row["status"].upper()
        counts[key] = counts.get(key, 0) + 1
    return counts
