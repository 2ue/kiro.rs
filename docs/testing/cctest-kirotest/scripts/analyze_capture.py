#!/usr/bin/env python3
"""只输出脱敏统计；不复制或打印完整抓包正文。"""
import collections
import json
import os
import sys

path = sys.argv[1]
counts = collections.Counter()
statuses = collections.Counter()
routes = collections.Counter()
sse_types = collections.Counter()
line_count = 0
bad_lines = 0
first = last = None
with open(path, "rb", buffering=1024 * 1024) as handle:
    for raw in handle:
        line_count += 1
        try:
            event = json.loads(raw)
        except json.JSONDecodeError:
            bad_lines += 1
            continue
        ts = event.get("ts")
        first = first or ts
        last = ts or last
        kind = event.get("type")
        counts[kind] += 1
        data = event.get("data") or {}
        if kind == "upstream_response":
            statuses[str(data.get("status"))] += 1
        if kind == "client_sse":
            routes[str(data.get("route", "?"))] += 1
            for chunk in str(data.get("text", "")).split("data: "):
                line = chunk.splitlines()[0] if chunk.splitlines() else ""
                try:
                    payload = json.loads(line)
                except json.JSONDecodeError:
                    continue
                event_type = payload.get("type") or (payload.get("delta") or {}).get("type")
                if event_type:
                    sse_types[event_type] += 1
print(json.dumps({
    "file": path,
    "bytes": os.path.getsize(path),
    "lines": line_count,
    "bad_lines": bad_lines,
    "time_range": [first, last],
    "event_counts": dict(counts),
    "upstream_statuses": dict(statuses),
    "client_routes": dict(routes),
    "client_sse_types": dict(sse_types),
}, ensure_ascii=False, indent=2))
