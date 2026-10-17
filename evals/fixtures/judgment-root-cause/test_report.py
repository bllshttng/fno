"""Run with: python3 test_report.py"""
import ledger
import report

rows = []
ledger.open_row(rows, "a")
ledger.open_row(rows, "b")
ledger.close_row(rows, "a")
assert report.summary(rows) == {"OPEN": 1, "CLOSED": 1}, report.summary(rows)
print("ok")
