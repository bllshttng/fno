#!/usr/bin/env python3
"""Measure cross-host mail bus lag from the primary's own stamps.

`fno_mail_bus` stamps `created_at` when a send lands on the primary and
`delivered_at` when the receiving host's daemon pull injects the row.
Both stamps come from the primary's clock (strftime in the INSERT and the
mark), so their difference is the send-to-inject lag with no cross-host
clock skew. Prints per-leg and overall p50/p95 in seconds, cross-host
rows only.

Reads store.remote_url from ~/.fno/config.toml, or pass --url.
"""

import argparse
import json
import re
import sys
import urllib.request
from collections import defaultdict
from pathlib import Path

ROUTE = "/v2/pipeline"

SQL = ("SELECT from_host, to_host, created_at, delivered_at FROM fno_mail_bus "
       "WHERE status = 'delivered' AND delivered_at IS NOT NULL ORDER BY seq")


def store_url(explicit):
    if explicit:
        return explicit
    config = Path.home() / ".fno" / "config.toml"
    match = re.search(r"^\[store\]\s*\n(?:^[^\[]*\n)*?^\s*remote_url\s*=\s*[\"']([^\"']+)[\"']",
                      config.read_text(), re.M)
    if not match:
        sys.exit(f"no [store] remote_url in {config}; pass --url")
    return match.group(1)


def cell(value):
    """One Hrana cell to a python scalar (sqld wraps integers as strings)."""
    if isinstance(value, dict):
        if value.get("type") == "integer":
            return int(value["value"])
        return value.get("value")
    return value


def percentile(sorted_values, pct):
    rank = round(pct / 100 * (len(sorted_values) - 1))
    return sorted_values[max(0, min(len(sorted_values) - 1, rank))]


def query(url, sql):
    body = json.dumps({"baton": None, "requests": [
        {"type": "execute", "stmt": {"sql": sql, "args": []}},
        {"type": "close"},
    ]}).encode()
    request = urllib.request.Request(url.rstrip("/") + ROUTE, data=body,
                                     headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(request, timeout=30) as reply:
        payload = json.load(reply)
    for result in payload.get("results") or []:
        if result.get("type") != "ok":
            message = (result.get("error") or {}).get("message", "statement failed")
            sys.exit(f"store refused: {message}")
        result_set = (result.get("response") or {}).get("result") or {}
        return [[cell(v) for v in row] for row in result_set.get("rows") or []]
    sys.exit("store returned no results")


def selftest():
    assert cell({"type": "integer", "value": "42"}) == 42
    assert cell({"type": "text", "value": "x"}) == "x"
    assert cell(7) == 7
    assert percentile([5], 95) == 5
    assert percentile([1, 2, 3, 4], 50) == 3
    assert percentile([1, 2, 3, 4], 95) == 4
    assert percentile([1, 2], 0) == 1
    print("selftest ok")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--url", help="store base URL (default: store.remote_url)")
    parser.add_argument("--selftest", action="store_true", help="check the math, exit")
    args = parser.parse_args()
    if args.selftest:
        selftest()
        return
    legs = defaultdict(list)
    for from_host, to_host, created_at, delivered_at in query(store_url(args.url), SQL):
        if from_host != to_host:
            legs[(from_host, to_host)].append(delivered_at - created_at)
    if not legs:
        print("no delivered cross-host rows on the bus")
        return
    for (from_host, to_host), values in sorted(legs.items()):
        values.sort()
        print(f"{from_host} -> {to_host}: n={len(values)} "
              f"p50={percentile(values, 50)}s p95={percentile(values, 95)}s")
    overall = sorted(v for values in legs.values() for v in values)
    print(f"overall: n={len(overall)} "
          f"p50={percentile(overall, 50)}s p95={percentile(overall, 95)}s")


if __name__ == "__main__":
    main()
