#!/usr/bin/env python3
"""Turn a Claude Code transcript into a short, redacted event list for replay.

Usage: extract.py <transcript.jsonl> [OLD=NEW ...] > events.jsonl

Each OLD=NEW pair replaces text, so a name or host never reaches the GIF.

Each output line is {"t": seconds_since_start, "kind": "...", "text": "..."}.
Kinds: prompt, say, tool, done. Only the first line of each message is kept.
"""

import json
import re
import sys
from datetime import datetime

# About what a 24-second replay can show and still be read.
MAX_EVENTS = 40


def parse_ts(value):
    return datetime.fromisoformat(value.replace("Z", "+00:00")).timestamp()


def first_line(text, limit=72):
    line = text.strip().splitlines()[0] if text.strip() else ""
    return line if len(line) <= limit else line[: limit - 3] + "..."


def tool_label(block):
    name = block.get("name", "")
    args = block.get("input", {}) or {}
    if name == "Bash":
        return args.get("description") or first_line(args.get("command", ""))
    if name in ("Edit", "Write", "Read"):
        path = args.get("file_path", "")
        return f"{name} {path.rsplit('/', 1)[-1]}"
    if name == "Skill":
        return f"skill {args.get('skill', '')}"
    return name


def events(lines):
    start = None
    for raw in lines:
        try:
            row = json.loads(raw)
        except json.JSONDecodeError:
            continue
        if row.get("type") not in ("user", "assistant") or "timestamp" not in row:
            continue
        ts = parse_ts(row["timestamp"])
        start = ts if start is None else start
        content = row.get("message", {}).get("content")
        if isinstance(content, str):
            content = [{"type": "text", "text": content}]
        for block in content or []:
            kind = block.get("type")
            if row["type"] == "user" and kind == "text":
                text = block["text"]
                if text.startswith("<") or "tool_result" in text:
                    continue
                yield {"t": ts - start, "kind": "prompt", "text": first_line(text)}
            elif row["type"] == "assistant" and kind == "text":
                yield {"t": ts - start, "kind": "say", "text": first_line(block["text"])}
            elif row["type"] == "assistant" and kind == "tool_use":
                yield {"t": ts - start, "kind": "tool", "text": tool_label(block)}


def thin(evs, cap):
    """Keep every prompt and reply; sample tool calls evenly to fit the cap."""
    tools = [e for e in evs if e["kind"] == "tool"]
    room = cap - (len(evs) - len(tools))
    if len(tools) <= room:
        return evs
    keep = {id(tools[int(i * len(tools) / max(room, 1))]) for i in range(max(room, 0))}
    return [e for e in evs if e["kind"] != "tool" or id(e) in keep]


def main(argv):
    if not argv:
        sys.exit(__doc__)
    swaps = [a.split("=", 1) for a in argv[1:] if "=" in a]
    with open(argv[0]) as f:
        evs = list(events(f))
    for ev in evs:
        for old, new in swaps:
            ev["text"] = ev["text"].replace(old, new)
        ev["text"] = re.sub(r"/Users/[^/\s]+", "~", ev["text"])
    for ev in thin([e for e in evs if e["text"]], MAX_EVENTS):
        print(json.dumps(ev))


if __name__ == "__main__":
    main(sys.argv[1:])
