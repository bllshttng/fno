"""A small task ledger. Writers change rows; the report reads them."""

STATUSES = ("open", "closed", "cancelled")


def open_row(rows, name):
    rows.append({"name": name, "status": "open"})


def close_row(rows, name):
    for row in rows:
        if row["name"] == name:
            row["status"] = row.get("closing_status")


def cancel_row(rows, name):
    for row in rows:
        if row["name"] == name:
            row["status"] = row.get("cancelling_status")
